//! The Extensions page: what the agent can reach, and what this build actually runs.
//!
//! An extensions screen in a cloud app installs add-ons. This app has no add-on format and no store to
//! install from, so the local answer to the same row is the list the agent itself is handed — the tool
//! registry — plus the two facts a user cannot get from anywhere else: which of those calls run without
//! interrupting, and which the sandbox would refuse. Both are computed by the engine that decides them
//! at run time ([`freecode_core::approvals::evaluate`]) against the settings in force, so the page cannot
//! tell a friendlier story than the loop acts. The registry holds nothing but calls that reach a runner,
//! so what is on this page is what a run can do; the count of each is on it because the switches decide
//! which of them the next prompt is actually offered.

use freecode_config::agent::{ApprovalMode, PermissionClass};
use freecode_config::Settings;
use freecode_core::approvals::{evaluate, ApprovalDecision, Pending};
use freecode_tools::{is_runnable, registry, SandboxPolicy, ToolDescriptor};
use serde::Serialize;
use serde_json::json;
use std::path::Path;

/// One tool as the page shows it. The class is what the tool can do to the machine; the decision is
/// what the current approval dial says about it. Both travel as the catalog keys the page looks its
/// wording up by, so the sentences live in the locale files like every other label — and the class
/// goes out as `PermissionClass`'s own serde spelling rather than a second copy of it, because the
/// UI's union is written against that spelling and a hand-typed one could drift from it silently.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRow {
    pub id: String,
    pub title: String,
    pub description: String,
    pub permission: PermissionClass,
    pub requires_network: bool,
    /// Whether a call reaches a runner in this build. The registry holds nothing but calls that do, so
    /// today this is true of every row — and the page keeps the field because it is the fact that would
    /// change first if a declaration were ever added without a verb, and a row saying "declared, not
    /// run" is the honest version of that mistake.
    pub runnable: bool,
    pub decision: &'static str,
}

/// Everything the page renders, in one read.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub tools: Vec<ToolRow>,
    /// How many of them a call can actually run through, printed beside the count of what is declared.
    /// The two figures are equal because the registry is built that way, and the page says both so the
    /// equality is something the user can see rather than take on trust.
    pub runnable: usize,
    /// The folder a runnable call may read, as the run's sandbox resolves it — the project folder
    /// picked in Settings when there is one. A path the user needs to be able to check, which is why
    /// it is on the page rather than only in the sandbox.
    pub workspace_root: String,
    /// The dial in force, as the settings document spells it — the same wire spelling the Settings
    /// surface writes, so the page and the control cannot report different modes.
    pub approval_mode: ApprovalMode,
    pub network_allowed: bool,
}

/// What the run would do with a call to this tool right now: run it, stop to ask, or refuse it. An
/// empty argument object is enough, because nothing in the policy reads an argument except the shell
/// command lists — and those only apply to a tool that takes a command.
fn decision_of(
    tool: &ToolDescriptor,
    sandbox: &SandboxPolicy,
    settings: &Settings,
) -> &'static str {
    match evaluate(tool, &json!({}), &settings.approvals, sandbox, "inventory") {
        Pending::Prompt(_) => "ask",
        Pending::Decided(ApprovalDecision::AutoAllowed | ApprovalDecision::UserApproved) => "auto",
        Pending::Decided(_) => "blocked",
    }
}

impl Inventory {
    pub fn build(root: &Path, settings: &Settings) -> Self {
        // The same policy the loop builds for a run, so the page reports the sandbox the next prompt
        // will meet rather than a description of it.
        let sandbox = SandboxPolicy {
            workspace_root: root.to_path_buf(),
            read_outside_workspace: false,
            network_allowed: settings.privacy.allow_remote_providers,
            shell_allowed: settings.approvals.shell_enabled,
            shell_timeout_secs: (settings.approvals.shell_timeout_ms / 1000).max(1),
            extra_denied: Vec::new(),
        };
        let tools: Vec<ToolRow> = registry()
            .into_iter()
            .map(|tool| ToolRow {
                id: tool.id.to_string(),
                title: tool.title.to_string(),
                description: tool.description.to_string(),
                permission: tool.permission,
                requires_network: tool.requires_network,
                runnable: is_runnable(tool.id),
                decision: decision_of(&tool, &sandbox, settings),
            })
            .collect();
        let runnable = tools.iter().filter(|row| row.runnable).count();
        Inventory {
            tools,
            runnable,
            workspace_root: root.display().to_string(),
            approval_mode: settings.approvals.mode,
            network_allowed: settings.privacy.allow_remote_providers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use freecode_config::Settings;

    fn inventory(settings: &Settings) -> Inventory {
        Inventory::build(Path::new("/tmp"), settings)
    }

    fn row<'a>(inv: &'a Inventory, id: &str) -> &'a ToolRow {
        inv.tools.iter().find(|r| r.id == id).expect("tool")
    }

