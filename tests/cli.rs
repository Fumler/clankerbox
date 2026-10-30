use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// Prints args, `$CB_TEST_ENV` and cwd, one per line. `profile show` fails for profiles in
/// `CB_STUB_MISSING`.
const STUB: &str = r#"#!/bin/sh
if [ "$2 $3 $4" = "--dry-run -v --log-file" ]; then
  printf '  Capabilities:\n  ──\n   r+w  /stub (dir) [profile]\n  ──\n  $ claude\n' >&2
  exit 0
fi
if [ "$1 $2" = "profile show" ]; then
  case " $CB_STUB_MISSING " in *" $3 "*) exit 1 ;; esac
  exit 0
fi
for a in "$@"; do printf '%s\n' "$a"; done
printf 'env=%s\n' "$CB_TEST_ENV"
printf 'cwd=%s\n' "$PWD"
"#;

struct Env {
    tmp: TempDir,
}

impl Env {
    fn new() -> Self {
        let env = Self {
            tmp: tempfile::tempdir().unwrap(),
        };
        for stub in ["bin/nono", "bin/claude"] {
            let path = env.path(stub);
            env.write(stub, STUB);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::create_dir_all(env.path("home/repo/.git")).unwrap();
        fs::create_dir_all(env.path("home/frontend")).unwrap();
        env
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.tmp.path().canonicalize().unwrap().join(rel)
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("cb").unwrap();
        cmd.args(args)
            .current_dir(self.path("home/repo"))
            .env_clear()
            .env("PATH", self.path("bin"))
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_RUNTIME_DIR", self.path("run"));
        cmd
    }

    fn stdout(&self, args: &[&str]) -> String {
        stdout_of(&mut self.cmd(args))
    }
}

fn stdout_of(cmd: &mut Command) -> String {
    let out = cmd.assert().success().get_output().stdout.clone();
    String::from_utf8(out).unwrap()
}

/// What the stub `nono` printed: its own arguments, the agent command with its arguments, and
/// the `env=`/`cwd=` lines.
struct Run {
    nono: Vec<String>,
    agent: Vec<String>,
    tail: Vec<String>,
}

fn run(out: &str) -> Run {
    let lines: Vec<String> = out.lines().map(str::to_owned).collect();
    let sep = lines.iter().position(|l| l == "--").expect("no `--`");
    let tail = lines
        .iter()
        .position(|l| l.starts_with("env="))
        .expect("no env line");
    Run {
        nono: lines[..sep].to_vec(),
        agent: lines[sep + 1..tail].to_vec(),
        tail: lines[tail..].to_vec(),
    }
}

impl Run {
    fn has(&self, flag: &str, value: &str) -> bool {
        self.nono.windows(2).any(|w| w[0] == flag && w[1] == value)
    }

    fn value(&self, flag: &str) -> &str {
        let at = self.nono.iter().position(|a| a == flag).expect(flag);
        &self.nono[at + 1]
    }

    /// nono's flags after the subcommand, each with its value if it takes one.
    fn flags(&self) -> BTreeSet<(String, Option<String>)> {
        let mut out = BTreeSet::new();
        let mut args = self.nono[1..].iter().peekable();
        while let Some(flag) = args.next() {
            let value = args.next_if(|v| !v.starts_with("--")).cloned();
            out.insert((flag.clone(), value));
        }
        out
    }

    /// The `--profile` value, or what cb's generated profile extends.
    fn base_profile(&self) -> String {
        let profile = self.value("--profile");
        if !profile.contains("/clankerbox/profiles/") {
            return profile.to_owned();
        }
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(profile).unwrap()).unwrap();
        json["extends"][0].as_str().unwrap_or_default().to_owned()
    }
}

const PROJECT: &str = r#"
[vars]
projects = ".."
[sandbox]
read = ["${projects}/frontend"]
env = { CB_TEST_ENV = "from-project" }
[agents.claude]
args = ["--model", "opus"]
"#;

