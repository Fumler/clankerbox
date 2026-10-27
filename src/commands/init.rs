use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::{Session, confirm, interactive, warn};
use crate::cli::GlobalOpts;
use crate::config::discover::{LOCAL_FILE, Origin, PROJECT_FILE, load_file};
use crate::config::trust::TrustStore;
use crate::config::vars::Vars;

const PROJECT_TEMPLATE: &str = include_str!("../../templates/project.toml");
const LOCAL_TEMPLATE: &str = include_str!("../../templates/local.toml");

pub fn init(opts: &GlobalOpts, local: bool, force: bool) -> Result<()> {
    let session = Session::open(opts)?;
    let cwd = session.cwd();
    let git = session.discovered.git.as_ref();
    let (name, template, origin) = if local {
        (LOCAL_FILE, LOCAL_TEMPLATE.to_owned(), Origin::Local)
    } else {
        let toolchains = detect_toolchains(cwd);
        if !toolchains.is_empty() {
            let names: Vec<_> = toolchains.iter().map(|t| t.name).collect();
            println!("detected {}", names.join(", "));
        }
        report_cache_grants(&toolchains, &session);
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
    let mut store = TrustStore::load(&session.dirs.state)?;
    store.trust(&layer);
    store.save()?;
    println!("created {}", path.display());
    Ok(())
}

struct Toolchain {
    name: &'static str,
    markers: &'static [&'static str],
    groups: &'static [&'static str],
    linux_groups: &'static [&'static str],
    macos_groups: &'static [&'static str],
    /// Shared caches the toolchain writes to.
    allow: &'static [&'static str],
    read: &'static [&'static str],
}

const NONE: Toolchain = Toolchain {
    name: "",
    markers: &[],
    groups: &[],
    linux_groups: &[],
    macos_groups: &[],
    allow: &[],
    read: &[],
};

const TOOLCHAINS: &[Toolchain] = &[
    Toolchain {
        name: "rust",
        markers: &["Cargo.toml"],
        groups: &["rust_runtime"],
        allow: &[
            "${env:CARGO_HOME:-${home}/.cargo}/registry",
            "${env:CARGO_HOME:-${home}/.cargo}/.package-cache",
            "${env:CARGO_HOME:-${home}/.cargo}/.package-cache-mutate",
            "${env:CARGO_HOME:-${home}/.cargo}/.global-cache",
        ],
        ..NONE
    },
    Toolchain {
        name: "go",
        markers: &["go.mod"],
        groups: &["go_runtime"],
        linux_groups: &["go_runtime_linux"],
        macos_groups: &["go_runtime_macos"],
        allow: &["${env:GOPATH:-${home}/go}/pkg"],
        ..NONE
    },
    Toolchain {
        name: "node",
        markers: &["package.json"],
        groups: &["node_runtime"],
        ..NONE
    },
    Toolchain {
        name: "pnpm",
        markers: &["pnpm-lock.yaml"],
        allow: &[
            "${env:XDG_DATA_HOME:-${home}/.local/share}/pnpm",
            "${env:XDG_STATE_HOME:-${home}/.local/state}/pnpm",
        ],
        ..NONE
    },
    Toolchain {
        name: "bun",
        markers: &["bun.lock", "bun.lockb", "bunfig.toml"],
        groups: &["bun_runtime"],
        ..NONE
    },
    Toolchain {
        name: "python",
        markers: &[
            "pyproject.toml",
            "requirements.txt",
            "setup.py",
            "Pipfile",
            "uv.lock",
        ],
        groups: &["python_runtime"],
        ..NONE
    },
    Toolchain {
        name: "maven",
        markers: &["pom.xml"],
        groups: &["java_runtime"],
        allow: &["${home}/.m2/repository"],
        ..NONE
    },
    Toolchain {
        name: "gradle",
        markers: &[
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "settings.gradle.kts",
        ],
        groups: &["java_runtime"],
        allow: &["${env:GRADLE_USER_HOME:-${home}/.gradle}"],
        ..NONE
    },
    Toolchain {
        name: "nix",
        markers: &["flake.nix", "shell.nix", "default.nix"],
        groups: &["nix_runtime"],
        ..NONE
    },
    Toolchain {
        name: "mise",
        markers: &["mise.toml", ".mise.toml", ".tool-versions"],
        groups: &["mise_manager"],
        ..NONE
    },
    Toolchain {
        name: "direnv",
        markers: &[".envrc"],
        allow: &["${env:XDG_DATA_HOME:-${home}/.local/share}/direnv"],
        read: &["${env:XDG_CONFIG_HOME:-${home}/.config}/direnv"],
        ..NONE
    },
];

fn detect_toolchains(dir: &Path) -> Vec<&'static Toolchain> {
    TOOLCHAINS
        .iter()
        .filter(|t| t.markers.iter().any(|m| dir.join(m).exists()))
        .collect()
}

