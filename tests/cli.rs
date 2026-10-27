use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// Prints args, selected env vars and cwd, one per line. `profile show` fails for profiles in
/// `CB_STUB_MISSING`.
const STUB: &str = r#"#!/bin/sh
if [ "$1 $2" = "profile show" ]; then
  case " $CB_STUB_MISSING " in *" $3 "*) exit 1 ;; esac
  exit 0
fi
for a in "$@"; do printf '%s\n' "$a"; done
printf 'env=%s\n' "$CB_TEST_ENV"
for var in HERDR_AGENT CLAUDE_CONFIG_DIR; do
  eval "value=\${$var-}"
  [ -n "$value" ] && printf '%s=%s\n' "$var" "$value"
done
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
        let out = self
            .cmd(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(out).unwrap()
    }
}

fn lines(s: &str) -> Vec<&str> {
    s.lines().collect()
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
    let expected = [
        "run",
        "--profile",
        "claude-code",
        "--allow-cwd",
        "--allow",
        repo.to_str().unwrap(),
        "--read",
        frontend.to_str().unwrap(),
        "--block-net",
        "--",
        "claude",
        "--model",
        "opus",
        "--resume",
        "env=from-project",
        &format!("cwd={}", repo.display()),
    ];
    assert_eq!(lines(&out), expected);

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
        "[agents.mine]\ninherits = \"claude\"\nargs = [\"--mine\"]\nsandbox.nono.profile = \"p\"",
    );
    let out = env.stdout(&["-A", "mine"]);
    assert!(out.contains("--profile\np\n"), "{out}");
    assert!(out.contains("--\nclaude\n--mine\n"), "{out}");
}

#[test]
fn unsandboxed_runs_agent_directly() {
    let env = Env::new();
    env.cmd(&["--unsandboxed", "--", "hi"])
        .assert()
        .success()
        .stdout(format!(
            "hi\nenv=\ncwd={}\n",
            env.path("home/repo").display()
        ))
        .stderr(predicate::str::contains("WITHOUT a sandbox"));
    env.cmd(&["--unsandboxed", "--read", "x"])
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

/// Config replacing `nclaude`-style shell wrappers around `nono wrap`.
const NCLAUDE_CONFIG: &str = r#"
[agents.claude]
sandbox.nono.profile = "nclaude"
sandbox.nono.mode = "direct"
sandbox.env = { HERDR_AGENT = "claude" }

[agents.pi]
sandbox.nono.profile = "npi"

[agents.omp]
sandbox.nono.profile = "nomp"

[profiles.personal]
sandbox.env = { CLAUDE_CONFIG_DIR = "${home}/.claude-personal" }

[profiles.personal.agents.claude]
sandbox.nono.profile = "nclaude-personal"

[profiles.personal.agents.pi]
sandbox.nono.profile = "npi-personal"
"#;

fn nclaude_env() -> Env {
    let env = Env::new();
    env.write("config/clankerbox/config.toml", NCLAUDE_CONFIG);
    env
}

/// `nclaude` → `cb`
#[test]
fn replaces_nclaude() {
    let env = nclaude_env();
    let repo = env.path("home/repo");
    let out = env.stdout(&[]);
    assert_eq!(
        lines(&out),
        [
            "wrap",
            "--profile",
            "nclaude",
            "--allow-cwd",
            "--allow",
            repo.to_str().unwrap(),
            "--",
            "claude",
            "env=",
            "HERDR_AGENT=claude",
            &format!("cwd={}", repo.display()),
        ]
    );
}

/// `nclaude-personal` → `cb -p personal`
#[test]
fn replaces_nclaude_personal() {
    let env = nclaude_env();
    let repo = env.path("home/repo");
    let personal = env.path("home/.claude-personal");
    let out = env.stdout(&["-p", "personal"]);
    assert_eq!(
        lines(&out),
        [
            "wrap",
            "--profile",
            "nclaude-personal",
            "--allow-cwd",
            "--allow",
            repo.to_str().unwrap(),
            "--",
            "claude",
            "env=",
            "HERDR_AGENT=claude",
            &format!("CLAUDE_CONFIG_DIR={}", personal.display()),
            &format!("cwd={}", repo.display()),
        ]
    );
}

/// `nclaude --allow … --read … -- --resume <id>` → same args to `cb`
#[test]
fn replaces_nclaude_with_one_off_grants_and_agent_args() {
    let env = nclaude_env();
    let repo = env.path("home/repo");
    let out = env.stdout(&[
        "--allow",
        "./some-dir",
        "--read",
        "../some-other-dir",
        "--",
        "--resume",
        "some-claude-session-id",
    ]);
    assert_eq!(
        lines(&out),
        [
            "wrap",
            "--profile",
            "nclaude",
            "--allow-cwd",
            "--allow",
            repo.to_str().unwrap(),
            "--allow",
            "./some-dir",
            "--read",
            "../some-other-dir",
            "--",
            "claude",
            "--resume",
            "some-claude-session-id",
            "env=",
            "HERDR_AGENT=claude",
            &format!("cwd={}", repo.display()),
        ]
    );
}

/// `npi` / `npi-personal`
#[test]
fn replaces_npi_and_reports_unknown_profiles() {
    let env = nclaude_env();
    let out = env.stdout(&["-A", "pi"]);
    assert!(out.starts_with("run\n--profile\nnpi\n"), "{out}");
    assert!(out.contains("--\npi\n"), "{out}");

    let out = env.stdout(&["-A", "pi", "-p", "personal"]);
    assert!(out.starts_with("run\n--profile\nnpi-personal\n"), "{out}");
    assert!(out.contains("CLAUDE_CONFIG_DIR="), "{out}");

    let out = env.stdout(&["-p", "personal", "--nono-profile", "other"]);
    assert!(out.starts_with("wrap\n--profile\nother\n"), "{out}");

    env.cmd(&["-p", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unknown profile `nope` (known: personal)",
        ));
}

#[test]
fn dir_option_sets_working_directory() {
    let env = Env::new();
    let repo = env.path("home/repo");
    let out = env
        .cmd(&["-C", repo.to_str().unwrap()])
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
        .stderr(predicate::str::contains("rust  rw  ~/.cargo/registry"));
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
fn missing_nono_profile_suggests_pull() {
    let env = Env::new();
    env.cmd(&[])
        .env("CB_STUB_MISSING", "claude-code")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "nono profile `claude-code` is not installed; run `nono pull nolabs-ai/claude`",
        ));
    env.cmd(&["--nono-profile", "custom"])
        .env("CB_STUB_MISSING", "custom")
        .assert()
        .failure()
        .stderr(predicate::str::contains("nono profile `custom` not found"));
}
