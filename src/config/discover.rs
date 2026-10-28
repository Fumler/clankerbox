use std::fmt::{self, Write as _};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};

use super::detect::{Toolchain, detect_toolchains, project_template, scan_subdirs};
use super::model::ConfigFile;
use crate::context::{Dirs, GitInfo};

pub const PROJECT_FILE: &str = ".clankerbox.toml";
pub const LOCAL_FILE: &str = ".clankerbox.local.toml";
const PRESETS: &str = include_str!("../presets.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    Preset,
    Global,
    Detected,
    Project,
    Local,
}

#[derive(Debug)]
pub struct Layer {
    pub origin: Origin,
    /// `None` for the built-in presets and detected config.
    pub path: Option<PathBuf>,
    /// Directory relative paths in this layer resolve against.
    pub dir: PathBuf,
    pub file: ConfigFile,
    /// The text `file` was parsed from; what the user reviews before trusting `hash`.
    pub content: String,
    pub hash: String,
}

impl Layer {
    pub fn needs_trust(&self) -> bool {
        matches!(
            self.origin,
            Origin::Detected | Origin::Project | Origin::Local
        )
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.path, self.origin) {
            (Some(path), _) => write!(f, "{}", path.display()),
            (None, Origin::Detected) => write!(f, "<detected config for {}>", self.dir.display()),
            (None, _) => f.write_str("<built-in presets>"),
        }
    }
}

/// Every layer that applies to a directory, lowest precedence first.
#[derive(Debug)]
pub struct Discovered {
    pub layers: Vec<Layer>,
    pub cwd: PathBuf,
    pub project_root: PathBuf,
    pub git: Option<GitInfo>,
    /// Set by [`Discovered::detect`] when there are no project files.
    pub detected: Option<Vec<&'static Toolchain>>,
}

impl Discovered {
    pub fn project_layers(&self) -> impl Iterator<Item = &Layer> {
        self.layers.iter().filter(|l| l.needs_trust())
    }

    pub fn approve_detected(&self) -> bool {
        self.layers
            .iter()
            .any(|l| l.origin == Origin::Global && l.file.approve_detected == Some(true))
    }

    /// `subdirs` searches subdirectories even outside a repository.
    pub fn detect(&mut self, dirs: &Dirs, subdirs: bool) -> Result<()> {
        if self.project_layers().next().is_some() {
            return Ok(());
        }
        let subdirs = subdirs || scan_subdirs(&self.project_root, &dirs.home);
        let toolchains = detect_toolchains(&self.project_root, subdirs);
        let content = project_template(&toolchains);
        let file = parse(&content, Path::new("<detected config>"))?;
        // Without comments, so template rewording keeps approvals.
        let hash = hash(toml::to_string(&file)?.as_bytes());
        self.layers.push(Layer {
            origin: Origin::Detected,
            path: None,
            dir: self.project_root.clone(),
            file,
            hash,
            content,
        });
        self.detected = Some(toolchains);
        Ok(())
    }
}

pub fn discover(cwd: &Path, dirs: &Dirs, no_project: bool) -> Result<Discovered> {
    let cwd = cwd
        .canonicalize()
        .with_context(|| format!("cannot access {}", cwd.display()))?;
    let git = GitInfo::discover(&cwd);

    let mut layers = vec![Layer {
        origin: Origin::Preset,
        path: None,
        dir: cwd.clone(),
        file: parse(PRESETS, Path::new("<built-in presets>"))?,
        content: PRESETS.to_owned(),
        hash: hash(PRESETS.as_bytes()),
    }];
    let global = dirs.global_config();
    if let Some(layer) = load_file(&global, Origin::Global)? {
        layers.push(layer);
    }

    let mut project = Vec::new();
    let mut explicit_root = None;
    if !no_project {
        // Outside git and $HOME, ancestors like /tmp may be writable by other users.
        let only_cwd = git.is_none() && !cwd.starts_with(&dirs.home);
        for dir in cwd.ancestors() {
            if git.is_none() && dir == dirs.home {
                break;
            }
            let mut at_dir = Vec::new();
            for (name, origin) in [(PROJECT_FILE, Origin::Project), (LOCAL_FILE, Origin::Local)] {
                if let Some(layer) = load_file(&dir.join(name), origin)? {
                    at_dir.push(layer);
                }
            }
            let is_root = at_dir.iter().any(|l| l.file.root == Some(true));
            // Collected leaf-first; reversed below so each local file follows its sibling.
            project.extend(at_dir.into_iter().rev());
            if is_root {
                explicit_root = Some(dir.to_path_buf());
                break;
            }
            if only_cwd || git.as_ref().is_some_and(|g| g.toplevel == dir) {
                break;
            }
        }
    }
    project.reverse();

    let project_root = explicit_root
        .or_else(|| git.as_ref().map(|g| g.toplevel.clone()))
        .or_else(|| project.first().map(|l| l.dir.clone()))
        .unwrap_or_else(|| cwd.clone());
    layers.extend(project);

    Ok(Discovered {
        layers,
        cwd,
        project_root,
        git,
        detected: None,
    })
}

pub fn load_file(path: &Path, origin: Origin) -> Result<Option<Layer>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    Ok(Some(Layer {
        origin,
        path: Some(path.to_path_buf()),
        dir: path.parent().unwrap_or(Path::new("/")).to_path_buf(),
        file: parse(&content, path)?,
        hash: hash(content.as_bytes()),
        content,
    }))
}

