use std::convert::Infallible;
use std::fmt::Write as _;
use std::fs::File;
use std::os::fd::{IntoRawFd, OwnedFd};
use std::path::Path;

use anstream::{eprint, eprintln, println};
use anyhow::{Result, bail};

use super::{
    Session, cache_grants_text, confirm, display_home, grant_lines, interactive, network_label,
    notes, warn,
};
use crate::cli::RunArgs;
use crate::config::Resolved;
use crate::config::discover::{Layer, Origin, PROJECT_FILE};
use crate::policy::{Access, Grant, Source};
use crate::sandbox::Sandbox;
use crate::style::{DIM, ERROR, LABEL};

pub fn run(args: &RunArgs) -> Result<Infallible> {
    let session = Session::open(&args.global)?;
    let resolved = session.resolve()?;
    let backend = session.backend(&resolved);
    if let Some(blocker) = &resolved.blocker {
        bail!("{blocker}");
    }
    if args.global.dry_run {
        for warning in &resolved.warnings {
            warn(warning);
        }
        let launch = session.launch(&*backend, &resolved, args)?;
        launch.write_files()?;
        println!("{}", launch.display_shell());
        std::process::exit(0);
    }
    backend.preflight(&mut |question| interactive().then(|| confirm(question).unwrap_or(false)))?;
    ensure_trusted(&session, &*backend, &resolved)?;
    for warning in &resolved.warnings {
        warn(warning);
    }
    if !resolved.sandboxed() {
        eprintln!(
            "cb: {ERROR}WARNING: running {} WITHOUT a sandbox{ERROR:#}",
            resolved.agent.command
        );
    }
    let launch = session.launch(&*backend, &resolved, args)?;
    for grant in resolved.policy.grants.iter().filter(|g| g.pin) {
        if let Err(e) = pin(&grant.path) {
            warn(format_args!(
                "cannot hold {} open: {e}",
                grant.path.display()
            ));
        }
    }
    for warning in backend.prepare() {
        warn(warning);
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

fn ensure_trusted(session: &Session, backend: &dyn Sandbox, resolved: &Resolved) -> Result<()> {
    let mut store = session.trust_store()?;
    if session
        .discovered
        .project_layers()
        .any(|l| store.is_auto_approved(l))
    {
        warn_auto_approved_caches(session, resolved);
    }
    let untrusted: Vec<&Layer> = session
        .discovered
        .project_layers()
        .filter(|l| !store.is_trusted(l))
        .collect();
    if untrusted.is_empty() {
        return Ok(());
    }
    let detected = untrusted.iter().any(|l| l.origin == Origin::Detected);
    let root = session.discovered.project_root.display();
    let list = untrusted
        .iter()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    if !interactive() {
        if detected {
            bail!(
                "no {PROJECT_FILE} for {root}; review the detected grants with `cb show`, then \
                 run `cb trust`, pass -y, or set `approve_detected = true` in the global config"
            );
        }
        bail!("untrusted project config:\n{list}\nreview it, then run `cb trust`");
    }
    eprint!("{}", review_text(session, backend, resolved, &untrusted));
    let question = if detected {
        "Run with these grants?"
    } else {
        "Trust these files and continue?"
    };
    if !confirm(question)? {
        if detected {
            bail!("not approved; {}", init_hint(session));
        }
        bail!("not trusted:\n{list}");
    }
    for layer in untrusted {
        store.trust(layer);
    }
    store.save()
}

/// What the trust prompt shows before asking; the same tail for every kind of config.
fn review_text(
    session: &Session,
    backend: &dyn Sandbox,
    resolved: &Resolved,
    untrusted: &[&Layer],
) -> String {
    let mut out = String::new();
    let toolchains = session.discovered.detected.as_deref().unwrap_or_default();
    if untrusted.iter().any(|l| l.origin == Origin::Detected) {
        let found = if toolchains.is_empty() {
            "no toolchains detected".to_owned()
        } else {
            let names: Vec<_> = toolchains.iter().map(|t| t.name).collect();
            format!("detected {}", names.join(", "))
        };
        let _ = writeln!(
            out,
            "\ncb: no {PROJECT_FILE} for {LABEL}{}{LABEL:#}; {found}\n",
            session.discovered.project_root.display()
        );
        let grants: Vec<_> = resolved.policy.grants.iter().collect();
        write_grants(&mut out, &grants);
    } else {
        for layer in untrusted {
            write_file_review(&mut out, layer, resolved);
        }
    }
    for line in backend
        .base_access()
        .iter()
        .chain(&notes(backend, resolved))
    {
        let _ = writeln!(out, "  {DIM}{line}{DIM:#}");
    }
    let _ = writeln!(
        out,
        "  {LABEL}network:{LABEL:#} {}\n",
        network_label(resolved.policy.network)
    );
    out.push_str(&cache_grants_text(toolchains, session, &init_hint(session)));
    out
}

fn write_file_review(out: &mut String, layer: &Layer, resolved: &Resolved) {
    let path = layer.path.as_deref().unwrap_or(Path::new(""));
    let _ = writeln!(
        out,
        "\ncb: {LABEL}{}{LABEL:#} is new or has changed:\n",
        path.display()
    );
    for line in layer.content.lines() {
        let _ = writeln!(out, "  {DIM}│{DIM:#} {line}");
    }
    let _ = writeln!(out);
    let source = Source::File(path.to_path_buf());
    let grants: Vec<_> = resolved
        .policy
        .grants
        .iter()
        .filter(|g| g.source == source)
        .collect();
    write_grants(out, &grants);
}

fn write_grants(out: &mut String, grants: &[&Grant]) {
    if grants.is_empty() {
        return;
    }
    let _ = writeln!(out, "  {LABEL}grants:{LABEL:#}");
    for line in grant_lines(grants) {
        let _ = writeln!(out, "    {line}");
    }
}

fn init_hint(session: &Session) -> String {
    format!(
        "`cb init` in {} writes these to a {PROJECT_FILE} you can edit.",
        session.discovered.project_root.display()
    )
}

fn warn_auto_approved_caches(session: &Session, resolved: &Resolved) {
    let caches: Vec<_> = resolved
        .policy
        .grants
        .iter()
        .filter(|g| g.source == Source::Detected && g.access == Access::ReadWrite)
        .map(|g| display_home(&g.path, &session.dirs.home))
        .collect();
    if !caches.is_empty() {
        warn(format_args!(
            "approved detected grants include write access to shared caches: {}",
            caches.join(", ")
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::cli::GlobalOpts;
    use crate::config::discover::discover;
    use crate::config::model::Platform;
    use crate::context::Dirs;

    fn session(home: &Path) -> Session {
        let dirs = Dirs {
            home: home.to_path_buf(),
            config: home.join("config"),
            state: home.join("state"),
            runtime: home.join("run"),
            claude_config: None,
            orca: Some(crate::orca::Pane {
                scripts: home.join(".orca/agent-hooks"),
                ..crate::orca::Pane::default()
            }),
        };
        let mut discovered = discover(&home.join("repo"), &dirs, false).unwrap();
        discovered.detect(&dirs, false).unwrap();
        Session {
            dirs,
            platform: Platform::default(),
            discovered,
            opts: GlobalOpts::default(),
        }
    }

    fn prompt(home: &Path) -> String {
        let session = session(home);
        let resolved = session.resolve().unwrap();
        let untrusted: Vec<_> = session.discovered.project_layers().collect();
        let backend = session.backend(&resolved);
        review_text(&session, &*backend, &resolved, &untrusted)
    }

    #[test]
    fn every_prompt_shows_backend_access_and_network() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(home.join("repo/.git")).unwrap();
        fs::write(home.join("repo/Cargo.toml"), "").unwrap();
        fs::create_dir_all(home.join(".orca/agent-hooks")).unwrap();

        let detected = prompt(&home);
        assert!(detected.contains("detected rust"), "{detected}");
        assert!(detected.contains("shared package caches"), "{detected}");

        fs::create_dir_all(home.join("other")).unwrap();
        fs::write(
            home.join("repo/.clankerbox.toml"),
            "[sandbox]\nread = [\"../other\"]",
        )
        .unwrap();
        let file = prompt(&home);
        assert!(file.contains("is new or has changed"), "{file}");

        for text in [detected, file] {
            assert!(
                text.contains("plus everything nono profile cb:claude allows"),
                "{text}"
            );
            assert!(text.contains("network:"), "{text}");
            assert!(text.contains("explains sandbox denials"), "{text}");
            assert!(text.contains("sandbox.orca = false"), "{text}");
        }
    }
}
