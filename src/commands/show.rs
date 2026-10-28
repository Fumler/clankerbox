use std::path::PathBuf;

use anstream::println;
use anyhow::Result;
use serde::Serialize;

use super::{Session, grant_lines, network_label, warn};
use crate::cli::RunArgs;
use crate::config::model::{BackendKind, NonoConfig};
use crate::policy::Policy;
use crate::sandbox::{self, GeneratedFile};
use crate::style::{DIM, ERROR, LABEL, OK, WARN};

#[derive(Serialize)]
struct Report<'a> {
    project_root: &'a PathBuf,
    config_files: Vec<ConfigFileReport>,
    agent: AgentReport<'a>,
    backend: BackendKind,
    policy: &'a Policy,
    base_access: Vec<String>,
    full_access: Vec<String>,
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
    let backend = sandbox::backend(&resolved, &session.dirs);
    let full_access = launch
        .write_files()
        .and_then(|()| backend.full_access(&launch))
        .unwrap_or_else(|e| {
            warn(format_args!("cannot list sandbox rules: {e:#}"));
            Vec::new()
        });
    let store = session.trust_store()?;

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
        base_access: backend.base_access(),
        full_access,
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
    let grants: Vec<_> = resolved.policy.grants.iter().collect();
    for line in grant_lines(&grants) {
        println!("  {line}");
    }
    print_sandbox_rules(&report);
    println!(
        "{LABEL}network{LABEL:#}       {}",
        network_label(resolved.policy.network)
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

fn print_sandbox_rules(report: &Report<'_>) {
    if report.full_access.is_empty() {
        for line in &report.base_access {
            println!("  {DIM}{line}{DIM:#}");
        }
        return;
    }
    println!("{LABEL}sandbox rules{LABEL:#} {DIM}(from the backend; [user] are cb's){DIM:#}");
    for line in &report.full_access {
        println!("  {line}");
    }
}
