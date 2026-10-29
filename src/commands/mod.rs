pub mod agents;
pub mod doctor;
pub mod edit;
pub mod init;
pub mod run;
pub mod show;
pub mod trust;

use std::env;
use std::fmt::Write as _;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anstream::{eprint, eprintln};
use anyhow::{Context, Result};

use crate::cli::{GlobalOpts, RunArgs};
use crate::config::detect::Toolchain;
use crate::config::discover::{Discovered, discover};
use crate::config::model::{BackendKind, Platform};
use crate::config::trust::TrustStore;
use crate::config::vars::Vars;
use crate::config::{self, Resolved};
use crate::context::Dirs;
use crate::herdr;
use crate::policy::{Access, Grant, Network};
use crate::sandbox::{self, Launch, Request, Sandbox};
use crate::style::{self, DIM, LABEL, WARN};

pub struct Session {
    pub dirs: Dirs,
    pub platform: Platform,
    pub discovered: Discovered,
    pub opts: GlobalOpts,
}

impl Session {
    pub fn open(opts: &GlobalOpts) -> Result<Self> {
        let mut session = Self::open_undetected(opts)?;
        if !opts.no_project {
            session
                .discovered
                .detect(&session.dirs, opts.detect_subdirs)?;
        }
        Ok(session)
    }

    pub fn open_undetected(opts: &GlobalOpts) -> Result<Self> {
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

    pub fn trust_store(&self) -> Result<TrustStore> {
        let mut store = TrustStore::load(&self.dirs.state)?;
        store.approve_detected = self.opts.yes || self.discovered.approve_detected();
        Ok(store)
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

    /// Build once per command: it reads the Herdr environment.
    pub fn backend(&self, resolved: &Resolved) -> Box<dyn Sandbox> {
        sandbox::backend(resolved, &self.dirs, herdr::detect().as_ref())
    }

    pub fn launch(
        &self,
        backend: &dyn Sandbox,
        resolved: &Resolved,
        run: &RunArgs,
    ) -> Result<Launch> {
        backend.build(&Request {
            policy: &resolved.policy,
            run,
            program: &resolved.agent.command,
            config_args: &resolved.agent.args,
            cwd: &self.discovered.cwd,
        })
    }
}

/// Lists write grants to shared caches, which unsandboxed builds also use.
/// Empty without cache grants.
pub fn cache_grants_text(toolchains: &[&Toolchain], session: &Session, hint: &str) -> String {
    let vars = Vars {
        project_root: session.discovered.project_root.clone(),
        home: session.dirs.home.clone(),
        cwd: session.cwd().clone(),
        ..Vars::default()
    };
    let rows: Vec<_> = toolchains
        .iter()
        .flat_map(|t| t.allow.iter().map(move |raw| (t.name, *raw)))
        .map(|(name, raw)| match vars.resolve_path(raw, session.cwd()) {
            Ok(p) => (name, display_home(&p, &vars.home), p.exists()),
            Err(_) => (name, raw.to_owned(), false),
        })
        .collect();
    let mut out = String::new();
    if rows.is_empty() {
        return out;
    }
    let _ = writeln!(
        out,
        "{}",
        warning(
            "write access to shared package caches; the agent can change packages your unsandboxed builds use:"
        )
    );
    let width = rows.iter().map(|(name, ..)| name.len()).max().unwrap_or(0);
    let rw = style::access(Access::ReadWrite);
    for (name, path, exists) in rows {
        let missing = if exists {
            String::new()
        } else {
            format!("  {DIM}(missing, not granted){DIM:#}")
        };
        let _ = writeln!(out, "  {name:<width$}  {rw}rw{rw:#}  {path}{missing}");
    }
    let _ = writeln!(out, "  {DIM}{hint}{DIM:#}");
    out
}

pub fn display_home(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

pub fn warn(message: impl std::fmt::Display) {
    eprintln!("{}", warning(message));
}

pub fn warning(message: impl std::fmt::Display) -> String {
    format!("cb: {WARN}warning:{WARN:#} {message}")
}

pub fn grant_lines(grants: &[&Grant]) -> Vec<String> {
    let width = grants
        .iter()
        .map(|g| g.path.as_os_str().len())
        .max()
        .unwrap_or(0);
    grants
        .iter()
        .map(|g| {
            let path = g.path.display().to_string();
            let access = style::access(g.access);
            format!(
                "{access}{}{access:#} {path:<width$}  {DIM}← {}{DIM:#}",
                g.access, g.source
            )
        })
        .collect()
}

pub fn network_label(network: Network) -> String {
    let style = style::network(network);
    format!("{style}{}{style:#}", format!("{network:?}").to_lowercase())
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
