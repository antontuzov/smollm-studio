//! Agent-side settings: the auto-approve policy behind the composer dropdown, privacy defaults,
//! context-compression thresholds and local-engine preferences.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How much side effect a tool call may cause. The sandbox engine keys off this, and the UI renders
/// every call as a transcript step regardless of class — nothing runs silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionClass {
    /// Reads that touch nothing: fs.read, fs.glob, fs.search, index.query.
    ReadOnly,
    /// Writes confined to the workspace: fs.write, fs.edit, diff.stage, artifact.emit.
    WorkspaceWrite,
    /// Anything that leaves the machine or cannot be undone: shell.run, web.fetch, git.commit,
    /// worktree.*, ext.invoke.
    SideEffect,
    /// Network egress beyond the local engine.
    Network,
}

/// The calls that run the project's own code: the user's shell, and the three runners built on the
/// same consent. Tests, builds and scripts all execute code the workspace already carries, under this
/// user's permissions, so the Terminal switch is their approval — not the dial, which cannot price
/// what a `package.json` script actually does. While the switch is off none of them is offered.
pub fn shell_governed(tool_id: &str) -> bool {
    matches!(
        tool_id,
        "terminal" | "run_tests" | "run_build" | "run_linter"
    )
}

/// Composer dropdown: ▷ Auto approve ▾.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Default: auto for reads, workspace edits and whatever the Privacy switch has already agreed
    /// to; ask for destructive calls.
    #[default]
    AutoApprove,
    /// Everything the other modes settle on their own is put to the window instead: the run waits on
    /// the prompt until the user answers it or its time is up. Reads are the exception — opening a file
    /// changes nothing, and a prompt in front of every one of them asks the user to approve a read.
    AskAlways,
    /// The agent may plan and read, but no mutating tool executes.
    PlanOnly,
    /// Per-tool overrides in `tool_overrides`.
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Approvals {
    pub mode: ApprovalMode,
    /// Tool id -> explicitly allowed or blocked, used by `ApprovalMode::Custom`.
    pub tool_overrides: BTreeMap<String, bool>,
    /// Per-command allow/deny list for shell.run.
    pub shell_allow: Vec<String>,
    pub shell_deny: Vec<String>,
    /// Off by default: a command runs with this user's permissions, so the switch is the consent the
    /// approval prompt would otherwise ask for. It covers the project runners — tests, build, linter —
    /// because those execute the same code by another name. Nothing in the loop offers any of them
    /// while it is off.
    pub shell_enabled: bool,
    pub shell_timeout_ms: u64,
    /// Off by default: a subagent is a second model run against the same weights, and the main run
    /// pays a tool result for its answer. The switch is the consent, the way Terminal access is, and
    /// nothing in the loop offers `spawn_subagent` while it is off.
    pub subagents_enabled: bool,
}

impl Default for Approvals {
    fn default() -> Self {
        Self {
            mode: ApprovalMode::AutoApprove,
            tool_overrides: BTreeMap::new(),
            shell_allow: Vec::new(),
            shell_deny: Vec::new(),
            shell_enabled: false,
            shell_timeout_ms: Self::default_timeout_ms(),
            subagents_enabled: false,
        }
    }
}

impl Approvals {
    pub fn default_timeout_ms() -> u64 {
        120_000
    }

