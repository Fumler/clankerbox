use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::cli::RunArgs;

/// Must not start with `herdr:`, which Herdr reserves for its own integrations.
const SOURCE: &str = "clankerbox";
const AGENT: &str = "claude";
/// JSON array: the `cb` command that recreates the session, minus `--resume <id>`.
pub const RESUME_ENV: &str = "CLANKERBOX_RESUME_ARGV";
/// Set in sessions cb reports; Herdr's own Claude hook skips those once patched.
pub const MARKER_ENV: &str = "CLANKERBOX_HERDR";
const NATIVE_HOOK: &str = "/herdr-agent-state.sh";
const NATIVE_HOOK_ID: &str = "# HERDR_INTEGRATION_ID=claude\n";
const NATIVE_HOOK_ANCHOR: &str = "[ \"${HERDR_ENV:-}\" = \"1\" ] || exit 0\n";
const NATIVE_HOOK_GUARD: &str =
    "[ -z \"${CLANKERBOX_HERDR:-}\" ] || exit 0  # added by cb: it reports this session itself\n";
/// `-y` would approve new detected grants unattended; `-C` is replaced by the absolute directory.
const NOT_RESUMED: &[&str] = &["--dry-run", "-y", "--yes", "-C"];
const MAX_RESUME_ARGS: usize = 64;
const MAX_RESUME_BYTES: usize = 8 * 1024;
/// Claude Code session ids are UUIDs.
const SESSION_ID_LEN: usize = 36;

pub struct Pane {
    pub id: String,
    pub socket: PathBuf,
}

impl Pane {
    pub fn from_env() -> Option<Self> {
        if std::env::var_os("HERDR_ENV")? != "1" {
            return None;
        }
        let id = std::env::var("HERDR_PANE_ID")
            .ok()
            .filter(|id| !id.is_empty())?;
        let socket = PathBuf::from(std::env::var_os("HERDR_SOCKET_PATH")?);
        socket.is_absolute().then_some(Self { id, socket })
    }
}

#[derive(Debug, Clone)]
pub struct Host {
    pub socket: PathBuf,
    /// Variables from cb's own environment that change what a resumed run resolves to; Herdr may
    /// restore the pane without them.
    pub resume_env: Vec<(String, String)>,
}

pub fn detect() -> Option<Host> {
    let socket = Pane::from_env()?.socket;
    socket.exists().then(|| Host {
        socket,
        resume_env: std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .filter(|(k, _)| k == "CLAUDE_CONFIG_DIR" || k.starts_with("CLANKERBOX_VAR_"))
            .collect(),
    })
}

/// Once Herdr's own Claude hook has reported a session, Herdr ignores every other source for the
/// pane. Herdr rewrites the script when it updates the integration, so this runs on every launch.
pub fn silence_native_hook(config_dirs: &[PathBuf], home: &Path) -> Vec<String> {
    let (scripts, mut problems) = native_hook_scripts(config_dirs, home);
    for script in scripts {
        if let Err(problem) = patch_native_hook(&script) {
            problems.push(format!(
                "{}: {problem}; Herdr will ignore cb's reports",
                script.display()
            ));
        }
    }
    problems
}

fn native_hook_scripts(config_dirs: &[PathBuf], home: &Path) -> (BTreeSet<PathBuf>, Vec<String>) {
    fn commands<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
        match value {
            Value::Object(map) => {
                out.extend(map.get("command").and_then(Value::as_str));
                map.values().for_each(|v| commands(v, out));
            }
            Value::Array(items) => items.iter().for_each(|v| commands(v, out)),
            _ => {}
        }
    }
    let mut scripts = BTreeSet::new();
    let mut problems = Vec::new();
    for dir in config_dirs {
        for file in ["settings.json", "settings.local.json"] {
            let Some(settings) = std::fs::read_to_string(dir.join(file))
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            else {
                continue;
            };
            let mut found = Vec::new();
            commands(&settings["hooks"], &mut found);
            for word in found.iter().flat_map(|command| shell_words(command)) {
                if !word.ends_with(NATIVE_HOOK) {
                    continue;
                }
                let bases = [
                    ("~/", home),
                    ("$HOME/", home),
                    ("${HOME}/", home),
                    ("$CLAUDE_CONFIG_DIR/", dir),
                    ("${CLAUDE_CONFIG_DIR}/", dir),
                ];
                let path = bases
                    .iter()
                    .find_map(|(prefix, base)| word.strip_prefix(prefix).map(|r| base.join(r)))
                    .unwrap_or_else(|| PathBuf::from(&word));
                if path.is_absolute() {
                    scripts.insert(path);
                } else {
                    problems.push(format!(
                        "cannot find Herdr's hook `{word}` named in {}; Herdr will ignore cb's \
                         reports",
                        dir.join(file).display()
                    ));
                }
            }
        }
    }
    (scripts, problems)
}

fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote = None;
    for c in command.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '\'' | '"') => {
                quote = Some(c);
                word.get_or_insert_default();
            }
            (None, c) if c.is_whitespace() => words.extend(word.take()),
            _ => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    words
}

/// Never through a symlink, and only a file with Herdr's header: the sandboxed agent can edit the
/// settings that name it.
fn patch_native_hook(script: &Path) -> Result<(), String> {
    use rustix::fs::{Mode, OFlags};
    use rustix::io::Errno;
    // One handle for the type check and the read: the agent can swap the path in between.
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let mut file = match rustix::fs::open(script, flags, Mode::empty()) {
        Ok(fd) => std::fs::File::from(fd),
        Err(Errno::NOENT) => return Ok(()),
        Err(Errno::LOOP) => return Err("a symlink, so cb won't edit it".into()),
        Err(e) => return Err(e.to_string()),
    };
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file, so cb won't edit it".into());
    }
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|e| e.to_string())?;
    if !text.contains(NATIVE_HOOK_ID) {
        return Err("not Herdr's Claude hook, so cb won't edit it".into());
    }
    if text.contains(NATIVE_HOOK_GUARD) {
        return Ok(());
    }
    let Some(at) = text.find(NATIVE_HOOK_ANCHOR) else {
        return Err("has changed, so cb can't make it skip this session".into());
    };
    // Renamed into place: a Claude session may run the hook meanwhile. `create_new` because the
    // sandboxed agent can write this directory and plant a symlink at the temp path.
    let tmp = script.with_extension(format!("sh.cb-{}", std::process::id()));
    let write = || {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(format!("{}{NATIVE_HOOK_GUARD}{}", &text[..at], &text[at..]).as_bytes())?;
        file.set_permissions(meta.permissions())?;
        std::fs::rename(&tmp, script)
    };
    write().map_err(|e| {
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            let _ = std::fs::remove_file(&tmp);
        }
        format!("cannot update it: {e}")
    })
}

/// Prints nothing: SessionStart and UserPromptSubmit stdout goes into Claude's context.
pub fn hook() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let Some(pane) = Pane::from_env() else {
        return;
    };
    let input: Value = serde_json::from_str(&input).unwrap_or_default();
    let resume = std::env::var(RESUME_ENV)
        .ok()
        .and_then(|v| serde_json::from_str(&v).ok());
    let seq = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    for request in requests(&input, &pane.id, resume, seq) {
        let _ = send(&pane.socket, &request);
    }
}

fn send(socket: &Path, request: &Value) -> std::io::Result<()> {
    let timeout = Some(Duration::from_millis(500));
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    stream.write_all(format!("{request}\n").as_bytes())?;
    BufReader::new(stream).read_line(&mut String::new())?;
    Ok(())
}

