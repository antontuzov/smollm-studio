//! Turning a tool call into an approval decision the UI can render and the loop can act on.
//!
//! Every side effect passes through here, which is what makes the transcript honest: a call that
//! runs silently is a call nobody can audit. The rules are the configured [`Approvals`] policy plus
//! three things the policy cannot express on its own — a mutating tool in Plan-only mode is refused
//! outright, a network-class tool is refused whenever no remote provider is configured, so the
//! "Local" badge keeps reflecting reality, and the calls that run project code (the shell and its
//! tests/build/linter runners) are refused whenever their switch is off, because no path check bounds
//! a command.

use freecode_config::agent::{shell_governed, ApprovalMode, Approvals, PermissionClass};
use freecode_tools::{summary, SandboxPolicy, ToolDescriptor};
use serde::{Deserialize, Serialize};

/// What the composer/inline prompt shows before a call runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub call_id: String,
    pub tool_id: String,
    /// One-line human description, e.g. "Run `cargo test`".
    pub headline: String,
    pub permission: PermissionClass,
    /// Milliseconds the UI waits before treating the request as `TimedOut`.
    pub timeout_ms: u64,
    /// Set when the call is a shell command, so the prompt can show the exact argv.
    #[serde(default)]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecision {
    /// Cleared by policy; run without interrupting the user.
    AutoAllowed,
    /// Cleared by the user at the prompt.
    UserApproved,
    /// Refused by the user.
    UserDenied,
    /// The prompt expired; treated as a denial, never as consent.
    TimedOut,
    /// Refused by policy without asking (Plan-only mutation, disabled network, denied command).
    BlockedByPolicy,
}

impl ApprovalDecision {
    pub fn is_allowed(self) -> bool {
        matches!(
            self,
            ApprovalDecision::AutoAllowed | ApprovalDecision::UserApproved
        )
    }

    pub fn i18n_key(self) -> &'static str {
        match self {
            ApprovalDecision::AutoAllowed => "approval.autoAllowed",
            ApprovalDecision::UserApproved => "approval.userApproved",
            ApprovalDecision::UserDenied => "approval.userDenied",
            ApprovalDecision::TimedOut => "approval.timedOut",
            ApprovalDecision::BlockedByPolicy => "approval.blockedByPolicy",
        }
    }
}

/// The outcome the loop needs: a decision, and whether the UI must prompt for it.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    Decided(ApprovalDecision),
    /// A prompt is required; the loop waits up to `request.timeout_ms`.
    Prompt(ApprovalRequest),
}

pub fn request(
    call_id: &str,
    tool: &ToolDescriptor,
    arguments: &serde_json::Value,
    approvals: &Approvals,
) -> ApprovalRequest {
    ApprovalRequest {
        call_id: call_id.to_string(),
        tool_id: tool.id.to_string(),
        headline: summary(tool.id, arguments),
        permission: tool.permission,
        timeout_ms: approvals.shell_timeout_ms,
        command: arguments
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    }
}

