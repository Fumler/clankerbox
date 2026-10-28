use anstream::println;
use anyhow::Result;

use super::Session;
use crate::cli::GlobalOpts;
use crate::config::discover::Layer;
use crate::config::trust::TrustStore;
use crate::style::{OK, WARN};

pub fn trust(opts: &GlobalOpts) -> Result<()> {
    update(opts, "trusted", OK, |store, layer| {
        store.trust(layer);
        true
    })
}

pub fn untrust(opts: &GlobalOpts) -> Result<()> {
    update(opts, "untrusted", WARN, TrustStore::untrust)
}

/// Apply `change` to every project file; it returns whether anything changed.
fn update(
    opts: &GlobalOpts,
    verb: &str,
    style: anstyle::Style,
    mut change: impl FnMut(&mut TrustStore, &Layer) -> bool,
) -> Result<()> {
    let session = Session::open(opts)?;
    let mut store = session.trust_store()?;
    let layers: Vec<_> = session.discovered.project_layers().collect();
    if layers.is_empty() {
        println!(
            "no project config files apply to {}",
            session.cwd().display()
        );
        return Ok(());
    }
    let changed: Vec<_> = layers
        .into_iter()
        .filter(|l| change(&mut store, l))
        .collect();
    store.save()?;
    for layer in changed {
        println!("{style}{verb}{style:#} {layer}");
    }
    Ok(())
}