pub fn requests(input: &Value, pane: &str, resume: Option<Vec<String>>, seq: u64) -> Vec<Value> {
    // Subagents share the pane; their tool events would override the main agent's state.
    if input.get("agent_id").is_some_and(|id| !id.is_null()) {
        return Vec::new();
    }
    let event = input["hook_event_name"].as_str().unwrap_or_default();
    let mut session = serde_json::Map::new();
    if let Some(id) = input["session_id"].as_str().filter(|id| !id.is_empty()) {
        session.insert("agent_session_id".into(), id.into());
        if let Some(path) = input["transcript_path"].as_str().filter(|p| !p.is_empty()) {
            session.insert("agent_session_path".into(), path.into());
        }
        let argv = resume.map(|mut argv| {
            argv.extend(["--resume".into(), id.into()]);
            argv
        });
        if let Some(argv) = argv.filter(|a| valid_resume(a)) {
            session.insert("resume_argv".into(), argv.into());
        }
    }
    let request = |method: &str, seq: u64, params: Value| {
        let mut params = params;
        let fields = params.as_object_mut().expect("params are an object");
        fields.extend([
            ("pane_id".into(), pane.into()),
            ("source".into(), SOURCE.into()),
            ("agent".into(), AGENT.into()),
            ("seq".into(), seq.into()),
        ]);
        json!({ "id": format!("{SOURCE}:{seq}"), "method": method, "params": params })
    };
    let report = |state: &str, message: Option<&str>| {
        let mut params = Value::Object(session.clone());
        params["state"] = state.into();
        if let Some(message) = message.filter(|m| !m.is_empty()) {
            params["message"] = message.into();
        }
        request("pane.report_agent", seq, params)
    };
    match event {
        "SessionStart" => {
            let mut params = Value::Object(session.clone());
            if let Some(source) = input["source"].as_str() {
                params["session_start_source"] = source.into();
            }
            // Compaction can happen mid-turn, so it changes the session but not the state.
            if input["source"] == "compact" {
                vec![request("pane.report_agent_session", seq, params)]
            } else {
                // Herdr takes a resume command only from the source holding the pane.
                vec![
                    report("idle", None),
                    request("pane.report_agent_session", seq + 1, params),
                ]
            }
        }
        "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" => vec![report("working", None)],
        "PermissionRequest" => vec![report("blocked", input["tool_name"].as_str())],
        "Notification" => vec![report("blocked", input["message"].as_str())],
        "Stop" | "StopFailure" => vec![report("idle", None)],
        // A SessionStart for the next session follows these.
        "SessionEnd" if !matches!(input["reason"].as_str(), Some("clear" | "resume")) => {
            vec![request("pane.release_agent", seq, json!({}))]
        }
        _ => Vec::new(),
    }
}

/// Herdr drops the whole report if its resume command breaks these rules.
fn valid_resume(argv: &[String]) -> bool {
    let bytes: usize = argv.iter().map(|a| a.len() + 1).sum();
    argv.len() <= MAX_RESUME_ARGS
        && bytes <= MAX_RESUME_BYTES
        && argv
            .iter()
            .all(|a| !a.contains('\'') && !a.chars().any(char::is_control))
}

impl Host {
    pub fn resume_argv(&self, run: &RunArgs, cwd: &Path) -> Option<Vec<String>> {
        resume_argv(run, cwd, &self.resume_env)
    }
}

