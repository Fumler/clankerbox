use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

const PROJECT_TEMPLATE: &str = include_str!("../../templates/project.toml");

#[derive(Debug)]
pub struct Toolchain {
    pub name: &'static str,
    markers: &'static [&'static str],
    groups: &'static [&'static str],
    linux_groups: &'static [&'static str],
    macos_groups: &'static [&'static str],
    /// Shared caches the toolchain writes to.
    pub allow: &'static [&'static str],
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
        allow: &[
            "${env:GRADLE_USER_HOME:-${home}/.gradle}",
            // JGit-based plugins write filesystem timestamp probes here.
            "${env:XDG_CONFIG_HOME:-${home}/.config}/jgit",
        ],
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
        // Read-only: writing its allow list would let the agent run code outside the sandbox.
        read: &[
            "${env:XDG_DATA_HOME:-${home}/.local/share}/direnv",
            "${env:XDG_CONFIG_HOME:-${home}/.config}/direnv",
        ],
        ..NONE
    },
];

const MAX_DEPTH: usize = 3;
const MAX_DIRS: usize = 2000;
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "dist",
    "out",
    "vendor",
    "venv",
    "__pycache__",
];

/// Outside a repository, subdirectories may be unrelated projects or other users' files.
pub fn scan_subdirs(root: &Path, home: &Path) -> bool {
    !home.starts_with(root) && [".git", ".jj"].iter().any(|vcs| root.join(vcs).exists())
}

pub fn detect_toolchains(root: &Path, subdirs: bool) -> Vec<&'static Toolchain> {
    let max_depth = if subdirs { MAX_DEPTH } else { 0 };
    let mut found = [false; TOOLCHAINS.len()];
    let mut queue = VecDeque::from([(root.to_path_buf(), 0)]);
    let mut queued = 1;
    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        // Sorted, so `MAX_DIRS` cuts off the same directories every time.
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            // Not following symlinks keeps the walk inside `root`.
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if depth < max_depth
                    && queued < MAX_DIRS
                    && !name.starts_with('.')
                    && !SKIP_DIRS.contains(&name)
                {
                    queue.push_back((entry.path(), depth + 1));
                    queued += 1;
                }
                continue;
            }
            for (hit, toolchain) in found.iter_mut().zip(TOOLCHAINS) {
                *hit |= toolchain.markers.contains(&name);
            }
        }
    }
    TOOLCHAINS
        .iter()
        .zip(found)
        .filter_map(|(t, hit)| hit.then_some(t))
        .collect()
}

pub fn project_template(toolchains: &[&Toolchain]) -> String {
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
        let toolchains = detect_toolchains(tmp.path(), false);
        let file = parse(&project_template(&toolchains), Path::new("t.toml")).unwrap();
        assert_eq!(file.sandbox.nono.groups, ["java_runtime"]);
        assert_eq!(
            file.sandbox.read,
            [
                "${env:XDG_DATA_HOME:-${home}/.local/share}/direnv",
                "${env:XDG_CONFIG_HOME:-${home}/.config}/direnv",
            ]
        );
        assert_eq!(
            file.sandbox.allow,
            [
                "${home}/.m2/repository",
                "${env:GRADLE_USER_HOME:-${home}/.gradle}",
                "${env:XDG_CONFIG_HOME:-${home}/.config}/jgit",
            ]
        );
    }

    #[test]
    fn detected_toolchains_become_groups() {
        let tmp = tempfile::tempdir().unwrap();
        for marker in ["Cargo.toml", "go.mod", "flake.nix"] {
            fs::write(tmp.path().join(marker), "").unwrap();
        }
        let toolchains = detect_toolchains(tmp.path(), false);
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
    fn detects_monorepo_modules_but_not_dependencies_or_hidden_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for (dir, marker) in [
            ("services/api", "pom.xml"),
            ("node_modules/dep", "Cargo.toml"),
            (".cache", "go.mod"),
            ("a/b/c/d", "flake.nix"),
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
            fs::write(root.join(dir).join(marker), "").unwrap();
        }
        let names: Vec<_> = detect_toolchains(root, true)
            .iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, ["maven"]);
        assert!(detect_toolchains(root, false).is_empty());
    }

    #[test]
    fn scans_subdirs_only_in_repositories_below_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        for (vcs, scan) in [(None, false), (Some(".git"), true), (Some(".jj"), true)] {
            let root = home.join(format!("p{vcs:?}"));
            fs::create_dir_all(&root).unwrap();
            if let Some(vcs) = vcs {
                fs::create_dir_all(root.join(vcs)).unwrap();
            }
            assert_eq!(scan_subdirs(&root, &home), scan, "{vcs:?}");
        }
        fs::create_dir_all(home.join(".git")).unwrap();
        assert!(!scan_subdirs(&home, &home));
    }

    #[test]
    fn no_toolchains_leaves_template_unchanged() {
        assert_eq!(project_template(&[]), PROJECT_TEMPLATE);
        parse(PROJECT_TEMPLATE, Path::new("t.toml")).unwrap();
    }
}
