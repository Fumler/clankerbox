use std::env;
use std::process::Command;

use anstream::println;
use anyhow::{Context, Result, bail};

use super::Session;
use crate::cli::GlobalOpts;
use crate::config::discover::{Origin, load_file};
use crate::config::trust::TrustStore;
use crate::style::OK;

pub fn edit(opts: &GlobalOpts, local: bool) -> Result<()> {
    let session = Session::open(opts)?;
    let origin = if local {
        Origin::Local
    } else {
        Origin::Project
    };
    let Some(layer) = session
        .discovered
        .project_layers()
        .filter(|l| l.origin == origin)
        .last()
    else {
        let init = if local { "init --local" } else { "init" };
        bail!("no config file to edit; create one with `cb {init}`");
    };
    let path = layer.path.clone().expect("project layers have a path");

    let editor = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    // Through the shell so that editors with arguments (`code --wait`) work.
    let status = Command::new("sh")
        .args(["-c", &format!("{editor} \"$1\""), "sh"])
        .arg(&path)
        .status()
        .with_context(|| format!("cannot run editor `{editor}`"))?;
    if !status.success() {
        bail!(
            "editor exited with {status}; {} not trusted",
            path.display()
        );
    }

    let layer = load_file(&path, origin)?.context("file was removed")?;
    let mut store = TrustStore::load(&session.dirs.state)?;
    store.trust(&layer);
    store.save()?;
    println!("{OK}trusted{OK:#} {}", path.display());
    Ok(())
}