    /// The four workspace reads, the plan list, the three repository reads, the three workspace
    /// writes, the nine browser calls, the two web calls, the one command, its three runners and
    /// the one delegation are what the agent loop actually offers today, and the page has to say so:
    /// a list that showed more than these as live would describe an agent this build does not have.
    #[test]
    fn the_reads_the_writes_the_web_calls_the_shell_and_the_delegation_are_what_a_call_can_run_through(
    ) {
        let inv = inventory(&Settings::default());
        assert_eq!(inv.runnable, 32, "{:?}", inv.tools);
        for id in ["read_file", "list_dir", "glob", "grep_search"] {
            assert!(row(&inv, id).runnable, "{id}");
            assert_eq!(row(&inv, id).decision, "auto", "{id}");
        }
        // A repository read is a read: it runs a fixed argv against the index and asks for no switch.
        for id in ["git_status", "git_diff", "git_log"] {
            assert!(row(&inv, id).runnable, "{id}");
            assert_eq!(row(&inv, id).decision, "auto", "{id}");
        }
        // The writes are offered, and they are auto under the default dial because the sandbox
        // bounds them to the workspace — which is the whole reason they can be. What they may not do
        // is reach a name outside it, the create cannot take a file that is already there, and the
        // patch has to find every line it says it is changing.
        for id in ["write_file", "edit_file", "apply_patch"] {
            assert!(row(&inv, id).runnable, "{id}");
            assert_eq!(row(&inv, id).decision, "auto", "{id}");
        }
        // The browser calls act on the panel this window shows, so they need no switch of their own:
        // one that was closed is refused by naming the panel, not by hiding the tool.
        for id in [
            "browser_open",
            "browser_read",
            "browser_click",
            "browser_type",
            "browser_press",
            "browser_wait",
            "browser_console",
            "browser_capture",
            "browser_check",
        ] {
            assert!(row(&inv, id).runnable, "{id}");
            assert_eq!(row(&inv, id).decision, "auto", "{id}");
        }
        // The web tools have executors, so they are runnable in this build — but a local machine is
        // the default, and the page shows the gate closed rather than the plumbing.
        for id in ["web_search", "fetch_url"] {
            assert!(row(&inv, id).runnable, "{id}");
            assert_eq!(row(&inv, id).decision, "blocked", "{id}");
        }
        // The command and the three project runners have executors too, and one shared switch — off
        // by default, so the page shows them refused rather than merely waiting for the dial.
        for id in ["terminal", "run_tests", "run_build", "run_linter"] {
            assert!(row(&inv, id).runnable, "{id}");
            assert_eq!(row(&inv, id).decision, "blocked", "{id}");
        }
        // The delegation is answered by the run loop rather than an executor, so it is runnable in
        // this build — but a second model run costs the same context as the first, and the switch
        // that pays for it starts off.
        assert!(row(&inv, "spawn_subagent").runnable);
        assert_eq!(row(&inv, "spawn_subagent").decision, "blocked");
        // The repository's own two writes stay inside the checkout, so the sandbox bounds them the
        // way it bounds a file write.
        for id in ["git_stage", "git_branch"] {
            assert!(row(&inv, id).runnable, "{id}");
            assert_eq!(row(&inv, id).decision, "auto", "{id}");
        }
        // The commit asks: it rewrites history, and no dial but the user's own clears it.
        assert!(row(&inv, "git_commit").runnable);
        assert_eq!(row(&inv, "git_commit").decision, "ask");
        // The push and the pull request leave the machine, so the Privacy switch gates them first and
        // the page shows them refused while this machine stays local.
        for id in ["git_push", "github_pr"] {
            assert!(row(&inv, id).runnable, "{id}");
            assert_eq!(row(&inv, id).decision, "blocked", "{id}");
        }
        // And nothing on the page is a promise rather than a call: the registry the page prints is the
        // offer itself, so every row a user reads has a runner behind it.
        assert_eq!(inv.runnable, inv.tools.len(), "{:?}", inv.tools);
        assert!(row(&inv, "read_file").runnable);
    }

