use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// XDG base directories on every platform, including macOS, so dotfiles stay portable.
#[derive(Debug, Clone)]
pub struct Dirs {
    pub home: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    /// `$XDG_RUNTIME_DIR/clankerbox`, falling back to `$XDG_CACHE_HOME/clankerbox`.
    pub runtime: PathBuf,
    /// `$CLAUDE_CONFIG_DIR`.
    pub claude_config: Option<PathBuf>,
}

impl Dirs {
    pub fn from_env() -> Result<Self> {
        let home = env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .context("$HOME is not set")?;
        // Compared against canonical project paths, e.g. when /home is a symlink.
        let home = home.canonicalize().unwrap_or(home);
        let xdg = |var: &str, fallback: &str| {
            env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
                .join("clankerbox")
        };
        // Generated sandbox profiles live here, so never fall back to a shared directory like /tmp.
        let runtime = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map_or_else(|| xdg("XDG_CACHE_HOME", ".cache"), |p| p.join("clankerbox"));
        Ok(Self {
            config: xdg("XDG_CONFIG_HOME", ".config"),
            state: xdg("XDG_STATE_HOME", ".local/state"),
            runtime,
            claude_config: env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
            home,
        })
    }

    pub fn global_config(&self) -> PathBuf {
        self.config.join("config.toml")
    }
}

/// The first match for `command` on `$PATH`, or `command` itself if it is a path.
pub fn which(command: &str) -> Option<PathBuf> {
    if command.contains('/') {
        return Path::new(command).is_file().then(|| command.into());
    }
    env::split_paths(&env::var_os("PATH")?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(command))
        .find(|path| path.is_file())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitInfo {
    pub toplevel: PathBuf,
    /// Directory holding objects and refs; outside `toplevel` for linked worktrees.
    pub common_dir: PathBuf,
}

impl GitInfo {
    /// Reads `.git` directly so that a run never spawns git.
    ///
    /// The nearest `.git` always wins, even if it is broken (e.g. a stale worktree): skipping it
    /// would make an enclosing repository the project root.
    pub fn discover(start: &Path) -> Option<Self> {
        let dir = start
            .ancestors()
            .find(|dir| dir.join(".git").symlink_metadata().is_ok())?;
        let dot_git = dir.join(".git");
        let common_dir = Self::common_dir(dir, &dot_git).unwrap_or(dot_git);
        Some(Self {
            toplevel: dir.to_path_buf(),
            common_dir,
        })
    }

    fn common_dir(dir: &Path, dot_git: &Path) -> Option<PathBuf> {
        if dot_git.is_dir() {
            return dot_git.canonicalize().ok();
        }
        // Linked worktree or submodule: `.git` is a file containing `gitdir: <path>`.
        let content = fs::read_to_string(dot_git).ok()?;
        let gitdir = dir.join(content.strip_prefix("gitdir:")?.trim());
        let common = match fs::read_to_string(gitdir.join("commondir")) {
            Ok(common) => gitdir.join(common.trim()),
            Err(_) => gitdir,
        };
        common.canonicalize().ok()
    }

    pub fn main_worktree(&self) -> Option<&Path> {
        let main = self.common_dir.parent()?;
        (self.common_dir.file_name()? == ".git" && main != self.toplevel).then_some(main)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_plain_repo_and_linked_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let main = root.join("main");
        let wt_meta = main.join(".git/worktrees/feature");
        fs::create_dir_all(&wt_meta).unwrap();
        fs::create_dir_all(main.join("src")).unwrap();
        fs::write(wt_meta.join("commondir"), "../..\n").unwrap();
        let wt = root.join("feature");
        fs::create_dir_all(wt.join("nested")).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_meta.display())).unwrap();

        let plain = GitInfo::discover(&main.join("src")).unwrap();
        assert_eq!(plain.toplevel, main);
        assert_eq!(plain.common_dir, main.join(".git"));
        assert_eq!(plain.main_worktree(), None);

        let broken = root.join("main/src/stale");
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join(".git"), "gitdir: /nonexistent\n").unwrap();
        let stale = GitInfo::discover(&broken).unwrap();
        assert_eq!(stale.toplevel, broken);
        assert!(stale.common_dir.starts_with(&broken));

        let linked = GitInfo::discover(&wt.join("nested")).unwrap();
        assert_eq!(linked.toplevel, wt);
        assert_eq!(linked.common_dir, main.join(".git"));
        assert_eq!(linked.main_worktree(), Some(main.as_path()));
    }
}
