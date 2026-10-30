use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pane {
    /// Hook and status-line scripts Orca registers in agents' settings.
    pub scripts: PathBuf,
    /// Orca replaces the endpoint file by rename, so grant its directory.
    pub endpoint_dir: Option<PathBuf>,
    pub port: Option<u16>,
    pub claude_account: Account,
    pub user_data: Option<PathBuf>,
    pub cli_command: Option<PathBuf>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum Account {
    /// No switcher, or the user set their own `CLAUDE_CONFIG_DIR`.
    #[default]
    Unmanaged,
    Dir(PathBuf),
    /// Claude's own default: `CLAUDE_CONFIG_DIR` unset.
    Default,
    Invalid(String),
}

pub fn detect(var: impl Fn(&str) -> Option<String>, home: &Path) -> Option<Pane> {
    let var = |name: &str| var(name).filter(|v| !v.is_empty());
    var("ORCA_PANE_KEY")?;
    let path = |name: &str| var(name).map(PathBuf::from).filter(|p| p.is_absolute());
    let endpoint = path("ORCA_AGENT_HOOK_ENDPOINT");
    let endpoint_dir = endpoint
        .as_deref()
        .and_then(Path::parent)
        .and_then(|dir| dir.canonicalize().ok())
        .filter(|dir| hooks_dir(dir) && !home.starts_with(dir));
    // The file has the current port; the pane's variable goes stale when Orca restarts.
    let port = endpoint
        .as_deref()
        .and_then(endpoint_port)
        .or_else(|| var("ORCA_AGENT_HOOK_PORT")?.parse().ok());
    Some(Pane {
        scripts: home.join(".orca/agent-hooks"),
        endpoint_dir,
        port,
        claude_account: claude_account(&var, home),
        user_data: path("ORCA_USER_DATA_PATH"),
        cli_command: path("ORCA_CLI_COMMAND"),
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cli {
    /// Names the socket and holds its token.
    pub metadata: Option<PathBuf>,
    pub socket: Option<PathBuf>,
    /// The launcher, and the app it runs from.
    pub dirs: Vec<PathBuf>,
}

impl Pane {
    pub fn cli(&self, home: &Path) -> Cli {
        let metadata = self
            .user_data
            .as_ref()
            .map(|dir| dir.join("orca-runtime.json"))
            .filter(|file| file.is_file());
        let socket = metadata.as_deref().and_then(runtime_socket);
        let launcher = self
            .cli_command
            .as_deref()
            .and_then(|c| c.canonicalize().ok());
        let mut dirs: Vec<PathBuf> = launcher
            .iter()
            .filter_map(|l| l.parent().map(Path::to_path_buf))
            .collect();
        dirs.extend(launcher.as_deref().and_then(app_root));
        dirs.retain(|dir| !home.starts_with(dir));
        Cli {
            metadata,
            socket,
            dirs,
        }
    }
}

/// Orca keeps the endpoint in `<user data>/agent-hooks[/<namespace>]`; anything wider could
/// expose the rest of its user data, such as the CLI token.
fn hooks_dir(dir: &Path) -> bool {
    let named = |dir: Option<&Path>| {
        dir.and_then(Path::file_name)
            .is_some_and(|n| n == "agent-hooks")
    };
    named(Some(dir)) || named(dir.parent())
}

fn endpoint_port(endpoint: &Path) -> Option<u16> {
    std::fs::read_to_string(endpoint)
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("ORCA_AGENT_HOOK_PORT=")?.trim().parse().ok())
}

fn runtime_socket(metadata: &Path) -> Option<PathBuf> {
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(metadata).ok()?).ok()?;
    let unix = |t: &serde_json::Value| t["kind"] == "unix";
    let transport = match json["transports"].as_array() {
        Some(all) => all.iter().find(|t| unix(t))?,
        None => Some(&json["transport"]).filter(|t| unix(t))?,
    };
    let socket = PathBuf::from(transport["endpoint"].as_str()?);
    // Orca leaves the file behind when it crashes.
    (socket.is_absolute() && socket.exists()).then_some(socket)
}

/// The app directory holding `resources/bin/<launcher>`. On Linux, Orca's terminals get a shim
/// script that only `exec`s that launcher.
fn app_root(launcher: &Path) -> Option<PathBuf> {
    let root = |launcher: &Path| {
        let bin = launcher.parent()?;
        let resources = bin.parent()?;
        if bin.file_name()? != "bin" || !resources.file_name()?.eq_ignore_ascii_case("resources") {
            return None;
        }
        let parent = resources.parent()?;
        // macOS: `Orca.app/Contents/Resources`.
        if parent.file_name()? == "Contents" {
            parent.parent().map(Path::to_path_buf)
        } else {
            Some(parent.to_path_buf())
        }
    };
    root(launcher).or_else(|| {
        let script = std::fs::read_to_string(launcher).ok()?;
        let target = script.lines().find_map(|l| l.strip_prefix("exec \""))?;
        root(
            &PathBuf::from(target.split('"').next()?)
                .canonicalize()
                .ok()?,
        )
    })
}

/// Mirrors Orca's `claude` shell function, which cb bypasses.
fn claude_account(var: &impl Fn(&str) -> Option<String>, home: &Path) -> Account {
    let Some(pointer) = var("ORCA_CLAUDE_PROFILE_POINTER") else {
        return Account::Unmanaged;
    };
    let pointer = match pointer.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(pointer),
    };
    let current = var("CLAUDE_CONFIG_DIR");
    let user = var("ORCA_CLAUDE_USER_CONFIG_DIR");
    let same = |a: &str, b: &str| a.trim_end_matches('/') == b.trim_end_matches('/');
    if let Some(current) = &current
        && var("ORCA_CLAUDE_INJECTED_CONFIG_DIR").is_none_or(|i| !same(&i, current))
        && user.as_deref().is_none_or(|u| !same(u, current))
    {
        return Account::Unmanaged;
    }
    let selected = std::fs::read_to_string(&pointer).unwrap_or_default();
    let selected = selected.trim_end_matches('\n');
    let (dir, from) = match (selected.is_empty(), current, user) {
        (false, ..) => (selected.to_owned(), pointer.display().to_string()),
        (true, Some(_), Some(user)) => (user, "ORCA_CLAUDE_USER_CONFIG_DIR".to_owned()),
        (true, ..) => return Account::Default,
    };
    if Path::new(&dir).is_absolute() {
        Account::Dir(dir.into())
    } else {
        Account::Invalid(format!(
            "Orca's selected Claude account `{dir}` (from {from}) is not an absolute path"
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use super::*;

    fn pane(home: &Path, vars: &[(&str, &str)]) -> Option<Pane> {
        let vars: BTreeMap<_, _> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        detect(|k| vars.get(k).cloned(), home)
    }

    #[test]
    fn detects_pane_hook_endpoint_and_port() {
        let home = Path::new("/home/me");
        assert_eq!(pane(home, &[]), None);
        assert_eq!(pane(home, &[("ORCA_PANE_KEY", "")]), None);
        let p = pane(
            home,
            &[
                ("ORCA_PANE_KEY", "tab:1"),
                ("ORCA_AGENT_HOOK_ENDPOINT", "relative/endpoint.env"),
                ("ORCA_AGENT_HOOK_PORT", "4123"),
            ],
        )
        .unwrap();
        assert_eq!(p.scripts, home.join(".orca/agent-hooks"));
        assert_eq!((p.endpoint_dir, p.port), (None, Some(4123)));
        assert_eq!(p.claude_account, Account::Unmanaged);

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let endpoint_in = |rel: &str, home: &Path| {
            let file = root.join(rel);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(
                &file,
                "ORCA_AGENT_HOOK_TOKEN=t\nORCA_AGENT_HOOK_PORT=5000\n",
            )
            .unwrap();
            pane(
                home,
                &[
                    ("ORCA_PANE_KEY", "tab:1"),
                    ("ORCA_AGENT_HOOK_ENDPOINT", file.to_str().unwrap()),
                    ("ORCA_AGENT_HOOK_PORT", "4123"),
                ],
            )
            .unwrap()
        };
        let p = endpoint_in("data/agent-hooks/endpoint.env", home);
        assert_eq!(p.port, Some(5000));
        assert_eq!(p.endpoint_dir, Some(root.join("data/agent-hooks")));
        let p = endpoint_in("data/agent-hooks/ns/endpoint.env", home);
        assert_eq!(p.endpoint_dir, Some(root.join("data/agent-hooks/ns")));
        assert_eq!(endpoint_in("data/endpoint.env", home).endpoint_dir, None);
        let p = endpoint_in("agent-hooks/endpoint.env", &root.join("agent-hooks/me"));
        assert_eq!(p.endpoint_dir, None);
    }

    #[test]
    fn cli_finds_socket_launcher_and_app_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let app = root.join("opt/Orca");
        fs::create_dir_all(app.join("resources/bin")).unwrap();
        fs::write(app.join("resources/bin/orca"), "").unwrap();
        let data = root.join("data");
        fs::create_dir_all(data.join("linux-orca-cli-shim")).unwrap();
        let shim = data.join("linux-orca-cli-shim/orca");
        fs::write(
            &shim,
            format!(
                "#!/usr/bin/env bash\nexec \"{}\" \"$@\"\n",
                app.join("resources/bin/orca").display()
            ),
        )
        .unwrap();
        let socket = data.join("o-42-rt.sock");
        fs::write(&socket, "").unwrap();
        fs::write(
            data.join("orca-runtime.json"),
            format!(
                r#"{{"transports":[{{"kind":"tcp"}},{{"kind":"unix","endpoint":"{}"}}]}}"#,
                socket.display()
            ),
        )
        .unwrap();
        let pane = |command: &Path| {
            pane(
                &root.join("home"),
                &[
                    ("ORCA_PANE_KEY", "p"),
                    ("ORCA_USER_DATA_PATH", data.to_str().unwrap()),
                    ("ORCA_CLI_COMMAND", command.to_str().unwrap()),
                ],
            )
            .unwrap()
        };

        let cli = pane(&shim).cli(&root.join("home"));
        assert_eq!(cli.metadata, Some(data.join("orca-runtime.json")));
        assert_eq!(cli.socket, Some(socket.clone()));
        assert_eq!(cli.dirs, [data.join("linux-orca-cli-shim"), app.clone()]);

        let cli = pane(&app.join("resources/bin/orca")).cli(&root.join("home"));
        assert_eq!(cli.dirs, [app.join("resources/bin"), app.clone()]);

        let mac = root.join("Applications/Orca.app");
        let mac_bin = mac.join("Contents/Resources/bin");
        fs::create_dir_all(&mac_bin).unwrap();
        fs::write(mac_bin.join("orca"), "").unwrap();
        let cli = pane(&mac_bin.join("orca")).cli(&root.join("home"));
        assert_eq!(cli.dirs, [mac_bin, mac]);

        fs::write(
            data.join("orca-runtime.json"),
            r#"{"transport":{"kind":"unix","endpoint":"rel.sock"}}"#,
        )
        .unwrap();
        assert_eq!(pane(&shim).cli(&root.join("home")).socket, None);
        fs::write(
            data.join("orca-runtime.json"),
            format!(
                r#"{{"transport":{{"kind":"unix","endpoint":"{}"}}}}"#,
                data.join("o-1-gone.sock").display()
            ),
        )
        .unwrap();
        assert_eq!(pane(&shim).cli(&root.join("home")).socket, None);
        let cli = pane(&shim).cli(&app.join("home"));
        assert_eq!(cli.dirs, [data.join("linux-orca-cli-shim")]);
    }

    #[test]
    fn follows_account_pointer_like_orcas_shell_function() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let account = |pointer: &str, vars: &[(&str, &str)]| {
            fs::write(home.join("pointer"), pointer).unwrap();
            let mut all = vec![
                ("ORCA_PANE_KEY", "p"),
                ("ORCA_CLAUDE_PROFILE_POINTER", "~/pointer"),
            ];
            all.extend_from_slice(vars);
            pane(home, &all).unwrap().claude_account
        };
        let injected = [
            ("CLAUDE_CONFIG_DIR", "/orca/a/home"),
            ("ORCA_CLAUDE_INJECTED_CONFIG_DIR", "/orca/a/home"),
        ];

        assert_eq!(
            account("/orca/b/home\n", &injected),
            Account::Dir("/orca/b/home".into())
        );
        assert_eq!(
            account("/orca/b/home", &[]),
            Account::Dir("/orca/b/home".into())
        );
        assert_eq!(account("", &injected), Account::Default);
        assert_eq!(
            account(
                "",
                &[
                    injected[0],
                    injected[1],
                    ("ORCA_CLAUDE_USER_CONFIG_DIR", "/me/claude")
                ]
            ),
            Account::Dir("/me/claude".into())
        );
        assert!(matches!(
            account("rel/home", &injected),
            Account::Invalid(_)
        ));
        let slashed = [
            ("CLAUDE_CONFIG_DIR", "/orca/a/home/"),
            ("ORCA_CLAUDE_INJECTED_CONFIG_DIR", "/orca/a/home"),
        ];
        assert_eq!(
            account("/orca/b/home", &slashed),
            Account::Dir("/orca/b/home".into())
        );
        let relative_user = [
            injected[0],
            injected[1],
            ("ORCA_CLAUDE_USER_CONFIG_DIR", "claude-home"),
        ];
        assert!(matches!(account("", &relative_user), Account::Invalid(_)));

        let own = [("CLAUDE_CONFIG_DIR", "/me/other")];
        assert_eq!(account("/orca/b/home", &own), Account::Unmanaged);
        let restored_user_dir = [
            ("CLAUDE_CONFIG_DIR", "/me/claude/"),
            ("ORCA_CLAUDE_USER_CONFIG_DIR", "/me/claude"),
        ];
        assert_eq!(
            account("/orca/b/home", &restored_user_dir),
            Account::Dir("/orca/b/home".into())
        );

        fs::remove_file(home.join("pointer")).unwrap();
        let p = pane(
            home,
            &[
                ("ORCA_PANE_KEY", "p"),
                ("ORCA_CLAUDE_PROFILE_POINTER", "~/pointer"),
            ],
        );
        assert_eq!(p.unwrap().claude_account, Account::Default);
    }
}
