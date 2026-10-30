pub mod detect;
pub mod discover;
pub mod model;
pub mod trust;
pub mod vars;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use self::discover::{Discovered, Layer, Origin};
use self::model::{BackendKind, ConfigFile, NonoConfig, Platform, SandboxConfig};
use self::vars::Vars;
use crate::context::{self, Dirs};
use crate::orca;
use crate::policy::{Access, Grant, Network, Policy, Source};

const CLAUDE_CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";

/// Subdirectories of Claude's config dir whose entries dotfiles managers commonly link.
const CLAUDE_LINKED_DIRS: &[&str] = &["skills", "agents", "commands", "output-styles"];

#[derive(Debug)]
pub struct Resolved {
    pub agent: Agent,
    pub backend: BackendKind,
    pub policy: Policy,
    pub nono: NonoConfig,
    /// Never one a project file sets: cb follows paths its files name.
    pub claude_config: Option<PathBuf>,
    pub notes: Vec<String>,
    /// Stops a run, but not commands that only inspect it.
    pub blocker: Option<String>,
    pub warnings: Vec<String>,
}

impl Resolved {
    pub fn sandboxed(&self) -> bool {
        self.backend != BackendKind::None
    }
}

#[derive(Debug, Clone)]
pub struct Agent {
    pub name: String,
    pub kind: Option<String>,
    pub command: String,
    pub args: Vec<String>,
}

type Fragment<'a> = (&'a Layer, &'a ConfigFile);

fn fragments(d: &Discovered, platform: Platform) -> Vec<Fragment<'_>> {
    d.layers
        .iter()
        .flat_map(|l| l.file.fragments(platform).into_iter().map(move |f| (l, f)))
        .collect()
}

/// Fragments of the requested profiles from every layer, in the order the profiles were given.
fn profile_fragments<'a>(
    base: &[Fragment<'a>],
    platform: Platform,
    profiles: &[String],
) -> Result<Vec<Fragment<'a>>> {
    let mut out = Vec::new();
    for name in profiles {
        let mut found = false;
        for &(layer, file) in base {
            let Some(profile) = file.profiles.get(name) else {
                continue;
            };
            if profile.root.is_some() || !profile.profiles.is_empty() {
                bail!("{layer}: profile `{name}` cannot set `root` or define profiles");
            }
            found = true;
            out.extend(profile.fragments(platform).into_iter().map(|f| (layer, f)));
        }
        if !found {
            let known = profile_names_in(base);
            let known: Vec<_> = known.iter().map(String::as_str).collect();
            bail!("unknown profile `{name}` (known: {})", known.join(", "));
        }
    }
    Ok(out)
}

pub fn profile_names(d: &Discovered, platform: Platform) -> BTreeSet<String> {
    profile_names_in(&fragments(d, platform))
}

