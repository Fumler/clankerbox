use std::path::PathBuf;

use anstream::println;
use anyhow::Result;
use serde::Serialize;

use super::Session;
use crate::cli::RunArgs;
use crate::config::model::{BackendKind, NonoConfig};
use crate::config::trust::TrustStore;
use crate::policy::Policy;
use crate::sandbox::GeneratedFile;
use crate::style::{self, DIM, ERROR, LABEL, OK, WARN};

#[derive(Serialize)]
struct Report<'a> {
    project_root: &'a PathBuf,
    config_files: Vec<ConfigFileReport>,
    agent: AgentReport<'a>,
    backend: BackendKind,
    policy: &'a Policy,
    nono: &'a NonoConfig,
    generated_files: &'a [GeneratedFile],
    command: Vec<String>,
    env: &'a std::collections::BTreeMap<String, String>,
    warnings: &'a [String],
}

#[derive(Serialize)]
struct ConfigFileReport {
    path: String,
    trusted: bool,
}

#[derive(Serialize)]
struct AgentReport<'a> {
    name: &'a str,
    kind: Option<&'a str>,
    command: &'a str,
    args: &'a [String],
}

pub fn show(run: &RunArgs, json: bool) -> Result<()> {
    let opts = &run.global;
    let session = Session::open(opts)?;
    let resolved = session.resolve()?;
    let launch = session.launch(&resolved, &run.sandbox, &run.agent_args)?;
    let store = TrustStore::load(&session.dirs.state)?;

    let report = Report {
        project_root: &session.discovered.project_root,
        config_files: session
            .discovered
            .layers
            .iter()
            .map(|l| ConfigFileReport {
                path: l.to_string(),
                trusted: store.is_trusted(l),
            })
            .collect(),
        agent: AgentReport {
            name: &resolved.agent.name,
            kind: resolved.agent.kind.as_deref(),
            command: &resolved.agent.command,
            args: &resolved.agent.args,
        },
        backend: resolved.backend,
        policy: &resolved.policy,
        nono: &resolved.nono,
        generated_files: &launch.files,
        command: std::iter::once(&launch.program)
            .chain(&launch.args)
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        env: &launch.env,
        warnings: &resolved.warnings,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!(
        "{LABEL}project root{LABEL:#}  {}",
        report.project_root.display()
    );
    println!("{LABEL}config files{LABEL:#}");
    for file in &report.config_files {
        if file.trusted {
            println!("  {OK}✓{OK:#} {}", file.path);
        } else {
            println!("  {ERROR}✗ untrusted:{ERROR:#} {}", file.path);
        }
    }
    let kind = report
        .agent
        .kind
        .map(|k| format!(" {DIM}(kind: {k}){DIM:#}"))
        .unwrap_or_default();
    println!("{LABEL}agent{LABEL:#}         {}{kind}", report.agent.name);
    if !opts.profiles.is_empty() {
        println!("{LABEL}profiles{LABEL:#}      {}", opts.profiles.join(", "));
    }
    println!(
        "{LABEL}backend{LABEL:#}       {}",
        format!("{:?}", report.backend).to_lowercase()
    );
    println!("{LABEL}grants{LABEL:#}");
    let width = resolved
        .policy
        .grants
        .iter()
        .map(|g| g.path.as_os_str().len())
        .max()
        .unwrap_or(0);
    for g in &resolved.policy.grants {
        let path = g.path.display().to_string();
        let access = style::access(g.access);
        println!(
            "  {access}{}{access:#} {path:<width$}  {DIM}← {}{DIM:#}",
            g.access, g.source
        );
    }
    let network = style::network(resolved.policy.network);
    println!(
        "{LABEL}network{LABEL:#}       {network}{}{network:#}",
        format!("{:?}", resolved.policy.network).to_lowercase()
    );
    for (k, v) in report.env {
        println!("{LABEL}env{LABEL:#}           {k}={v}");
    }
    for file in report.generated_files {
        println!("{LABEL}generated{LABEL:#}     {}", file.path.display());
    }
    println!("{LABEL}command{LABEL:#}       {}", launch.display_command());
    for warning in report.warnings {
        println!("{WARN}warning{WARN:#}       {warning}");
    }
    Ok(())
}
