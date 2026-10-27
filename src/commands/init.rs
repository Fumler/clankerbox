use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::{Session, confirm, interactive};
use crate::cli::GlobalOpts;
use crate::config::discover::{LOCAL_FILE, Origin, PROJECT_FILE, load_file};
use crate::config::trust::TrustStore;

const PROJECT_TEMPLATE: &str = include_str!("../../templates/project.toml");
const LOCAL_TEMPLATE: &str = include_str!("../../templates/local.toml");

pub fn init(opts: &GlobalOpts, local: bool, force: bool) -> Result<()> {
    let session = Session::open(opts)?;
    let cwd = session.cwd();
    let git = session.discovered.git.as_ref();
    let (name, template, origin) = if local {
        (LOCAL_FILE, LOCAL_TEMPLATE, Origin::Local)
    } else {
        (PROJECT_FILE, PROJECT_TEMPLATE, Origin::Project)
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

    fs::write(&path, template).with_context(|| format!("cannot write {}", path.display()))?;
    if local && let Some(git) = git {
        exclude_from_git(&git.common_dir, name)?;
    }
    let layer = load_file(&path, origin)?.expect("file was just written");
    let mut store = TrustStore::load(&session.dirs.state)?;
    store.trust(&layer);
    store.save()?;
    println!("created {}", path.display());
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