fn profile_names_in(fragments: &[Fragment<'_>]) -> BTreeSet<String> {
    fragments
        .iter()
        .flat_map(|(_, f)| f.profiles.keys().cloned())
        .collect()
}

pub fn agent_names(d: &Discovered, platform: Platform) -> BTreeSet<String> {
    fragments(d, platform)
        .iter()
        .flat_map(|(_, f)| f.agents.keys().cloned())
        .collect()
}

pub fn resolve(
    d: &Discovered,
    dirs: &Dirs,
    platform: Platform,
    agent: Option<&str>,
    profiles: &[String],
) -> Result<Resolved> {
    let base = fragments(d, platform);
    let overlay = profile_fragments(&base, platform, profiles)?;
    let vars = user_vars(&base, &overlay, d, dirs);
    let all = [&base[..], &overlay[..]].concat();
    let name = match agent {
        Some(name) => name.to_owned(),
        None => all
            .iter()
            .rev()
            .find_map(|(_, f)| f.agent.clone())
            .context("no agent configured; pass -A <agent>")?,
    };
    let chain = agent_chain(&name, &all)?;

    let mut builder = Builder {
        vars,
        pin_command_dirs: platform.wsl,
        policy: Policy::default(),
        backend: None,
        nono: NonoConfig::default(),
        claude_config_env: dirs.claude_config.clone(),
        claude_config_layer: None,
        orca: None,
        orca_cli: None,
        notes: Vec::new(),
        blocker: None,
        warnings: Vec::new(),
    };
    builder.builtin_grants(d);
    for layer in &d.layers {
        let nested = nested_sets_approve_detected(&layer.file);
        if nested || (layer.origin != Origin::Global && layer.file.approve_detected.is_some()) {
            builder.warnings.push(format!(
                "{layer}: ignoring `approve_detected`; it only works at the top level of the global config"
            ));
        }
    }

    // Precedence, lowest first: general settings, then each agent from the most basic in the
    // inheritance chain to the selected one, then the same again for profiles. Within each step,
    // later layers win.
    let (mut command, mut kind, mut args) = (None, None, Vec::new());
    for fragments in [&base, &overlay] {
        for &(layer, file) in fragments {
            builder.apply(&file.sandbox, layer);
        }
        for name in &chain {
            for &(layer, file) in fragments {
                let Some(agent) = file.agents.get(name) else {
                    continue;
                };
                command = agent.command.clone().or(command);
                kind = agent.kind.clone().or(kind);
                args.extend(agent.args.iter().cloned());
                builder.apply(&agent.sandbox, layer);
            }
        }
    }
    let command = command.with_context(|| format!("agent `{name}` has no `command`"))?;
    let claude_config = builder.claude_grants(kind.as_deref(), d, dirs);
    builder.orca_grants(dirs);

    Ok(Resolved {
        agent: Agent {
            name,
            kind,
            command,
            args,
        },
        backend: builder.backend.unwrap_or(BackendKind::Nono),
        policy: builder.policy,
        nono: builder.nono,
        claude_config,
        notes: builder.notes,
        blocker: builder.blocker,
        warnings: builder.warnings,
    })
}

fn nested_sets_approve_detected(file: &ConfigFile) -> bool {
    let w = &file.when;
    [&w.linux, &w.macos, &w.wsl]
        .into_iter()
        .flatten()
        .map(|f| &**f)
        .chain(file.profiles.values())
        .any(|f| f.approve_detected.is_some() || nested_sets_approve_detected(f))
}

/// The `inherits` chain for `name`, most basic agent first.
fn agent_chain(name: &str, fragments: &[Fragment<'_>]) -> Result<Vec<String>> {
    let mut chain: Vec<String> = Vec::new();
    let mut current = name.to_owned();
    loop {
        if chain.contains(&current) {
            chain.push(current);
            bail!("agent inheritance cycle: {}", chain.join(" -> "));
        }
        let defs: Vec<_> = fragments
            .iter()
            .filter_map(|(_, f)| f.agents.get(&current))
            .collect();
        if defs.is_empty() {
            let known: BTreeSet<_> = fragments
                .iter()
                .flat_map(|(_, f)| f.agents.keys())
                .collect();
            let known: Vec<_> = known.into_iter().map(String::as_str).collect();
            bail!("unknown agent `{current}` (known: {})", known.join(", "));
        }
        let parent = defs.iter().rev().find_map(|a| a.inherits.clone());
        chain.push(current);
        match parent {
            Some(parent) => current = parent,
            None => break,
        }
    }
    chain.reverse();
    Ok(chain)
}

/// User variables by precedence: presets < project files < global < local files < profiles <
/// environment.
fn user_vars(
    base: &[Fragment<'_>],
    profiles: &[Fragment<'_>],
    d: &Discovered,
    dirs: &Dirs,
) -> Vars {
    let mut user = BTreeMap::new();
    for origin in [
        Origin::Preset,
        Origin::Detected,
        Origin::Project,
        Origin::Global,
        Origin::Local,
    ] {
        for (_, file) in base.iter().filter(|(l, _)| l.origin == origin) {
            user.extend(file.vars.clone());
        }
    }
    for (_, file) in profiles {
        user.extend(file.vars.clone());
    }
    user.extend(vars::env_overrides());
    Vars {
        user,
        project_root: d.project_root.clone(),
        home: dirs.home.clone(),
        cwd: d.cwd.clone(),
    }
}

fn source_of(layer: &Layer) -> Source {
    match (&layer.path, layer.origin) {
        (Some(path), _) => Source::File(path.clone()),
        (None, Origin::Detected) => Source::Detected,
        (None, _) => Source::Preset,
    }
}

struct Builder {
    vars: Vars,
    /// Directories on WSL's 9p `/mnt/*` only match Landlock rules while their dentry stays
    /// cached, so `read_commands` grants there need an fd held open for the agent's lifetime.
    pin_command_dirs: bool,
    policy: Policy,
    backend: Option<BackendKind>,
    nono: NonoConfig,
    claude_config_env: Option<PathBuf>,
    /// The layer that set `CLAUDE_CONFIG_DIR` in config.
    claude_config_layer: Option<(Source, Origin)>,
    orca: Option<bool>,
    orca_cli: Option<bool>,
    notes: Vec<String>,
    blocker: Option<String>,
    warnings: Vec<String>,
}

impl Builder {
    fn builtin_grants(&mut self, d: &Discovered) {
        // Running from ~ (or above it) outside any project must not hand the agent all of $HOME.
        if self.vars.home.starts_with(&d.project_root) {
            self.warnings.push(format!(
                "not granting project root {} because it contains $HOME; \
                 the sandbox profile decides access to the current directory",
                d.project_root.display()
            ));
        } else {
            self.grant(
                d.project_root.clone(),
                Access::ReadWrite,
                Source::Builtin("project root"),
            );
        }
        if let Some(git) = &d.git
            && !git.common_dir.starts_with(&d.project_root)
        {
            self.grant(
                git.common_dir.clone(),
                Access::ReadWrite,
                Source::Builtin("git common dir"),
            );
        }
    }

    fn kind_grants(&mut self, kind: Option<&str>, d: &Discovered) -> Option<PathBuf> {
        if kind != Some("claude") {
            return None;
        }
        if let Some(main) = d.git.as_ref().and_then(|g| g.main_worktree())
            && let Ok(path) = main.join(".claude/settings.local.json").canonicalize()
        {
            self.grant(
                path,
                Access::ReadWrite,
                Source::Builtin("claude settings from main worktree"),
            );
        }
        let config_dir = self.claude_config_dir()?;
        self.claude_symlink_targets(&config_dir);
        Some(config_dir)
    }

    /// Claude's config dir, granted read-write with its lock file when `CLAUDE_CONFIG_DIR` sets
    /// it. `None` when its symlinks must not be followed: a project file could otherwise point
    /// it at a repo directory whose links reach anywhere.
    fn claude_config_dir(&mut self) -> Option<PathBuf> {
        let (raw, source, origin) = match self.policy.env.get(CLAUDE_CONFIG_DIR) {
            Some(value) => {
                let (source, origin) = self.claude_config_layer.clone()?;
                (PathBuf::from(value), source, origin)
            }
            None => match self.claude_config_env.clone() {
                Some(raw) => (raw, Source::Builtin(CLAUDE_CONFIG_DIR), Origin::Global),
                None => return Some(self.vars.home.join(".claude")),
            },
        };
        let entry = format!("{CLAUDE_CONFIG_DIR}={}", raw.display());
        let dir = match raw.canonicalize() {
            Ok(dir) if raw.is_absolute() && dir.is_dir() => dir,
            _ => {
                self.warn(&source, &entry, "not an existing absolute directory");
                return None;
            }
        };
        if self.vars.home.starts_with(&dir) {
            self.warn(&source, &entry, "contains $HOME");
            return None;
        }
        self.grant(dir.clone(), Access::ReadWrite, source.clone());
        let mut lock = dir.clone().into_os_string();
        lock.push(".lock");
        if let Ok(lock) = PathBuf::from(lock).canonicalize() {
            self.grant(lock, Access::ReadWrite, source);
        }
        matches!(origin, Origin::Preset | Origin::Global).then_some(dir)
    }

    /// Landlock checks symlink targets, so dotfiles-managed Claude config needs its targets.
    /// Read-only: the targets are often a dotfiles repo.
    fn claude_symlink_targets(&mut self, config_dir: &Path) {
        let Ok(config_dir) = config_dir.canonicalize() else {
            return;
        };
        let home = self.vars.home.clone();
        let mut targets = BTreeSet::new();
        let dirs = std::iter::once(config_dir.clone())
            .chain(CLAUDE_LINKED_DIRS.iter().map(|d| config_dir.join(d)));
        for dir in dirs {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_symlink())
                    && let Ok(target) = entry.path().canonicalize()
                    && !target.starts_with(&config_dir)
                    && !home.starts_with(&target)
                {
                    targets.insert(target);
                }
            }
        }
        for target in compact(targets, &home) {
            self.grant(
                target,
                Access::Read,
                Source::Builtin("claude config symlink"),
            );
        }
    }

    fn claude_grants(
        &mut self,
        kind: Option<&str>,
        d: &Discovered,
        dirs: &Dirs,
    ) -> Option<PathBuf> {
        let account = self
            .orca_pane(dirs)
            .filter(|_| kind == Some("claude"))
            .filter(|_| !self.policy.env.contains_key(CLAUDE_CONFIG_DIR))
            .map(|pane| &pane.claude_account);
        match account {
            Some(orca::Account::Dir(dir)) if dir.is_dir() => {
                let dir = dir.to_string_lossy().into_owned();
                self.policy.env.insert(CLAUDE_CONFIG_DIR.into(), dir);
                self.claude_config_layer = Some((Source::Builtin("Orca account"), Origin::Global));
            }
            Some(orca::Account::Default) => {
                self.claude_config_env = None;
                if dirs.claude_config.is_some() {
                    self.policy.env_remove.push(CLAUDE_CONFIG_DIR.into());
                }
            }
            // Blocked below; `env` still names the previous account.
            Some(orca::Account::Dir(_) | orca::Account::Invalid(_)) => {
                self.claude_config_env = None;
            }
            Some(orca::Account::Unmanaged) | None => {}
        }
        let claude_config = self.kind_grants(kind, d);
        self.blocker = match account {
            Some(orca::Account::Dir(dir)) if !dir.is_dir() => {
                let missing = format!(
                    "Orca's selected Claude account folder {} is missing; sign in to it again or \
                     choose another account in Orca",
                    dir.display()
                );
                self.warnings.push(missing.clone());
                Some(missing)
            }
            Some(orca::Account::Invalid(e)) => {
                self.warnings.push(e.clone());
                Some(e.clone())
            }
            Some(orca::Account::Dir(dir)) if claude_config.is_none() => Some(format!(
                "cannot grant Orca's selected Claude account folder {}",
                dir.display()
            )),
            Some(orca::Account::Dir(dir)) => {
                self.notes.push(format!(
                    "Claude Code uses the account selected in Orca, {} (read-write); it follows \
                     Orca's account switcher",
                    dir.display()
                ));
                None
            }
            _ => None,
        };
        claude_config
    }

    fn orca_pane<'a>(&self, dirs: &'a Dirs) -> Option<&'a orca::Pane> {
        dirs.orca.as_ref().filter(|_| self.orca != Some(false))
    }

    fn orca_grants(&mut self, dirs: &Dirs) {
        if let Some(pane) = self.orca_pane(dirs) {
            self.orca_hook_grants(pane);
        }
        // Independent of `orca`, which a project file can set.
        let Some(pane) = dirs.orca.as_ref() else {
            return;
        };
        if self.orca_cli == Some(true)
            && let Some(note) = self.orca_cli_grants(pane)
        {
            self.notes.push(note);
        }
    }

    /// Read-only: unsandboxed panes run these scripts and source the endpoint file.
    fn orca_hook_grants(&mut self, pane: &orca::Pane) {
        let dirs = [
            (Some(&pane.scripts), "Orca hook scripts"),
            (pane.endpoint_dir.as_ref(), "Orca hook endpoint"),
        ];
        let mut granted = false;
        for (dir, what) in dirs {
            if let Some(Ok(path)) = dir.map(|d| d.canonicalize())
                && !self.vars.home.starts_with(&path)
            {
                self.grant(path, Access::Read, Source::Builtin(what));
                granted = true;
            }
        }
        if !granted {
            return;
        }
        if self.policy.network == Network::Block
            && let Some(port) = pane.port
        {
            extend_unique(&mut self.policy.open_ports, &[port]);
        }
        self.notes.push(
            "in an Orca pane: read access to Orca's hook scripts and endpoint, so Orca shows the \
             agent's status; turn off with `sandbox.orca = false`"
                .to_owned(),
        );
    }

    fn orca_cli_grants(&mut self, pane: &orca::Pane) -> Option<String> {
        let cli = pane.cli(&self.vars.home);
        let source = Source::Builtin("Orca CLI");
        let Some(socket) = cli.socket else {
            self.warn(&source, "orca_cli", "cannot find Orca's runtime socket");
            return None;
        };
        for path in cli.metadata.into_iter().chain(cli.dirs) {
            self.grant(path, Access::Read, source.clone());
        }
        let note = format!(
            "`sandbox.orca_cli` lets the agent connect to Orca's socket {}: it can control every \
             Orca terminal, including unsandboxed shells",
            socket.display()
        );
        self.policy.sockets.push(socket);
        Some(note)
    }

    /// `path` must exist: whether it is a file decides the kind of grant.
    fn grant(&mut self, path: PathBuf, access: Access, source: Source) {
        self.policy.grant(Grant {
            file: !path.is_dir(),
            path,
            access,
            pin: false,
            source,
        });
    }

    fn apply(&mut self, sandbox: &SandboxConfig, layer: &Layer) {
        let source = source_of(layer);
        if sandbox.backend.is_some() {
            self.backend = sandbox.backend;
        }
        if sandbox.orca.is_some() {
            self.orca = sandbox.orca;
        }
        // Gives control of unsandboxed Orca terminals, so a project file must not turn it on.
        if sandbox.orca_cli.is_some() {
            if layer.origin == Origin::Global {
                self.orca_cli = sandbox.orca_cli;
            } else {
                self.warnings.push(format!(
                    "{layer}: ignoring `orca_cli`; it only works in the global config"
                ));
            }
        }
        for (entries, access) in [
            (&sandbox.read, Access::Read),
            (&sandbox.write, Access::Write),
            (&sandbox.allow, Access::ReadWrite),
        ] {
            for raw in entries {
                match self.vars.resolve_path(raw, &layer.dir).and_then(|p| {
                    p.canonicalize()
                        .map_err(|e| format!("{}: {e}", p.display()))
                }) {
                    Ok(path) => self.grant(path, access, source.clone()),
                    Err(e) => self.warn(&source, raw, &e),
                }
            }
        }
        for cmd in &sandbox.read_commands {
            match context::which(cmd).and_then(|p| p.parent().map(Path::to_path_buf)) {
                Some(path) => self.policy.grant(Grant {
                    path,
                    access: Access::Read,
                    file: false,
                    pin: self.pin_command_dirs,
                    source: source.clone(),
                }),
                None => self.warn(&source, cmd, "not found on $PATH"),
            }
        }

        for (key, value) in &sandbox.env {
            match self.vars.expand(value, &layer.dir) {
                Ok(value) => {
                    if key == CLAUDE_CONFIG_DIR {
                        self.claude_config_layer = Some((source.clone(), layer.origin));
                    }
                    self.policy.env.insert(key.clone(), value);
                }
                Err(e) => self.warn(&source, key, &e),
            }
        }
        let p = &mut self.policy;
        if let Some(network) = sandbox.network {
            p.network = network;
        }
        extend_unique(&mut p.open_ports, &sandbox.open_ports);
        extend_unique(&mut p.listen_ports, &sandbox.listen_ports);
        p.extra_args.extend(sandbox.extra_args.iter().cloned());

        let nono = &sandbox.nono;
        if nono.mode.is_some() {
            self.nono.mode = nono.mode;
        }
        if nono.profile.is_some() {
            self.nono.profile.clone_from(&nono.profile);
        }
        if nono.explain_denials.is_some() {
            self.nono.explain_denials = nono.explain_denials;
        }
        if nono.herdr.is_some() {
            self.nono.herdr = nono.herdr;
        }
        extend_unique(&mut self.nono.extends, &nono.extends);
        extend_unique(&mut self.nono.groups, &nono.groups);
    }

    fn warn(&mut self, source: &Source, entry: &str, reason: &str) {
        // Caches the user's toolchain hasn't created yet.
        if *source == Source::Detected {
            return;
        }
        self.warnings
            .push(format!("{source}: skipping `{entry}`: {reason}"));
    }
}