#[test]
fn runs_nono_with_project_grants_after_trust() {
    let env = Env::new();
    env.write("home/repo/.clankerbox.toml", PROJECT);

    env.cmd(&[])
        .assert()
        .failure()
        .stderr(predicate::str::contains("untrusted project config"));
    env.cmd(&["trust"]).assert().success();

    let out = env.stdout(&["--block-net", "--", "--resume"]);
    let repo = env.path("home/repo");
    let frontend = env.path("home/frontend");
    let r = run(&out);
    assert_eq!(r.nono[0], "run");
    assert!(r.has("--allow", repo.to_str().unwrap()));
    assert!(r.has("--read", frontend.to_str().unwrap()));
    assert!(r.nono.contains(&"--block-net".to_owned()));
    assert_eq!(r.agent[0], "claude");
    assert_eq!(r.agent[1..3], ["--model", "opus"]);
    assert_eq!(r.agent[3], "--plugin-dir");
    assert_eq!(r.agent.last().unwrap(), "--resume");
    assert_eq!(
        r.tail,
        [
            "env=from-project".to_owned(),
            format!("cwd={}", repo.display())
        ]
    );

    env.write(
        "home/repo/.clankerbox.toml",
        &format!("{PROJECT}\n# edited"),
    );
    env.cmd(&[]).assert().failure();
}

