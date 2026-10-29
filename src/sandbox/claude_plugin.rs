use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{GeneratedFile, shell_quote};
use crate::config::discover::hash;

const ADVICE: &str = include_str!("claude-plugin/advice.md");
const FAILING_TOOLS: &str = "Read|Write|Edit|MultiEdit|NotebookEdit|Glob|Grep|Bash";
const HERDR_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

pub fn generate(
    runtime_dir: &Path,
    cb: &Path,
    explain_denials: bool,
    herdr: bool,
) -> (PathBuf, Vec<GeneratedFile>) {
    let cb = shell_quote(&cb.to_string_lossy());
    let mut events = Map::new();
    let mut add = |event: &str, matcher: Option<&str>, hook: Value| {
        let mut group = json!({ "hooks": [hook] });
        if let Some(matcher) = matcher {
            group["matcher"] = matcher.into();
        }
        if let Value::Array(groups) = events.entry(event).or_insert_with(|| json!([])) {
            groups.push(group);
        }
    };
    if explain_denials {
        let hook = json!({ "type": "command", "command": format!("{cb} hook explain-denial") });
        add("PostToolUseFailure", Some(FAILING_TOOLS), hook.clone());
        add("PostToolUse", Some("Bash"), hook);
    }
    if herdr {
        let command = format!("{cb} hook herdr");
        // Claude may exit before an async SessionEnd hook has run.
        let ending = json!({ "type": "command", "command": command, "timeout": 5 });
        let hook = json!({ "type": "command", "command": command, "async": true });
        for event in HERDR_EVENTS {
            let hook = if *event == "SessionEnd" {
                &ending
            } else {
                &hook
            };
            add(event, None, hook.clone());
        }
        add(
            "Notification",
            Some("permission_prompt|elicitation_dialog"),
            hook,
        );
    }
    let hooks = json!({ "hooks": events });
    let files = [
        (
            ".claude-plugin/plugin.json",
            include_str!("claude-plugin/plugin.json").to_owned(),
        ),
        ("hooks/hooks.json", format!("{hooks:#}\n")),
    ];
    let all: Vec<_> = files.iter().flat_map(|(p, c)| [*p, c]).collect();
    let dir = runtime_dir
        .join("claude-plugin")
        .join(&hash(all.join("\0").as_bytes())[..16]);
    let files = files
        .into_iter()
        .map(|(path, contents)| GeneratedFile {
            path: dir.join(path),
            contents,
        })
        .collect();
    (dir, files)
}

/// Checks only the error and stderr: commands and stdout often merely mention denials.
pub fn explain_denial(input: &Value) -> Option<String> {
    let response = &input["tool_response"];
    let text = [&input["error"], &response["stderr"], response]
        .into_iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    // Whole words only: build output names crates like `landlock`.
    let words: Vec<_> = text.split(|c: char| !c.is_alphanumeric()).collect();
    let denied = ["operation not permitted", "permission denied"]
        .iter()
        .any(|s| text.contains(s))
        || words.iter().any(|w| matches!(*w, "eperm" | "eacces"))
        || (words.contains(&"sandbox") && words.contains(&"denied"));
    denied.then(|| {
        json!({
            "hookSpecificOutput": {
                "hookEventName": input["hook_event_name"],
                "additionalContext": ADVICE,
            },
        })
        .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hooks(explain_denials: bool, herdr: bool) -> Value {
        let (_, files) = generate(
            Path::new("/run"),
            Path::new("/bin/cb"),
            explain_denials,
            herdr,
        );
        let file = files.iter().find(|f| f.path.ends_with("hooks/hooks.json"));
        serde_json::from_str(&file.unwrap().contents).unwrap()
    }

    fn commands(hooks: &Value, event: &str) -> Vec<String> {
        hooks["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|g| g["hooks"].as_array().unwrap())
            .map(|h| h["command"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn herdr_hooks_only_when_enabled() {
        let explain = hooks(true, false);
        assert_eq!(
            explain["hooks"].as_object().unwrap().len(),
            2,
            "{explain:#}"
        );
        assert!(!explain.to_string().contains("hook herdr"));

        let herdr = hooks(false, true);
        for event in HERDR_EVENTS.iter().chain(&["Notification"]) {
            assert_eq!(commands(&herdr, event), ["/bin/cb hook herdr"], "{event}");
        }
        assert_eq!(herdr["hooks"]["PostToolUse"][0]["hooks"][0]["async"], true);
        assert!(
            herdr["hooks"]["SessionEnd"][0]["hooks"][0]
                .get("async")
                .is_none()
        );
        assert_eq!(
            herdr["hooks"]["Notification"][0]["matcher"],
            "permission_prompt|elicitation_dialog"
        );

        let both = hooks(true, true);
        assert_eq!(
            commands(&both, "PostToolUse"),
            ["/bin/cb hook explain-denial", "/bin/cb hook herdr"]
        );
    }

    #[test]
    fn explains_only_errors_and_stderr() {
        let failure = json!({
            "hook_event_name": "PostToolUseFailure",
            "tool_input": { "file_path": "/x" },
            "error": "EACCES: permission denied, open '/x'",
        });
        let out: Value = serde_json::from_str(&explain_denial(&failure).unwrap()).unwrap();
        assert_eq!(
            out["hookSpecificOutput"]["hookEventName"],
            "PostToolUseFailure"
        );
        assert!(
            out["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .contains("nono why --self")
        );

        let stderr = json!({
            "hook_event_name": "PostToolUse",
            "tool_response": { "stdout": "", "stderr": "cat: /x: Permission denied" },
        });
        assert!(explain_denial(&stderr).is_some());

        let quiet = json!({
            "hook_event_name": "PostToolUse",
            "cwd": "/home/me/landlock-rs",
            "tool_input": { "command": "grep -rn 'Permission denied' src" },
            "tool_response": { "stdout": "src/a.rs: Permission denied", "stderr": "" },
        });
        assert!(explain_denial(&quiet).is_none());

        let build = json!({
            "hook_event_name": "PostToolUse",
            "tool_response": { "stderr": "   Compiling landlock v0.4.2\n   Compiling sleeperman v1.0.0" },
        });
        assert!(explain_denial(&build).is_none());
    }
}
