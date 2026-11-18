//! Which tool calls run on their own and which wait for the user.
//!
//! The first version has one mode, "workspace write": reading and editing
//! inside the workspace is allowed, every command asks, and anything that
//! touches a path outside the workspace asks.

use std::path::Path;

use serde_json::Value;

use crate::tools::{inside_workspace, resolve};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Ask {
        /// The rule that asked: `command`, `outside_read`, or `outside_write`.
        /// A session-wide approval covers later requests with the same reason.
        reason: &'static str,
        /// The command or path shown to the user.
        detail: String,
    },
}

pub fn check(tool: &str, input: &Value, workspace: &Path) -> Verdict {
    let outside = |name: &str| {
        let path = resolve(workspace, input[name].as_str().unwrap_or("."));
        (!inside_workspace(workspace, &path)).then(|| path.to_string_lossy().into_owned())
    };
    match tool {
        "run_command" => Verdict::Ask { reason: "command", detail: input["command"].as_str().unwrap_or_default().to_string() },
        "edit_file" => match outside("path") {
            Some(path) => Verdict::Ask { reason: "outside_write", detail: path },
            None => Verdict::Allow,
        },
        "read_file" | "search" => match outside("path") {
            Some(path) => Verdict::Ask { reason: "outside_read", detail: path },
            None => Verdict::Allow,
        },
        // Unknown tools reach the executor, which answers them with an error result.
        _ => Verdict::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn allows_the_workspace_and_asks_for_commands_and_outside_paths() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        assert_eq!(check("read_file", &json!({ "path": "src/a.rs" }), workspace), Verdict::Allow);
        assert_eq!(check("search", &json!({ "pattern": "x" }), workspace), Verdict::Allow);
        assert_eq!(check("edit_file", &json!({ "path": "new/dir/a.rs" }), workspace), Verdict::Allow);
        assert_eq!(
            check("run_command", &json!({ "command": "cargo test" }), workspace),
            Verdict::Ask { reason: "command", detail: "cargo test".into() }
        );
        assert!(matches!(check("edit_file", &json!({ "path": "../x.rs" }), workspace), Verdict::Ask { reason: "outside_write", .. }));
        assert!(matches!(check("read_file", &json!({ "path": "../x.rs" }), workspace), Verdict::Ask { reason: "outside_read", .. }));
        assert!(matches!(check("search", &json!({ "pattern": "x", "path": ".." }), workspace), Verdict::Ask { reason: "outside_read", .. }));
    }
}
