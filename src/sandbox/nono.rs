use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::{Confirm, GeneratedFile, Launch, Request, Sandbox, claude_plugin};
use crate::config::discover::hash;
use crate::config::model::{NonoConfig, NonoMode};
use crate::context;
use crate::herdr;
use crate::policy::{Access, Network};

const DEFAULT_PROFILE: &str = "default";
// nono rejects `:` in names, so these can't clash.
const BUILTIN_PREFIX: &str = "cb:";

struct Builtin {
    name: &'static str,
    profile: &'static str,
    // Created before each run: Landlock and Seatbelt drop grants for missing paths.
    home_dirs: &'static [&'static str],
    runtime_dirs: &'static [&'static str],
}

const BUILTINS: &[Builtin] = &[Builtin {
    name: "claude",
    // nolabs-ai/claude, minus its session hook.
    profile: include_str!("profiles/claude.json"),
    home_dirs: &[".claude", ".cache/claude", ".local/state/claude/locks"],
    runtime_dirs: &["cc-socks"],
}];

fn builtin(profile: &str) -> Result<Option<&'static Builtin>> {
    let Some(name) = profile.strip_prefix(BUILTIN_PREFIX) else {
        return Ok(None);
    };
    let found = BUILTINS.iter().find(|b| b.name == name);
    let known: Vec<_> = BUILTINS.iter().map(|b| b.name).collect();
    found.map(Some).with_context(|| {
        format!(
            "unknown built-in nono profile `{profile}` (known: {BUILTIN_PREFIX}{})",
            known.join(&format!(", {BUILTIN_PREFIX}"))
        )
    })
}

pub fn check_profile(profile: &str) -> Result<()> {
    match status(profile)? {
        Status::Found => Ok(()),
        Status::Missing(pack) => Err(missing(profile, pack)),
    }
}

enum Status {
    Found,
    /// With the registry pack providing it, if any.
    Missing(Option<&'static str>),
}

fn status(profile: &str) -> Result<Status> {
    if builtin(profile)?.is_some() || profile_installed(profile) {
        return Ok(Status::Found);
    }
    Ok(Status::Missing(registry_pack(profile)))
}

fn missing(profile: &str, pack: Option<&str>) -> anyhow::Error {
    match pack {
        Some(pack) => {
            anyhow::anyhow!("nono profile `{profile}` is not installed; run `nono pull {pack}`")
        }
        None => anyhow::anyhow!("nono profile `{profile}` not found (see `nono profile list`)"),
    }
}

/// nono profile names are alphanumeric with hyphens, and pack refs add one `/`.
fn is_file(profile: &str) -> bool {
    profile.starts_with(['/', '.', '~'])
        || Path::new(profile)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
}

pub struct Nono {
    pub options: NonoConfig,
    pub agent_kind: Option<String>,
    pub cb: PathBuf,
    pub home: PathBuf,
    pub runtime_dir: PathBuf,
    pub herdr: Option<herdr::Host>,
    pub claude_config: Option<PathBuf>,
}

/// Registry pack providing a nono profile.
fn registry_pack(profile: &str) -> Option<&'static str> {
    match profile {
        "claude-code" | "claude" => Some("nolabs-ai/claude"),
        "pi" => Some("nolabs-ai/pi"),
        "omp" => Some("nolabs-ai/omp"),
        _ => None,
    }
}