/// Leaves out command-line agent arguments: a prompt among them would be sent again on resume.
fn resume_argv(run: &RunArgs, cwd: &Path, env: &[(String, String)]) -> Option<Vec<String>> {
    let mut argv = Vec::new();
    if !env.is_empty() {
        argv.push("env".to_owned());
        argv.extend(env.iter().map(|(k, v)| format!("{k}={v}")));
    }
    argv.push("cb".to_owned());
    let resumed = run
        .options
        .iter()
        .filter(|o| !NOT_RESUMED.contains(&o.flag.as_str()));
    for word in resumed.flat_map(|o| &o.typed).chain(&run.sandbox) {
        argv.push(word.to_str()?.to_owned());
    }
    argv.extend(["-C".to_owned(), cwd.to_str()?.to_owned()]);
    argv.push("--".to_owned());
    let session = ["--resume".to_owned(), "0".repeat(SESSION_ID_LEN)];
    valid_resume(&[&argv[..], &session].concat()).then_some(argv)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    const NONE: [Value; 0] = [];

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn resume(args: &[&str]) -> Option<Vec<String>> {
        let run = crate::cli::split_run_args(os(args)).unwrap();
        resume_argv(&run, Path::new("/abs/dir"), &[])
    }

    fn reqs(input: &Value) -> Vec<Value> {
        requests(input, "w1:p1", Some(vec!["cb".into(), "--".into()]), 7)
    }

    fn methods(requests: &[Value]) -> Vec<&str> {
        requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn session_start_holds_pane_then_reports_session_with_resume() {
        let out = reqs(&json!({
            "hook_event_name": "SessionStart",
            "session_id": "abc",
            "transcript_path": "/t/abc.jsonl",
            "source": "startup",
        }));
        assert_eq!(
            out,
            [
                json!({
                    "id": "clankerbox:7",
                    "method": "pane.report_agent",
                    "params": {
                        "pane_id": "w1:p1",
                        "source": "clankerbox",
                        "agent": "claude",
                        "seq": 7,
                        "state": "idle",
                        "agent_session_id": "abc",
                        "agent_session_path": "/t/abc.jsonl",
                        "resume_argv": ["cb", "--", "--resume", "abc"],
                    },
                }),
                json!({
                    "id": "clankerbox:8",
                    "method": "pane.report_agent_session",
                    "params": {
                        "pane_id": "w1:p1",
                        "source": "clankerbox",
                        "agent": "claude",
                        "seq": 8,
                        "agent_session_id": "abc",
                        "agent_session_path": "/t/abc.jsonl",
                        "session_start_source": "startup",
                        "resume_argv": ["cb", "--", "--resume", "abc"],
                    },
                }),
            ]
        );

        let compact = reqs(&json!({
            "hook_event_name": "SessionStart",
            "session_id": "abc",
            "source": "compact",
        }));
        assert_eq!(methods(&compact), ["pane.report_agent_session"]);
        assert_eq!(compact[0]["params"]["seq"], 7);
    }

    #[test]
    fn events_map_to_states() {
        for (event, state) in [
            ("UserPromptSubmit", "working"),
            ("PostToolUse", "working"),
            ("PostToolUseFailure", "working"),
            ("PermissionRequest", "blocked"),
            ("Notification", "blocked"),
            ("Stop", "idle"),
            ("StopFailure", "idle"),
        ] {
            let out = reqs(&json!({
                "hook_event_name": event,
                "session_id": "abc",
                "tool_name": "Bash",
            }));
            assert_eq!(methods(&out), ["pane.report_agent"], "{event}");
            assert_eq!(out[0]["params"]["state"], state, "{event}");
            assert_eq!(
                out[0]["params"]["resume_argv"],
                json!(["cb", "--", "--resume", "abc"])
            );
        }
        let blocked = reqs(&json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash" }));
        assert_eq!(blocked[0]["params"]["message"], "Bash");
        assert!(blocked[0]["params"].get("resume_argv").is_none());
        assert_eq!(reqs(&json!({ "hook_event_name": "PreCompact" })), NONE);
        assert_eq!(reqs(&json!(null)), NONE);
    }

    #[test]
    fn subagent_events_are_skipped() {
        let out = reqs(&json!({
            "hook_event_name": "PostToolUse",
            "session_id": "abc",
            "agent_id": "a1",
        }));
        assert_eq!(out, NONE);
    }

    fn native_hook(home: &Path, body: &str) -> (PathBuf, Vec<PathBuf>) {
        let script = home.join(".claude/hooks/herdr-agent-state.sh");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, body).unwrap();
        let settings = json!({ "hooks": { "SessionStart": [{ "hooks": [
            { "type": "command", "command": "bash '~/.claude/hooks/herdr-agent-state.sh' session" },
            { "type": "command", "command": "other-hook" },
        ] }] } });
        std::fs::create_dir_all(home.join("cfg")).unwrap();
        std::fs::write(home.join("cfg/settings.json"), settings.to_string()).unwrap();
        (script, vec![home.join("cfg"), home.join("missing")])
    }

    const HERDR_SCRIPT: &str = "#!/bin/sh\n# HERDR_INTEGRATION_ID=claude\nset -eu\n\
                                [ \"${HERDR_ENV:-}\" = \"1\" ] || exit 0\necho reported\n";

    #[test]
    fn native_hook_from_settings_gets_one_guard_before_its_herdr_check() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let original = HERDR_SCRIPT;
        let (script, dirs) = native_hook(home, original);
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750)).unwrap();

        for _ in 0..2 {
            assert_eq!(silence_native_hook(&dirs, home), Vec::<String>::new());
        }
        let patched = std::fs::read_to_string(&script).unwrap();
        assert_eq!(patched.matches(NATIVE_HOOK_GUARD).count(), 1);
        assert_eq!(patched.replace(NATIVE_HOOK_GUARD, ""), original);
        assert!(patched.find(NATIVE_HOOK_GUARD) < patched.find(NATIVE_HOOK_ANCHOR));
        let mode = std::fs::metadata(&script).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o750);

        native_hook(home, "#!/bin/sh\n# HERDR_INTEGRATION_ID=claude\nrun\n");
        let problems = silence_native_hook(&dirs, home);
        assert!(
            problems.len() == 1 && problems[0].contains("has changed"),
            "{problems:?}"
        );

        std::fs::write(&script, original).unwrap();
        assert_eq!(
            silence_native_hook(&[home.join("missing")], home),
            Vec::<String>::new()
        );
        assert_eq!(std::fs::read_to_string(&script).unwrap(), original);
    }

    #[test]
    fn patched_native_hook_skips_only_marked_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let (script, dirs) = native_hook(tmp.path(), HERDR_SCRIPT);
        silence_native_hook(&dirs, tmp.path());
        let run = |marked: bool| {
            let mut cmd = std::process::Command::new("sh");
            cmd.arg(&script).env_clear().env("HERDR_ENV", "1");
            if marked {
                cmd.env(MARKER_ENV, "1");
            }
            String::from_utf8(cmd.output().unwrap().stdout).unwrap()
        };
        assert_eq!(run(false), "reported\n");
        assert_eq!(run(true), "");
    }

    #[test]
    fn only_herdrs_own_hook_file_is_edited() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let anchor_only = "[ \"${HERDR_ENV:-}\" = \"1\" ] || exit 0\n";
        let (script, dirs) = native_hook(home, anchor_only);
        let problems = silence_native_hook(&dirs, home);
        assert!(
            problems[0].contains("not Herdr's Claude hook"),
            "{problems:?}"
        );
        assert_eq!(std::fs::read_to_string(&script).unwrap(), anchor_only);

        let target = home.join("victim");
        std::fs::write(&target, HERDR_SCRIPT).unwrap();
        std::fs::remove_file(&script).unwrap();
        std::os::unix::fs::symlink(&target, &script).unwrap();
        let problems = silence_native_hook(&dirs, home);
        assert!(problems[0].contains("a symlink"), "{problems:?}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), HERDR_SCRIPT);
    }

    #[test]
    fn shell_words_keep_quoted_spaces() {
        assert_eq!(
            shell_words("bash '/Users/John Doe/h.sh' \"a b\"c  session "),
            ["bash", "/Users/John Doe/h.sh", "a bc", "session"]
        );
        assert_eq!(shell_words("x ''"), ["x", ""]);
    }

    #[test]
    fn planted_temp_symlink_is_not_written_through() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let (script, dirs) = native_hook(home, HERDR_SCRIPT);
        let victim = home.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        let planted = script.with_extension(format!("sh.cb-{}", std::process::id()));
        std::os::unix::fs::symlink(&victim, &planted).unwrap();

        let problems = silence_native_hook(&dirs, home);
        assert!(problems[0].contains("cannot update it"), "{problems:?}");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(std::fs::read_to_string(&script).unwrap(), HERDR_SCRIPT);
    }

    #[test]
    fn hook_paths_resolve_home_and_config_dir_or_are_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let (home, dir) = (tmp.path(), tmp.path().join("cfg"));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = json!({ "hooks": { "SessionStart": [{ "hooks": [
            { "command": "bash \"$HOME\"/a/herdr-agent-state.sh" },
            { "command": "bash \"${CLAUDE_CONFIG_DIR}/hooks/herdr-agent-state.sh\" session" },
            { "command": "bash $HERDR_DIR/herdr-agent-state.sh" },
        ] }] } });
        std::fs::write(dir.join("settings.local.json"), settings.to_string()).unwrap();
        let (scripts, problems) = native_hook_scripts(std::slice::from_ref(&dir), home);
        assert_eq!(
            scripts.into_iter().collect::<Vec<_>>(),
            [
                home.join("a/herdr-agent-state.sh"),
                dir.join("hooks/herdr-agent-state.sh")
            ]
        );
        assert!(
            problems.len() == 1 && problems[0].contains("$HERDR_DIR"),
            "{problems:?}"
        );
    }

    #[test]
    fn session_end_releases_only_when_quitting() {
        let end =
            |reason: &str| reqs(&json!({ "hook_event_name": "SessionEnd", "reason": reason }));
        for reason in ["prompt_input_exit", "logout", "other"] {
            let out = end(reason);
            assert_eq!(methods(&out), ["pane.release_agent"]);
            assert_eq!(
                out[0]["params"],
                json!({ "pane_id": "w1:p1", "source": "clankerbox", "agent": "claude", "seq": 7 })
            );
        }
        assert_eq!(end("clear"), NONE);
        assert_eq!(end("resume"), NONE);
    }

    #[test]
    fn resume_argv_keeps_cb_options_only() {
        let argv = resume(&[
            "-A",
            "claude-opus",
            "-p",
            "personal",
            "--profile=work",
            "--nono-profile",
            "cb:claude",
            "-C",
            "rel",
            "--no-project",
            "--dry-run",
            "-y",
            "--detect-subdirs",
            "--allow",
            "./x",
            "--nono-dry-run",
            "--nono-agent=p",
        ])
        .unwrap();
        assert_eq!(
            argv.join(" "),
            "cb -A claude-opus -p personal --profile=work --nono-profile cb:claude --no-project \
             --detect-subdirs --allow ./x --nono-dry-run --nono-agent=p -C /abs/dir --"
        );

        assert_eq!(
            resume(&[
                "-Crel",
                "--yes",
                "--",
                "fix the tests",
                "--model",
                "x",
                "-c"
            ])
            .unwrap()
            .join(" "),
            "cb -C /abs/dir --"
        );
    }

    #[test]
    fn resume_argv_sets_environment_through_env() {
        let run = crate::cli::split_run_args(os(&["-p", "w"])).unwrap();
        let env = [(
            "CLAUDE_CONFIG_DIR".to_owned(),
            "/home/me/.claude-w".to_owned(),
        )];
        assert_eq!(
            resume_argv(&run, Path::new("/d"), &env).unwrap().join(" "),
            "env CLAUDE_CONFIG_DIR=/home/me/.claude-w cb -p w -C /d --"
        );
    }

    #[test]
    fn resume_argv_follows_herdr_rules_with_room_for_session() {
        assert!(resume(&["--read=don't"]).is_none());
        assert!(resume(&["--read=a\nb"]).is_none());
        assert!(resume(&["--x"; 58]).is_some());
        assert!(resume(&["--x"; 59]).is_none());
        // Fits on its own, but not with `--resume <id>`.
        let long = format!("--x={}", "x".repeat(MAX_RESUME_BYTES - 50));
        assert!(resume(&[&long]).is_none());

        let near_limit = vec!["x".to_owned(); MAX_RESUME_ARGS - 1];
        let out = requests(
            &json!({ "hook_event_name": "Stop", "session_id": "abc" }),
            "p",
            Some(near_limit),
            1,
        );
        assert_eq!(out[0]["params"]["state"], "idle");
        assert!(out[0]["params"].get("resume_argv").is_none());
    }
}
