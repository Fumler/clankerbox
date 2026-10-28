use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use anstream::{eprint, println};
use anyhow::{Context, Result, bail};

use super::{Session, cache_grants_text, confirm, interactive};
use crate::cli::GlobalOpts;
use crate::config::detect::{detect_toolchains, project_template, scan_subdirs};
use crate::config::discover::{LOCAL_FILE, Origin, PROJECT_FILE, load_file};
use crate::style::{LABEL, OK};

const LOCAL_TEMPLATE: &str = include_str!("../../templates/local.toml");

pub fn init(opts: &GlobalOpts, local: bool, force: bool) -> Result<()> {
    let session = Session::open_undetected(opts)?;
    let cwd = session.cwd();
    let git = session.discovered.git.as_ref();
    let (name, template, origin) = if local {
        (LOCAL_FILE, LOCAL_TEMPLATE.to_owned(), Origin::Local)
    } else {
        let subdirs = opts.detect_subdirs || scan_subdirs(cwd, &session.dirs.home);
        let toolchains = detect_toolchains(cwd, subdirs);
        if !toolchains.is_empty() {
            let names: Vec<_> = toolchains.iter().map(|t| t.name).collect();
            println!("{LABEL}detected{LABEL:#} {}", names.join(", "));
        }
        eprint!(
            "{}",
            cache_grants_text(
                &toolchains,
                &session,
                &format!("Remove entries from `allow` in {PROJECT_FILE} if unwanted."),
            )
        );
        (PROJECT_FILE, project_template(&toolchains), Origin::Project)
    };
    let path = cwd.join(name);
    if path.exists() && !force {
        bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    if let Some(git) = git
        && &git.toplevel != cwd
        && !force
    {
        let question = format!(
            "{} is not the repository root ({}). Create {name} here anyway?",
            cwd.display(),
            git.toplevel.display()
        );
        if !interactive() {
            bail!("{question} Pass --force to confirm.");
        }
        if !confirm(&question)? {
            bail!("aborted");
        }
    }

    fs::write(&path, &template).with_context(|| format!("cannot write {}", path.display()))?;
    if local && let Some(git) = git {
        exclude_from_git(&git.common_dir, name)?;
    }
    let layer = load_file(&path, origin)?.expect("file was just written");
    let mut store = session.trust_store()?;
    store.trust(&layer);
    store.save()?;
    println!("{OK}created{OK:#} {}", path.display());
    Ok(())
}

/// Add `pattern` to the repository's `info/exclude`, which is never committed.
fn exclude_from_git(common_dir: &Path, pattern: &str) -> Result<()> {
    let path = common_dir.join("info/exclude");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == pattern) {
        return Ok(());
    }
    fs::create_dir_all(path.parent().expect("has parent"))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let newline = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    writeln!(file, "{newline}{pattern}")?;
    Ok(())
}