fn profile_installed(profile: &str) -> bool {
    Command::new("nono")
        .args(["profile", "show", profile])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

impl Nono {
    fn with_groups(&self, profile: &str, name: &str) -> Result<serde_json::Value> {
        let mut profile: serde_json::Value =
            serde_json::from_str(profile).with_context(|| format!("{name}: invalid JSON"))?;
        if !profile.is_object() {
            bail!("{name}: not a JSON object");
        }
        let include = &mut profile["groups"]["include"];
        if include.is_null() {
            *include = serde_json::json!([]);
        }
        include
            .as_array_mut()
            .with_context(|| format!("{name}: `groups.include` is not a list"))?
            .extend(self.options.groups.iter().map(|g| g.as_str().into()));
        Ok(profile)
    }

    // `nono wrap` grants /proc/self to the first process only: nolabs-ai/nono#1984.
    fn wrap_needs_proc(&self) -> bool {
        cfg!(target_os = "linux") && self.options.mode == Some(NonoMode::Direct)
    }

    fn explain_denials(&self) -> bool {
        self.agent_kind.as_deref() == Some("claude") && self.options.explain_denials != Some(false)
    }

    fn claude_plugin(
        &self,
        request: &Request<'_>,
        args: &mut Vec<OsString>,
        files: &mut Vec<GeneratedFile>,
        env: &mut BTreeMap<String, String>,
    ) -> Result<Option<PathBuf>> {
        let herdr = self.herdr();
        if let Some(host) = herdr {
            args.extend(["--allow-unix-socket".into(), host.socket.clone().into()]);
            env.insert(herdr::MARKER_ENV.into(), "1".into());
            if let Some(argv) = host.resume_argv(request.run, request.cwd) {
                env.insert(herdr::RESUME_ENV.into(), serde_json::to_string(&argv)?);
            }
        }
        if !self.explain_denials() && herdr.is_none() {
            return Ok(None);
        }
        let (dir, plugin_files) = claude_plugin::generate(
            &self.runtime_dir,
            &self.cb,
            self.explain_denials(),
            herdr.is_some(),
        );
        files.extend(plugin_files);
        args.extend(["--read".into(), dir.clone().into()]);
        if self.cb.is_absolute() {
            args.extend(["--read-file".into(), self.cb.clone().into()]);
        }
        Ok(Some(dir))
    }

    fn herdr(&self) -> Option<&herdr::Host> {
        let on = self.agent_kind.as_deref() == Some("claude") && self.options.herdr != Some(false);
        self.herdr.as_ref().filter(|_| on)
    }
}

impl Sandbox for Nono {
    fn preflight(&self, confirm: Confirm<'_>) -> Result<()> {
        if context::which("nono").is_none() {
            bail!("nono not found on $PATH; install it from https://nono.sh");
        }
        let Some(profile) = self.options.profile.as_deref() else {
            return Ok(());
        };
        if let Some(builtin) = builtin(profile)? {
            return create_dirs(&self.home, builtin);
        }
        // `build` reads profile files, relative to the agent's directory.
        if is_file(profile) {
            return Ok(());
        }
        let pack = match status(profile)? {
            Status::Found => return Ok(()),
            Status::Missing(None) => return Err(missing(profile, None)),
            Status::Missing(Some(pack)) => pack,
        };
        let question =
            format!("nono profile `{profile}` is not installed. Run `nono pull {pack}`?");
        match confirm(&question) {
            Some(true) => {
                let status = Command::new("nono")
                    .args(["pull", pack])
                    .status()
                    .context("cannot run nono pull")?;
                if !status.success() {
                    bail!("`nono pull {pack}` failed");
                }
                Ok(())
            }
            Some(false) => bail!("nono profile `{profile}` is not installed"),
            None => Err(missing(profile, Some(pack))),
        }
    }

    fn prepare(&self) -> Vec<String> {
        match (self.herdr(), &self.claude_config) {
            (Some(_), Some(config)) => {
                herdr::silence_native_hook(std::slice::from_ref(config), &self.home)
            }
            _ => Vec::new(),
        }
    }

    fn build(&self, request: &Request<'_>) -> Result<Launch> {
        let base = self.options.profile.as_deref();
        let mut files = Vec::new();
        let subcommand = match self.options.mode.unwrap_or(NonoMode::Supervised) {
            NonoMode::Supervised => "run",
            NonoMode::Direct => "wrap",
        };
        let mut args: Vec<OsString> = vec![subcommand.into()];

        // nono takes no group flags and can't extend a profile file, so groups are merged into
        // a copy of a built-in profile or profile file.
        let builtin = base.map(builtin).transpose()?.flatten();
        let base_file = base.filter(|b| builtin.is_none() && is_file(b));
        if builtin.is_some() || !self.options.groups.is_empty() {
            let profile = match (builtin, base_file) {
                (Some(builtin), _) => self.with_groups(builtin.profile, "built-in profile")?,
                (None, Some(file)) => {
                    let path = match file.strip_prefix("~/") {
                        Some(rest) => self.home.join(rest),
                        None => request.cwd.join(file),
                    };
                    let contents = std::fs::read_to_string(&path)
                        .with_context(|| format!("cannot read nono profile {}", path.display()))?;
                    self.with_groups(&contents, &path.display().to_string())?
                }
                (None, None) => serde_json::json!({
                    "extends": [base.unwrap_or(DEFAULT_PROFILE)],
                    "groups": { "include": self.options.groups },
                }),
            };
            let contents = serde_json::to_string_pretty(&profile)?;
            let path = self
                .runtime_dir
                .join("profiles")
                .join(format!("{}.json", &hash(contents.as_bytes())[..16]));
            args.extend(["--profile".into(), path.clone().into()]);
            files.push(GeneratedFile { path, contents });
        } else if let Some(base) = base {
            args.extend(["--profile".into(), base.into()]);
        }
        for extends in &self.options.extends {
            args.extend(["--extends".into(), extends.into()]);
        }

        let policy = request.policy;
        args.push("--allow-cwd".into());
        if self.wrap_needs_proc() {
            args.extend(["--read".into(), "/proc".into()]);
        }
        for grant in &policy.grants {
            let flag = match (grant.access, grant.file) {
                (Access::Read, false) => "--read",
                (Access::Write, false) => "--write",
                (Access::ReadWrite, false) => "--allow",
                (Access::Read, true) => "--read-file",
                (Access::Write, true) => "--write-file",
                (Access::ReadWrite, true) => "--allow-file",
            };
            args.extend([flag.into(), grant.path.clone().into()]);
        }
        for socket in &policy.sockets {
            args.extend(["--allow-unix-socket".into(), socket.clone().into()]);
        }
        if policy.network == Network::Block {
            args.push("--block-net".into());
        }
        for (flag, ports) in [
            ("--open-port", &policy.open_ports),
            ("--listen-port", &policy.listen_ports),
        ] {
            for port in ports {
                args.extend([flag.into(), port.to_string().into()]);
            }
        }
        let mut env = policy.env.clone();
        let plugin = self.claude_plugin(request, &mut args, &mut files, &mut env)?;
        args.extend(policy.extra_args.iter().map(OsString::from));
        args.extend(request.run.passthrough());
        args.push("--".into());
        args.push(request.program.into());
        args.extend(request.config_args.iter().map(OsString::from));
        if let Some(dir) = plugin {
            args.extend(["--plugin-dir".into(), dir.into()]);
        }
        args.extend(request.run.agent_args.iter().cloned());

        Ok(Launch {
            program: "nono".into(),
            args,
            env,
            env_remove: policy.env_remove.clone(),
            files,
            cwd: request.cwd.to_path_buf(),
        })
    }

    fn base_access(&self) -> Vec<String> {
        let base = self.options.profile.as_deref().unwrap_or(DEFAULT_PROFILE);
        let profiles: Vec<_> = std::iter::once(base)
            .chain(self.options.extends.iter().map(String::as_str))
            .collect();
        let groups = if self.options.groups.is_empty() {
            String::new()
        } else {
            format!(" (groups: {})", self.options.groups.join(", "))
        };
        let mut lines = vec![
            format!(
                "plus everything nono profile {} allows{groups}",
                profiles.join(", ")
            ),
            "nono lists every rule when it starts; `cb show` lists them now".to_owned(),
        ];
        if self.wrap_needs_proc() {
            lines.push(
                "plus read access to /proc, which direct mode needs for child processes; \
                 other processes' command lines become readable"
                    .to_owned(),
            );
        }
        lines
    }

    fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.explain_denials() {
            notes.push(
                "loads cb's Claude Code plugin, which explains sandbox denials to Claude (read \
                 access to it and to cb); turn off with `sandbox.nono.explain_denials = false`"
                    .to_owned(),
            );
        }
        if let Some(herdr::Host { socket, .. }) = self.herdr() {
            notes.push(format!(
                "reports Claude's state to Herdr through cb's Claude Code plugin, with access to \
                 the Herdr socket {}: the agent can control every Herdr pane, including \
                 unsandboxed shells. Adds a line to Herdr's own Claude hook \
                 (hooks/herdr-agent-state.sh) so it skips these sessions. Turn off with \
                 `sandbox.nono.herdr = false`",
                socket.display()
            ));
        }
        notes
    }

    fn full_access(&self, launch: &Launch) -> Result<Vec<String>> {
        let mut args = launch.args.clone();
        // After the subcommand; `-v` lists group paths, the log file hides `-v` logs.
        let dry_run = ["--dry-run", "-v", "--log-file", "/dev/null"].map(OsString::from);
        args.splice(1..1, dry_run);
        let out = launch
            .command_with_args(&args)
            .stdin(Stdio::null())
            .output()
            .context("cannot run nono")?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            bail!("`nono --dry-run` failed: {}", stderr.trim());
        }
        let separator = |line: &&str| line.trim_start().starts_with('─');
        let rules: Vec<_> = stderr
            .lines()
            .skip_while(|l| !separator(l))
            .skip(1)
            .take_while(|l| !separator(l))
            .map(|l| l.strip_prefix("  ").unwrap_or(l).trim_end().to_owned())
            .collect();
        if rules.is_empty() {
            bail!("no rules in `nono --dry-run` output");
        }
        Ok(rules)
    }
}