/// Replaces every target set covering all entries of a directory with the directory itself:
/// the same paths at scan time, in one grant. Never widens to `home` or above.
fn compact(mut targets: BTreeSet<PathBuf>, home: &Path) -> BTreeSet<PathBuf> {
    let parents: BTreeSet<PathBuf> = targets
        .iter()
        .filter_map(|t| t.parent().map(Path::to_path_buf))
        .filter(|p| !home.starts_with(p))
        .collect();
    for parent in parents {
        let Ok(entries) = fs::read_dir(&parent) else {
            continue;
        };
        let entries: Option<Vec<PathBuf>> = entries.map(|e| e.ok().map(|e| e.path())).collect();
        if let Some(entries) = entries
            && entries.len() > 1
            && entries.iter().all(|e| targets.contains(e))
        {
            targets.retain(|t| t.parent() != Some(&parent));
            targets.insert(parent);
        }
    }
    targets
}

fn extend_unique<T: PartialEq + Clone>(into: &mut Vec<T>, from: &[T]) {
    for item in from {
        if !into.contains(item) {
            into.push(item.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::config::discover::{LOCAL_FILE, PROJECT_FILE, discover};
    use crate::policy::Network;

    struct Fixture {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        dirs: Dirs,
    }

    impl Fixture {
        fn new(global: &str) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().canonicalize().unwrap();
            let dirs = Dirs {
                home: home.clone(),
                config: home.join("config"),
                state: home.join("state"),
                runtime: home.join("run"),
                claude_config: None,
                orca: None,
            };
            fs::create_dir_all(&dirs.config).unwrap();
            fs::write(dirs.global_config(), global).unwrap();
            Self {
                _tmp: tmp,
                home,
                dirs,
            }
        }

        fn write(&self, rel: &str, content: &str) -> PathBuf {
            let path = self.home.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
            path
        }

        fn resolve(&self, cwd: &str, agent: Option<&str>) -> Result<Resolved> {
            self.resolve_profiles(cwd, agent, &[])
        }

        fn resolve_profiles(
            &self,
            cwd: &str,
            agent: Option<&str>,
            profiles: &[&str],
        ) -> Result<Resolved> {
            let mut d = discover(&self.home.join(cwd), &self.dirs, false)?;
            d.detect(&self.dirs, false)?;
            let profiles: Vec<String> = profiles
                .iter()
                .map(std::string::ToString::to_string)
                .collect();
            resolve(&d, &self.dirs, Platform::default(), agent, &profiles)
        }
    }

    fn grants(r: &Resolved) -> Vec<(String, Access)> {
        r.policy
            .grants
            .iter()
            .map(|g| {
                (
                    g.path.file_name().unwrap().to_string_lossy().into(),
                    g.access,
                )
            })
            .collect()
    }

    #[test]
    fn defaults_to_claude_preset() {
        let f = Fixture::new("");
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        let r = f.resolve("repo", None).unwrap();
        assert_eq!(r.agent.name, "claude");
        assert_eq!(r.agent.kind.as_deref(), Some("claude"));
        assert_eq!(r.backend, BackendKind::Nono);
        assert_eq!(r.nono.profile.as_deref(), Some("cb:claude"));
        assert_eq!(grants(&r), [("repo".into(), Access::ReadWrite)]);
    }

    #[test]
    fn detected_monorepo_toolchains_grant_caches() {
        let f = Fixture::new("");
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        fs::create_dir_all(f.home.join(".m2/repository")).unwrap();
        f.write("repo/services/api/pom.xml", "");
        let r = f.resolve("repo", None).unwrap();
        assert!(r.nono.groups.contains(&"java_runtime".to_owned()));
        let cache = r
            .policy
            .grants
            .iter()
            .find(|g| g.path == f.home.join(".m2/repository"))
            .unwrap();
        assert_eq!(cache.access, Access::ReadWrite);
        assert_eq!(cache.source, Source::Detected);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn approve_detected_is_ignored_outside_global_top_level() {
        let f = Fixture::new("[profiles.p]\napprove_detected = true");
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        f.write("repo/.clankerbox.toml", "approve_detected = true");
        let d = discover(&f.home.join("repo"), &f.dirs, false).unwrap();
        assert!(!d.approve_detected());
        let r = f.resolve("repo", None).unwrap();
        let ignored: Vec<_> = r
            .warnings
            .iter()
            .filter(|w| w.contains("ignoring `approve_detected`"))
            .collect();
        assert_eq!(ignored.len(), 2, "{:?}", r.warnings);

        let f = Fixture::new("approve_detected = true");
        let d = discover(&f.home, &f.dirs, false).unwrap();
        assert!(d.approve_detected());
    }

    #[test]
    fn merges_layers_with_var_overrides() {
        let f = Fixture::new(
            r#"
            [vars]
            projects = "~/elsewhere"
            [agents.claude]
            args = ["--global"]
            "#,
        );
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        fs::create_dir_all(f.home.join("elsewhere/frontend")).unwrap();
        fs::create_dir_all(f.home.join("shared")).unwrap();
        f.write("repo/notes.txt", "");
        f.write(
            &format!("repo/{PROJECT_FILE}"),
            r#"
            [vars]
            projects = ".."
            [sandbox]
            read = ["${projects}/frontend", "notes.txt", "../missing"]
            network = "block"
            nono.groups = ["node_runtime"]
            [agents.claude]
            args = ["--project"]
            "#,
        );
        f.write(
            &format!("repo/{LOCAL_FILE}"),
            r#"
            [sandbox]
            allow = ["${home}/shared"]
            nono.profile = "only-a-default"
            [agents.claude]
            sandbox.nono.profile = "mine"
            "#,
        );

        let r = f.resolve("repo", None).unwrap();
        assert_eq!(r.agent.args, ["--global", "--project"]);
        assert_eq!(
            grants(&r),
            [
                ("repo".into(), Access::ReadWrite),
                ("frontend".into(), Access::Read),
                ("notes.txt".into(), Access::Read),
                ("shared".into(), Access::ReadWrite),
            ]
        );
        assert!(r.policy.grants.iter().any(|g| g.file));
        assert_eq!(r.policy.network, Network::Block);
        assert_eq!(r.nono.profile.as_deref(), Some("mine"));
        assert_eq!(r.nono.groups, ["node_runtime"]);
        // A general profile only applies to agents that don't set their own.
        let r = f.resolve("repo", Some("codex")).unwrap();
        assert_eq!(r.nono.profile.as_deref(), Some("only-a-default"));
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("../missing"));
    }

    #[test]
    fn agent_inheritance_and_override() {
        let f = Fixture::new(
            r#"
            [agents.claude-personal]
            inherits = "claude"
            sandbox.env = { CLAUDE_CONFIG_DIR = "/x" }
            "#,
        );
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        f.write(
            &format!("repo/{PROJECT_FILE}"),
            "agent = \"claude-personal\"\n[agents.claude]\nargs = [\"--model\", \"opus\"]",
        );
        let r = f.resolve("repo", None).unwrap();
        assert_eq!(r.agent.name, "claude-personal");
        assert_eq!(r.agent.command, "claude");
        assert_eq!(r.agent.kind.as_deref(), Some("claude"));
        assert_eq!(r.agent.args, ["--model", "opus"]);
        assert_eq!(r.policy.env["CLAUDE_CONFIG_DIR"], "/x");

        let r = f.resolve("repo", Some("pi")).unwrap();
        assert_eq!(r.agent.command, "pi");
        assert_eq!(r.agent.args, [] as [std::string::String; 0]);

        let err = f.resolve("repo", Some("nope")).unwrap_err();
        assert!(err.to_string().contains("unknown agent `nope`"), "{err}");
    }

    #[test]
    fn derived_agent_beats_later_parent_overrides() {
        let f = Fixture::new(
            r#"
            [agents.mine]
            inherits = "claude"
            command = "wrapper"
            sandbox.nono.profile = "custom"
            "#,
        );
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        f.write(
            &format!("repo/{PROJECT_FILE}"),
            "[agents.claude]\ncommand = \"claude2\"\nsandbox.nono.profile = \"p\"",
        );
        let r = f.resolve("repo", Some("mine")).unwrap();
        assert_eq!(r.agent.command, "wrapper");
        assert_eq!(r.nono.profile.as_deref(), Some("custom"));
        assert_eq!(f.resolve("repo", None).unwrap().agent.command, "claude2");
    }

    #[test]
    fn explain_denials_is_overridden_by_later_layers_only_when_set() {
        let f = Fixture::new("[sandbox.nono]\nexplain_denials = false");
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        f.write(&format!("repo/{PROJECT_FILE}"), "[sandbox]\nread = []");
        assert_eq!(
            f.resolve("repo", None).unwrap().nono.explain_denials,
            Some(false)
        );

        f.write(
            &format!("repo/{LOCAL_FILE}"),
            "[agents.claude]\nsandbox.nono.explain_denials = true",
        );
        assert_eq!(
            f.resolve("repo", None).unwrap().nono.explain_denials,
            Some(true)
        );
        assert_eq!(
            f.resolve("repo", Some("pi")).unwrap().nono.explain_denials,
            Some(false)
        );
    }

    #[test]
    fn herdr_is_overridden_by_later_layers_only_when_set() {
        let f = Fixture::new("[sandbox.nono]\nherdr = false");
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        f.write(&format!("repo/{PROJECT_FILE}"), "[sandbox]\nread = []");
        assert_eq!(f.resolve("repo", None).unwrap().nono.herdr, Some(false));

        f.write(
            &format!("repo/{LOCAL_FILE}"),
            "[agents.claude]\nsandbox.nono.herdr = true",
        );
        assert_eq!(f.resolve("repo", None).unwrap().nono.herdr, Some(true));
        assert_eq!(
            f.resolve("repo", Some("pi")).unwrap().nono.herdr,
            Some(false)
        );
    }

    #[test]
    fn orca_hook_grants_never_cover_home_and_note_only_what_is_granted() {
        let mut f = Fixture::new("");
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        fs::create_dir_all(f.home.join(".orca")).unwrap();
        std::os::unix::fs::symlink(&f.home, f.home.join(".orca/agent-hooks")).unwrap();
        f.dirs.orca = Some(orca::Pane {
            scripts: f.home.join(".orca/agent-hooks"),
            port: Some(4000),
            ..orca::Pane::default()
        });
        f.write(
            &format!("repo/{LOCAL_FILE}"),
            "[sandbox]\nnetwork = \"block\"",
        );
        let r = f.resolve("repo", None).unwrap();
        assert!(!r.policy.grants.iter().any(|g| g.path == f.home));
        assert!(r.notes.is_empty(), "{:?}", r.notes);
        assert_eq!(r.policy.open_ports, Vec::<u16>::new());

        fs::remove_file(f.home.join(".orca/agent-hooks")).unwrap();
        fs::create_dir_all(f.home.join(".orca/agent-hooks")).unwrap();
        let r = f.resolve("repo", None).unwrap();
        assert!(r.notes.iter().any(|n| n.contains("Orca's hook scripts")));
        assert_eq!(r.policy.open_ports, [4000]);
    }

    #[test]
    fn orca_cli_only_from_global_config() {
        let f = Fixture::new("");
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        f.write(
            &format!("repo/{PROJECT_FILE}"),
            "[sandbox]\norca_cli = true",
        );
        f.write(
            &format!("repo/{LOCAL_FILE}"),
            "[profiles.cli.sandbox]\norca_cli = true",
        );
        let r = f.resolve_profiles("repo", None, &["cli"]).unwrap();
        assert_eq!(
            r.warnings
                .iter()
                .filter(|w| w.contains("ignoring `orca_cli`"))
                .count(),
            2,
            "{:?}",
            r.warnings
        );

        let f = Fixture::new("[profiles.cli.sandbox]\norca_cli = true");
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        let r = f.resolve_profiles("repo", None, &["cli"]).unwrap();
        assert!(!r.warnings.iter().any(|w| w.contains("orca_cli")));
    }

    #[test]
    fn orca_account_yields_to_config_and_applies_to_claude_only() {
        let mut f = Fixture::new("");
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        fs::create_dir_all(f.home.join(".orca/agent-hooks")).unwrap();
        let account = f.home.join("orca/b");
        f.dirs.orca = Some(orca::Pane {
            scripts: f.home.join(".orca/agent-hooks"),
            claude_account: orca::Account::Dir(account.clone()),
            ..orca::Pane::default()
        });
        let hooks = (".orca/agent-hooks", Access::Read);

        let missing = f.resolve("repo", None).unwrap();
        let blocker = missing.blocker.unwrap();
        assert!(blocker.contains("is missing"), "{blocker}");
        assert!(!missing.policy.env.contains_key("CLAUDE_CONFIG_DIR"));
        assert!(!account.exists());
        fs::create_dir_all(&account).unwrap();

        let r = f.resolve("repo", None).unwrap();
        assert_eq!(r.blocker, None);
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains("account selected in Orca"))
        );
        assert_eq!(r.policy.env["CLAUDE_CONFIG_DIR"], account.to_str().unwrap());
        assert!(r.policy.grants.iter().any(|g| g.path == account
            && g.access == Access::ReadWrite
            && g.source == Source::Builtin("Orca account")));
        assert!(r.policy.grants.iter().any(|g| g.path.ends_with(hooks.0)
            && g.access == hooks.1
            && g.source == Source::Builtin("Orca hook scripts")));

        let pi = f.resolve("repo", Some("pi")).unwrap();
        assert!(pi.policy.env.is_empty());
        assert!(pi.policy.grants.iter().any(|g| g.path.ends_with(hooks.0)));

        f.write(
            &format!("repo/{LOCAL_FILE}"),
            "[sandbox]\nenv = { CLAUDE_CONFIG_DIR = \"${home}/mine\" }",
        );
        fs::create_dir_all(f.home.join("mine")).unwrap();
        let r = f.resolve("repo", None).unwrap();
        assert_eq!(
            r.policy.env["CLAUDE_CONFIG_DIR"],
            f.home.join("mine").to_str().unwrap()
        );
        assert!(!r.policy.grants.iter().any(|g| g.path == account));
        fs::remove_file(f.home.join(format!("repo/{LOCAL_FILE}"))).unwrap();

        f.dirs.orca.as_mut().unwrap().claude_account = orca::Account::Invalid("bad".into());
        assert_eq!(
            f.resolve("repo", None).unwrap().blocker.as_deref(),
            Some("bad")
        );
        assert_eq!(f.resolve("repo", Some("pi")).unwrap().blocker, None);
    }

    #[test]
    fn claude_config_dir_from_config_beats_environment() {
        let mut f = Fixture::new(
            "[agents.claude]\nsandbox.env = { CLAUDE_CONFIG_DIR = \"${home}/.claude-personal\" }",
        );
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        fs::create_dir_all(f.home.join(".claude-personal")).unwrap();
        fs::create_dir_all(f.home.join(".claude-env")).unwrap();
        f.write(".claude-personal.lock", "");
        f.dirs.claude_config = Some(f.home.join(".claude-env"));

        let r = f.resolve("repo", None).unwrap();
        assert_eq!(
            grants(&r),
            [
                ("repo".into(), Access::ReadWrite),
                (".claude-personal".into(), Access::ReadWrite),
                (".claude-personal.lock".into(), Access::ReadWrite),
            ]
        );
        assert!(r.policy.grants[2].file);

        assert_eq!(grants(&f.resolve("repo", Some("pi")).unwrap()).len(), 1);

        f.write("config/config.toml", "");
        let r = f.resolve("repo", None).unwrap();
        assert_eq!(grants(&r)[1], (".claude-env".into(), Access::ReadWrite));

        for bad in [".claude-missing", "relative"] {
            f.dirs.claude_config = Some(if bad == "relative" {
                PathBuf::from("relative")
            } else {
                f.home.join(bad)
            });
            let r = f.resolve("repo", None).unwrap();
            assert_eq!(grants(&r).len(), 1, "{bad}");
            assert!(
                r.warnings.iter().any(|w| w.contains("CLAUDE_CONFIG_DIR")),
                "{bad}"
            );
        }
    }

    #[test]
    fn claude_config_symlinks_grant_targets_read_only() {
        let f = Fixture::new("");
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        let link = |target: &str, link: &str| {
            let link = f.home.join(link);
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(f.home.join(target), link).unwrap();
        };
        f.write("dotfiles/settings.json", "{}");
        f.write(".claude/real.md", "");
        for skill in ["a", "b"] {
            fs::create_dir_all(f.home.join("dotfiles/skills").join(skill)).unwrap();
            link(
                &format!("dotfiles/skills/{skill}"),
                &format!(".claude/skills/{skill}"),
            );
        }
        fs::create_dir_all(f.home.join("other/c")).unwrap();
        fs::create_dir_all(f.home.join("other/unlinked")).unwrap();
        link("dotfiles/settings.json", ".claude/settings.json");
        link("other/c", ".claude/agents/c");
        link(".claude/real.md", ".claude/inside.md");
        link("nowhere", ".claude/dangling");

        let r = f.resolve("repo", None).unwrap();
        let mut found: Vec<_> = r
            .policy
            .grants
            .iter()
            .filter(|g| g.source == Source::Builtin("claude config symlink"))
            .map(|g| {
                assert_eq!(g.access, Access::Read);
                g.path
                    .strip_prefix(&f.home)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        found.sort();
        assert_eq!(
            found,
            ["dotfiles/settings.json", "dotfiles/skills", "other/c"]
        );
        assert_eq!(grants(&f.resolve("repo", Some("pi")).unwrap()).len(), 1);
    }

    #[test]
    fn project_claude_config_dir_is_attributed_and_its_symlinks_ignored() {
        let f = Fixture::new("");
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        fs::create_dir_all(f.home.join("repo/cfg")).unwrap();
        f.write(".ssh/id_ed25519", "secret");
        std::os::unix::fs::symlink(f.home.join(".ssh/id_ed25519"), f.home.join("repo/cfg/key"))
            .unwrap();
        let project = f.write(
            &format!("repo/{PROJECT_FILE}"),
            "[sandbox]\nenv = { CLAUDE_CONFIG_DIR = \"${project_root}/cfg\" }",
        );

        let r = f.resolve("repo", None).unwrap();
        let cfg = r
            .policy
            .grants
            .iter()
            .find(|g| g.path.ends_with("cfg"))
            .unwrap();
        assert_eq!(cfg.source, Source::File(project.clone()));
        assert!(
            !r.policy
                .grants
                .iter()
                .any(|g| g.path.ends_with("id_ed25519")),
            "{:?}",
            r.policy.grants
        );

        f.write(
            &format!("repo/{PROJECT_FILE}"),
            "[sandbox]\nenv = { CLAUDE_CONFIG_DIR = \"${home}\" }",
        );
        let r = f.resolve("repo", None).unwrap();
        assert_eq!(grants(&r), [("repo".into(), Access::ReadWrite)]);
        assert!(
            r.warnings.iter().any(|w| w.contains("contains $HOME")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn claude_symlinks_never_reach_home_and_need_a_valid_config_dir() {
        let mut f = Fixture::new("");
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        fs::create_dir_all(f.home.join(".claude")).unwrap();
        std::os::unix::fs::symlink(&f.home, f.home.join(".claude/ws")).unwrap();
        std::os::unix::fs::symlink("/", f.home.join(".claude/root")).unwrap();
        f.write("dotfiles/CLAUDE.md", "");
        std::os::unix::fs::symlink(
            f.home.join("dotfiles/CLAUDE.md"),
            f.home.join(".claude/CLAUDE.md"),
        )
        .unwrap();
        assert_eq!(
            grants(&f.resolve("repo", None).unwrap()),
            [
                ("repo".into(), Access::ReadWrite),
                ("CLAUDE.md".into(), Access::Read),
            ]
        );

        f.dirs.claude_config = Some(f.home.join(".claude-missing"));
        assert_eq!(
            grants(&f.resolve("repo", None).unwrap()),
            [("repo".into(), Access::ReadWrite)]
        );
    }

    #[test]
    fn compacting_never_widens_to_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        fs::create_dir(home.join("a")).unwrap();
        fs::create_dir(home.join("b")).unwrap();
        let targets = BTreeSet::from([home.join("a"), home.join("b")]);
        assert_eq!(compact(targets.clone(), &home), targets);
    }

    #[test]
    fn linux_presets_add_temp_read_group() {
        let f = Fixture::new("");
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        let mut d = discover(&f.home.join("repo"), &f.dirs, false).unwrap();
        d.detect(&f.dirs, false).unwrap();
        let linux = Platform {
            linux: true,
            ..Platform::default()
        };
        let r = resolve(&d, &f.dirs, linux, None, &[]).unwrap();
        assert_eq!(r.nono.groups, ["linux_temp_read"]);
        let r = resolve(&d, &f.dirs, Platform::default(), None, &[]).unwrap();
        assert_eq!(r.nono.groups, [] as [String; 0]);
    }

    #[test]
    fn inheritance_cycles_are_errors() {
        let f = Fixture::new(
            "[agents.a]\ninherits = \"b\"\n[agents.b]\ninherits = \"a\"\ncommand = \"x\"",
        );
        let err = f.resolve(".", Some("a")).unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
    }

    #[test]
    fn home_is_never_granted_as_project_root() {
        let f = Fixture::new("");
        let r = f.resolve(".", None).unwrap();
        assert!(r.policy.grants.is_empty(), "{:?}", r.policy.grants);
        assert!(r.warnings[0].contains("contains $HOME"), "{:?}", r.warnings);
    }

    #[test]
    fn profiles_overlay_every_layer() {
        let f = Fixture::new(
            r#"
            [agents.claude]
            sandbox.nono.profile = "nclaude"
            [agents.pi]
            sandbox.nono.profile = "npi"

            [profiles.personal.agents.claude]
            sandbox.nono.profile = "nclaude-personal"
            sandbox.env = { CLAUDE_CONFIG_DIR = "${home}/.claude-personal" }
            [profiles.personal.agents.pi]
            sandbox.nono.profile = "npi-personal"
            "#,
        );
        fs::create_dir_all(f.home.join("repo/.git")).unwrap();
        f.write(
            &format!("repo/{PROJECT_FILE}"),
            "[agents.claude]\nsandbox.nono.profile = \"project\"",
        );

        let r = f.resolve("repo", None).unwrap();
        assert_eq!(r.nono.profile.as_deref(), Some("project"));

        let r = f.resolve_profiles("repo", None, &["personal"]).unwrap();
        assert_eq!(r.nono.profile.as_deref(), Some("nclaude-personal"));
        let expected = f.home.join(".claude-personal");
        assert_eq!(
            r.policy.env["CLAUDE_CONFIG_DIR"],
            expected.to_str().unwrap()
        );

        let r = f
            .resolve_profiles("repo", Some("pi"), &["personal"])
            .unwrap();
        assert_eq!(r.nono.profile.as_deref(), Some("npi-personal"));
        assert!(r.policy.env.is_empty());

        let err = f.resolve_profiles("repo", None, &["nope"]).unwrap_err();
        assert!(err.to_string().contains("known: personal"), "{err}");
    }

    #[test]
    fn worktree_grants_common_dir_and_claude_settings() {
        let f = Fixture::new("");
        let main = f.home.join("main");
        let meta = main.join(".git/worktrees/wt");
        fs::create_dir_all(&meta).unwrap();
        fs::write(meta.join("commondir"), "../..").unwrap();
        f.write("main/.claude/settings.local.json", "{}");
        f.write("wt/.git", &format!("gitdir: {}", meta.display()));

        let r = f.resolve("wt", None).unwrap();
        let sources: Vec<_> = r
            .policy
            .grants
            .iter()
            .map(|g| g.source.to_string())
            .collect();
        assert_eq!(
            sources,
            [
                "built-in: project root",
                "built-in: git common dir",
                "built-in: claude settings from main worktree",
            ]
        );
        assert_eq!(r.policy.grants[1].path, main.join(".git"));
        assert!(Path::new(&r.policy.grants[2].path).ends_with("settings.local.json"));
    }
}