    /// The Subagents switch is the delegation's consent, and the page follows it the way the shell
    /// row follows the Terminal switch: on means the default dial lets it run, Ask still asks, and
    /// neither switch opens the other's tool.
    #[test]
    fn the_subagents_switch_is_what_moves_the_delegation_from_blocked_to_auto() {
        let settings = Settings {
            approvals: freecode_config::agent::Approvals {
                subagents_enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let inv = inventory(&settings);
        assert_eq!(row(&inv, "spawn_subagent").decision, "auto");
        assert_eq!(row(&inv, "terminal").decision, "blocked");
        let asking = Settings {
            approvals: freecode_config::agent::Approvals {
                mode: ApprovalMode::AskAlways,
                subagents_enabled: true,
                ..Default::default()
            },
            ..settings.clone()
        };
        assert_eq!(row(&inventory(&asking), "spawn_subagent").decision, "ask");
        let mut plan = settings;
        plan.approvals.mode = ApprovalMode::PlanOnly;
        assert_eq!(row(&inventory(&plan), "spawn_subagent").decision, "blocked");
    }

    /// Turning the Privacy switch on is the only consent a web call gets, so the page has to move with
    /// it. A row that stayed "blocked" after the switch would tell the user the control does nothing —
    /// and the Settings copy already promises it does.
    #[test]
    fn the_privacy_switch_is_what_moves_a_web_row_from_blocked_to_auto() {
        let mut settings = Settings::default();
        settings.privacy.allow_remote_providers = true;
        let inv = inventory(&settings);
        assert!(inv.network_allowed);
        assert_eq!(row(&inv, "web_search").decision, "auto");
        assert_eq!(row(&inv, "fetch_url").decision, "auto");
        // The two calls that reach GitHub open, but they do not run on the dial's own authority: a
        // push and a pull request are the two things this app must never do without being told to.
        for id in ["git_push", "github_pr"] {
            assert_eq!(row(&inv, id).decision, "ask", "{id}");
        }
        // Nothing else on the list changed: the switch opens the network class, not a shell.
        assert_eq!(row(&inv, "terminal").decision, "blocked");
    }

    /// The Terminal switch is the shell's consent the way the Privacy switch is the web calls': with
    /// it on, the default dial runs a command and the three runners built on the same consent; in Ask
    /// mode they still have to ask, which this build cannot show, so the page says the truth for each.
    #[test]
    fn the_terminal_switch_is_what_moves_the_shell_from_blocked_to_auto() {
        let settings = Settings {
            approvals: freecode_config::agent::Approvals {
                shell_enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let inv = inventory(&settings);
        for id in ["terminal", "run_tests", "run_build", "run_linter"] {
            assert_eq!(row(&inv, id).decision, "auto", "{id}");
        }
        // The repository reads were never behind it.
        assert_eq!(row(&inv, "git_status").decision, "auto");
        let asking = Settings {
            approvals: freecode_config::agent::Approvals {
                mode: ApprovalMode::AskAlways,
                shell_enabled: true,
                ..Default::default()
            },
            ..settings
        };
        for id in ["terminal", "run_tests", "run_build", "run_linter"] {
            assert_eq!(row(&inventory(&asking), id).decision, "ask", "{id}");
        }
    }

    /// A declared side effect under the default dial still asks, and the page says "ask" — which is
    /// the difference between an agent and a script, so it is not something to smooth over. A commit
    /// is that row: it has an executor now, and it still asks. The command rows are not it any more;
    /// they have executors and a switch, and the switch's off position is what keeps them out of a
    /// run.
    #[test]
    fn a_side_effecting_tool_under_auto_approve_still_has_to_ask() {
        let inv = inventory(&Settings::default());
        assert_eq!(row(&inv, "git_commit").decision, "ask");
        assert!(row(&inv, "git_commit").runnable);
    }

    /// Plan-only is the dial that turns the agent into a reader. The page has to move with it, or it
    /// is describing a permission the user took away in Settings.
    #[test]
    fn plan_only_blocks_a_mutating_tool_without_the_page_arguing_about_it() {
        let mut settings = Settings::default();
        settings.approvals.mode = ApprovalMode::PlanOnly;
        let inv = inventory(&settings);
        assert_eq!(inv.approval_mode, ApprovalMode::PlanOnly);
        assert_eq!(row(&inv, "write_file").decision, "blocked");
        assert_eq!(row(&inv, "read_file").decision, "auto");
    }

    /// A network tool with no remote provider configured is refused before it is offered, which is
    /// what keeps the "Local" badge a statement rather than an intention.
    #[test]
    fn a_network_tool_is_blocked_while_the_machine_stays_local() {
        let inv = inventory(&Settings::default());
        assert!(!inv.network_allowed);
        let web = inv
            .tools
            .iter()
            .find(|r| r.requires_network)
            .expect("a network-class tool");
        assert_eq!(web.decision, "blocked");
    }

    #[test]
    fn every_declared_tool_is_listed_once() {
        let inv = inventory(&Settings::default());
        assert_eq!(inv.tools.len(), registry().len());
        let mut ids: Vec<String> = inv.tools.iter().map(|r| r.id.clone()).collect();
        let before = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), before);
    }

    /// The page's TypeScript unions are written against these exact spellings, and the compiler on
    /// either side cannot see the other. Serde is the only source of them, so the test is what notices
    /// a rename — a renamed class would otherwise render as a missing label rather than an error.
    #[test]
    fn the_wire_spells_every_field_the_page_reads_it_by() {
        let value = serde_json::to_value(inventory(&Settings::default())).expect("serialisable");
        assert_eq!(value["approvalMode"], "auto_approve");
        assert_eq!(value["networkAllowed"], false);
        assert_eq!(value["workspaceRoot"], "/tmp");
        let classes: Vec<&str> = value["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["permission"].as_str().expect("a class"))
            .collect();
        for expected in ["read_only", "workspace_write", "side_effect", "network"] {
            assert!(
                classes.contains(&expected),
                "{expected} missing from {classes:?}"
            );
        }
        let first = &value["tools"][0];
        assert!(
            first.get("requiresNetwork").is_some() && first.get("runnable").is_some(),
            "{first}"
        );
    }
}
