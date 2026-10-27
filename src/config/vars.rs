use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

pub const ENV_PREFIX: &str = "CLANKERBOX_VAR_";

const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, Default)]
pub struct Vars {
    /// User variables, already merged by precedence.
    pub user: BTreeMap<String, String>,
    pub project_root: PathBuf,
    pub home: PathBuf,
    pub cwd: PathBuf,
}

impl Vars {
    /// `config_dir` is the directory of the file the entry came from.
    pub fn expand(&self, input: &str, config_dir: &Path) -> Result<String, String> {
        self.expand_depth(input, config_dir, 0)
    }

    fn expand_depth(&self, input: &str, config_dir: &Path, depth: usize) -> Result<String, String> {
        if depth > MAX_DEPTH {
            return Err(format!("variables nested too deeply in `{input}`"));
        }
        let mut out = String::with_capacity(input.len());
        let mut rest = input;
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let end =
                closing_brace(after).ok_or_else(|| format!("unterminated `${{` in `{input}`"))?;
            let expr = &after[..end];
            let (name, fallback) = match expr.split_once(":-") {
                Some((name, fallback)) => (name, Some(fallback.to_owned())),
                None => (expr, None),
            };
            let value = match (self.lookup(name, config_dir), fallback) {
                (Some(value), _) | (None, Some(value)) => {
                    self.expand_depth(&value, config_dir, depth + 1)?
                }
                (None, None) => return Err(format!("undefined variable `{name}`")),
            };
            out.push_str(&value);
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    fn lookup(&self, name: &str, config_dir: &Path) -> Option<String> {
        let path = |p: &Path| Some(p.to_string_lossy().into_owned());
        match name {
            "project_root" => path(&self.project_root),
            "config_dir" => path(config_dir),
            "home" => path(&self.home),
            "cwd" => path(&self.cwd),
            _ => match name.strip_prefix("env:") {
                Some(var) => env::var(var).ok(),
                None => self.user.get(name).cloned(),
            },
        }
    }

    /// The path is not required to exist.
    pub fn resolve_path(&self, input: &str, config_dir: &Path) -> Result<PathBuf, String> {
        let expanded = self.expand(input, config_dir)?;
        let path = match expanded.strip_prefix('~') {
            Some("") => self.home.clone(),
            Some(rest) if rest.starts_with('/') => self.home.join(&rest[1..]),
            _ => PathBuf::from(&expanded),
        };
        Ok(config_dir.join(path))
    }
}

pub fn env_overrides() -> impl Iterator<Item = (String, String)> {
    env::vars_os().filter_map(|(k, v)| {
        let name = k.to_str()?.strip_prefix(ENV_PREFIX)?.to_owned();
        Some((name, v.into_string().ok()?))
    })
}

/// Index of the `}` closing a `${` whose contents start `s`, skipping nested `${...}`.
fn closing_brace(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '$' if chars.next_if(|&(_, c)| c == '{').is_some() => depth += 1,
            '}' if depth == 0 => return Some(i),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars {
        Vars {
            user: [
                ("projects".into(), "..".into()),
                ("nested".into(), "${projects}/x".into()),
                ("loop".into(), "${loop}".into()),
            ]
            .into(),
            project_root: "/repo".into(),
            home: "/home/me".into(),
            cwd: "/repo/sub".into(),
        }
    }

    #[test]
    fn expands_user_builtin_and_fallback() {
        let v = vars();
        let dir = Path::new("/repo/sub");
        assert_eq!(v.expand("${projects}/fe", dir).unwrap(), "../fe");
        assert_eq!(v.expand("${nested}", dir).unwrap(), "../x");
        assert_eq!(
            v.expand("${config_dir}:${project_root}", dir).unwrap(),
            "/repo/sub:/repo"
        );
        assert_eq!(v.expand("${nope:-/opt}", dir).unwrap(), "/opt");
        assert_eq!(v.expand("${nope:-${projects}/y}", dir).unwrap(), "../y");
        assert_eq!(v.expand("${a:-${b:-c}}d", dir).unwrap(), "cd");
        assert_eq!(v.expand("plain $HOME", dir).unwrap(), "plain $HOME");
        assert!(v.expand("${nope}", dir).unwrap_err().contains("undefined"));
        assert!(v.expand("${loop}", dir).unwrap_err().contains("nested"));
        assert!(
            v.expand("${open", dir)
                .unwrap_err()
                .contains("unterminated")
        );
    }

    #[test]
    fn resolves_paths() {
        let v = vars();
        let dir = Path::new("/repo/sub");
        assert_eq!(
            v.resolve_path("${projects}/fe", dir).unwrap(),
            Path::new("/repo/sub/../fe")
        );
        assert_eq!(v.resolve_path("~/x", dir).unwrap(), Path::new("/home/me/x"));
        assert_eq!(v.resolve_path("~", dir).unwrap(), Path::new("/home/me"));
        assert_eq!(v.resolve_path("/abs", dir).unwrap(), Path::new("/abs"));
    }
}