#[test]
fn var_overrides_from_environment() {
    let env = Env::new();
    fs::create_dir_all(env.path("elsewhere/frontend")).unwrap();
    env.write("home/repo/.clankerbox.toml", PROJECT);
    env.cmd(&["trust"]).assert().success();
    let elsewhere = env.path("elsewhere");
    let out = env
        .cmd(&["show", "--json"])
        .env("CLANKERBOX_VAR_projects", &elsewhere)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let paths: Vec<_> = report["policy"]["grants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| PathBuf::from(g["path"].as_str().unwrap()))
        .collect();
    assert!(paths.contains(&elsewhere.join("frontend")), "{paths:?}");
}

#[test]
fn agent_flag_and_global_config() {
    let env = Env::new();
    env.write(
        "config/clankerbox/config.toml",
        "approve_detected = true\n[agents.mine]\ninherits = \"claude\"\nargs = [\"--mine\"]\nsandbox.nono.profile = \"p\"",
    );
    let r = run(&env.stdout(&["-A", "mine"]));
    assert_eq!(r.base_profile(), "p");
    assert_eq!(r.agent[0], "claude");
    assert_eq!(r.agent[1..3], ["--mine", "--plugin-dir"]);
}

#[test]
fn unsandboxed_runs_agent_directly() {
    let env = Env::new();
    env.cmd(&["-y", "--unsandboxed", "--", "hi"])
        .assert()
        .success()
        .stdout(format!(
            "hi\nenv=\ncwd={}\n",
            env.path("home/repo").display()
        ))
        .stderr(predicate::str::contains("WITHOUT a sandbox"));
    env.cmd(&["-y", "--unsandboxed", "--read", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("sandbox is disabled"));
}

#[test]
fn init_local_excludes_from_git_and_trusts() {
    let env = Env::new();
    env.cmd(&["init"]).assert().success();
    env.cmd(&["init", "--local"]).assert().success();
    env.cmd(&["init"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
    let exclude = fs::read_to_string(env.path("home/repo/.git/info/exclude")).unwrap();
    assert_eq!(exclude, ".clankerbox.local.toml\n");
    env.cmd(&[]).assert().success();
}

#[test]
fn init_outside_repo_root_needs_force() {
    let env = Env::new();
    fs::create_dir_all(env.path("home/repo/pkg")).unwrap();
    env.cmd(&["-C", "pkg", "init"])
        .current_dir(env.path("home/repo"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));
    let pkg = env.path("home/repo/pkg");
    env.cmd(&["init", "--force"])
        .current_dir(&pkg)
        .assert()
        .success();
    assert!(Path::new(&pkg.join(".clankerbox.toml")).exists());
}

#[test]
fn config_typos_are_reported_with_suggestion() {
    let env = Env::new();
    env.write("home/repo/.clankerbox.toml", "[sandbox]\nraed = []\n");
    env.cmd(&["show"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("did you mean `read`?"));
}

#[test]
fn unknown_subcommand_and_schema() {
    let env = Env::new();
    env.cmd(&["clude"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown subcommand `clude`"));
    let schema: serde_json::Value = serde_json::from_str(&env.stdout(&["schema"])).unwrap();
    assert!(schema["properties"]["sandbox"].is_object());
}

/// Every flag a default run passes to nono. A new default grant must be added here on purpose.
#[test]
fn default_run_passes_only_expected_flags() {
    for direct in [false, true] {
        let env = Env::new();
        if direct {
            env.write(
                "config/clankerbox/config.toml",
                "[sandbox.nono]\nmode = \"direct\"",
            );
        }
        let r = run(&env.stdout(&["-y"]));
        let profile = r.value("--profile").to_owned();
        assert!(profile.starts_with(env.path("run/clankerbox/profiles").to_str().unwrap()));
        assert_eq!(r.agent[..2], ["claude", "--plugin-dir"]);
        let plugin = r.agent[2].clone();
        assert!(plugin.starts_with(env.path("run/clankerbox/claude-plugin").to_str().unwrap()));
        assert_eq!(r.agent.len(), 3);

        let cb = assert_cmd::cargo::cargo_bin("cb").canonicalize().unwrap();
        let pair = |flag: &str, value: &str| (flag.to_owned(), Some(value.to_owned()));
        let mut expected = BTreeSet::from([
            pair("--profile", &profile),
            ("--allow-cwd".to_owned(), None),
            pair("--allow", env.path("home/repo").to_str().unwrap()),
            pair("--read", &plugin),
            pair("--read-file", cb.to_str().unwrap()),
        ]);
        if direct && cfg!(target_os = "linux") {
            expected.insert(pair("--read", "/proc"));
        }
        assert_eq!(r.nono[0], if direct { "wrap" } else { "run" });
        assert_eq!(r.flags(), expected, "direct: {direct}");
    }
}

#[test]
fn direct_mode_uses_wrap_and_reads_proc_on_linux() {
    let env = Env::new();
    env.write(
        "config/clankerbox/config.toml",
        "approve_detected = true\n[sandbox.nono]\nmode = \"direct\"",
    );
    let r = run(&env.stdout(&[]));
    assert_eq!(r.nono[0], "wrap");
    assert_eq!(r.has("--read", "/proc"), cfg!(target_os = "linux"));
}

#[test]
fn one_off_grants_and_agent_args_pass_through() {
    let env = Env::new();
    let r = run(&env.stdout(&[
        "-y",
        "--allow",
        "./some-dir",
        "--read",
        "../other",
        "--",
        "--resume",
        "id",
    ]));
    assert!(r.has("--allow", "./some-dir"));
    assert!(r.has("--read", "../other"));
    assert!(r.agent.ends_with(&["--resume".into(), "id".into()]));
}

#[test]
fn cb_profiles_override_agent_settings() {
    let env = Env::new();
    env.write(
        "config/clankerbox/config.toml",
        r#"
approve_detected = true
[agents.pi]
sandbox.nono.profile = "pi-base"
[profiles.work]
sandbox.env = { CB_TEST_ENV = "work" }
agents.pi.sandbox.nono.profile = "pi-work"
"#,
    );
    let r = run(&env.stdout(&["-A", "pi"]));
    assert_eq!(r.base_profile(), "pi-base");
    assert_eq!(r.agent, ["pi"]);

    let r = run(&env.stdout(&["-A", "pi", "-p", "work"]));
    assert_eq!(r.base_profile(), "pi-work");
    assert_eq!(r.tail[0], "env=work");

    let r = run(&env.stdout(&["-A", "pi", "-p", "work", "--nono-profile", "other"]));
    assert_eq!(r.base_profile(), "other");

    env.cmd(&["-p", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unknown profile `nope` (known: work)",
        ));
}

#[test]
fn dir_option_sets_working_directory() {
    let env = Env::new();
    let repo = env.path("home/repo");
    let out = env
        .cmd(&["-y", "-C", repo.to_str().unwrap()])
        .current_dir(env.path("home"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let out = String::from_utf8(out).unwrap();
    assert!(out.ends_with(&format!("cwd={}\n", repo.display())), "{out}");
}

#[test]
fn non_utf8_environment_is_tolerated() {
    use std::os::unix::ffi::OsStrExt;
    let env = Env::new();
    env.cmd(&["show"])
        .env("JUNK", std::ffi::OsStr::from_bytes(b"\xff"))
        .assert()
        .success();
}

#[test]
fn symlinked_home_is_still_never_granted() {
    let env = Env::new();
    let link = env.path("home-link");
    std::os::unix::fs::symlink(env.path("home"), &link).unwrap();
    let out = env
        .cmd(&["show", "--json"])
        .current_dir(env.path("home"))
        .env("HOME", &link)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(report["policy"]["grants"], serde_json::json!([]));
}

#[test]
fn init_lists_cache_write_grants() {
    let env = Env::new();
    env.write("home/repo/Cargo.toml", "");
    env.cmd(&["init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("detected rust"))
        .stderr(predicate::str::contains(
            "rust  rw  ~/.cargo/registry  (missing, not granted)",
        ));
}

#[test]
fn missing_nono_is_a_clear_error() {
    let env = Env::new();
    fs::remove_file(env.path("bin/nono")).unwrap();
    env.cmd(&[])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "nono not found on $PATH; install it from https://nono.sh",
        ));
    env.cmd(&["show"]).assert().success();
}

#[test]
fn claude_uses_builtin_profile_without_pull() {
    let env = Env::new();
    let r = run(&env.stdout(&["-y"]));
    let profile = r.value("--profile");
    let plugin = &r.agent[r.agent.iter().position(|a| a == "--plugin-dir").unwrap() + 1];
    assert!(Path::new(plugin).join("hooks/hooks.json").is_file());
    assert!(r.has("--read", plugin));
    assert!(env.stdout(&["show"]).contains("explains sandbox denials"));
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(profile).unwrap()).unwrap();
    assert_eq!(json["extends"], "default");
    assert!(
        json["filesystem"]["allow"]
            .as_array()
            .unwrap()
            .contains(&"$HOME/.claude".into())
    );
    for dir in [
        "home/.claude",
        "home/.cache/claude",
        "home/.local/state/claude/locks",
        "run/cc-socks",
    ] {
        assert!(env.path(dir).is_dir(), "{dir}");
    }
    let mode = fs::metadata(env.path("run/cc-socks"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);

    env.cmd(&["-y", "--nono-profile", "cb:nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unknown built-in nono profile `cb:nope` (known: cb:claude)",
        ));
}

#[test]
fn missing_nono_profile_suggests_pull() {
    let env = Env::new();
    env.cmd(&["-y", "--nono-profile", "claude-code"])
        .env("CB_STUB_MISSING", "claude-code")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "nono profile `claude-code` is not installed; run `nono pull nolabs-ai/claude`",
        ));
    env.cmd(&["-y", "--nono-profile", "custom"])
        .env("CB_STUB_MISSING", "custom")
        .assert()
        .failure()
        .stderr(predicate::str::contains("nono profile `custom` not found"));
}

#[test]
fn dry_run_prints_command_without_running() {
    let env = Env::new();
    env.write(
        "home/repo/.clankerbox.toml",
        "[sandbox]\nenv = { CB_TEST_ENV = \"a b\" }",
    );
    let out = env.stdout(&["--dry-run", "--read", "../x y", "--", "--resume", "id"]);
    assert!(
        out.starts_with("CB_TEST_ENV='a b' nono run --profile "),
        "{out}"
    );
    assert!(out.contains(" --read '../x y' -- claude "), "{out}");
    assert!(out.ends_with(" --resume id\n"), "{out}");

    let other = env.path("home/frontend");
    let out = env.stdout(&["--dry-run", "-C", other.to_str().unwrap()]);
    assert!(
        out.starts_with(&format!("cd {} && ", other.display())),
        "{out}"
    );
}

#[test]
fn nono_dry_run_passes_through() {
    let env = Env::new();
    let out = env.stdout(&["--dry-run", "--nono-dry-run"]);
    assert!(out.contains(" --dry-run -- claude"), "{out}");
    let out = env.stdout(&["-y", "--nono-dry-run"]);
    assert!(out.contains("--dry-run\n--\nclaude\n"), "{out}");
}

#[test]
fn detected_grants_need_approval_without_config() {
    let env = Env::new();
    fs::create_dir_all(env.path("home/.cargo/registry")).unwrap();
    env.write("home/repo/crates/a/Cargo.toml", "");
    env.cmd(&[]).assert().failure().stderr(
        predicate::str::contains("no .clankerbox.toml").and(predicate::str::contains("-y")),
    );
    let show = env.stdout(&["show"]);
    assert!(show.contains("untrusted: <detected config for"), "{show}");
    assert!(show.contains("← detected toolchains"), "{show}");
    assert!(show.contains("sandbox rules"), "{show}");
    assert!(show.contains("  r+w  /stub (dir) [profile]"), "{show}");

    env.stdout(&["-y"]);
    env.stdout(&["--no-project"]);
    env.cmd(&["trust"]).assert().success();
    let out = env.stdout(&[]);
    let registry = env.path("home/.cargo/registry");
    assert!(
        out.contains(&format!("--allow\n{}\n", registry.display())),
        "{out}"
    );

    env.write("home/repo/flake.nix", "");
    env.cmd(&[]).assert().failure();
    env.cmd(&["doctor"]).assert().success();
    env.write("config/clankerbox/config.toml", "approve_detected = true");
    env.stdout(&[]);
    let show = env.stdout(&["show"]);
    assert!(!show.contains("untrusted"), "{show}");
}

#[test]
fn approve_detected_in_project_file_is_ignored_with_warning() {
    let env = Env::new();
    env.write("home/repo/.clankerbox.toml", "approve_detected = true");
    env.cmd(&["trust"]).assert().success();
    let show = env.stdout(&["show"]);
    assert!(show.contains("ignoring `approve_detected`"), "{show}");
}

#[test]
fn init_in_home_ignores_projects_below_it() {
    let env = Env::new();
    env.write("home/repo/Cargo.toml", "");
    env.cmd(&["init"])
        .current_dir(env.path("home"))
        .assert()
        .success();
    let config = fs::read_to_string(env.path("home/.clankerbox.toml")).unwrap();
    assert!(!config.contains("rust_runtime"), "{config}");
}

#[test]
fn subdirs_are_searched_only_in_repositories() {
    let env = Env::new();
    fs::create_dir_all(env.path("home/.m2/repository")).unwrap();
    for project in ["plain", "jj"] {
        env.write(&format!("home/{project}/api/pom.xml"), "");
    }
    fs::create_dir_all(env.path("home/jj/.jj")).unwrap();
    let show = |dir: &str, args: &[&str]| {
        stdout_of(
            env.cmd(&[&["show"], args].concat())
                .current_dir(env.path(dir)),
        )
    };
    let m2 = "repository  ← detected toolchains";
    assert!(!show("home/plain", &[]).contains(m2));
    assert!(show("home/plain", &["--detect-subdirs"]).contains(m2));
    assert!(show("home/jj", &[]).contains(m2));
}

#[test]
fn auto_approved_cache_grants_are_warned_about() {
    let env = Env::new();
    fs::create_dir_all(env.path("home/.cargo/registry")).unwrap();
    env.write("home/repo/Cargo.toml", "");
    env.cmd(&["-y"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "write access to shared caches: ~/.cargo/registry",
        ));
}

#[test]
fn hook_explains_denials_and_ignores_bad_input() {
    let env = Env::new();
    let denied = r#"{"hook_event_name":"PostToolUseFailure","error":"Permission denied"}"#;
    let out = env
        .cmd(&["hook", "explain-denial"])
        .write_stdin(denied)
        .assert()
        .success();
    let out: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(
        out["hookSpecificOutput"]["hookEventName"],
        "PostToolUseFailure"
    );
    for input in [r#"{"error":"no such file"}"#, "not json"] {
        env.cmd(&["hook", "explain-denial"])
            .write_stdin(input)
            .assert()
            .success()
            .stdout("");
    }
}

#[test]
fn doctor_checks_profile_paths() {
    let env = Env::new();
    env.cmd(&["doctor", "--nono-profile", "./typo.json"])
        .env("CB_STUB_MISSING", "./typo.json")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "nono profile `./typo.json` not found",
        ));
}

fn herdr_server(socket: &Path) -> std::thread::JoinHandle<Vec<serde_json::Value>> {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let mut requests = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let Ok((stream, _)) = listener.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            reader.read_line(&mut line).unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            let reply = serde_json::json!({ "id": request["id"], "result": { "type": "ok" } });
            (&stream)
                .write_all(format!("{reply}\n").as_bytes())
                .unwrap();
            let release = request["method"] == "pane.release_agent";
            requests.push(request);
            if release {
                break;
            }
        }
        requests
    })
}

#[test]
fn herdr_hook_reports_to_socket_silently() {
    let env = Env::new();
    let socket = env.path("herdr.sock");
    let server = herdr_server(&socket);
    let hook = |input: &str, herdr: bool| {
        let mut cmd = env.cmd(&["hook", "herdr"]);
        if herdr {
            cmd.env("HERDR_ENV", "1")
                .env("HERDR_PANE_ID", "w1:p1")
                .env("HERDR_SOCKET_PATH", &socket)
                .env("CLANKERBOX_RESUME_ARGV", r#"["cb","-p","personal","--"]"#);
        }
        cmd.write_stdin(input).assert().success().stdout("");
    };
    hook(
        r#"{"hook_event_name":"SessionStart","session_id":"abc","source":"startup"}"#,
        false,
    );
    hook("not json", true);
    hook(
        r#"{"hook_event_name":"SessionStart","session_id":"abc","source":"startup"}"#,
        true,
    );
    hook(
        r#"{"hook_event_name":"SessionEnd","session_id":"abc","reason":"prompt_input_exit"}"#,
        true,
    );
    let requests = server.join().unwrap();
    let methods: Vec<_> = requests.iter().map(|r| r["method"].clone()).collect();
    assert_eq!(
        methods,
        [
            "pane.report_agent",
            "pane.report_agent_session",
            "pane.release_agent"
        ]
    );
    let report = &requests[0]["params"];
    assert_eq!(report["pane_id"], "w1:p1");
    assert_eq!(report["source"], "clankerbox");
    assert_eq!(report["state"], "idle");
    assert_eq!(
        report["resume_argv"],
        serde_json::json!(["cb", "-p", "personal", "--", "--resume", "abc"])
    );
    assert!(requests[1]["params"]["seq"].as_u64() > report["seq"].as_u64());
    assert!(requests[2]["params"]["seq"].as_u64() > requests[1]["params"]["seq"].as_u64());

    fs::remove_file(&socket).unwrap();
    hook(r#"{"hook_event_name":"Stop","session_id":"abc"}"#, true);
}

#[test]
fn herdr_pane_grants_socket_and_passes_resume_command() {
    let env = Env::new();
    let socket = env.path("herdr.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let herdr = |args: &[&str]| {
        stdout_of(
            env.cmd(args)
                .env("HERDR_ENV", "1")
                .env("HERDR_PANE_ID", "w1:p1")
                .env("HERDR_SOCKET_PATH", &socket),
        )
    };

    let native = "# HERDR_INTEGRATION_ID=claude\nset -eu\n\
                  [ \"${HERDR_ENV:-}\" = \"1\" ] || exit 0\nreport\n";
    env.write("home/.claude/hooks/herdr-agent-state.sh", native);
    env.write(
        "home/.claude/settings.json",
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"bash '~/.claude/hooks/herdr-agent-state.sh' session"}]}]}}"#,
    );
    let dry = herdr(&["--dry-run", "--", "--model", "opus", "-c"]);
    let script = env.path("home/.claude/hooks/herdr-agent-state.sh");
    assert_eq!(fs::read_to_string(&script).unwrap(), native);
    env.cmd(&[])
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "w1:p1")
        .env("HERDR_SOCKET_PATH", &socket)
        .assert()
        .failure()
        .stderr(predicate::str::contains("no .clankerbox.toml"));
    assert_eq!(fs::read_to_string(&script).unwrap(), native);

    let r = run(&herdr(&["-y", "--", "--model", "opus", "-c"]));
    assert!(r.has("--allow-unix-socket", socket.to_str().unwrap()));
    let plugin = &r.agent[r.agent.iter().position(|a| a == "--plugin-dir").unwrap() + 1];
    let hooks = fs::read_to_string(Path::new(plugin).join("hooks/hooks.json")).unwrap();
    assert!(hooks.contains("hook herdr"), "{hooks}");
    let patched = fs::read_to_string(&script).unwrap();
    assert!(patched.contains("CLANKERBOX_HERDR"), "{patched}");

    let repo = env.path("home/repo");
    let resume = format!(r#"["cb","-C","{}","--"]"#, repo.display());
    assert!(
        dry.starts_with(&format!(
            "CLANKERBOX_HERDR=1 CLANKERBOX_RESUME_ARGV='{resume}' nono run"
        )),
        "{dry}"
    );
    let show = herdr(&["show"]);
    assert!(show.contains("every Herdr pane"), "{show}");
    let json: serde_json::Value =
        serde_json::from_str(&herdr(&["show", "--json", "--", "--model", "opus"])).unwrap();
    assert_eq!(json["env"]["CLANKERBOX_RESUME_ARGV"], resume);

    let plain = run(&env.stdout(&["-y"]));
    assert!(!plain.nono.iter().any(|a| a.contains("unix-socket")));
    assert!(!env.stdout(&["show"]).contains("Herdr"));
    assert!(
        !env.stdout(&["--dry-run"])
            .contains("CLANKERBOX_RESUME_ARGV")
    );

    env.write(
        "config/clankerbox/config.toml",
        "[sandbox.nono]\nherdr = false",
    );
    let off = herdr(&["--dry-run"]);
    assert!(
        !off.contains("unix-socket") && !off.contains("RESUME"),
        "{off}"
    );
}

