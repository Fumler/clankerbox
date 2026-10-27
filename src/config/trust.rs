use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::discover::Layer;

#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    /// Canonical config path → SHA-256 of the trusted contents.
    entries: BTreeMap<String, String>,
}

impl TrustStore {
    pub fn load(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join("trust.toml");
        let entries = match fs::read_to_string(&path) {
            Ok(content) => toml::from_str(&content)
                .with_context(|| format!("corrupt trust store {}", path.display()))?,
            Err(e) if e.kind() == ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        Ok(Self { path, entries })
    }

    pub fn is_trusted(&self, layer: &Layer) -> bool {
        !layer.needs_trust() || self.entries.get(&key(layer)) == Some(&layer.hash)
    }

    pub fn trust(&mut self, layer: &Layer) {
        self.entries.insert(key(layer), layer.hash.clone());
    }

    /// Returns whether an entry was removed.
    pub fn untrust(&mut self, layer: &Layer) -> bool {
        self.entries.remove(&key(layer)).is_some()
    }

    /// Write the store atomically.
    pub fn save(&self) -> Result<()> {
        let dir = self.path.parent().expect("trust store path has a parent");
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let tmp = self.path.with_extension("toml.tmp");
        fs::write(&tmp, toml::to_string(&self.entries)?)
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("cannot write {}", self.path.display()))
    }
}

fn key(layer: &Layer) -> String {
    let path = layer.path.as_deref().expect("project layers have a path");
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::discover::{Origin, load_file};

    #[test]
    fn trust_follows_content() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(".clankerbox.toml");
        fs::write(&file, "agent = \"pi\"").unwrap();
        let layer = load_file(&file, Origin::Project).unwrap().unwrap();

        let mut store = TrustStore::load(tmp.path()).unwrap();
        assert!(!store.is_trusted(&layer));
        store.trust(&layer);
        store.save().unwrap();

        let store = TrustStore::load(tmp.path()).unwrap();
        assert!(store.is_trusted(&layer));

        fs::write(&file, "agent = \"claude\"").unwrap();
        let changed = load_file(&file, Origin::Project).unwrap().unwrap();
        assert!(!store.is_trusted(&changed));
    }
}