fn project_template(toolchains: &[&Toolchain]) -> String {
    let list = |field: fn(&Toolchain) -> &'static [&'static str]| {
        let mut items: Vec<&str> = Vec::new();
        for item in toolchains.iter().flat_map(|t| field(t)) {
            if !items.contains(item) {
                items.push(item);
            }
        }
        match items.as_slice() {
            [] => String::new(),
            [one] => format!("[\"{one}\"]"),
            many => {
                let lines = many.iter().fold(String::new(), |mut out, i| {
                    let _ = writeln!(out, "    \"{i}\",");
                    out
                });
                format!("[\n{lines}]")
            }
        }
    };
    let mut out = PROJECT_TEMPLATE.to_owned();
    for (placeholder, key, items) in [
        (GROUPS_PLACEHOLDER, "groups", list(|t| t.groups)),
        (ALLOW_PLACEHOLDER, "allow", list(|t| t.allow)),
    ] {
        if !items.is_empty() {
            out = out.replacen(placeholder, &format!("{key} = {items}"), 1);
        }
    }
    let read = list(|t| t.read);
    if !read.is_empty() {
        out = out.replacen(READ_ANCHOR, &format!("{READ_ANCHOR}\nread = {read}"), 1);
    }
    for (platform, groups) in [
        ("linux", list(|t| t.linux_groups)),
        ("macos", list(|t| t.macos_groups)),
    ] {
        if !groups.is_empty() {
            let _ = write!(out, "\n[when.{platform}.sandbox.nono]\ngroups = {groups}\n");
        }
    }
    out
}

const GROUPS_PLACEHOLDER: &str = "# groups = [\"node_runtime\"]";
const ALLOW_PLACEHOLDER: &str = "# allow = []";
const READ_ANCHOR: &str = "# read = [\"${projects}/frontend\"]";

/// Lists write grants to shared caches, which unsandboxed builds also use.
fn report_cache_grants(toolchains: &[&Toolchain], session: &Session) {
    let vars = Vars {
        project_root: session.discovered.project_root.clone(),
        home: session.dirs.home.clone(),
        cwd: session.cwd().clone(),
        ..Vars::default()
    };
    let rows: Vec<_> = toolchains
        .iter()
        .flat_map(|t| t.allow.iter().map(move |raw| (t.name, *raw)))
        .map(|(name, raw)| {
            let path = vars
                .resolve_path(raw, session.cwd())
                .map_or_else(|_| raw.to_owned(), |p| display_home(&p, &vars.home));
            (name, path)
        })
        .collect();
    if rows.is_empty() {
        return;
    }
    warn(
        "write access to shared package caches; the agent can change packages your unsandboxed builds use:",
    );
    let width = rows.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    for (name, path) in rows {
        eprintln!("  {name:<width$}  rw  {path}");
    }
    eprintln!("  Remove entries from `allow` in {PROJECT_FILE} if unwanted.");
}

fn display_home(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::discover::parse;

    #[test]
    fn template_contains_placeholders() {
        for placeholder in [GROUPS_PLACEHOLDER, ALLOW_PLACEHOLDER, READ_ANCHOR] {
            assert!(PROJECT_TEMPLATE.contains(placeholder), "{placeholder}");
        }
    }

    #[test]
    fn direnv_adds_read_and_shared_groups_are_deduplicated() {
        let tmp = tempfile::tempdir().unwrap();
        for marker in [".envrc", "pom.xml", "build.gradle"] {
            fs::write(tmp.path().join(marker), "").unwrap();
        }
        let toolchains = detect_toolchains(tmp.path());
        let file = parse(&project_template(&toolchains), Path::new("t.toml")).unwrap();
        assert_eq!(file.sandbox.nono.groups, ["java_runtime"]);
        assert_eq!(
            file.sandbox.read,
            ["${env:XDG_CONFIG_HOME:-${home}/.config}/direnv"]
        );
        assert_eq!(file.sandbox.allow.len(), 3);
    }

    #[test]
    fn detected_toolchains_become_groups() {
        let tmp = tempfile::tempdir().unwrap();
        for marker in ["Cargo.toml", "go.mod", "flake.nix"] {
            fs::write(tmp.path().join(marker), "").unwrap();
        }
        let toolchains = detect_toolchains(tmp.path());
        let names: Vec<_> = toolchains.iter().map(|t| t.name).collect();
        assert_eq!(names, ["rust", "go", "nix"]);

        let file = parse(&project_template(&toolchains), Path::new("t.toml")).unwrap();
        assert_eq!(
            file.sandbox.nono.groups,
            ["rust_runtime", "go_runtime", "nix_runtime"]
        );
        assert_eq!(
            file.sandbox.allow,
            [
                "${env:CARGO_HOME:-${home}/.cargo}/registry",
                "${env:CARGO_HOME:-${home}/.cargo}/.package-cache",
                "${env:CARGO_HOME:-${home}/.cargo}/.package-cache-mutate",
                "${env:CARGO_HOME:-${home}/.cargo}/.global-cache",
                "${env:GOPATH:-${home}/go}/pkg",
            ]
        );
        let when = file.when;
        assert_eq!(
            when.linux.unwrap().sandbox.nono.groups,
            ["go_runtime_linux"]
        );
        assert_eq!(
            when.macos.unwrap().sandbox.nono.groups,
            ["go_runtime_macos"]
        );
    }

    #[test]
    fn no_toolchains_leaves_template_unchanged() {
        assert_eq!(project_template(&[]), PROJECT_TEMPLATE);
        parse(PROJECT_TEMPLATE, Path::new("t.toml")).unwrap();
    }
}
