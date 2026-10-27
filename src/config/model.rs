use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A single clankerbox configuration file.
#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Stop discovering project files above this one and treat its directory as the project
    /// root. Only meaningful in project files.
    pub root: Option<bool>,

    /// Agent to run when none is given with `-A/--agent`.
    pub agent: Option<String>,

    /// Variables usable as `${name}` in paths and `env` values. Project files supply defaults;
    /// the global config, `.clankerbox.local.toml`, profiles and `CLANKERBOX_VAR_<name>` override
    /// them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, String>,

    /// Sandbox settings for every agent; an agent's own `sandbox` settings take precedence.
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// Agent definitions and per-agent overrides.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agents: BTreeMap<String, AgentConfig>,

    /// Named overlays selected with `-p/--profile`, applied after all layers. Each profile has
    /// the same format as a config file, except for `root` and `profiles`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, ConfigFile>,

    /// Platform-conditional settings, merged in after the rest of this file when they match.
    #[serde(default)]
    pub when: When,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct When {
    pub linux: Option<Box<ConfigFile>>,
    pub macos: Option<Box<ConfigFile>>,
    /// Windows Subsystem for Linux (also matches `linux`).
    pub wsl: Option<Box<ConfigFile>>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    /// Sandbox implementation to use.
    pub backend: Option<BackendKind>,

    /// Paths (directories or files) readable inside the sandbox.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<String>,
    /// Paths (directories or files) writable but not readable inside the sandbox.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write: Vec<String>,
    /// Paths (directories or files) readable and writable inside the sandbox.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Commands looked up on `$PATH` whose containing directory becomes readable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_commands: Vec<String>,

    /// Outbound network access.
    pub network: Option<Network>,
    /// Localhost TCP ports the agent may both connect to and listen on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_ports: Vec<u16>,
    /// TCP ports the agent may listen on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub listen_ports: Vec<u16>,

    /// Environment variables set for the agent.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,

    /// Raw arguments passed to the sandbox backend, before command-line passthrough.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,

    /// Options for the nono backend.
    #[serde(default)]
    pub nono: NonoConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    /// Sandbox with nono (<https://nono.sh>).
    Nono,
    /// No sandbox at all. For debugging only.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Allow,
    Block,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NonoConfig {
    /// Base nono profile (name or path).
    pub profile: Option<String>,
    /// Additional base profiles, passed as `--extends`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extends: Vec<String>,
    /// Policy groups to include on top of the profile (see `nono profile groups`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Agent to inherit settings from.
    pub inherits: Option<String>,
    /// Built-in behaviour to enable, e.g. `"claude"`. Inherited.
    pub kind: Option<String>,
    /// Executable to run.
    pub command: Option<String>,
    /// Arguments passed to the agent before command-line arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Sandbox settings applying only to this agent.
    #[serde(default)]
    pub sandbox: SandboxConfig,
}

impl ConfigFile {
    /// This file followed by every `[when.*]` section matching `platform`, recursively.
    pub fn fragments(&self, platform: Platform) -> Vec<&ConfigFile> {
        let mut out = vec![self];
        let w = &self.when;
        for (on, section) in [
            (platform.linux, &w.linux),
            (platform.macos, &w.macos),
            (platform.wsl, &w.wsl),
        ] {
            if on && let Some(section) = section {
                out.extend(section.fragments(platform));
            }
        }
        out
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Platform {
    pub linux: bool,
    pub macos: bool,
    pub wsl: bool,
}

impl Platform {
    pub fn detect() -> Self {
        let linux = cfg!(target_os = "linux");
        let wsl = linux
            && (std::env::var_os("WSL_DISTRO_NAME").is_some()
                || std::fs::read_to_string("/proc/sys/kernel/osrelease")
                    .is_ok_and(|r| r.to_ascii_lowercase().contains("microsoft")));
        Self {
            linux,
            macos: cfg!(target_os = "macos"),
            wsl,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn when_sections_apply_in_order() {
        let file: ConfigFile = toml::from_str(
            r#"
            agent = "a"
            [when.linux]
            agent = "b"
            [when.wsl]
            agent = "c"
            "#,
        )
        .unwrap();
        let wsl = Platform {
            linux: true,
            macos: false,
            wsl: true,
        };
        let agents: Vec<_> = file
            .fragments(wsl)
            .iter()
            .map(|f| f.agent.clone().unwrap())
            .collect();
        assert_eq!(agents, ["a", "b", "c"]);
        let mac = Platform {
            macos: true,
            ..Default::default()
        };
        assert_eq!(file.fragments(mac).len(), 1);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = toml::from_str::<ConfigFile>("[sandbox]\nraed = []").unwrap_err();
        assert!(err.to_string().contains("raed"), "{err}");
    }
}
