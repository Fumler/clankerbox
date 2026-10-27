use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

pub use crate::config::model::Network;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Read,
    Write,
    #[serde(rename = "read-write")]
    ReadWrite,
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Access::Read => "ro",
            Access::Write => "wo",
            Access::ReadWrite => "rw",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase", tag = "kind", content = "value")]
pub enum Source {
    Builtin(&'static str),
    Preset,
    File(PathBuf),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Builtin(what) => write!(f, "built-in: {what}"),
            Source::Preset => f.write_str("built-in presets"),
            Source::File(path) => write!(f, "{}", path.display()),
        }
    }
}

/// Access to one existing, canonical path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Grant {
    pub path: PathBuf,
    pub access: Access,
    /// A single file rather than a directory tree.
    pub file: bool,
    /// Hold the path open for the agent's lifetime.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pin: bool,
    pub source: Source,
}

#[derive(Debug, Clone, Serialize)]
pub struct Policy {
    pub grants: Vec<Grant>,
    pub network: Network,
    pub open_ports: Vec<u16>,
    pub listen_ports: Vec<u16>,
    pub env: BTreeMap<String, String>,
    /// From config files; command-line passthrough is kept separately in `Request`.
    pub extra_args: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            grants: Vec::new(),
            network: Network::Allow,
            open_ports: Vec::new(),
            listen_ports: Vec::new(),
            env: BTreeMap::new(),
            extra_args: Vec::new(),
        }
    }
}

impl Policy {
    /// Add a grant unless an identical path/access pair is already present.
    pub fn grant(&mut self, grant: Grant) {
        let duplicate = self
            .grants
            .iter()
            .any(|g| g.path == grant.path && g.access == grant.access);
        if !duplicate {
            self.grants.push(grant);
        }
    }
}