#[test]
fn orca_pane_grants_hooks_read_only_and_follows_account_switch() {
    let env = Env::new();
    let scripts = env.path("home/.orca/agent-hooks");
    let endpoint = env.path("orca-data/agent-hooks");
    let account = env.path("orca-data/claude-profiles/b/home");
    fs::create_dir_all(&scripts).unwrap();
    fs::create_dir_all(&endpoint).unwrap();
    fs::create_dir_all(&account).unwrap();
    env.write("orca-data/pointer", &format!("{}\n", account.display()));
    let orca = |args: &[&str]| {
        stdout_of(
            env.cmd(args)
                .env("ORCA_PANE_KEY", "tab:1")
                .env("ORCA_AGENT_HOOK_ENDPOINT", endpoint.join("endpoint.env"))
                .env("ORCA_AGENT_HOOK_PORT", "4123")
                .env("ORCA_CLAUDE_PROFILE_POINTER", env.path("orca-data/pointer"))
                .env(
                    "CLAUDE_CONFIG_DIR",
                    env.path("orca-data/claude-profiles/a/home"),
                )
                .env(
                    "ORCA_CLAUDE_INJECTED_CONFIG_DIR",
                    env.path("orca-data/claude-profiles/a/home"),
                ),
        )
    };

    let r = run(&orca(&["-y"]));
    assert!(r.has("--read", scripts.to_str().unwrap()));
    assert!(r.has("--read", endpoint.to_str().unwrap()));
    assert!(!r.has("--allow", scripts.to_str().unwrap()));
    assert!(r.has("--allow", account.to_str().unwrap()));
    assert!(!r.nono.contains(&"--open-port".to_owned()));
    let dry = orca(&["--dry-run"]);
    assert!(
        dry.starts_with(&format!("CLAUDE_CONFIG_DIR={} nono run", account.display())),
        "{dry}"
    );
    let show = orca(&["show"]);
    assert!(show.contains("Orca hook scripts"), "{show}");
    assert!(show.contains("sandbox.orca = false"), "{show}");

    let unsandboxed = orca(&["--unsandboxed", "show"]);
    assert!(!unsandboxed.contains("in an Orca pane"), "{unsandboxed}");

    env.write(
        "orca-data/pointer",
        &env.path("orca-data/gone").display().to_string(),
    );
    let previous = env.path("orca-data/claude-profiles/a/home");
    fs::create_dir_all(&previous).unwrap();
    let blocked = |args: &[&str]| {
        env.cmd(args)
            .env("ORCA_PANE_KEY", "tab:1")
            .env("ORCA_CLAUDE_PROFILE_POINTER", env.path("orca-data/pointer"))
            .env("CLAUDE_CONFIG_DIR", &previous)
            .env("ORCA_CLAUDE_INJECTED_CONFIG_DIR", &previous)
            .assert()
    };
    blocked(&["-y"])
        .failure()
        .stderr(predicate::str::contains("is missing"));
    blocked(&["--dry-run"])
        .failure()
        .stderr(predicate::str::contains("is missing"));
    let show = String::from_utf8(blocked(&["show"]).success().get_output().stdout.clone()).unwrap();
    assert!(show.contains("is missing"), "{show}");
    assert!(!show.contains("claude-profiles/a"), "{show}");

    env.write("orca-data/pointer", "");
    let dry = orca(&["--dry-run"]);
    assert!(
        dry.starts_with("env -u CLAUDE_CONFIG_DIR nono run"),
        "{dry}"
    );

    env.write(
        "config/clankerbox/config.toml",
        "[sandbox]\nnetwork = \"block\"",
    );
    assert!(run(&orca(&["-y"])).has("--open-port", "4123"));

    env.write("config/clankerbox/config.toml", "[sandbox]\norca = false");
    let r = run(&orca(&["-y"]));
    assert!(!r.nono.iter().any(|a| a.contains(".orca")));
    assert!(!orca(&["--dry-run"]).contains("CLAUDE_CONFIG_DIR"));
    assert!(!orca(&["show"]).contains("Orca"));
}

