pub mod agents;
pub mod doctor;
pub mod edit;
pub mod init;
pub mod run;
pub mod show;
pub mod trust;

use std::env;
use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anstream::{eprint, eprintln};
use anyhow::{Context, Result};

use crate::cli::GlobalOpts;
use crate::config::discover::{Discovered, discover};
use crate::config::model::{BackendKind, Platform};
use crate::config::{self, Resolved};
use crate::context::Dirs;
use crate::sandbox::{self, Launch, Request};
use crate::style::{LABEL, WARN};

pub struct Session {
    pub dirs: Dirs,
    pub platform: Platform,
    pub discovered: Discovered,
    pub opts: GlobalOpts,
}

impl Session {
    pub fn open(opts: &GlobalOpts) -> Result<Self> {
        let cwd = match &opts.dir {
            Some(dir) => dir.clone(),
            None => env::current_dir().context("cannot determine current directory")?,
        };
        let dirs = Dirs::from_env()?;
        let discovered = discover(&cwd, &dirs, opts.no_project)?;
        Ok(Self {
            dirs,
            platform: Platform::detect(),
            discovered,
            opts: opts.clone(),
        })
    }

    pub fn cwd(&self) -> &PathBuf {
        &self.discovered.cwd
    }

    pub fn resolve(&self) -> Result<Resolved> {
        self.resolve_agent(self.opts.agent.as_deref())
    }

    pub fn resolve_agent(&self, agent: Option<&str>) -> Result<Resolved> {
        let mut resolved = config::resolve(
            &self.discovered,
            &self.dirs,
            self.platform,
            agent,
            &self.opts.profiles,
        )?;
        if let Some(profile) = &self.opts.nono_profile {
            resolved.nono.profile = Some(profile.clone());
        }
        if self.opts.unsandboxed {
            resolved.backend = BackendKind::None;
        }
        Ok(resolved)
    }

    pub fn launch(
        &self,
        resolved: &Resolved,
        sandbox_args: &[OsString],
        agent_args: &[OsString],
    ) -> Result<Launch> {
        let args = resolved
            .agent
            .args
            .iter()
            .map(OsString::from)
            .chain(agent_args.iter().cloned())
            .collect();
        sandbox::backend(resolved, &self.dirs).build(&Request {
            policy: &resolved.policy,
            passthrough: sandbox_args,
            program: &resolved.agent.command,
            args,
            cwd: &self.discovered.cwd,
        })
    }
}

pub fn warn(message: impl std::fmt::Display) {
    eprintln!("cb: {WARN}warning:{WARN:#} {message}");
}

pub fn interactive() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

/// Ask a yes/no question on stderr; defaults to no.
pub fn confirm(question: &str) -> Result<bool> {
    eprint!("{LABEL}{question}{LABEL:#} [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}