pub fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

pub fn parse(content: &str, path: &Path) -> Result<ConfigFile> {
    toml::from_str(content).map_err(|e| {
        let hint = suggestion(e.message())
            .map(|s| format!("help: did you mean `{s}`?\n"))
            .unwrap_or_default();
        anyhow!("invalid config {}:\n{e}{hint}", path.display())
    })
}

/// For serde's "unknown field `x`, expected one of `a`, `b`" messages, the closest valid key.
fn suggestion(message: &str) -> Option<&str> {
    let rest = message.strip_prefix("unknown field `")?;
    let (unknown, expected) = rest.split_once('`')?;
    expected
        .split('`')
        .skip(1)
        .step_by(2)
        .map(|candidate| (strsim::jaro_winkler(unknown, candidate), candidate))
        .filter(|(score, _)| *score > 0.8)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, candidate)| candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(home: &Path) -> Dirs {
        Dirs {
            home: home.to_path_buf(),
            config: home.join(".config/clankerbox"),
            state: home.join(".local/state/clankerbox"),
            runtime: home.join("run"),
            claude_config: None,
        }
    }

    #[test]
    fn presets_parse() {
        parse(PRESETS, Path::new("presets")).unwrap();
    }

    #[test]
    fn suggests_closest_key() {
        let err = parse("[sandbox]\nraed = []", Path::new("x.toml")).unwrap_err();
        assert!(err.to_string().contains("did you mean `read`?"), "{err}");
        let err = parse("[sandbox]\nzzzzzz = []", Path::new("x.toml")).unwrap_err();
        assert!(!err.to_string().contains("did you mean"), "{err}");
    }

    #[test]
    fn walks_from_cwd_to_git_root_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let repo = home.join("repo");
        let pkg = repo.join("pkg");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&pkg).unwrap();
        fs::write(home.join(PROJECT_FILE), "agent = \"outside\"").unwrap();
        fs::write(repo.join(PROJECT_FILE), "").unwrap();
        fs::write(repo.join(LOCAL_FILE), "").unwrap();
        fs::write(pkg.join(PROJECT_FILE), "").unwrap();

        let d = discover(&pkg, &dirs(&home), false).unwrap();
        let paths: Vec<_> = d
            .project_layers()
            .map(|l| l.path.clone().unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                repo.join(PROJECT_FILE),
                repo.join(LOCAL_FILE),
                pkg.join(PROJECT_FILE)
            ]
        );
        assert_eq!(d.project_root, repo);

        let d = discover(&pkg, &dirs(&home), true).unwrap();
        assert_eq!(d.project_layers().count(), 0);
    }

    #[test]
    fn root_marker_stops_walk_and_sets_project_root() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let repo = home.join("repo");
        let pkg = repo.join("pkg");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&pkg).unwrap();
        fs::write(repo.join(PROJECT_FILE), "").unwrap();
        fs::write(pkg.join(PROJECT_FILE), "root = true").unwrap();

        let d = discover(&pkg, &dirs(&home), false).unwrap();
        assert_eq!(d.project_layers().count(), 1);
        assert_eq!(d.project_root, pkg);
    }

    #[test]
    fn detects_config_only_without_project_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let repo = home.join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("crates/a")).unwrap();
        fs::write(repo.join("crates/a/Cargo.toml"), "").unwrap();

        let mut d = discover(&repo, &dirs(&home), false).unwrap();
        assert_eq!(d.project_layers().count(), 0);
        d.detect(&dirs(&home), false).unwrap();
        let detected: Vec<_> = d.project_layers().collect();
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].origin, Origin::Detected);
        assert_eq!(detected[0].dir, repo);
        assert_eq!(detected[0].file.sandbox.nono.groups, ["rust_runtime"]);
        let layer = detected[0];
        assert_ne!(layer.hash, hash(layer.content.as_bytes()));
        assert_eq!(
            layer.hash,
            hash(toml::to_string(&layer.file).unwrap().as_bytes())
        );
        let names: Vec<_> = d.detected.unwrap().iter().map(|t| t.name).collect();
        assert_eq!(names, ["rust"]);

        fs::write(repo.join(LOCAL_FILE), "").unwrap();
        let mut d = discover(&repo, &dirs(&home), false).unwrap();
        d.detect(&dirs(&home), false).unwrap();
        assert!(d.project_layers().all(|l| l.origin == Origin::Local));
        assert!(d.detected.is_none());
    }

    #[test]
    fn outside_git_and_home_only_cwd_is_searched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let cwd = root.join("shared/build");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(root.join("shared").join(PROJECT_FILE), "root = true").unwrap();
        fs::write(cwd.join(PROJECT_FILE), "").unwrap();

        let d = discover(&cwd, &dirs(&root.join("home")), false).unwrap();
        assert_eq!(d.project_layers().count(), 1);
        assert_eq!(d.project_root, cwd);
    }

    #[test]
    fn outside_git_uses_outermost_config_dir_below_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let a = home.join("a");
        let b = a.join("b");
        fs::create_dir_all(&b).unwrap();
        fs::write(home.join(PROJECT_FILE), "").unwrap();
        fs::write(a.join(PROJECT_FILE), "").unwrap();

        let d = discover(&b, &dirs(&home), false).unwrap();
        assert_eq!(d.project_layers().count(), 1);
        assert_eq!(d.project_root, a);
    }
}
