pub mod discover;
pub mod model;
pub mod trust;
pub mod vars;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use self::discover::{Discovered, Layer, Origin};
use self::model::{BackendKind, ConfigFile, NonoConfig, Platform, SandboxConfig};
use self::vars::Vars;
use crate::context::{self, Dirs};
use crate::policy::{Access, Grant, Policy, Source};

#[derive(Debug)]
pub struct Resolved {
    pub agent: Agent,
    pub backend: BackendKind,
    pub policy: Policy,
    pub nono: NonoConfig,
    pub warnings: Vec<String>,
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
        warnings: Vec::new(),
    };
    builder.builtin_grants(d);

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
    builder.kind_grants(kind.as_deref(), d);

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
        warnings: builder.warnings,
    })
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
    match &layer.path {
        Some(path) => Source::File(path.clone()),
        None => Source::Preset,
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
            self.grant_dir(d.project_root.clone(), Source::Builtin("project root"));
        }
        if let Some(git) = &d.git
            && !git.common_dir.starts_with(&d.project_root)
        {
            self.grant_dir(git.common_dir.clone(), Source::Builtin("git common dir"));
        }
    }

    fn kind_grants(&mut self, kind: Option<&str>, d: &Discovered) {
        if kind == Some("claude")
            && let Some(main) = d.git.as_ref().and_then(|g| g.main_worktree())
            && let Ok(path) = main.join(".claude/settings.local.json").canonicalize()
        {
            self.policy.grant(Grant {
                path,
                access: Access::ReadWrite,
                file: true,
                pin: false,
                source: Source::Builtin("claude settings from main worktree"),
            });
        }
    }

    fn grant_dir(&mut self, path: PathBuf, source: Source) {
        self.policy.grant(Grant {
            path,
            access: Access::ReadWrite,
            file: false,
            pin: false,
            source,
        });
    }

    fn apply(&mut self, sandbox: &SandboxConfig, layer: &Layer) {
        let source = source_of(layer);
        if sandbox.backend.is_some() {
            self.backend = sandbox.backend;
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
                    Ok(path) => self.policy.grant(Grant {
                        file: !path.is_dir(),
                        path,
                        access,
                        pin: false,
                        source: source.clone(),
                    }),
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
        if nono.profile.is_some() {
            self.nono.profile.clone_from(&nono.profile);
        }
        extend_unique(&mut self.nono.extends, &nono.extends);
        extend_unique(&mut self.nono.groups, &nono.groups);
    }

    fn warn(&mut self, source: &Source, entry: &str, reason: &str) {
        self.warnings
            .push(format!("{source}: skipping `{entry}`: {reason}"));
    }
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
            let d = discover(&self.home.join(cwd), &self.dirs, false)?;
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
        assert_eq!(r.nono.profile.as_deref(), Some("claude-code"));
        assert_eq!(grants(&r), [("repo".into(), Access::ReadWrite)]);
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
