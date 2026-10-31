//! The event stream between the agent loop and the window.
//!
//! Everything the transcript, the status loader line, the "+N −M" pill and the "Local" badge render
//! from arrives as one of these, in order, across the IPC boundary — which is why no step may run
//! silently: a side effect that produced no event would be invisible in the UI.
//!
//! Reasoning and answer text are separate deltas on purpose. The fork re-renders earlier reasoning
//! into later prompts and the prompt cache then misses on every turn of a tool loop
//! (Bonsai-demo#183), so `ThoughtDelta` feeds the collapsed block and only `AnswerDelta` is replayed
//! into history.

use std::collections::BTreeMap;

use freecode_config::agent::PermissionClass;
use freecode_llama::load_guard::{Blocker, BlockerCode, CtaCode};
use freecode_models::EngineLock;
use freecode_tools::Summary;
use serde::{Deserialize, Serialize};

use crate::transcript::{DiffStat, TranscriptStep};

/// A load-guard refusal rendered in the transcript: the code the window looks its two sentences up
/// by, plus the one action that clears it. The acceptance criteria name this path — a stock-llama.cpp
/// attempt must show a clear error with an install-fork CTA, not a crash log.
///
/// The headline and the instruction belong to the locale catalogs, which is why they are not fields
/// here: the same refusal has to read in the language the interface is in, and a host that carried
/// its own English would leave a copy of each sentence that no user in the other language ever sees.
/// What the host does carry is `technical`, because a path, a pin or a metadata key is a measurement
/// rather than copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedInfo {
    pub code: BlockerCode,
    /// The values of the fields [`BlockerCode::params`] names, for the sentences to interpolate.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// Paths, key names, pins and upstream issue numbers behind the refusal, kept for the disclosure
    /// a user opens when the sentence above was not enough to act on.
    pub technical: String,
    pub cta_code: CtaCode,
    pub cta_url: String,
}

impl BlockedInfo {
    /// `model_repo` is the Hugging Face repo the weights came from; the blockers about the file on
    /// disk point there, while the ones about the engine point at the GitHub fork in `lock`.
    pub fn from_blocker(blocker: &Blocker, lock: &EngineLock, model_repo: &str) -> Self {
        let cta = blocker.cta(lock, model_repo);
        let code = blocker.code();
        let params = blocker.params();
        let names: Vec<&str> = params.iter().map(|(name, _)| *name).collect();
        let sent = code.params();
        assert!(
            sent.len() == names.len() && sent.iter().all(|name| names.contains(name)),
            "{:?} must carry exactly {:?}, got {names:?}",
            code,
            sent
        );
        Self {
            code,
            params: params
                .into_iter()
                .map(|(name, value)| (name.to_string(), value))
                .collect(),
            technical: blocker.technical(),
            cta_code: cta.code,
            cta_url: cta.url,
        }
    }
}

/// What a tool call produced, in the shape its transcript row shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutcome {
    pub ok: bool,
    /// One-line result text under the row label.
    pub summary: String,
    /// Which catalog sentence [`Self::summary`] is, and the fields it names. `None` for a line that is
    /// data rather than wording — a page's title — and for a refusal, whose sentence the approval layer
    /// has already written.
    ///
    /// The English stays because the row is not the only reader: a run's rows are what the filed
    /// conversation replays, and a window in another language should still show what was measured.
    #[serde(default)]
    pub summary_key: Option<String>,
    #[serde(default)]
    pub summary_params: serde_json::Value,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub diff: Option<DiffStat>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
}

impl ToolOutcome {
    pub fn ok(summary: Summary) -> Self {
        Self {
            ok: true,
            summary_key: summary.key.map(|key| key.i18n_key().to_string()),
            summary_params: summary.params,
            summary: summary.text,
            exit_code: None,
            diff: None,
            artifacts: Vec::new(),
        }
    }

