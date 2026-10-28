use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{GeneratedFile, shell_quote};
use crate::config::discover::hash;

const ADVICE: &str = include_str!("claude-plugin/advice.md");
const FAILING_TOOLS: &str = "Read|Write|Edit|MultiEdit|NotebookEdit|Glob|Grep|Bash";

pub fn generate(runtime_dir: &Path, cb: &Path) -> (PathBuf, Vec<GeneratedFile>) {
    let command = format!("{} hook explain-denial", shell_quote(&cb.to_string_lossy()));
    let hook = |matcher| json!([{ "matcher": matcher, "hooks": [{ "type": "command", "command": command }] }]);
    let hooks = json!({
        "hooks": {
            "PostToolUseFailure": hook(FAILING_TOOLS),
            "PostToolUse": hook("Bash"),
        },
    });
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