#[test]
fn orca_cli_is_opt_in_and_grants_its_socket() {
    let env = Env::new();
    let data = env.path("orca-data");
    let socket = data.join("o-42-rt.sock");
    fs::create_dir_all(&data).unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    env.write(
        "orca-data/orca-runtime.json",
        &format!(
            r#"{{"transports":[{{"kind":"unix","endpoint":"{}"}}]}}"#,
            socket.display()
        ),
    );
    let launcher = env.path("app/Orca/resources/bin/orca");
    env.write("app/Orca/resources/bin/orca", "");
    let orca = |args: &[&str]| {
        stdout_of(
            env.cmd(args)
                .env("ORCA_PANE_KEY", "tab:1")
                .env("ORCA_USER_DATA_PATH", &data)
                .env("ORCA_CLI_COMMAND", &launcher),
        )
    };

    let r = run(&orca(&["-y"]));
    assert!(!r.nono.iter().any(|a| a.contains("unix-socket")));
    assert!(!r.nono.iter().any(|a| a.contains("orca-runtime.json")));

    env.write(
        "config/clankerbox/config.toml",
        "[sandbox]\norca_cli = true",
    );
    let r = run(&orca(&["-y"]));
    assert!(r.has("--allow-unix-socket", socket.to_str().unwrap()));
    assert!(r.has(
        "--read-file",
        data.join("orca-runtime.json").to_str().unwrap()
    ));
    assert!(r.has("--read", env.path("app/Orca").to_str().unwrap()));
    let show = orca(&["show"]);
    assert!(show.contains("every Orca terminal"), "{show}");
    assert!(show.contains("o-42-rt.sock"), "{show}");

    env.write(
        "config/clankerbox/config.toml",
        "[sandbox]\norca = false\norca_cli = true",
    );
    let r = run(&orca(&["-y"]));
    assert!(r.has("--allow-unix-socket", socket.to_str().unwrap()));
    assert!(!r.nono.iter().any(|a| a.contains("agent-hooks")));
}
