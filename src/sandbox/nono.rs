use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::{Confirm, GeneratedFile, Launch, Request, Sandbox};
use crate::config::discover::hash;
use crate::config::model::{NonoConfig, NonoMode};
use crate::context;
use crate::policy::{Access, Network};

const DEFAULT_PROFILE: &str = "default";

pub struct Nono {
    pub options: NonoConfig,
    pub runtime_dir: PathBuf,
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

impl Sandbox for Nono {
    fn preflight(&self, confirm: Confirm<'_>) -> Result<()> {
        if context::which("nono").is_none() {
            bail!("nono not found on $PATH; install it from https://nono.sh");
        }
        let Some(profile) = self.options.profile.as_deref() else {
            return Ok(());
        };
        if profile.contains('/') || profile_installed(profile) {
            return Ok(());
        }
        let Some(pack) = registry_pack(profile) else {
            bail!("nono profile `{profile}` not found (see `nono profile list`)");
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
            None => bail!("nono profile `{profile}` is not installed; run `nono pull {pack}`"),
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

        // nono has no flag for policy groups, so they need a generated profile.
        if !self.options.groups.is_empty() {
            let profile = serde_json::json!({
                "extends": [base.unwrap_or(DEFAULT_PROFILE)],
                "groups": { "include": self.options.groups },
            });
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
        args.extend(policy.extra_args.iter().map(OsString::from));
        args.extend(request.passthrough.iter().cloned());
        args.push("--".into());
        args.push(request.program.into());
        args.extend(request.args.iter().cloned());

        Ok(Launch {
            program: "nono".into(),
            args,
            env: policy.env.clone(),
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
        vec![
            format!(
                "plus everything nono profile {} allows{groups}",
                profiles.join(", ")
            ),
            "nono lists every rule when it starts; `cb show` lists them now".to_owned(),
        ]
    }

    fn full_access(&self, launch: &Launch) -> Result<Vec<String>> {
        let mut args = launch.args.clone();
        // After the subcommand; `-v` lists group paths, the log file hides `-v` logs.
        let dry_run = ["--dry-run", "-v", "--log-file", "/dev/null"].map(OsString::from);
        args.splice(1..1, dry_run);
        let out = Command::new(&launch.program)
            .args(&args)
            .envs(&launch.env)
            .current_dir(&launch.cwd)
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::policy::{Grant, Policy, Source};

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn build(options: NonoConfig, passthrough: &[&str]) -> Launch {
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
        let nono = Nono {
            options,
            runtime_dir: "/run/cb".into(),
        };
        nono.build(&Request {
            policy: &policy,
            passthrough: &os(passthrough),
            program: "claude",
            args: os(&["--model", "opus"]),
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
        let nono = Nono {
            options: NonoConfig {
                profile: Some("claude-code".into()),
                extends: vec!["extra".into()],
                groups: vec!["rust_runtime".into()],
                ..NonoConfig::default()
            },
            runtime_dir: PathBuf::new(),
        };
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
            },
            &["--read", "/tmp/x"],
        );
        assert_eq!(launch.program, "nono");
        assert_eq!(
            args(&launch).join(" "),
            "run --profile claude-code --extends wsl --allow-cwd --allow /repo \
             --read-file /home/me/.gitconfig --block-net --open-port 3000 --allow-gpu \
             --read /tmp/x -- claude --model opus"
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
}