    pub fn blocked(reason: &str) -> Self {
        Self {
            ok: false,
            summary: reason.to_string(),
            summary_key: None,
            summary_params: serde_json::Value::Null,
            exit_code: None,
            diff: None,
            artifacts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRef {
    pub kind: String,
    pub title: String,
    /// Workspace-relative, never absolute: paths shown in the UI must not leak the home directory.
    pub path: Option<String>,
}

/// What a run worked around rather than refused. The code names one sentence in the locale catalogs:
/// the host contributes the numbers it measured, the catalog owns the wording, and a Chinese
/// interface then reads its own notice instead of an English sentence glued into a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoticeCode {
    /// The saved reasoning effort is not among the selected model's accepted ones.
    EffortNotSupported,
    /// The saved reasoning effort went nowhere because this route sends no effort field at all.
    ///
    /// A remote provider and the FreeCode service each answer without one, and
    /// none of them falls back to the local engine. Said rather than left to the dial's own silence,
    /// because the user's choice is a promise the run could not keep.
    EffortNotSent,
    /// The supervised server died between runs and is being started again.
    EngineRestarted,
    /// The model emitted a tool call in a chat that offers it no tools.
    ToolCallWithoutTools,
    /// The model asked for a call this build does not have, while tools were offered. A model inventing
    /// a name is what reaches this now: the registry holds no entry without a runner behind it.
    ToolNotOffered,
    /// The tool loop gave up before the answer came, because the run kept calling tools.
    ToolLoopCapped,
    /// The prompt was already too big for the window when the run was measured.
    PromptTooLong,
    /// The server could not replay the whole conversation and dropped its earliest turns.
    ContextTrimmed,
    /// The reply hit its token cap, so it ends mid-sentence.
    BudgetExhausted,
    /// The exchange happened but did not reach the conversation store, so the sidebar will not show it
    /// after the window closes.
    NotFiled,
    /// The model has no projector file on this machine, so the picture it would have read cannot be.
    ProjectorAbsent,
    /// The server the conversation is running on was started without a projector, and taking the
    /// picture means starting it again with one.
    ProjectorLoading,
    /// The selected model publishes no projector at all, so it cannot be given a picture.
    ModelTakesNoImages,
    /// A free turn was refused before a token landed and an older build moved it to the next model in
    /// its list, which is why the answer that followed was not the one the picker named.
    ///
    /// The gateway this announced is gone, and nothing emits this code now. The variant stays
    /// because a conversation filed while it did fire has to read back the way it was seen, and the
    /// transcript has no other way to say that a different model answered it.
    FreeModelFallback,
    /// The prompt asked for the agent loop and the route that answers it carries none of this app's
    /// tools, so the run's work happens out of this window's sight.
    ///
    /// The OpenCode service named this once, and was wrong to: it writes into its own working
    /// directory, which Review could not show and the undo journal could not reach. It now runs its
    /// session inside the project folder and asks before every write, so the change lands in this
    /// window's rows, diff and Review panel — and nothing emits this notice. The variant stays because
    /// a conversation filed while it did fire has to read back the way it was seen.
    RouteHasNoTools,
    /// A run changed files and the host would have run the project's check, but Terminal access is
    /// off, so the change went out unverified.
    ChecksNotRun,
    /// A run changed files and the workspace declares no test, build or lint command to run.
    ChecksUnavailable,
    /// The check the host ran after a run's edits ended with a failing status.
    ChecksFailed,
    /// The run carries instructions the user wrote in a file, not in this conversation.
    ///
    /// The system turn is where an answer's habits come from, and a folder's `AGENTS.md` now goes into
    /// it, so the transcript names the files rather than letting a borrowed instruction read as the
    /// app's own.
    InstructionsRead,
    /// One of those files is longer than the room the instruction files share, so the run saw part of
    /// it. Said because a project that wrote down a rule the run never received would otherwise be
    /// answered as if the rule had reached it.
    InstructionsCut,
}

impl NoticeCode {
    /// Every code the host can emit. `verify-transcript-i18n` walks this to require one catalog entry
    /// per code, so a notice added here without its sentence in both locales fails the build instead
    /// of rendering its own key in the middle of a transcript.
    pub const ALL: [Self; 20] = [
        Self::EffortNotSupported,
        Self::EffortNotSent,
        Self::EngineRestarted,
        Self::ToolCallWithoutTools,
        Self::ToolNotOffered,
        Self::ToolLoopCapped,
        Self::PromptTooLong,
        Self::ContextTrimmed,
        Self::BudgetExhausted,
        Self::NotFiled,
        Self::ProjectorAbsent,
        Self::ProjectorLoading,
        Self::ModelTakesNoImages,
        Self::FreeModelFallback,
        Self::RouteHasNoTools,
        Self::ChecksNotRun,
        Self::ChecksUnavailable,
        Self::ChecksFailed,
        Self::InstructionsRead,
        Self::InstructionsCut,
    ];

    pub fn i18n_key(self) -> &'static str {
        match self {
            Self::EffortNotSupported => "notice.effort-not-supported",
            Self::EffortNotSent => "notice.effort-not-sent",
            Self::EngineRestarted => "notice.engine-restarted",
            Self::ToolCallWithoutTools => "notice.tool-call-without-tools",
            Self::ToolNotOffered => "notice.tool-not-offered",
            Self::ToolLoopCapped => "notice.tool-loop-capped",
            Self::PromptTooLong => "notice.prompt-too-long",
            Self::ContextTrimmed => "notice.context-trimmed",
            Self::BudgetExhausted => "notice.budget-exhausted",
            Self::NotFiled => "notice.not-filed",
            Self::ProjectorAbsent => "notice.projector-absent",
            Self::ProjectorLoading => "notice.projector-loading",
            Self::ModelTakesNoImages => "notice.model-takes-no-images",
            Self::FreeModelFallback => "notice.free-model-fallback",
            Self::RouteHasNoTools => "notice.route-has-no-tools",
            Self::ChecksNotRun => "notice.checks-not-run",
            Self::ChecksUnavailable => "notice.checks-unavailable",
            Self::ChecksFailed => "notice.checks-failed",
            Self::InstructionsRead => "notice.instructions-read",
            Self::InstructionsCut => "notice.instructions-cut",
        }
    }

    /// The fields the sentence interpolates. Declared here because both halves of a notice have to
    /// agree on them: the gate reads each locale's `{{braces}}` back against this list, and
    /// [`AgentEvent::notice`] names the call that sent another set. A number that went out under a
    /// different name leaves a blank in the middle of the sentence the user reads.
    pub fn params(self) -> &'static [&'static str] {
        match self {
            Self::EffortNotSupported => &["model", "accepted", "chosen", "sent"],
            Self::EffortNotSent => &["model", "chosen"],
            Self::EngineRestarted => &["attempt", "max", "log"],
            Self::ToolCallWithoutTools => &["name"],
            Self::ToolNotOffered => &["name"],
            Self::ToolLoopCapped => &["rounds"],
            Self::PromptTooLong => &["tokens", "window"],
            Self::ContextTrimmed => &[],
            Self::BudgetExhausted => &["maxTokens"],
            Self::NotFiled => &["why"],
            Self::ProjectorAbsent => &["model", "file"],
            Self::ProjectorLoading => &["file"],
            Self::ModelTakesNoImages => &["model"],
            Self::FreeModelFallback => &["from", "to"],
            Self::RouteHasNoTools => &["model"],
            Self::ChecksNotRun => &[],
            Self::ChecksUnavailable => &[],
            Self::ChecksFailed => &["code"],
            Self::InstructionsRead => &["files", "chars"],
            Self::InstructionsCut => &["files", "chars"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum AgentEvent {
    /// A run began. `local` drives the "Local" badge and is false only when a remote provider is
    /// actually configured.
    RunStarted {
        run_id: String,
        model_label: String,
        local: bool,
        /// How many tool rounds this run may spend before the host stops asking — the denominator of
        /// the composer's step pill, and `0` for a run that was never offered a tool. It rides the
        /// announcement rather than a constant the interface copies, because the cap belongs to the
        /// loop: a window that guessed it would keep guessing after the host changed its budget.
        max_rounds: usize,
        /// Whether the run may only read and propose. The approval dial's Plan-only mode puts the run
        /// on the read-only offer and the planning contract, and this is what lets the transcript say
        /// the answer is a plan rather than a change it decided not to make.
        plan: bool,
    },
    /// Collapsed reasoning text, streamed.
    ThoughtDelta {
        text: String,
    },
    /// Answer text, streamed.
    AnswerDelta {
        text: String,
    },
    /// A tool row appeared in the `Running… Ns` state.
    ToolStarted {
        step: TranscriptStep,
    },
    /// A tool row finished.
    ToolFinished {
        step: TranscriptStep,
        outcome: ToolOutcome,
    },
    /// The "Ran N tools" batch header, emitted when the batch closes.
    ToolBatchClosed {
        step: TranscriptStep,
    },
    /// Output a running command just wrote, for the window's terminal panel.
    ///
    /// It is not a row: the command's row opened with `ToolStarted` and closes with `ToolFinished`,
    /// and this is what the command printed in between, in the order its pipes were read. `step_id`
    /// names that row, so the panel appends to the run the id belongs to rather than to whichever tab
    /// the reader happens to be looking at.
    TerminalOutput {
        step_id: String,
        /// Already decoded into text. A character split across two pipe reads is held back until the
        /// read that completes it, so a UTF-8 character is never sent as a replacement mark.
        chunk: String,
    },
    /// Approval required before the call may run: the prompt the window answers, and the wait this run
    /// is parked on until it does.
    ///
    /// `call_id` is what the window's answer names, so a batch that has two calls waiting at once can
    /// tell them apart. `timeout_ms` is the host's own patience, sent rather than copied by the window
    /// so the countdown the user reads is the one that actually decides.
    ApprovalRequested {
        call_id: String,
        tool_id: String,
        headline: String,
        permission: PermissionClass,
        timeout_ms: u64,
        /// The exact command, when this is a call that runs one: the headline says what the run is
        /// about to do in the interface's language, and this is the argv behind it, unedited.
        #[serde(default)]
        command: Option<String>,
    },
    /// The answer a prompt got, from the user or from its time running out. It closes the card the
    /// window is showing, which is what makes a denied call and an unanswered one read differently.
    ApprovalResolved {
        call_id: String,
        decision: crate::approvals::ApprovalDecision,
    },
    /// Staged edits, so the pill updates during the run rather than only at the end.
    DiffStaged {
        diff: DiffStat,
    },
    /// The "Context compressed" divider.
    ContextCompressed {
        before_tokens: u32,
        after_tokens: u32,
        summary: String,
    },
    /// A sandbox refusal, surfaced as its own row with the reason.
    SandboxBlocked {
        tool_id: String,
        code: String,
        reason: String,
    },
    /// The load-guard refused to launch: stock engine, missing pin, or a file that is not a Prism
    /// export.
    EngineBlocked {
        info: BlockedInfo,
    },
    /// Something the run adapted to, surfaced as a quiet line beside the answer it did not stop.
    /// The sentence itself is the locale catalog's, keyed by `code` and filled from `params` — the
    /// host contributes what it measured, never the wording, so a translated interface reads its own
    /// notice instead of an English one glued into a row.
    Warning {
        code: NoticeCode,
        /// The fields the catalog's sentence interpolates, by the names [`NoticeCode::params`] lists.
        #[serde(default)]
        params: BTreeMap<String, String>,
    },
    /// The run ended. `stopped` distinguishes the stop button from a natural finish.
    RunFinished {
        run_id: String,
        stopped: bool,
        /// Percent of the server's context window the finished run left filled, counted by the server
        /// itself. `None` when the stream never reported its usage: the meter shows unknown rather than
        /// a number nobody measured.
        context_used: Option<f32>,
        total_diff: Option<DiffStat>,
        /// What the run cost. Always there, because the host timed the run itself: a stream the user
        /// stopped has no rates from the engine, and it still has the twelve seconds they waited.
        run: RunReport,
    },
    Failed {
        message: String,
    },
}

impl AgentEvent {
    /// One notice: the code the window looks its sentence up by, and the values of the fields
    /// [`NoticeCode::params`] names. The wording belongs to the catalog alone — a host that carried
    /// its own sentence too would leave two copies of it to drift apart, one of which the user never
    /// sees in their language.
    ///
    /// Panics in a debug build when the names sent are not the names declared: the alternative is a
    /// notice with a blank where its number should be, which nobody notices until it is on screen.
    pub fn notice(code: NoticeCode, params: &[(&str, &str)]) -> Self {
        let sent = code.params();
        let names: Vec<&str> = params.iter().map(|(name, _)| *name).collect();
        assert!(
            sent.len() == names.len()
                && sent.iter().all(|name| names.contains(name))
                && names.iter().all(|name| sent.contains(name)),
            "{:?} must carry exactly {:?}, got {names:?}",
            code,
            sent
        );
        Self::Warning {
            code,
            params: params
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        }
    }
}

/// What one finished run cost.
///
/// Only `elapsed_ms` is certain, because only the host's wall clock is always there: it is the wait
/// the user sat through, round trip and every delta over IPC included, and it stands whether or not
/// the engine ever answered. The other five are read out of the stream's own usage and timing frames,
/// and a frame the fork never sends takes its number with it — an interrupted stream ends before the
/// final one, so a run the user stopped has no rates to show. `None` is the honest place holder there:
/// a rate divided out of token counts nobody timed is a number that gets believed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub generated_tokens: Option<u64>,
    pub prompt_tokens: Option<u64>,
    /// Prompt tokens the server took from its prefix cache instead of re-evaluating. The replayed
    /// history across a conversation is what makes this number grow, and it is the only visible
    /// proof that the design works.
    pub cached_tokens: Option<u64>,
    pub generated_per_second: Option<f64>,
    pub prompt_per_second: Option<f64>,
    pub elapsed_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{StepKind, TranscriptStep};
    use freecode_tools::sandbox::SandboxViolation;
    use serde_json::json;
    use std::path::Path;

    fn lock() -> EngineLock {
        EngineLock::load_default(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
    }

    #[test]
    fn events_are_self_describing_on_the_wire() {
        let e = AgentEvent::RunStarted {
            run_id: "r1".into(),
            model_label: "Ternary Bonsai 2 27B (PQ2_0)".into(),
            local: true,
            max_rounds: 4,
            plan: true,
        };
        let raw = serde_json::to_value(&e).unwrap();
        assert_eq!(raw["type"], json!("runStarted"), "camelCase discriminator");
        assert_eq!(raw["modelLabel"], json!("Ternary Bonsai 2 27B (PQ2_0)"));
        assert_eq!(raw["local"], json!(true));
        assert_eq!(raw["maxRounds"], json!(4));
        assert_eq!(raw["plan"], json!(true));
    }

    #[test]
    fn reasoning_and_answer_deltas_are_distinct_events() {
        let t = serde_json::to_value(AgentEvent::ThoughtDelta {
            text: "checking".into(),
        })
        .unwrap();
        let a = serde_json::to_value(AgentEvent::AnswerDelta {
            text: "done".into(),
        })
        .unwrap();
        assert_eq!(t["type"], json!("thoughtDelta"));
        assert_eq!(a["type"], json!("answerDelta"));
    }

    #[test]
    fn a_running_commands_output_names_the_row_it_belongs_to() {
        let raw = serde_json::to_value(AgentEvent::TerminalOutput {
            step_id: "s1".into(),
            chunk: "compiling\n".into(),
        })
        .unwrap();
        assert_eq!(raw["type"], json!("terminalOutput"));
        assert_eq!(raw["stepId"], json!("s1"));
        assert_eq!(raw["chunk"], json!("compiling\n"));
    }

    #[test]
    fn a_notice_sends_its_code_and_the_fields_its_sentence_names() {
        let e = AgentEvent::notice(
            NoticeCode::PromptTooLong,
            &[("tokens", "5107"), ("window", "4096")],
        );
        let raw = serde_json::to_value(&e).unwrap();
        assert_eq!(raw["type"], json!("warning"));
        // The kebab spelling is what the catalog key ends with, so the wire and `i18n_key` cannot be
        // renamed apart without the gate in `verify-transcript-i18n` seeing it.
        assert_eq!(raw["code"], json!("prompt-too-long"));
        assert_eq!(raw["params"]["tokens"], json!("5107"));
        assert_eq!(raw["params"]["window"], json!("4096"));
    }

    #[test]
    fn a_notice_that_sends_a_field_by_another_name_is_refused() {
        // The window's copy interpolates `{{window}}`; a call that sent `width` instead would leave a
        // blank in the middle of the line the user reads, so the debug build says so at the call.
        let r = std::panic::catch_unwind(|| {
            AgentEvent::notice(
                NoticeCode::PromptTooLong,
                &[("tokens", "5107"), ("width", "4096")],
            )
        });
        assert!(
            r.is_err(),
            "a mismatched field name must be caught, not rendered"
        );
    }

    #[test]
    fn a_blocked_launch_carries_the_action_that_fixes_it() {
        let blocker = Blocker::StockEngineRejected {
            path: Path::new("/opt/homebrew/bin/llama-server").to_path_buf(),
        };
        let info =
            BlockedInfo::from_blocker(&blocker, &lock(), "prism-ml/Ternary-Bonsai-2-27B-gguf");
        assert_eq!(info.code, BlockerCode::StockEngineRejected);
        assert_eq!(info.cta_code, CtaCode::InstallEngine);
        assert!(info
            .cta_url
            .starts_with("https://github.com/PrismML-Eng/llama.cpp/releases/"),);
        // The evidence crosses; the sentences do not. What the user reads is the catalog's, so a
        // Chinese interface renders its own headline rather than this English one beside it.
        assert!(info.technical.contains("no automatic fallback"));
        let wire = serde_json::to_value(&info).unwrap();
        assert_eq!(wire["ctaUrl"], json!(info.cta_url));
        assert_eq!(wire["code"], json!("stock-engine-rejected"));
        assert!(wire.get("title").is_none() && wire.get("detail").is_none());
    }

    /// The two refusals about one particular file are the only ones whose sentences name it, and the
    /// catalog can only fill in what the host measured.
    #[test]
    fn a_refusal_about_one_file_sends_which_file() {
        let info = BlockedInfo::from_blocker(
            &Blocker::IntegrityFailed {
                file: "Ternary-Bonsai-2-27B-PQ2_0.gguf".into(),
                status: "size 463290464 != 463290465".into(),
            },
            &lock(),
            "prism-ml/Ternary-Bonsai-2-27B-gguf",
        );
        assert_eq!(info.code, BlockerCode::IntegrityMismatch);
        assert_eq!(info.params["file"], "Ternary-Bonsai-2-27B-PQ2_0.gguf");
        assert_eq!(info.cta_code, CtaCode::RedownloadFile);

        // The same variant, and a different sentence: a file that never arrived is a task, not a
        // corrupted one.
        let absent = BlockedInfo::from_blocker(
            &Blocker::IntegrityFailed {
                file: "Ternary-Bonsai-2-27B-PQ2_0.gguf".into(),
                status: "not downloaded".into(),
            },
            &lock(),
            "prism-ml/Ternary-Bonsai-2-27B-gguf",
        );
        assert_eq!(absent.code, BlockerCode::WeightsNotDownloaded);
        assert_eq!(absent.cta_code, CtaCode::RedownloadFile);
        assert!(absent
            .cta_url
            .ends_with("/resolve/main/Ternary-Bonsai-2-27B-PQ2_0.gguf"));
    }

    #[test]
    fn tool_rows_round_trip_with_their_step_and_outcome() {
        let step = TranscriptStep::running_tool(
            "edit_file",
            StepKind::EditFile,
            json!({ "path": "src/app.rs" }),
        );
        let outcome = ToolOutcome {
            diff: Some(DiffStat {
                lines_added: 12,
                lines_removed: 4,
                files_changed: 1,
            }),
            ..ToolOutcome::ok(Summary::said(
                freecode_tools::SummaryKey::CopyReplaced,
                json!({}),
                "1 copy replaced",
            ))
        };
        let event = AgentEvent::ToolFinished {
            step: step.clone(),
            outcome,
        };
        let raw = serde_json::to_string(&event).unwrap();
        let back: AgentEvent = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, event);
        assert!(raw.contains("filesChanged"), "camelCase on the wire: {raw}");
        assert!(!raw.contains("files_changed"), "snake_case leaked: {raw}");
    }

    #[test]
    fn a_sandbox_refusal_serializes_its_own_reason() {
        let violation = SandboxViolation::OutsideWorkspace {
            path: "/etc/hosts".into(),
            root: "/work".into(),
        };
        let e = AgentEvent::SandboxBlocked {
            tool_id: "write_file".into(),
            code: violation.code().into(),
            reason: violation.to_string(),
        };
        let raw = serde_json::to_value(&e).unwrap();
        assert_eq!(raw["type"], json!("sandboxBlocked"));
        assert!(
            raw["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("outside the workspace"),
            "{raw}"
        );
        assert_eq!(raw["code"], json!("outside-workspace"));
        // A protected-path refusal gets its own code, so the row names the rule it enforced.
        assert_eq!(
            SandboxViolation::DeniedPath {
                path: "/work/.env".into(),
                hit: ".env".into(),
            }
            .code(),
            "denied-path"
        );
    }

    #[test]
    fn an_approval_resolution_travels_as_a_typed_decision() {
        let e = AgentEvent::ApprovalResolved {
            call_id: "c1".into(),
            decision: crate::approvals::ApprovalDecision::UserDenied,
        };
        let raw = serde_json::to_value(&e).unwrap();
        assert_eq!(raw["decision"], json!("userDenied"));
        assert_eq!(raw["callId"], json!("c1"));
    }
}