    /// Decide one call without asking the user. Returns `None` when a prompt is required.
    ///
    /// A network call is the one class with a switch in front of it as well as a dial behind it. The
    /// Privacy switch decides whether anything may leave the machine at all, and the approval engine in
    /// `freecode-core` refuses the call before it reaches this function unless that switch is on. What
    /// is left for the dial is the question the switch cannot answer: not whether the run may reach the
    /// internet, but whether this page is what it is going to read. Auto says yes to each lookup, Ask
    /// stops at each one, and off means the tool was never offered.
    pub fn decide(&self, tool_id: &str, class: PermissionClass) -> Option<bool> {
        if let Some(explicit) = self.tool_overrides.get(tool_id) {
            return Some(*explicit);
        }
        // The shell and the project runners are the side effects a switch speaks for: the switch is what
        // lets them run at all, and under Auto or Custom it is also what settles a command without a
        // prompt. Ask mode still stops at each line — the switch says this run may have a shell, not
        // that any command in it has been read. They only reach here while the sandbox's own
        // `shell_allowed` is on; the engine refuses a switched-off shell before the mode is consulted.
        if shell_governed(tool_id) && self.shell_enabled && self.mode != ApprovalMode::AskAlways {
            return Some(matches!(
                self.mode,
                ApprovalMode::AutoApprove | ApprovalMode::Custom
            ));
        }
        // The same bargain for delegation, which is a second model run rather than a change to the
        // folder. Plan-only still refuses it above, because `evaluate` treats a second model run as a
        // side effect rather than as a read.
        if tool_id == "spawn_subagent"
            && self.subagents_enabled
            && self.mode != ApprovalMode::AskAlways
        {
            return Some(matches!(
                self.mode,
                ApprovalMode::AutoApprove | ApprovalMode::Custom
            ));
        }
        match self.mode {
            // Ask means "stop before anything changes". A read changes nothing, so it runs as it does
            // under Auto: a dial that stopped at every file a run opened would be a wall of prompts in
            // front of the one decision the user meant to make.
            ApprovalMode::AskAlways => matches!(class, PermissionClass::ReadOnly).then_some(true),
            ApprovalMode::PlanOnly => Some(matches!(class, PermissionClass::ReadOnly)),
            ApprovalMode::AutoApprove | ApprovalMode::Custom => match class {
                PermissionClass::ReadOnly
                | PermissionClass::WorkspaceWrite
                | PermissionClass::Network => Some(true),
                PermissionClass::SideEffect => None,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Privacy {
    /// OFF unless the user turns it on: false is derivable here, and a test asserts it stays false.
    pub telemetry: bool,
    pub allow_remote_providers: bool,
}

/// Which side of the wire answers the next prompt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelSource {
    /// The supervised fork server on this machine.
    #[default]
    Local,
    /// The OpenAI-compatible endpoint named below, with the key held in the OS keychain.
    Remote,
}

/// Where a remote prompt goes, and how much room it has there.
///
/// The key is deliberately absent: it belongs in the platform keychain, never in a document this app
/// saves to disk and hands across the IPC boundary. `base_url` and `model` are kept even while the
/// source is Local so switching back to Remote does not lose what was typed; nothing is sent while
/// `source` is Local or [`Privacy::allow_remote_providers`] is off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RemoteProvider {
    pub source: ModelSource,
    /// An OpenAI-compatible base URL, e.g. `https://api.openai.com/v1`. Plain HTTP is only accepted
    /// for a loopback host, where there is no network to protect.
    pub base_url: String,
    /// The model id the provider expects, e.g. `gpt-4o-mini`.
    pub model: String,
    /// The provider's documented context window. It drives the context meter and the answer budget;
    /// the app cannot ask an arbitrary provider for this the way it asks its own server.
    pub context_tokens: u32,
}

impl Default for RemoteProvider {
    fn default() -> Self {
        Self {
            source: ModelSource::Local,
            base_url: String::new(),
            model: String::new(),
            context_tokens: 32_768,
        }
    }
}

impl RemoteProvider {
    /// Both halves needed before a request can be addressed to a provider.
    pub fn configured(&self) -> bool {
        !self.base_url.trim().is_empty() && !self.model.trim().is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Context {
    /// Fraction of the model's 262144-token window at which older turns are summarized and the
    /// "Context compressed" divider is emitted.
    pub compress_at_ratio: f32,
    /// Recent turns, plus every open file handle, are kept verbatim.
    pub keep_recent_turns: usize,
}

impl Context {
    pub fn compress_threshold(&self, window: u32) -> u32 {
        let ratio = self.compress_at_ratio.clamp(0.1, 0.95) as f64;
        (f64::from(window) * ratio).round() as u32
    }
}

/// Which local pack to load, and where the weights live.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PackingChoice {
    /// Detect Metal / CUDA-Ada / CUDA-Hopper-Blackwell / Vulkan / CPU and pick accordingly.
    #[default]
    Auto,
    Forced(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LocalModel {
    pub catalog_id: String,
    pub packing: PackingChoice,
    pub model_dir: Option<String>,
    /// Name of an environment variable holding a read-only HF token, for gated repos. We store the
    /// variable name, never the secret.
    pub hf_token_env: Option<String>,
    pub offload_all_layers: bool,
    pub flash_attention: bool,
    pub vision_lazy_load: bool,
    /// Single-user server tuning: `-np 1 --cache-ram 24576` keeps the prompt cache from thrashing
    /// across slots (Bonsai-demo#183).
    pub dedicated_server_slots: bool,
}

impl Default for LocalModel {
    fn default() -> Self {
        Self {
            catalog_id: crate::DEFAULT_MODEL_ID.into(),
            packing: PackingChoice::Auto,
            model_dir: None,
            hf_token_env: None,
            offload_all_layers: true,
            flash_attention: true,
            vision_lazy_load: true,
            dedicated_server_slots: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_approve_allows_reads_edits_and_a_switched_on_network_but_asks_for_side_effects() {
        let a = Approvals::default();
        assert_eq!(a.decide("fs.read", PermissionClass::ReadOnly), Some(true));
        assert_eq!(
            a.decide("fs.edit", PermissionClass::WorkspaceWrite),
            Some(true)
        );
        assert_eq!(a.decide("shell.run", PermissionClass::SideEffect), None);
        // The Privacy switch is this class's consent, and the approval engine will not reach this
        // function for a network tool with the switch off. A prompt here would be one nothing answers.
        assert_eq!(a.decide("web_search", PermissionClass::Network), Some(true));
        assert_eq!(
            Approvals {
                mode: ApprovalMode::AskAlways,
                ..Default::default()
            }
            .decide("web_search", PermissionClass::Network),
            None,
            "Ask still means ask, even for a class the switch allowed"
        );
    }

    #[test]
    fn plan_only_blocks_mutations_and_custom_override_wins() {
        let mut a = Approvals {
            mode: ApprovalMode::PlanOnly,
            ..Default::default()
        };
        assert_eq!(
            a.decide("fs.edit", PermissionClass::WorkspaceWrite),
            Some(false)
        );
        a.mode = ApprovalMode::Custom;
        a.tool_overrides.insert("shell.run".into(), true);
        assert_eq!(
            a.decide("shell.run", PermissionClass::SideEffect),
            Some(true)
        );
    }

    #[test]
    fn the_terminal_is_off_until_its_switch_is_on_and_never_auto_runs_in_ask_mode() {
        let off = Approvals::default();
        assert!(!off.shell_enabled);
        assert_eq!(
            off.decide("terminal", PermissionClass::SideEffect),
            None,
            "a switched-off shell is not approved by the mode"
        );
        let on = Approvals {
            shell_enabled: true,
            ..Default::default()
        };
        assert_eq!(
            on.decide("terminal", PermissionClass::SideEffect),
            Some(true)
        );
        // The project runners execute the same code by another name, so one switch answers for all of
        // them — and a call whose switch is off is never approved by the dial.
        for id in ["run_tests", "run_build", "run_linter"] {
            assert!(shell_governed(id), "{id} runs project code");
            assert_eq!(off.decide(id, PermissionClass::SideEffect), None, "{id}");
            assert_eq!(
                on.decide(id, PermissionClass::SideEffect),
                Some(true),
                "{id}"
            );
        }
        // A read of the repository is not project code and is not behind the switch.
        assert!(!shell_governed("git_status"));
        assert_eq!(
            off.decide("git_status", PermissionClass::ReadOnly),
            Some(true)
        );
        assert_eq!(
            Approvals {
                shell_enabled: true,
                mode: ApprovalMode::AskAlways,
                ..Default::default()
            }
            .decide("terminal", PermissionClass::SideEffect),
            None,
            "Ask still means ask, even with the switch on"
        );
        // An explicit override outranks the switch in both directions.
        let mut blocked = on.clone();
        blocked.tool_overrides.insert("terminal".into(), false);
        assert_eq!(
            blocked.decide("terminal", PermissionClass::SideEffect),
            Some(false)
        );
    }

    #[test]
    fn delegation_is_off_until_its_switch_is_on_and_never_auto_runs_in_ask_mode() {
        let off = Approvals::default();
        assert!(!off.subagents_enabled);
        assert_eq!(
            off.decide("spawn_subagent", PermissionClass::SideEffect),
            None,
            "a switched-off subagent is not approved by the mode"
        );
        let on = Approvals {
            subagents_enabled: true,
            ..Default::default()
        };
        assert_eq!(
            on.decide("spawn_subagent", PermissionClass::SideEffect),
            Some(true)
        );
        assert_eq!(
            Approvals {
                subagents_enabled: true,
                mode: ApprovalMode::AskAlways,
                ..Default::default()
            }
            .decide("spawn_subagent", PermissionClass::SideEffect),
            None,
            "Ask still means ask, even with the switch on"
        );
        // The switch is about delegation, not about the shell beside it.
        assert_eq!(on.decide("terminal", PermissionClass::SideEffect), None);
    }

    #[test]
    fn telemetry_is_off_by_default() {
        assert!(!Privacy::default().telemetry);
        assert!(!Privacy::default().allow_remote_providers);
    }

    #[test]
    fn the_remote_provider_starts_local_unaddressed_and_with_the_key_absent() {
        let p = RemoteProvider::default();
        assert_eq!(p.source, ModelSource::Local);
        assert!(!p.configured());
        assert_eq!(p.context_tokens, 32_768);
        let configured = RemoteProvider {
            source: ModelSource::Remote,
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
            ..RemoteProvider::default()
        };
        assert!(configured.configured());
        assert!(!RemoteProvider {
            model: "  ".into(),
            ..configured
        }
        .configured());
    }

    #[test]
    fn compression_threshold_tracks_the_262k_window() {
        let c = Context {
            compress_at_ratio: 0.75,
            keep_recent_turns: 8,
        };
        assert_eq!(c.compress_threshold(262_144), 196_608);
        assert_eq!(
            Context {
                compress_at_ratio: 9.0,
                ..c
            }
            .compress_threshold(1_000),
            950
        );
    }
}