/// Decide without a user, applying policy then the explicit lists. `Pending::Prompt` means ask.
pub fn evaluate(
    tool: &ToolDescriptor,
    arguments: &serde_json::Value,
    approvals: &Approvals,
    sandbox: &SandboxPolicy,
    call_id: &str,
) -> Pending {
    // 1. Plan-only mode never mutates, and it does not offer to.
    if approvals.mode == ApprovalMode::PlanOnly && !tool.is_read_only() {
        return Pending::Decided(ApprovalDecision::BlockedByPolicy);
    }

    // 2. Network tools need an explicitly configured remote provider. `network_allowed` is set from
    // Privacy.allowRemoteProviders, so a blocked call here is what keeps the "Local" badge true.
    if tool.requires_network && !sandbox.network_allowed {
        return Pending::Decided(ApprovalDecision::BlockedByPolicy);
    }

    // 2b. The command runs with the user's own permissions, so the path sandbox cannot speak for it —
    // and the runners that execute the project's tests, build and lint are the same code by another
    // name. Their switch is the consent, and while it is off the call is refused without asking.
    if shell_governed(tool.id) && !sandbox.shell_allowed {
        return Pending::Decided(ApprovalDecision::BlockedByPolicy);
    }

    // 2c. A subagent is a second model run, and its own switch is that run's consent. Off means the
    // call does not reach the loop at all — a prompt would ask for permission to turn a switch the
    // page has already set. On, and the dial's answer decides whether the run also stops to ask.
    if tool.id == "spawn_subagent" && !approvals.subagents_enabled {
        return Pending::Decided(ApprovalDecision::BlockedByPolicy);
    }

    // 3. An explicitly denied shell command is blocked without prompting; an explicitly allowed one
    //    runs without asking. These lists are the user's own words, so they outrank the mode.
    if let Some(cmd) = arguments.get("command").and_then(|v| v.as_str()) {
        if matches_any(&approvals.shell_deny, cmd) {
            return Pending::Decided(ApprovalDecision::BlockedByPolicy);
        }
        if matches_any(&approvals.shell_allow, cmd) {
            return Pending::Decided(ApprovalDecision::AutoAllowed);
        }
    }

    // 4. The configured mode (including per-tool overrides).
    match approvals.decide(tool.id, tool.permission) {
        Some(true) => Pending::Decided(ApprovalDecision::AutoAllowed),
        Some(false) => Pending::Decided(ApprovalDecision::BlockedByPolicy),
        None => Pending::Prompt(request(call_id, tool, arguments, approvals)),
    }
}

