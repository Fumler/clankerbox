use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::discover::{Layer, Origin};

#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    /// Canonical config path → SHA-256 of the trusted contents.
    entries: BTreeMap<String, String>,
    pub approve_detected: bool,
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
        Ok(Self {
            path,
            entries,
            approve_detected: false,
        })
    }

    pub fn is_trusted(&self, layer: &Layer) -> bool {
        !layer.needs_trust() || self.is_stored(layer) || self.is_auto_approved(layer)
    }

    /// Detected config trusted only through `approve_detected`.
    pub fn is_auto_approved(&self, layer: &Layer) -> bool {
        self.approve_detected && layer.origin == Origin::Detected && !self.is_stored(layer)
    }

    fn is_stored(&self, layer: &Layer) -> bool {
        self.entries.get(&key(layer)) == Some(&layer.hash)
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
    // Detected config: project root, never a config file path.
    let path = layer.path.as_deref().unwrap_or(&layer.dir);
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::discover::{hash, load_file};
    use crate::config::model::ConfigFile;

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

    #[test]
    fn detected_config_is_keyed_by_project_root() {
        let tmp = tempfile::tempdir().unwrap();
        let detected = |dir: &Path, content: &str| Layer {
            origin: Origin::Detected,
            path: None,
            dir: dir.to_path_buf(),
            file: ConfigFile::default(),
            content: content.to_owned(),
            hash: hash(content.as_bytes()),
        };
        let a = tmp.path().join("a");
        fs::create_dir_all(&a).unwrap();
        let mut store = TrustStore::load(tmp.path()).unwrap();
        store.trust(&detected(&a, "x"));
        assert!(store.is_trusted(&detected(&a, "x")));
        assert!(!store.is_trusted(&detected(&a, "y")));
        assert!(!store.is_trusted(&detected(tmp.path(), "x")));
        store.approve_detected = true;
        assert!(store.is_trusted(&detected(tmp.path(), "x")));
    }
}
