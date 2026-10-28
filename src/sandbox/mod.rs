mod claude_plugin;
mod nono;
mod unsandboxed;

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use rustix::process::getuid;
use serde::Serialize;

pub use self::claude_plugin::explain_denial;
pub use self::nono::{Nono, check_profile as check_nono_profile};
pub use self::unsandboxed::Unsandboxed;
use crate::config::Resolved;
use crate::config::model::BackendKind;
use crate::context::Dirs;
use crate::policy::Policy;

#[derive(Debug)]
pub struct Request<'a> {
    pub policy: &'a Policy,
    /// From the command line; config-file backend args are in `Policy::extra_args`.
    pub passthrough: &'a [OsString],
    pub program: &'a str,
    pub args: Vec<OsString>,
    /// Must be the directory config was discovered from, or grants won't match what runs.
    pub cwd: &'a Path,
}

#[derive(Debug)]
pub struct Launch {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub files: Vec<GeneratedFile>,
}

#[derive(Debug, Serialize)]
pub struct GeneratedFile {
    pub path: PathBuf,
    pub contents: String,
}

/// Asks the user a yes/no question; `None` when not interactive.
pub type Confirm<'a> = &'a mut dyn FnMut(&str) -> Option<bool>;

pub trait Sandbox {
    /// Checks (and with consent, fixes) the environment before a run.
    fn preflight(&self, _confirm: Confirm<'_>) -> Result<()> {
        Ok(())
    }

    fn build(&self, request: &Request<'_>) -> Result<Launch>;

    /// Access the backend allows on top of `Policy` grants, as lines for the user.
    fn base_access(&self) -> Vec<String>;

    /// Additions to a run other than access, as lines for the user.
    fn notes(&self) -> Vec<String> {
        Vec::new()
    }

    /// Every rule `launch` would apply, as the backend reports them; empty if unsupported.
    fn full_access(&self, _launch: &Launch) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}

pub fn backend(resolved: &Resolved, dirs: &Dirs) -> Box<dyn Sandbox> {
    match resolved.backend {
        BackendKind::Nono => Box::new(Nono {
            options: resolved.nono.clone(),
            agent_kind: resolved.agent.kind.clone(),
            cb: std::env::current_exe().unwrap_or_else(|_| "cb".into()),
            home: dirs.home.clone(),
            runtime_dir: dirs.runtime.clone(),
        }),
        BackendKind::None => Box::new(Unsandboxed),
    }
}

impl Launch {
    pub fn write_files(&self) -> Result<()> {
        for file in &self.files {
            write_private(&file.path, &file.contents)
                .with_context(|| format!("cannot write {}", file.path.display()))?;
        }
        Ok(())
    }

    pub fn exec(self) -> Result<Infallible> {
        self.write_files()?;
        let err = Command::new(&self.program)
            .args(&self.args)
            .envs(&self.env)
            .current_dir(&self.cwd)
            .exec();
        Err(err).with_context(|| format!("cannot execute {}", self.program.to_string_lossy()))
    }

    /// Shell-quoted, for display only.
    /// Copy-pasteable shell form, including env vars and a `cd` when `cwd` isn't the current dir.
    pub fn display_shell(&self) -> String {
        let mut parts = Vec::new();
        if std::env::current_dir().ok().as_deref() != Some(self.cwd.as_path()) {
            parts.push(format!(
                "cd {} &&",
                shell_quote(&self.cwd.to_string_lossy())
            ));
        }
        parts.extend(
            self.env
                .iter()
                .map(|(k, v)| format!("{k}={}", shell_quote(v))),
        );
        parts.push(self.display_command());
        parts.join(" ")
    }

    pub fn display_command(&self) -> String {
        std::iter::once(&self.program)
            .chain(&self.args)
            .map(|a| shell_quote(&a.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Write `contents` to `path` in a directory only the current user can modify, without following
/// symlinks, since the sandbox trusts these files.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    let dir = path.parent().context("no parent directory")?;
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != getuid().as_raw() || meta.mode() & 0o022 != 0 {
        bail!("{} is not a private directory owned by you", dir.display());
    }
    let tmp = dir.join(format!(".{}.tmp", std::process::id()));
    let _ = fs::remove_file(&tmp);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?
        .write_all(contents.as_bytes())?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn shell_quote(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn private_writes_create_owner_only_dirs_and_reject_shared_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("run/profiles/p.json");
        write_private(&file, "{}").unwrap();
        write_private(&file, "{\"x\": 1}").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "{\"x\": 1}");
        let mode = fs::metadata(file.parent().unwrap()).unwrap().mode();
        assert_eq!(mode & 0o777, 0o700);

        let shared = tmp.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(write_private(&shared.join("p.json"), "{}").is_err());

        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(file.parent().unwrap(), &link).unwrap();
        assert!(write_private(&link.join("p.json"), "{}").is_err());
    }
}