/// Prefix matching on the whole command or its first token, so an `rm` deny entry blocks
/// `rm -rf target` without sweeping up `echo rm`.
fn matches_any(list: &[String], command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or_default();
    list.iter()
        .any(|entry| command.trim().starts_with(entry.trim()) || first == entry.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use freecode_tools::find;
    use serde_json::json;
    use std::path::Path;

    fn sandbox(root: &Path) -> SandboxPolicy {
        SandboxPolicy {
            workspace_root: root.to_path_buf(),
            ..Default::default()
        }
    }

    #[test]
    fn reads_and_workspace_edits_run_without_interrupting_the_user() {
        let dir = tempfile::tempdir().unwrap();
        let approvals = Approvals::default();
        for id in ["read_file", "write_file"] {
            let tool = find(id).unwrap();
            let got = evaluate(
                &tool,
                &json!({ "path": "a.txt" }),
                &approvals,
                &sandbox(dir.path()),
                "c",
            );
            assert_eq!(got, Pending::Decided(ApprovalDecision::AutoAllowed), "{id}");
        }
    }

    #[test]
    fn side_effects_prompt_with_the_exact_command_in_the_headline() {
        let dir = tempfile::tempdir().unwrap();
        // Ask mode still asks for a command the switch has admitted; what it cannot do is satisfy the
        // prompt in this build, which is why the switch — not the mode — is the terminal's consent.
        let approvals = Approvals {
            mode: ApprovalMode::AskAlways,
            shell_enabled: true,
            ..Default::default()
        };
        let shell = SandboxPolicy {
            shell_allowed: true,
            ..sandbox(dir.path())
        };
        let tool = find("terminal").unwrap();
        let args = json!({ "command": "cargo test" });
        match evaluate(&tool, &args, &approvals, &shell, "call-1") {
            Pending::Prompt(req) => {
                assert_eq!(req.headline, "Run `cargo test`");
                assert_eq!(req.command.as_deref(), Some("cargo test"));
                assert_eq!(req.timeout_ms, Approvals::default_timeout_ms());
                assert_eq!(req.call_id, "call-1");
            }
            other => panic!("terminal must prompt: {other:?}"),
        }
    }

    #[test]
    fn a_switched_off_shell_is_refused_before_the_mode_can_approve_it() {
        let dir = tempfile::tempdir().unwrap();
        // Auto mode approves workspace writes; the shell is the one call it must not approve while the
        // Terminal switch is off, because the switch is the only place that consent can be given.
        let tool = find("terminal").unwrap();
        let args = json!({ "command": "cargo test" });
        assert_eq!(
            evaluate(
                &tool,
                &args,
                &Approvals::default(),
                &sandbox(dir.path()),
                "c"
            ),
            Pending::Decided(ApprovalDecision::BlockedByPolicy)
        );
        // The project runners are the same consent: a switched-off shell does not execute the
        // workspace's own tests either, whatever permission class the registry gives them.
        for id in ["run_tests", "run_build", "run_linter"] {
            let runner = find(id).unwrap();
            assert_eq!(
                evaluate(
                    &runner,
                    &json!({}),
                    &Approvals::default(),
                    &sandbox(dir.path()),
                    "c"
                ),
                Pending::Decided(ApprovalDecision::BlockedByPolicy),
                "{id}"
            );
        }
        // A git read is not project code: it reports what the repository already holds, and the
        // switch has nothing to admit.
        let git = find("git_status").unwrap();
        assert_eq!(
            evaluate(
                &git,
                &json!({}),
                &Approvals::default(),
                &sandbox(dir.path()),
                "c"
            ),
            Pending::Decided(ApprovalDecision::AutoAllowed)
        );
        // And on, in Auto mode, the switch's answer runs it without a prompt.
        let on = Approvals {
            shell_enabled: true,
            ..Default::default()
        };
        let shell = SandboxPolicy {
            shell_allowed: true,
            ..sandbox(dir.path())
        };
        assert_eq!(
            evaluate(&tool, &args, &on, &shell, "c"),
            Pending::Decided(ApprovalDecision::AutoAllowed)
        );
    }

    #[test]
    fn plan_only_refuses_mutation_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let approvals = Approvals {
            mode: ApprovalMode::PlanOnly,
            ..Default::default()
        };
        let tool = find("write_file").unwrap();
        let got = evaluate(
            &tool,
            &json!({ "path": "a.txt", "contents": "x" }),
            &approvals,
            &sandbox(dir.path()),
            "c",
        );
        assert_eq!(got, Pending::Decided(ApprovalDecision::BlockedByPolicy));
        // Reads still work, or planning could not gather context.
        let read = find("read_file").unwrap();
        assert!(matches!(
            evaluate(
                &read,
                &json!({ "path": "a.txt" }),
                &approvals,
                &sandbox(dir.path()),
                "c"
            ),
            Pending::Decided(ApprovalDecision::AutoAllowed)
        ));
    }

    #[test]
    fn network_tools_are_policy_blocked_until_a_provider_is_configured() {
        let dir = tempfile::tempdir().unwrap();
        let approvals = Approvals {
            mode: ApprovalMode::AskAlways,
            ..Default::default()
        };
        let tool = find("web_search").unwrap();
        // Neither sandbox nor privacy gate is open: blocked, not prompted.
        let got = evaluate(
            &tool,
            &json!({ "query": "rust" }),
            &approvals,
            &sandbox(dir.path()),
            "c",
        );
        assert_eq!(got, Pending::Decided(ApprovalDecision::BlockedByPolicy));
    }

    /// The other half of that gate, and the reason it is the whole consent: with the switch on, a
    /// web call runs on the dial's own authority rather than waiting for a prompt this build cannot
    /// show. A run that stopped to ask would read as a stalled server.
    #[test]
    fn a_network_call_runs_once_the_privacy_switch_has_agreed_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let s = SandboxPolicy {
            workspace_root: dir.path().to_path_buf(),
            network_allowed: true,
            ..Default::default()
        };
        let approvals = Approvals::default();
        for id in ["web_search", "fetch_url"] {
            let tool = find(id).unwrap();
            let args = if id == "web_search" {
                json!({ "query": "ternary bonsai" })
            } else {
                json!({ "url": "https://en.wikipedia.org/wiki/Rust_(programming_language)" })
            };
            assert_eq!(
                evaluate(&tool, &args, &approvals, &s, "c"),
                Pending::Decided(ApprovalDecision::AutoAllowed),
                "{id} with the switch on"
            );
        }
        // Plan-only still refuses it: there the mode speaks before the switch gets to.
        let planner = Approvals {
            mode: ApprovalMode::PlanOnly,
            ..Default::default()
        };
        assert_eq!(
            evaluate(
                &find("web_search").unwrap(),
                &json!({ "query": "rust" }),
                &planner,
                &s,
                "c"
            ),
            Pending::Decided(ApprovalDecision::BlockedByPolicy)
        );
    }

    #[test]
    fn the_users_own_allow_and_deny_lists_outrank_the_mode() {
        let dir = tempfile::tempdir().unwrap();
        let approvals = Approvals {
            shell_allow: vec!["cargo test".into()],
            shell_deny: vec!["rm".into()],
            shell_enabled: true,
            ..Default::default()
        };
        let tool = find("terminal").unwrap();
        let s = SandboxPolicy {
            shell_allowed: true,
            ..sandbox(dir.path())
        };
        assert_eq!(
            evaluate(
                &tool,
                &json!({ "command": "cargo test -p freecode-core" }),
                &approvals,
                &s,
                "c"
            ),
            Pending::Decided(ApprovalDecision::AutoAllowed)
        );
        assert_eq!(
            evaluate(
                &tool,
                &json!({ "command": "rm -rf target" }),
                &approvals,
                &s,
                "c"
            ),
            Pending::Decided(ApprovalDecision::BlockedByPolicy)
        );
        // A command that mentions but is not `rm` must not be swept up by the deny entry; with the
        // switch on, Auto mode runs what the lists did not deny.
        assert_eq!(
            evaluate(&tool, &json!({ "command": "echo rm" }), &approvals, &s, "c"),
            Pending::Decided(ApprovalDecision::AutoAllowed)
        );
        // The switch outranks the allow list: with it off nothing runs, however the list reads.
        let off = Approvals {
            shell_allow: vec!["cargo test".into()],
            ..Default::default()
        };
        assert_eq!(
            evaluate(
                &tool,
                &json!({ "command": "cargo test" }),
                &off,
                &sandbox(dir.path()),
                "c"
            ),
            Pending::Decided(ApprovalDecision::BlockedByPolicy)
        );
    }

    /// The dial's own bargain, and the reason a prompt is worth having: Ask stops at every change and
    /// at no read. A run that opened six files to answer one question would otherwise put six cards in
    /// front of the user for the one thing they meant to decide on.
    #[test]
    fn ask_prompts_for_every_change_and_for_no_read() {
        let dir = tempfile::tempdir().unwrap();
        let approvals = Approvals {
            mode: ApprovalMode::AskAlways,
            ..Default::default()
        };
        for id in ["read_file", "list_dir", "grep_search", "git_status"] {
            let tool = find(id).unwrap();
            assert_eq!(
                evaluate(
                    &tool,
                    &json!({ "path": "a" }),
                    &approvals,
                    &sandbox(dir.path()),
                    "c"
                ),
                Pending::Decided(ApprovalDecision::AutoAllowed),
                "{id} is a read and changes nothing"
            );
        }
        for id in ["write_file", "edit_file", "apply_patch"] {
            let tool = find(id).unwrap();
            assert!(
                matches!(
                    evaluate(
                        &tool,
                        &json!({ "path": "a.txt" }),
                        &approvals,
                        &sandbox(dir.path()),
                        "c"
                    ),
                    Pending::Prompt(_)
                ),
                "{id} changes the folder and is the user's to approve"
            );
        }
    }

    #[test]
    fn a_timeout_denies_rather_than_consenting() {
        assert!(!ApprovalDecision::TimedOut.is_allowed());
        assert!(ApprovalDecision::UserApproved.is_allowed());
        assert_eq!(ApprovalDecision::TimedOut.i18n_key(), "approval.timedOut");
    }
}