fn create_dirs(home: &Path, builtin: &Builtin) -> Result<()> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let home_dirs = builtin.home_dirs.iter().map(|d| home.join(d));
    let runtime_dirs = runtime
        .iter()
        .flat_map(|r| builtin.runtime_dirs.iter().map(move |d| r.join(d)));
    for dir in home_dirs.chain(runtime_dirs) {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::cli::RunArgs;
    use crate::policy::{Grant, Policy, Source};

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn build(options: NonoConfig, passthrough: &[&str]) -> Launch {
        launch(&nono_for(None, options), passthrough)
    }

    fn build_for(kind: Option<&str>, options: NonoConfig) -> Launch {
        launch(&nono_for(kind, options), &[])
    }

    fn nono_for(kind: Option<&str>, options: NonoConfig) -> Nono {
        Nono {
            options,
            agent_kind: kind.map(Into::into),
            cb: "/bin/cb".into(),
            home: PathBuf::new(),
            runtime_dir: "/run/cb".into(),
            herdr: None,
            claude_config: None,
        }
    }

    fn launch(nono: &Nono, passthrough: &[&str]) -> Launch {
        let mut policy = Policy::default();
        policy.grant(Grant {
            path: "/repo".into(),
            access: Access::ReadWrite,
            file: false,
            pin: false,
            source: Source::Builtin("project root"),
        });
        policy.grant(Grant {
            path: "/home/me/.gitconfig".into(),
            access: Access::Read,
            file: true,
            pin: false,
            source: Source::Preset,
        });
        policy.network = Network::Block;
        policy.open_ports = vec![3000];
        policy.extra_args = vec!["--allow-gpu".into()];
        nono.build(&Request {
            policy: &policy,
            run: &RunArgs {
                sandbox: os(passthrough),
                agent_args: os(&["-c"]),
                ..RunArgs::default()
            },
            program: "claude",
            config_args: &["--model".into(), "opus".into()],
            cwd: Path::new("/repo"),
        })
        .unwrap()
    }

    fn args(launch: &Launch) -> Vec<String> {
        launch
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn base_access_names_profiles_and_groups() {
        let nono = nono_for(
            None,
            NonoConfig {
                profile: Some("claude-code".into()),
                extends: vec!["extra".into()],
                groups: vec!["rust_runtime".into()],
                ..NonoConfig::default()
            },
        );
        assert_eq!(
            nono.base_access(),
            [
                "plus everything nono profile claude-code, extra allows (groups: rust_runtime)",
                "nono lists every rule when it starts; `cb show` lists them now",
            ]
        );
    }

    #[test]
    fn maps_policy_to_flags() {
        let launch = build(
            NonoConfig {
                mode: None,
                profile: Some("claude-code".into()),
                extends: vec!["wsl".into()],
                groups: vec![],
                explain_denials: None,
                herdr: None,
            },
            &["--read", "/tmp/x"],
        );
        assert_eq!(launch.program, "nono");
        assert_eq!(
            args(&launch).join(" "),
            "run --profile claude-code --extends wsl --allow-cwd --allow /repo \
             --read-file /home/me/.gitconfig --block-net --open-port 3000 --allow-gpu \
             --read /tmp/x -- claude --model opus -c"
        );
        assert!(launch.files.is_empty());

        let direct = build(
            NonoConfig {
                mode: Some(NonoMode::Direct),
                ..Default::default()
            },
            &[],
        );
        assert_eq!(args(&direct)[0], "wrap");
    }

    #[test]
    fn groups_generate_profile_extending_base() {
        let launch = build(
            NonoConfig {
                profile: Some("claude-code".into()),
                groups: vec!["node_runtime".into()],
                ..Default::default()
            },
            &[],
        );
        let [file] = &launch.files[..] else {
            panic!("expected one file")
        };
        assert!(file.path.starts_with("/run/cb/profiles"));
        let profile: serde_json::Value = serde_json::from_str(&file.contents).unwrap();
        assert_eq!(profile["extends"], serde_json::json!(["claude-code"]));
        assert_eq!(
            profile["groups"]["include"],
            serde_json::json!(["node_runtime"])
        );
        assert_eq!(args(&launch)[2], file.path.to_string_lossy());
    }

    #[test]
    fn builtin_profile_is_written_with_groups_merged() {
        let launch = build(
            NonoConfig {
                profile: Some("cb:claude".into()),
                groups: vec!["go_runtime".into()],
                ..Default::default()
            },
            &[],
        );
        let [file] = &launch.files[..] else {
            panic!("expected one file")
        };
        assert_eq!(args(&launch)[2], file.path.to_string_lossy());
        let profile: serde_json::Value = serde_json::from_str(&file.contents).unwrap();
        assert_eq!(profile["extends"], "default");
        assert!(profile.get("session_hooks").is_none());
        let groups = profile["groups"]["include"].as_array().unwrap();
        assert!(groups.contains(&"git_config".into()));
        assert_eq!(groups.last().unwrap(), "go_runtime");
    }

    #[test]
    fn builtin_profiles_parse_and_unknown_ones_fail() {
        for b in BUILTINS {
            let _: serde_json::Value = serde_json::from_str(b.profile).unwrap();
        }
        assert!(builtin("claude").unwrap().is_none());
        assert!(builtin("cb:claude").unwrap().is_some());
        assert!(builtin("cb:other").is_err());
    }

    #[test]
    fn claude_gets_denial_plugin_read_only_unless_disabled() {
        let launch = build_for(Some("claude"), NonoConfig::default());
        let argv = args(&launch);
        let dir = &argv[argv.iter().position(|a| a == "--plugin-dir").unwrap() + 1];
        assert!(dir.starts_with("/run/cb/claude-plugin/"));
        let program = argv.iter().position(|a| a == "claude").unwrap();
        assert_eq!(
            argv[program + 1..],
            ["--model", "opus", "--plugin-dir", dir, "-c"]
        );
        let read = argv
            .windows(2)
            .position(|w| w[0] == "--read" && w[1] == *dir);
        assert!(read.unwrap() < program);
        assert!(argv.windows(2).any(|w| w == ["--read-file", "/bin/cb"]));
        for file in [".claude-plugin/plugin.json", "hooks/hooks.json"] {
            assert!(
                launch
                    .files
                    .iter()
                    .any(|f| f.path == Path::new(dir).join(file)),
                "{file}"
            );
        }
        for f in launch
            .files
            .iter()
            .filter(|f| f.path.extension().is_some_and(|e| e == "json"))
        {
            let _: serde_json::Value = serde_json::from_str(&f.contents).unwrap();
        }

        let off = NonoConfig {
            explain_denials: Some(false),
            ..NonoConfig::default()
        };
        for launch in [
            build_for(Some("claude"), off),
            build(NonoConfig::default(), &[]),
        ] {
            assert!(!args(&launch).contains(&"--plugin-dir".to_owned()));
            assert!(launch.files.is_empty());
        }
    }

    #[test]
    fn herdr_pane_grants_socket_and_hooks_for_claude_unless_disabled() {
        let herdr = |kind, options| Nono {
            herdr: Some(herdr::Host {
                socket: "/run/herdr.sock".into(),
                resume_env: Vec::new(),
            }),
            ..nono_for(kind, options)
        };
        let nono = herdr(Some("claude"), NonoConfig::default());
        let run = launch(&nono, &[]);
        let argv = args(&run);
        assert!(
            argv.windows(2)
                .any(|w| w == ["--allow-unix-socket", "/run/herdr.sock"])
        );
        assert_eq!(run.env[herdr::RESUME_ENV], r#"["cb","-C","/repo","--"]"#);
        let hooks = run
            .files
            .iter()
            .find(|f| f.path.ends_with("hooks/hooks.json"))
            .unwrap();
        assert!(
            hooks.contents.contains("hook herdr") && hooks.contents.contains("hook explain-denial")
        );
        assert!(nono.notes().iter().any(|n| n.contains("/run/herdr.sock")
            && n.contains("every Herdr pane")
            && n.contains("sandbox.nono.herdr = false")));

        let only_herdr = herdr(
            Some("claude"),
            NonoConfig {
                explain_denials: Some(false),
                ..NonoConfig::default()
            },
        );
        let run = launch(&only_herdr, &[]);
        assert!(args(&run).contains(&"--plugin-dir".to_owned()));
        let hooks = run
            .files
            .iter()
            .find(|f| f.path.ends_with("hooks/hooks.json"))
            .unwrap();
        assert!(!hooks.contents.contains("explain-denial"));

        let off = NonoConfig {
            herdr: Some(false),
            ..NonoConfig::default()
        };
        for nono in [
            herdr(Some("claude"), off),
            herdr(None, NonoConfig::default()),
        ] {
            let run = launch(&nono, &[]);
            assert!(!args(&run).contains(&"--allow-unix-socket".to_owned()));
            assert!(run.env.is_empty());
            assert!(!nono.notes().iter().any(|n| n.contains("Herdr")));
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn direct_mode_reads_proc() {
        let direct = NonoConfig {
            mode: Some(NonoMode::Direct),
            ..NonoConfig::default()
        };
        let launch = build(direct.clone(), &[]);
        assert!(args(&launch).windows(2).any(|w| w == ["--read", "/proc"]));
        let nono = nono_for(None, direct);
        assert!(nono.base_access().iter().any(|l| l.contains("/proc")));

        let launch = build(NonoConfig::default(), &[]);
        assert!(!args(&launch).contains(&"/proc".to_owned()));
    }

    #[test]
    fn groups_are_merged_into_profile_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("mine.json"),
            r#"{"extends": "default", "filesystem": {"read": ["/x"]}}"#,
        )
        .unwrap();
        fs::write(tmp.path().join("bad.json"), "[]").unwrap();
        let nono = |profile: &str, groups: &[&str]| {
            nono_for(
                None,
                NonoConfig {
                    profile: Some(profile.into()),
                    groups: groups.iter().map(|g| (*g).into()).collect(),
                    ..NonoConfig::default()
                },
            )
        };
        let build = |nono: Nono| {
            nono.build(&Request {
                policy: &Policy::default(),
                run: &RunArgs::default(),
                program: "claude",
                config_args: &[],
                cwd: tmp.path(),
            })
        };

        let launch = build(nono("./mine.json", &["go_runtime"])).unwrap();
        let [file] = &launch.files[..] else {
            panic!("expected one file")
        };
        let profile: serde_json::Value = serde_json::from_str(&file.contents).unwrap();
        assert_eq!(profile["extends"], "default");
        assert_eq!(profile["filesystem"]["read"], serde_json::json!(["/x"]));
        assert_eq!(
            profile["groups"]["include"],
            serde_json::json!(["go_runtime"])
        );

        let launch = build(nono("./mine.json", &[])).unwrap();
        assert_eq!(args(&launch)[1..3], ["--profile", "./mine.json"]);
        assert!(build(nono("./bad.json", &["go_runtime"])).is_err());
        assert!(build(nono("./missing.json", &["go_runtime"])).is_err());
    }
}
