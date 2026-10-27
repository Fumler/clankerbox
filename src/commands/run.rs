use std::convert::Infallible;
use std::fs::File;
use std::os::fd::{IntoRawFd, OwnedFd};
use std::path::Path;

use anyhow::{Result, bail};

use super::{Session, confirm, interactive, warn};
use crate::cli::RunArgs;
use crate::config::Resolved;
use crate::config::discover::Layer;
use crate::config::model::BackendKind;
use crate::config::trust::TrustStore;
use crate::policy::Source;
use crate::sandbox;

pub fn run(args: &RunArgs) -> Result<Infallible> {
    let session = Session::open(&args.global)?;
    let resolved = session.resolve()?;
    if args.global.dry_run {
        for warning in &resolved.warnings {
            warn(warning);
        }
        let launch = session.launch(&resolved, &args.sandbox, &args.agent_args)?;
        launch.write_files()?;
        println!("{}", launch.display_shell());
        std::process::exit(0);
    }
    sandbox::backend(&resolved, &session.dirs)
        .preflight(&mut |question| interactive().then(|| confirm(question).unwrap_or(false)))?;
    ensure_trusted(&session, &resolved)?;
    for warning in &resolved.warnings {
        warn(warning);
    }
    if resolved.backend == BackendKind::None {
        eprintln!(
            "cb: WARNING: running {} WITHOUT a sandbox",
            resolved.agent.command
        );
    }
    let launch = session.launch(&resolved, &args.sandbox, &args.agent_args)?;
    for grant in resolved.policy.grants.iter().filter(|g| g.pin) {
        if let Err(e) = pin(&grant.path) {
            warn(format_args!(
                "cannot hold {} open: {e}",
                grant.path.display()
            ));
        }
    }
    launch.exec()
}

/// Open `path` without close-on-exec and leak the fd, so it stays open in the agent.
fn pin(path: &Path) -> std::io::Result<()> {
    let fd = OwnedFd::from(File::open(path)?);
    rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::empty())?;
    let _ = fd.into_raw_fd();
    Ok(())
}

fn ensure_trusted(session: &Session, resolved: &Resolved) -> Result<()> {
    let mut store = TrustStore::load(&session.dirs.state)?;
    let untrusted: Vec<&Layer> = session
        .discovered
        .project_layers()
        .filter(|l| !store.is_trusted(l))
        .collect();
    if untrusted.is_empty() {
        return Ok(());
    }
    let list = untrusted
        .iter()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    if !interactive() {
        bail!("untrusted project config:\n{list}\nreview it, then run `cb trust`");
    }
    for layer in &untrusted {
        review(layer, resolved);
    }
    if !confirm("Trust these files and continue?")? {
        bail!("not trusted:\n{list}");
    }
    for layer in untrusted {
        store.trust(layer);
    }
    store.save()
}

fn review(layer: &Layer, resolved: &Resolved) {
    let path = layer.path.as_deref().unwrap_or(Path::new(""));
    eprintln!(
        "\ncb: {path} is new or has changed:\n",
        path = path.display()
    );
    for line in layer.content.lines() {
        eprintln!("  │ {line}");
    }
    let source = Source::File(path.to_path_buf());
    let grants: Vec<_> = resolved
        .policy
        .grants
        .iter()
        .filter(|g| g.source == source)
        .collect();
    if !grants.is_empty() {
        eprintln!("\n  grants:");
        for grant in grants {
            eprintln!("    {} {}", grant.access, grant.path.display());
        }
    }
    eprintln!();
}
