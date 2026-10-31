//! Agent tools: their declarations, their permission classes, the sandbox every path goes through,
//! and the executors that answer from it.
//!
//! Four pieces:
//!   * [`registry`] — the tool list the model is handed, with a JSON schema and a
//!     [`PermissionClass`] per entry, plus the OpenAI-shaped `tools` payload;
//!   * [`sandbox`] — lexical + canonicalized containment, protected paths, and the network gate;
//!   * [`exec`] — the workspace executors and the two network ones, and [`exec::execute`] as the one
//!     entry point that runs a call after [`authorize`] has cleared it;
//!   * [`web`] — the search and the guarded page read those network calls run.
//!
//! A shell now has an executor, but it is the one capability the path sandbox cannot bound: it runs
//! only while the user's Terminal access switch is on, and [`authorize`] refuses it otherwise. The
//! project runners — tests, build, linter — execute the same code by another name, so they answer to
//! that switch too; the three git reads report what the repository already holds and need no switch.
//! The engine's tool-call quirk is already accounted for here: an empty `arguments` object must be sent
//! as `"{}"`, because the fork answers HTTP 500 otherwise (llama.cpp#112).

pub mod exec;
pub mod github;
pub mod page;
pub mod pixels;
pub mod registry;
pub mod sandbox;
pub mod web;

use std::path::Path;

use freecode_config::agent::shell_governed;

pub use exec::{
    declared_checks, execute, execute_watched, plan, write_targets, Edit, Executed, PlanItem,
    PlanStatus, Summary, SummaryKey, Watcher, MAX_PLAN_ITEMS, MAX_RESULT_BYTES,
};
pub use github::{GitHubToken, PullRequest};
pub use page::{
    is_page_tool, page_op, PageHost, PageOp, MAX_CONSOLE_LINES, MAX_PAGE_TEXT, PAGE_TOOLS,
};
pub use registry::{
    executable_specs, find, is_delegated, is_executable, is_runnable, patch_target,
    read_only_specs, registry, summary, ToolCall, ToolDescriptor, DELEGATED, EXECUTABLE,
};
pub use sandbox::{Access, SandboxPolicy, SandboxViolation};
pub use web::{
    read_page, search, Hit, Page, WebError, DEFAULT_PAGE_CHARS, MAX_PAGE_CHARS, MAX_RESULTS,
};

#[derive(Debug, thiserror::Error)]
pub enum ToolsError {
    #[error("no tool named {0} is registered")]
    UnknownTool(String),
    #[error("{0} is in the {1} approval class, which is not granted for this run")]
    PermissionDenied(String, &'static str),
    #[error(
        "{0} reaches the internet, and nothing leaves this machine while the Privacy switch is off"
    )]
    NetworkNotConfigured(String),
    #[error("this build has no executor for {0}: it is declared but not implemented")]
    NoExecutor(String),
    #[error("{0}")]
    NotConfigured(String),
    #[error("Terminal access is switched off in Settings, so nothing was run")]
    ShellDisabled,
    #[error("{tool} could not open {path} ({reason})")]
    Io {
        tool: String,
        path: String,
        reason: String,
    },
    #[error(transparent)]
    Sandbox(#[from] SandboxViolation),
    #[error(transparent)]
    Web(#[from] WebError),
    #[error("invalid arguments for {tool}: {reason}")]
    InvalidArguments { tool: String, reason: String },
    #[error("{0}")]
    Page(String),
}

/// The keys whose values are filesystem paths, in the registry's schemas.
const PATH_KEYS: &[&str] = &["path", "file", "from", "to", "cwd"];

/// Validate a call against the registry and the sandbox in one place, so the loop cannot forget
/// either check. Reads are gated as reads, everything else as a write.
pub fn authorize(
    tool_id: &str,
    arguments: &serde_json::Value,
    policy: &SandboxPolicy,
) -> Result<ToolDescriptor, ToolsError> {
    let tool = find(tool_id).ok_or_else(|| ToolsError::UnknownTool(tool_id.into()))?;
    if tool.requires_network {
        policy
            .check_network()
            .map_err(|_| ToolsError::NetworkNotConfigured(tool_id.into()))?;
    }
    if shell_governed(tool.id) && !policy.shell_allowed {
        return Err(ToolsError::ShellDisabled);
    }
    let access = if tool.is_read_only() {
        Access::Read
    } else {
        Access::Write
    };
    for key in PATH_KEYS {
        if let Some(raw) = arguments.get(key).and_then(|v| v.as_str()) {
            policy.resolve_path(Path::new(raw), access)?;
        }
    }
    // A runner's `paths` is an array, which the loop above cannot see; the linter executor resolves
    // each entry before it builds an argument list, and this is the check that keeps the same rule in
    // one place for every caller that reaches `authorize` alone.
    if let Some(paths) = arguments.get("paths").and_then(|v| v.as_array()) {
        for raw in paths.iter().filter_map(|v| v.as_str()) {
            policy.resolve_path(Path::new(raw), Access::Read)?;
        }
    }
    Ok(tool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use freecode_config::agent::PermissionClass;
    use serde_json::json;

    fn policy(root: &Path) -> SandboxPolicy {
        SandboxPolicy {
            workspace_root: root.to_path_buf(),
            ..Default::default()
        }
    }

    #[test]
    fn an_unknown_tool_is_rejected_rather_than_defaulted_to() {
        let dir = tempfile::tempdir().unwrap();
        let err = authorize("rm_everything", &json!({}), &policy(dir.path()))
            .expect_err("unregistered tools must not run");
        assert!(matches!(err, ToolsError::UnknownTool(_)), "{err:?}");
    }

    #[test]
    fn a_path_argument_is_sandboxed_at_authorization_time() {
        let dir = tempfile::tempdir().unwrap();
        let p = policy(dir.path());
        let err = authorize(
            "write_file",
            &json!({ "path": "../../etc/hosts", "contents": "x" }),
            &p,
        )
        .expect_err("escaping the workspace must be refused");
        assert!(matches!(err, ToolsError::Sandbox(_)), "{err:?}");
        authorize(
            "write_file",
            &json!({ "path": "src/lib.rs", "contents": "x" }),
            &p,
        )
        .expect("an in-workspace write is authorized");
    }

    #[test]
    fn reading_outside_is_allowed_only_when_the_policy_opts_in() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("..").join("elsewhere.txt");
        let strict = policy(dir.path());
        let err = authorize("read_file", &json!({ "path": outside }), &strict)
            .expect_err("default policy keeps reads inside the workspace");
        assert!(
            matches!(
                err,
                ToolsError::Sandbox(SandboxViolation::ReadOutsideNotAllowed { .. })
            ),
            "{err:?}"
        );
        let lenient = SandboxPolicy {
            read_outside_workspace: true,
            ..strict
        };
        authorize("read_file", &json!({ "path": outside }), &lenient)
            .expect("an explicit opt-in permits the read");
    }

    #[test]
    fn network_tools_stay_locked_without_a_configured_provider() {
        let dir = tempfile::tempdir().unwrap();
        let p = policy(dir.path());
        let err = authorize("web_search", &json!({ "query": "rust" }), &p)
            .expect_err("no remote provider is configured");
        assert!(
            matches!(err, ToolsError::NetworkNotConfigured(_)),
            "{err:?}"
        );
        let on = SandboxPolicy {
            network_allowed: true,
            ..p
        };
        let tool = authorize("web_search", &json!({ "query": "rust" }), &on).unwrap();
        assert_eq!(tool.permission, PermissionClass::Network);
    }
}
