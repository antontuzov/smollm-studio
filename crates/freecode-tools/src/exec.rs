//! The executors: four calls that read the workspace, three that change files in it, one command, and
//! two that reach the internet.
//!
//! Every one of them takes `(arguments, policy)` and goes through [`authorize`] first, so a call
//! cannot reach the disk without the sandbox having resolved its path, and the loop has no way to skip
//! that check. The shell is the one capability a path check cannot bound, so it is refused outright
//! unless the user's Terminal access switch is on; when it runs, it starts in the workspace, is killed
//! as a process group at the configured timeout, and hands back only the tail of its output. The two
//! network calls stand behind the same switch that admits a remote provider, and [`crate::web`] is
//! where their own rules live.
//!
//! The three writes here are `edit_file`, `write_file` and `apply_patch`, and they are the reason the
//! sandbox's `Access::Write` exists: a path outside the workspace root, or onto a protected name, is
//! refused by [`authorize`] before this file opens anything. What is left to the executors is the part
//! a sandbox cannot decide — whether the text the model quoted is actually in the file and actually
//! only once, whether a path it wants to create is still free to create, and whether every line a
//! patch says it is changing is still the line the file holds.
//!
//! Three budgets, and all three are about the *context window* rather than about safety:
//!   * [`MAX_RESULT_BYTES`] caps what goes back into the prompt. A tool result is prefilled on every
//!     later round of the run and replayed for every later turn of the conversation, so one 5 MB file
//!     read into a 32k-token window costs the rest of the task;
//!   * [`MAX_FILE_BYTES`] skips a file too big to be source, and [`MAX_WALK_ENTRIES`] bounds the walk;
//!   * [`SKIP_DIRS`] keeps build and vendor trees out of it, because descending into `target/` is the
//!     slowest possible way to find nothing.
//!
//! The sandbox is what refuses a path. These only decide how much of an allowed answer to send, and
//! they say what was left out rather than cutting it quietly.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::github::GitHubToken;
use crate::page::{page_op, PageHost, PageOp, MAX_CONSOLE_LINES, MAX_PAGE_TEXT};
use crate::pixels::Expect;
use crate::{authorize, registry::is_executable, web, Access, SandboxPolicy, ToolsError};

/// The longest plan the model may keep: enough for a real task, short enough that the list is
/// something a person reads rather than a second transcript.
pub const MAX_PLAN_ITEMS: usize = 20;
/// One step's text, in characters. A step is a phrase, and a model that writes a paragraph has
/// written the answer twice.
pub const MAX_PLAN_TEXT: usize = 160;

/// Where one step of a plan stands. The three states are the whole vocabulary: a step that is not
/// started, the one being worked on, and one whose result has been checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanStatus {
    Pending,
    Active,
    Done,
}

impl PlanStatus {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "pending" => Some(Self::Pending),
            "active" => Some(Self::Active),
            "done" => Some(Self::Done),
            _ => None,
        }
    }

    /// The mark the model is handed back, and the one a terminal fallback draws.
    fn mark(self) -> &'static str {
        match self {
            Self::Pending => "[ ]",
            Self::Active => "[~]",
            Self::Done => "[x]",
        }
    }
}

/// One step of the plan a run keeps. It is carried on the transcript row so the window can draw the
/// list the model last stated, rather than a summary of it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub text: String,
    pub status: PlanStatus,
}

/// Read and validate a `todo_write` call's items. The executor and the transcript row share this, so
/// the list the user reads is exactly the list the tool accepted.
pub fn plan(arguments: &Value) -> Result<Vec<PlanItem>, ToolsError> {
    let tool = "todo_write";
    let items = arguments
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| ToolsError::InvalidArguments {
            tool: tool.into(),
            reason: "items must be a list of { text, status } objects".into(),
        })?;
    if items.is_empty() || items.len() > MAX_PLAN_ITEMS {
        return Err(ToolsError::InvalidArguments {
            tool: tool.into(),
            reason: format!("a plan holds 1 to {MAX_PLAN_ITEMS} steps"),
        });
    }
    let mut plan = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let at = index + 1;
        let text = item
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| ToolsError::InvalidArguments {
                tool: tool.into(),
                reason: format!("step {at} has no text"),
            })?;
        if text.chars().count() > MAX_PLAN_TEXT {
            return Err(ToolsError::InvalidArguments {
                tool: tool.into(),
                reason: format!("step {at} is longer than {MAX_PLAN_TEXT} characters"),
            });
        }
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .and_then(PlanStatus::parse)
            .ok_or_else(|| ToolsError::InvalidArguments {
                tool: tool.into(),
                reason: format!("step {at} needs a status of pending, active or done"),
            })?;
        plan.push(PlanItem {
            text: text.to_string(),
            status,
        });
    }
    // Two steps being worked on at once would make the list disagree with the run it describes.
    if plan
        .iter()
        .filter(|i| i.status == PlanStatus::Active)
        .count()
        > 1
    {
        return Err(ToolsError::InvalidArguments {
            tool: tool.into(),
            reason: "only one step may be active".into(),
        });
    }
    Ok(plan)
}

/// Most result text one call may hand back, in bytes — about 3k tokens, against the smallest context
/// window this app starts a server with.
pub const MAX_RESULT_BYTES: usize = 12 * 1024;

/// A file past this size is not source the model should be reading, and reading it is the slowest way
/// to answer nothing. It is also what bounds one read's allocation.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Files one walk may collect before it stops and says so.
const MAX_WALK_ENTRIES: usize = 5_000;

/// How many entries or paths one answer may list.
const MAX_LISTED: usize = 200;

/// Matching lines `grep_search` reports before it stops.
const MAX_MATCHES: usize = 100;

/// The depth a walk descends. Past it the workspace is a build artefact, not a source tree.
const MAX_DEPTH: usize = 12;

/// Lines of the file shown around a change, either side. `grep_search` takes its context from the
/// call; an edit's hunk has no argument to ask with, and two is what makes a changed line
/// identifiable in the file it came from without reprinting the file.
const CTX_LINES: usize = 2;

/// Trees no read enumerates. A budget, not a security rule — the sandbox's denied names are what keep
/// `.env` and `.ssh` out of an answer.
const SKIP_DIRS: &[&str] = &[
    ".hg",
    ".svn",
    ".next",
    ".cache",
    ".venv",
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
];

/// What one call changed on disk, per file. Only the three writes produce one, and the counts are the
/// replacement's own arithmetic rather than a diff engine's: the lines it took out and the lines it
/// put back, with the lines it only passed over cancelled from both sides. For a call that swaps one
/// block of text for another that is exactly what a diff reports for the hunk, not an approximation of
/// one — and for a file that was not there before, it is every line in it counted as added. A patch
/// states both sides itself, so its counts are the `+` and `-` lines it carried, summed over hunks.
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    /// Workspace-relative, in forward slashes — the same spelling `glob` and `grep_search` answer
    /// with, so the model can hand it straight back to `read_file`.
    pub path: String,
    pub added: u32,
    pub removed: u32,
    /// How many copies of `old_string` were replaced, or — for a patch — how many hunks it applied to
    /// this file. More than one on an edit only when the call asked for `replace_all`, which is the
    /// difference between "fixed a line" and "renamed a symbol".
    pub occurrences: u32,
    /// Each block of the change as diff lines — `+`, `−`, and the two lines of context on either side
    /// — without the caption the model's copy carries, because the file it names is already this
    /// edit's `path`. Every hunk is kept rather than folded into one preview, so the review can number
    /// each against the line it opens on. The same text the tool result quotes, so the review a window
    /// draws and the answer that wrote it cannot disagree about what changed.
    pub hunks: Vec<EditHunk>,
}

/// One block of a file's change: the diff lines the row's review draws, and the line the block opens
/// on in the file as it stood before the write.
#[derive(Debug, Clone, PartialEq)]
pub struct EditHunk {
    pub at: u32,
    pub lines: String,
}

/// Which of the app's own sentences a row's one-line result is.
///
/// A summary is the part of a run that is wording rather than measurement, so it is the part that can
/// be said in another language: the catalog holds the sentence and this names it, which is the same
/// contract `freecode_core::transcript::StepKind` runs the label above it on. The English a call also
/// builds is what the *model* reads inside [`Executed::content`] — a tool result goes back into the
/// prompt, and a prompt in the interface's language would be a prompt in a language nobody asked the
/// model to answer in.
///
/// [`SummaryKey::params`] is the list `verify-transcript-i18n` holds both halves to: a catalog blank
/// no key names renders as an empty hole mid-sentence, and a key that sends a field the sentence does
/// not ask for has measured something the row never shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryKey {
    /// A file with no lines in it at all.
    Empty,
    /// `{{count}}` lines — a whole file, or the last line a range could reach.
    Lines,
    /// A range whose end falls before its start.
    NoRange,
    /// One line longer than the whole output budget, so no part of it fits.
    LineTooLong,
    /// `{{from}}`–`{{to}}` of a `{{total}}`-line file.
    LineRange,
    /// The same range, ended early by the byte budget rather than by the file.
    LineRangeCapped,
    /// `{{listed}}` entries shown of `{{total}}` the directory holds.
    EntriesShown,
    /// Exactly one file matched `{{pattern}}`.
    FileMatches,
    /// `{{count}}` files matched `{{pattern}}`.
    FilesMatch,
    /// Nothing in the workspace matched `{{pattern}}`.
    NoMatches,
    /// One matching line, in the one file it was found in.
    MatchInFile,
    /// One matching line, spread across `{{files}}` files.
    MatchInFiles,
    /// `{{count}}` matching lines, all of them in one file.
    MatchesInFile,
    /// `{{count}}` matching lines across `{{files}}` files.
    MatchesInFiles,
    /// One copy of the searched text replaced.
    CopyReplaced,
    /// `{{count}}` copies replaced, which is a rename rather than a fix.
    CopiesReplaced,
    /// The search answered nothing for `{{query}}`.
    NoResults,
    /// One hit.
    Result,
    /// `{{count}}` hits.
    Results,
    /// A page read only as far as the character budget, so its `{{title}}` says so.
    PageCut,
    /// One file created.
    FileCreated,
    /// `{{done}}` of `{{total}}` steps of the run's plan are checked off.
    PlanSteps,
    /// One hunk of a patch applied.
    HunkApplied,
    /// `{{count}}` hunks applied, in one or more places in the same file.
    HunksApplied,
    /// `{{count}}` files changed by one patch.
    FilesPatched,
    /// A command that finished, with `{{code}}` as its exit status.
    TerminalExit,
    /// A command stopped by its timeout after `{{seconds}}` seconds.
    TerminalTimedOut,
    /// A command ended by a signal rather than by an exit status.
    TerminalStopped,
    /// A subagent that answered, with the `{{rounds}}` it spent and the `{{reads}}` it made.
    SubagentReport,
    /// A working tree with nothing in it to report: a clean status, or a diff with no lines.
    GitClean,
    /// The workspace is not inside a git repository, so there is nothing to report about one.
    GitNotRepo,
    /// One path changed in the working tree.
    ChangedFile,
    /// `{{count}}` paths changed in the working tree.
    ChangedFiles,
    /// One file in the diff, with the lines it adds and takes away.
    GitDiffFile,
    /// `{{files}}` files in the diff, with the lines they add and take away.
    GitDiff,
    /// One commit in the log.
    GitCommit,
    /// `{{count}}` commits in the log.
    GitCommits,
    /// `{{count}}` paths put into the index.
    GitStaged,
    /// A commit that was made, named by its `{{sha}}`.
    GitCommitted,
    /// The checkout moved to `{{branch}}`.
    GitSwitched,
    /// `{{branch}}` went to its remote.
    GitPushed,
    /// Pull request #`{{number}}` was opened.
    GitPullRequest,
    /// A page put into the window's browser panel, named by `{{file}}`.
    BrowserOpened,
    /// The visible text one read of the page took back, `{{count}}` characters of it.
    BrowserText,
    /// A click that landed on `{{target}}`, the element the selector matched.
    BrowserClicked,
    /// Text put into `{{target}}`, the field the selector matched.
    BrowserTyped,
    /// `{{key}}` pressed on the page.
    BrowserPressed,
    /// `{{target}}` appeared on the page before the wait ran out.
    BrowserFound,
    /// `{{target}}` never appeared on the page; the content says what the page showed instead.
    BrowserNotFound,
    /// The page has logged nothing to its console since it loaded.
    BrowserClean,
    /// `{{count}}` console lines the page has logged since it loaded.
    BrowserProblems,
    /// The page saved as a PNG of `{{size}}` pixels, kept at the `{{file}}` the content names.
    BrowserCaptured,
    /// A `{{check}}` of the page's picture held, measured on a PNG of `{{size}}` pixels.
    BrowserChecked,
    /// A `{{check}}` of the page's picture did not hold, measured on a PNG of `{{size}}` pixels.
    BrowserCheckFailed,
}

impl SummaryKey {
    /// Every key the executors can answer with, so a catalog entry no executor reaches is named as the
    /// dead copy it is.
    pub const ALL: [Self; 54] = [
        Self::Empty,
        Self::Lines,
        Self::NoRange,
        Self::LineTooLong,
        Self::LineRange,
        Self::LineRangeCapped,
        Self::EntriesShown,
        Self::FileMatches,
        Self::FilesMatch,
        Self::NoMatches,
        Self::MatchInFile,
        Self::MatchInFiles,
        Self::MatchesInFile,
        Self::MatchesInFiles,
        Self::CopyReplaced,
        Self::CopiesReplaced,
        Self::NoResults,
        Self::Result,
        Self::Results,
        Self::PageCut,
        Self::FileCreated,
        Self::PlanSteps,
        Self::HunkApplied,
        Self::HunksApplied,
        Self::FilesPatched,
        Self::TerminalExit,
        Self::TerminalTimedOut,
        Self::TerminalStopped,
        Self::SubagentReport,
        Self::GitClean,
        Self::GitNotRepo,
        Self::ChangedFile,
        Self::ChangedFiles,
        Self::GitDiffFile,
        Self::GitDiff,
        Self::GitCommit,
        Self::GitCommits,
        Self::GitStaged,
        Self::GitCommitted,
        Self::GitSwitched,
        Self::GitPushed,
        Self::GitPullRequest,
        Self::BrowserOpened,
        Self::BrowserText,
        Self::BrowserClicked,
        Self::BrowserTyped,
        Self::BrowserPressed,
        Self::BrowserFound,
        Self::BrowserNotFound,
        Self::BrowserClean,
        Self::BrowserProblems,
        Self::BrowserCaptured,
        Self::BrowserChecked,
        Self::BrowserCheckFailed,
    ];

    pub fn i18n_key(self) -> &'static str {
        match self {
            Self::Empty => "transcript.summary.empty",
            Self::Lines => "transcript.summary.lines",
            Self::NoRange => "transcript.summary.noRange",
            Self::LineTooLong => "transcript.summary.lineTooLong",
            Self::LineRange => "transcript.summary.lineRange",
            Self::LineRangeCapped => "transcript.summary.lineRangeCapped",
            Self::EntriesShown => "transcript.summary.entriesShown",
            Self::FileMatches => "transcript.summary.fileMatches",
            Self::FilesMatch => "transcript.summary.filesMatch",
            Self::NoMatches => "transcript.summary.noMatches",
            Self::MatchInFile => "transcript.summary.matchInFile",
            Self::MatchInFiles => "transcript.summary.matchInFiles",
            Self::MatchesInFile => "transcript.summary.matchesInFile",
            Self::MatchesInFiles => "transcript.summary.matchesInFiles",
            Self::CopyReplaced => "transcript.summary.copyReplaced",
            Self::CopiesReplaced => "transcript.summary.copiesReplaced",
            Self::NoResults => "transcript.summary.noResults",
            Self::Result => "transcript.summary.result",
            Self::Results => "transcript.summary.results",
            Self::PageCut => "transcript.summary.pageCut",
            Self::FileCreated => "transcript.summary.fileCreated",
            Self::PlanSteps => "transcript.summary.planSteps",
            Self::HunkApplied => "transcript.summary.hunkApplied",
            Self::HunksApplied => "transcript.summary.hunksApplied",
            Self::FilesPatched => "transcript.summary.filesPatched",
            Self::TerminalExit => "transcript.summary.terminalExit",
            Self::TerminalTimedOut => "transcript.summary.terminalTimedOut",
            Self::TerminalStopped => "transcript.summary.terminalStopped",
            Self::SubagentReport => "transcript.summary.subagentReport",
            Self::GitClean => "transcript.summary.gitClean",
            Self::GitNotRepo => "transcript.summary.gitNotRepo",
            // The diff pill and a git count are the same sentence; reusing its key keeps one wording
            // for "N files changed" rather than a second copy that could drift from it.
            Self::ChangedFile => "transcript.diff.fileChanged",
            Self::ChangedFiles => "transcript.diff.filesChanged",
            Self::GitDiffFile => "transcript.summary.gitDiffFile",
            Self::GitDiff => "transcript.summary.gitDiff",
            Self::GitCommit => "transcript.summary.gitCommit",
            Self::GitCommits => "transcript.summary.gitCommits",
            Self::GitStaged => "transcript.summary.gitStaged",
            Self::GitCommitted => "transcript.summary.gitCommitted",
            Self::GitSwitched => "transcript.summary.gitSwitched",
            Self::GitPushed => "transcript.summary.gitPushed",
            Self::GitPullRequest => "transcript.summary.gitPullRequest",
            Self::BrowserOpened => "transcript.summary.browserOpened",
            Self::BrowserText => "transcript.summary.browserText",
            Self::BrowserClicked => "transcript.summary.browserClicked",
            Self::BrowserTyped => "transcript.summary.browserTyped",
            Self::BrowserPressed => "transcript.summary.browserPressed",
            Self::BrowserFound => "transcript.summary.browserFound",
            Self::BrowserNotFound => "transcript.summary.browserNotFound",
            Self::BrowserClean => "transcript.summary.browserClean",
            Self::BrowserProblems => "transcript.summary.browserProblems",
            Self::BrowserCaptured => "transcript.summary.browserCaptured",
            Self::BrowserChecked => "transcript.summary.browserChecked",
            Self::BrowserCheckFailed => "transcript.summary.browserCheckFailed",
        }
    }

    /// The fields this sentence has blanks for.
    pub fn params(self) -> &'static [&'static str] {
        match self {
            Self::Lines
            | Self::MatchesInFile
            | Self::CopiesReplaced
            | Self::Results
            | Self::HunksApplied
            | Self::FilesPatched
            | Self::ChangedFile
            | Self::ChangedFiles
            | Self::GitCommits
            | Self::GitStaged
            | Self::BrowserText
            | Self::BrowserProblems => &["count"],
            Self::NoRange => &["from", "to"],
            Self::LineTooLong => &["line"],
            Self::LineRange | Self::LineRangeCapped => &["from", "to", "total"],
            Self::EntriesShown => &["listed", "total"],
            Self::FilesMatch => &["count", "pattern"],
            Self::FileMatches | Self::NoMatches => &["pattern"],
            Self::MatchInFiles => &["files"],
            Self::MatchesInFiles => &["count", "files"],
            Self::NoResults => &["query"],
            Self::PageCut => &["title"],
            Self::TerminalExit => &["code"],
            Self::TerminalTimedOut => &["seconds"],
            Self::SubagentReport => &["rounds", "reads"],
            Self::PlanSteps => &["done", "total"],
            Self::BrowserOpened => &["file"],
            Self::BrowserClicked
            | Self::BrowserTyped
            | Self::BrowserFound
            | Self::BrowserNotFound => &["target"],
            Self::BrowserPressed => &["key"],
            Self::BrowserCaptured => &["size"],
            Self::BrowserChecked | Self::BrowserCheckFailed => &["check", "size"],
            Self::GitDiffFile => &["added", "removed"],
            Self::GitDiff => &["files", "added", "removed"],
            Self::GitCommitted => &["sha"],
            Self::GitSwitched | Self::GitPushed => &["branch"],
            Self::GitPullRequest => &["number"],
            Self::Empty
            | Self::MatchInFile
            | Self::CopyReplaced
            | Self::Result
            | Self::FileCreated
            | Self::HunkApplied
            | Self::TerminalStopped
            | Self::GitClean
            | Self::GitNotRepo
            | Self::GitCommit
            | Self::BrowserClean => &[],
        }
    }
}

/// The one line under a transcript row's label.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// Which sentence this is, or `None` when the line is the thing itself — a page's title — and a
    /// catalog has no business re-wording it.
    pub key: Option<SummaryKey>,
    /// Its fields, named exactly as [`SummaryKey::params`] lists them.
    pub params: Value,
    /// The same line in English, which is what [`Executed::content`] quotes to the model.
    pub text: String,
    /// The PNG a capture or a check drew, when the call kept one. The line above names the file; this
    /// is how the host reaches it without reading the sentence, so the row that reports the call can
    /// show the frame itself. `None` on every call that drew nothing.
    pub picture: Option<String>,
}

impl Summary {
    /// A line the catalog words.
    pub fn said(key: SummaryKey, params: Value, text: impl Into<String>) -> Self {
        // Debug-only, because a misnamed field is a hole in a sentence rather than a wrong sentence,
        // and the release build should show the hole rather than stop a run over it.
        if let Some(got) = params.as_object() {
            let want: std::collections::BTreeSet<String> = key
                .params()
                .iter()
                .map(|name| (*name).to_string())
                .collect();
            let named: std::collections::BTreeSet<String> = got.keys().cloned().collect();
            debug_assert_eq!(
                named,
                want,
                "{} is sent {named:?} and its sentence has blanks for {want:?}",
                key.i18n_key()
            );
        }
        Self {
            key: Some(key),
            params,
            text: text.into(),
            picture: None,
        }
    }

    /// A line that is data the tool found, not a statement about it.
    pub fn verbatim(text: impl Into<String>) -> Self {
        Self {
            key: None,
            params: Value::Null,
            text: text.into(),
            picture: None,
        }
    }

    /// Whether the work this line reports did not succeed. Only the three command endings, a wait
    /// that ran out and a picture check that did not hold can say so: every other summary describes a
    /// read or a write that returned without failing.
    pub fn failed(&self) -> bool {
        match self.key {
            Some(SummaryKey::TerminalTimedOut | SummaryKey::TerminalStopped) => true,
            Some(SummaryKey::TerminalExit) => {
                self.params.get("code").and_then(Value::as_i64) != Some(0)
            }
            // A wait that ran out is the one page result that did not find what it asked for: the row
            // says so instead of showing the wait as work that succeeded.
            Some(SummaryKey::BrowserNotFound) => true,
            // A check that failed is the answer the run asked for, not an error — but it is a row that
            // must not read as work that succeeded, because the page did not draw what was claimed.
            Some(SummaryKey::BrowserCheckFailed) => true,
            _ => false,
        }
    }
}

/// What one call produced: a clause for the transcript row and the text the model is handed.
#[derive(Debug, Clone, PartialEq)]
pub struct Executed {
    /// The one line under the row's label — "307 of 5000 lines, capped here", "37 matches in 12 files".
    pub summary: Summary,
    /// The tool result, as the model sees it. Never longer than [`MAX_RESULT_BYTES`] plus the sentence
    /// that says what was left out.
    pub content: String,
    /// The files this call changed, for the run's diff and the review. Empty for every read, which is
    /// what keeps a "+0 −0" pill off a run that touched nothing. One entry for `edit_file` and
    /// `write_file`, and one per file for a patch that names several.
    pub edits: Vec<Edit>,
}

/// Where a running command's bytes go while it is still running — the pipe's own chunks, in the order
/// each drain thread read them. The two streams share one sink, so the order across them is the order
/// the reads happened in rather than the order the process wrote; a terminal view is a window onto the
/// command, not a transcript of it, and the answer the model gets still merges them the same way.
///
/// Only the three callers that run a command pass one. It is `Send + Sync` because each pipe drains on
/// its own thread, and it must not block: it is called on the thread that is reading the pipe.
pub type Watcher = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// Run one call. The single entry point: authorization, the executable gate, then the work.
///
/// Takes the tool id and its parsed arguments rather than a [`crate::ToolCall`], because the wire's
/// own call shape lives in `freecode-llama` and this crate has to stay testable without it.
pub fn execute(
    tool_id: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
) -> Result<Executed, ToolsError> {
    execute_watched(tool_id, arguments, policy, None, None, None)
}

/// [`execute`] with a sink for a running command's output. The watcher sees the same bytes the
/// executor captures — both streams, as they arrive — so the window can show a build finishing line by
/// line while the call that started it is still open. It changes nothing about what runs: the same
/// authorization, the same executable gate, the same process group and deadline.
///
/// `page` is the window's browser panel and `github` the machine's GitHub credential. Both are
/// doors only the host can open, so a call that needs one is told so rather than failing mute.
pub fn execute_watched(
    tool_id: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
    watcher: Option<&Watcher>,
    page: Option<&dyn PageHost>,
    github: Option<&dyn GitHubToken>,
) -> Result<Executed, ToolsError> {
    let tool = authorize(tool_id, arguments, policy)?;
    if !is_executable(tool.id) {
        return Err(ToolsError::NoExecutor(tool.id.to_string()));
    }
    match tool.id {
        "read_file" => read_file(tool.id, arguments, policy),
        "list_dir" => list_dir(tool.id, arguments, policy),
        "glob" => find_files(tool.id, arguments, policy),
        "grep_search" => search_content(tool.id, arguments, policy),
        "todo_write" => todo_write(arguments),
        "edit_file" => edit_file(tool.id, arguments, policy),
        "write_file" => write_file(tool.id, arguments, policy),
        "apply_patch" => apply_patch(tool.id, arguments, policy),
        "terminal" => terminal(tool.id, arguments, policy, watcher),
        "run_tests" => run_project(tool.id, arguments, policy, ProjectKind::Tests, watcher),
        "run_build" => run_project(tool.id, arguments, policy, ProjectKind::Build, watcher),
        "run_linter" => run_project(tool.id, arguments, policy, ProjectKind::Lint, watcher),
        "git_status" => git_read(tool.id, arguments, policy, GitRead::Status),
        "git_diff" => git_read(tool.id, arguments, policy, GitRead::Diff),
        "git_log" => git_read(tool.id, arguments, policy, GitRead::Log),
        // The route to GitHub: three calls that stay inside the checkout and two that leave it.
        "git_stage" => git_stage(tool.id, arguments, policy),
        "git_commit" => git_commit(tool.id, arguments, policy),
        "git_branch" => git_branch(tool.id, arguments, policy),
        "git_push" => git_push(tool.id, arguments, policy),
        "github_pr" => github_pr(tool.id, arguments, policy, github),
        // The two that leave the machine. Neither takes the policy: `authorize` has already refused
        // them without the Privacy switch, and a URL is not a path the sandbox can resolve.
        "web_search" => web_search(tool.id, arguments),
        "fetch_url" => fetch_url(tool.id, arguments),
        // Nine calls that act inside the document the window's browser panel is showing rather than
        // on disk or the network. The page itself is reached through [`PageHost`], which only the
        // window can answer; a run with no panel — a subagent's — is told so rather than failing mute.
        "browser_open" | "browser_read" | "browser_click" | "browser_type" | "browser_press"
        | "browser_wait" | "browser_console" | "browser_capture" | "browser_check" => {
            page_tool(tool.id, arguments, policy, page)
        }
        // Unreachable while [`registry::EXECUTABLE`](crate::registry::EXECUTABLE) and this match move
        // together, and a refusal rather than a panic if they ever do not.
        other => Err(ToolsError::NoExecutor(other.to_string())),
    }
}

/// The files a write call will change, resolved through the same sandbox the call itself goes
/// through. Every other tool answers nothing.
///
/// The host holds each file's text from before the call so the change can be reverted. A patch names
/// its files differently from the other two writes, which is why the pairing lives beside the
/// executors rather than in the window; a patch that does not parse names nothing, because the call
/// that would have applied it is about to fail anyway.
pub fn write_targets(tool_id: &str, arguments: &Value, policy: &SandboxPolicy) -> Vec<PathBuf> {
    let labels: Vec<String> = match tool_id {
        "edit_file" | "write_file" => text_arg(arguments, "path")
            .map(|path| vec![path.to_string()])
            .unwrap_or_default(),
        "apply_patch" => text_arg(arguments, "patch")
            .and_then(|raw| parse_patch(tool_id, raw).ok())
            .map(|sections| sections.into_iter().map(|section| section.path).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    labels
        .iter()
        .filter_map(|label| resolve(policy, label, Access::Write).ok())
        .collect()
}

/// How much of a page's text an `open` or a `wait` quotes. A read has the whole page and its own
/// budget; these two show what the page looks like now, and the model asks for a read when it needs
/// the rest.
const PAGE_PEEK_TEXT: usize = 1200;

/// The nine calls that act on the page in the window's browser panel.
///
/// Only `browser_open` names a file, and that path goes through the same sandbox as a read: it must
/// resolve inside the workspace, exist, and be a page. The rest act on the document the panel is
/// already showing, which is [`PageHost`]'s to answer — and a run with no panel (a subagent's, or the
/// host's own post-run check) says so rather than pretending the call ran.
///
/// The page is the *rendered* document, not the file: its text is what its own script wrote, and a
/// click is the event a person's mouse sends. That is the point of these calls — `read_file` already
/// reads the source, and a page that only works after its script runs cannot be checked that way.
fn page_tool(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
    page: Option<&dyn PageHost>,
) -> Result<Executed, ToolsError> {
    let op = page_op(tool).ok_or_else(|| ToolsError::NoExecutor(tool.to_string()))?;
    let host = page.ok_or_else(|| {
        ToolsError::Page(
            "there is no browser panel to act on here: the page calls drive the window's preview \
             panel, which only the run the user is watching has. A subagent's run, or the host's \
             own check after a run, has no page."
                .into(),
        )
    })?;
    let patience = op.patience();
    match op {
        PageOp::Open => {
            let raw = required(tool, arguments, "path")?;
            let path = resolve(policy, raw, Access::Read)?;
            let shown = relative(policy, &path);
            if !path.is_file() {
                return Err(ToolsError::InvalidArguments {
                    tool: tool.to_string(),
                    reason: format!("\"{shown}\" is not a file; browser_open shows one HTML page"),
                });
            }
            if !is_html(&path) {
                return Err(ToolsError::InvalidArguments {
                    tool: tool.to_string(),
                    reason: format!(
                        "\"{shown}\" is not an HTML page. browser_open shows .html and .htm files, \
                         and read_file is the call that reads anything else."
                    ),
                });
            }
            let answer = ask(host, op, json!({ "path": shown }), patience)?;
            let url = page_str(&answer, "url");
            let title = page_str(&answer, "title");
            let text = page_str(&answer, "text");
            let mut content = format!("Opened \"{shown}\" in the browser panel.\nAddress: {url}\n");
            if !title.is_empty() {
                content.push_str(&format!("Title: {title}\n"));
            }
            let (peek, more) = page_text(&text, PAGE_PEEK_TEXT);
            if peek.trim().is_empty() {
                content.push_str("\nThe page shows no text yet.\n");
            } else {
                content.push_str(&format!("\nText:\n{peek}\n"));
                if more {
                    content.push_str("(More text was left out; browser_read shows the page.)\n");
                }
            }
            content.push_str(
                "\nWhat is in the panel is what the page's own script made of it. browser_read, \
                 browser_click, browser_type, browser_press, browser_wait and browser_console act \
                 on this page.",
            );
            Ok(Executed {
                summary: Summary::said(
                    SummaryKey::BrowserOpened,
                    json!({ "file": shown.clone() }),
                    format!("Opened {shown} in the browser"),
                ),
                content,
                edits: Vec::new(),
            })
        }
        PageOp::Read => {
            let selector = text_arg(arguments, "selector")
                .map(str::trim)
                .filter(|selector| !selector.is_empty());
            let fields = match selector {
                Some(selector) => json!({ "selector": selector }),
                None => json!({}),
            };
            let answer = ask(host, op, fields, patience)?;
            let url = page_str(&answer, "url");
            let title = page_str(&answer, "title");
            let text = page_str(&answer, "text");
            let (shown, more) = page_text(&text, MAX_PAGE_TEXT);
            let mut content = format!(
                "Page: {}\nAddress: {url}\n",
                if title.is_empty() {
                    "(untitled)"
                } else {
                    title.as_str()
                }
            );
            if let Some(selector) = selector {
                content.push_str(&format!("Element: {selector}\n"));
            }
            if shown.trim().is_empty() {
                content.push_str("\nThe page shows no text.\n");
            } else {
                content.push_str(&format!("\nText:\n{shown}\n"));
            }
            if more {
                content.push_str(&format!(
                    "\n(The page's text is longer than {MAX_PAGE_TEXT} characters; the rest is left \
                     out.)\n"
                ));
            }
            let count = text.chars().count();
            Ok(Executed {
                summary: Summary::said(
                    SummaryKey::BrowserText,
                    json!({ "count": count }),
                    format!("Read the page ({count} characters)"),
                ),
                content,
                edits: Vec::new(),
            })
        }
        PageOp::Click => {
            let selector = required(tool, arguments, "selector")?;
            let mut fields = json!({ "selector": selector });
            if let Some(x) = arguments.get("x").and_then(Value::as_f64) {
                fields["x"] = json!(x);
            }
            if let Some(y) = arguments.get("y").and_then(Value::as_f64) {
                fields["y"] = json!(y);
            }
            let answer = ask(host, op, fields, patience)?;
            let target = named(page_str(&answer, "target"), selector);
            Ok(Executed {
                summary: Summary::said(
                    SummaryKey::BrowserClicked,
                    json!({ "target": target.clone() }),
                    format!("Clicked {target} on the page"),
                ),
                content: format!(
                    "Clicked {target}.\n\nThe page has had its turn to react; browser_read shows \
                     what it displays now."
                ),
                edits: Vec::new(),
            })
        }
        PageOp::Type => {
            let text = required(tool, arguments, "text")?;
            let selector = text_arg(arguments, "selector")
                .map(str::trim)
                .filter(|selector| !selector.is_empty());
            let submit = flag_arg(arguments, "submit");
            let mut fields = json!({ "text": text });
            if let Some(selector) = selector {
                fields["selector"] = json!(selector);
            }
            if submit {
                fields["submit"] = json!(true);
            }
            let answer = ask(host, op, fields, patience)?;
            let target = named(
                page_str(&answer, "target"),
                selector.unwrap_or("the focused field"),
            );
            let enter = if submit { ", then pressed Enter" } else { "" };
            Ok(Executed {
                summary: Summary::said(
                    SummaryKey::BrowserTyped,
                    json!({ "target": target.clone() }),
                    format!("Typed into {target} on the page"),
                ),
                content: format!(
                    "Typed into {target}: \"{text}\"{enter}.\n\nbrowser_read shows what the page \
                     displays now."
                ),
                edits: Vec::new(),
            })
        }
        PageOp::Press => {
            let key = required(tool, arguments, "key")?;
            let answer = ask(host, op, json!({ "key": key }), patience)?;
            let key = named(page_str(&answer, "key"), key);
            Ok(Executed {
                summary: Summary::said(
                    SummaryKey::BrowserPressed,
                    json!({ "key": key.clone() }),
                    format!("Pressed {key} on the page"),
                ),
                content: format!(
                    "Pressed {key} on the page. browser_read shows what it displays now."
                ),
                edits: Vec::new(),
            })
        }
        PageOp::Wait => {
            let selector = required(tool, arguments, "selector")?;
            let timeout = number_arg(arguments, "timeout_ms")
                .unwrap_or(DEFAULT_WAIT_MS)
                .clamp(100, MAX_WAIT_MS);
            let answer = ask(
                host,
                op,
                json!({ "selector": selector, "timeout_ms": timeout }),
                patience,
            )?;
            let found = answer.get("found").and_then(Value::as_bool) == Some(true);
            let elapsed = answer
                .get("elapsed_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let text = page_str(&answer, "text");
            let (peek, _) = page_text(&text, PAGE_PEEK_TEXT);
            if !found {
                return Ok(Executed {
                    summary: Summary::said(
                        SummaryKey::BrowserNotFound,
                        json!({ "target": selector }),
                        format!("Waited {timeout} ms for {selector}: not found"),
                    ),
                    content: format!(
                        "\"{selector}\" did not appear within {timeout} ms. The page shows:\n{peek}\n\
                         \nA page that draws nothing is usually a script that failed; browser_console \
                         says why."
                    ),
                    edits: Vec::new(),
                });
            }
            Ok(Executed {
                summary: Summary::said(
                    SummaryKey::BrowserFound,
                    json!({ "target": selector }),
                    format!("Found {selector} on the page"),
                ),
                content: format!(
                    "\"{selector}\" appeared after {elapsed} ms.\n\nThe page shows:\n{peek}"
                ),
                edits: Vec::new(),
            })
        }
        PageOp::Console => {
            let answer = ask(host, op, json!({}), patience)?;
            let lines: Vec<(String, String)> = answer
                .get("lines")
                .and_then(Value::as_array)
                .map(|lines| {
                    lines
                        .iter()
                        .filter_map(|line| {
                            let text = page_str(line, "text");
                            (!text.is_empty()).then(|| (page_str(line, "level"), text))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if lines.is_empty() {
                return Ok(Executed {
                    summary: Summary::said(
                        SummaryKey::BrowserClean,
                        json!({}),
                        "Read the console: nothing logged",
                    ),
                    content:
                        "The console is clean: the page has logged no errors or warnings since \
                              it loaded."
                            .into(),
                    edits: Vec::new(),
                });
            }
            let mut content = format!(
                "The page logged {} console line{} since it loaded:\n",
                lines.len(),
                if lines.len() == 1 { "" } else { "s" }
            );
            for (level, text) in lines.iter().take(MAX_CONSOLE_LINES) {
                let level = if level.is_empty() {
                    "log"
                } else {
                    level.as_str()
                };
                content.push_str(&format!("[{level}] {text}\n"));
            }
            if lines.len() > MAX_CONSOLE_LINES {
                content.push_str(&format!(
                    "({} more lines left out.)\n",
                    lines.len() - MAX_CONSOLE_LINES
                ));
            }
            let count = lines.len();
            Ok(Executed {
                summary: Summary::said(
                    SummaryKey::BrowserProblems,
                    json!({ "count": count }),
                    format!("Read the console: {count} problems"),
                ),
                content,
                edits: Vec::new(),
            })
        }
        PageOp::Capture => {
            let answer = ask(host, op, json!({}), patience)?;
            let file = page_str(&answer, "path");
            let width = answer.get("width").and_then(Value::as_u64).unwrap_or(0);
            let height = answer.get("height").and_then(Value::as_u64).unwrap_or(0);
            let bytes = answer.get("bytes").and_then(Value::as_u64).unwrap_or(0);
            // A capture that came back without a picture is not a capture the run can rely on: an
            // empty PNG would read as "the page draws nothing", which is the one thing this call
            // exists to tell apart from "the page is fine".
            if file.is_empty() || width == 0 || height == 0 {
                return Err(ToolsError::Page(
                    "the panel answered the capture without a picture: it named no file, or no size. \
                     The page may still be painting; browser_wait can hold the call until something \
                     appears."
                        .into(),
                ));
            }
            let size = format!("{width}×{height}");
            let kb = (bytes as f64 / 1024.0).round().max(1.0);
            let mut summary = Summary::said(
                SummaryKey::BrowserCaptured,
                json!({ "size": size }),
                format!("Captured the page ({size})"),
            );
            summary.picture = Some(file.to_string());
            Ok(Executed {
                summary,
                content: format!(
                    "Saved a PNG of what the page is showing: {size} pixels, {kb} KB.\nFile: {file}\n\
                     \nThis is the page as it is drawn — its pixels, not its markup and not its text. \
                     The file is kept beside the app's own data rather than in the project folder, so \
                     no call in this workspace can change it.\n"
                ),
                edits: Vec::new(),
            })
        }
        PageOp::Check => {
            let expect = check_expect(tool, arguments)?;
            let fields =
                serde_json::to_value(&expect).map_err(|why| ToolsError::InvalidArguments {
                    tool: tool.to_string(),
                    reason: format!("the check cannot be written down: {why}"),
                })?;
            let answer = ask(host, op, fields, patience)?;
            let file = page_str(&answer, "path");
            let width = answer.get("width").and_then(Value::as_u64).unwrap_or(0);
            let height = answer.get("height").and_then(Value::as_u64).unwrap_or(0);
            let bytes = answer.get("bytes").and_then(Value::as_u64).unwrap_or(0);
            // A check with no picture behind it measured nothing: saying it passed would be the one
            // lie a check exists to prevent, and saying it failed would blame the page for a panel
            // that never drew it.
            if file.is_empty() || width == 0 || height == 0 {
                return Err(ToolsError::Page(
                    "the panel answered the check without a picture to measure: it named no file, or \
                     no size. The page may still be painting; browser_wait can hold the call until \
                     something appears."
                        .into(),
                ));
            }
            let passed = answer
                .get("passed")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    ToolsError::Page(
                        "the panel answered the check without a verdict, so nothing can be said \
                         about the picture."
                            .into(),
                    )
                })?;
            let detail = page_str(&answer, "detail");
            let size = format!("{width}×{height}");
            let kb = (bytes as f64 / 1024.0).round().max(1.0);
            let check = expect.name();
            let (key, headline) = if passed {
                (SummaryKey::BrowserChecked, "passed")
            } else {
                (SummaryKey::BrowserCheckFailed, "did not pass")
            };
            let mut content = format!(
                "Checked the page's picture: {check} — {headline}.\n\n{detail}\n\nThe PNG this \
                 check measured is {size} pixels, {kb} KB.\nFile: {file}\n\nThis is the page as it \
                 is drawn — its pixels, not its markup and not its text.\n"
            );
            if !passed {
                content.push_str(
                    "\nThe picture is what the page is drawing now, so change the page and run \
                     browser_check again to see whether it passes. browser_console answers why a \
                     page drew nothing; browser_read answers what it says.\n",
                );
            }
            let mut summary = Summary::said(
                key,
                json!({ "check": check, "size": size }),
                format!("Picture check {headline}: {check} ({size})"),
            );
            summary.picture = Some(file.to_string());
            Ok(Executed {
                summary,
                content,
                edits: Vec::new(),
            })
        }
    }
}

/// The assertion one check makes, validated before the panel is asked for a capture: a malformed
/// check should cost a refusal, not a frame the run took for nothing.
fn check_expect(tool: &str, arguments: &Value) -> Result<Expect, ToolsError> {
    let expect: Expect =
        serde_json::from_value(arguments.clone()).map_err(|why| ToolsError::InvalidArguments {
            tool: tool.to_string(),
            reason: format!(
                "{why}. expect is \"not-blank\", or \"colour\" with x and y (fractions of the \
                 picture, 0 to 1) and rgb (#rrggbb); tolerance is optional."
            ),
        })?;
    expect
        .validate()
        .map_err(|why| ToolsError::InvalidArguments {
            tool: tool.to_string(),
            reason: why,
        })?;
    Ok(expect)
}

/// The default and ceiling for `browser_wait`. The registry schema states the same range; this is the
/// executor's own bound, because the host is what actually sleeps.
const DEFAULT_WAIT_MS: usize = 3000;
const MAX_WAIT_MS: usize = 15_000;

/// One page operation, with the host's refusal passed through as it was worded: the host knows what
/// the page answered, and a second wrapper sentence would bury it.
fn ask(
    host: &dyn PageHost,
    op: PageOp,
    fields: Value,
    patience: Duration,
) -> Result<Value, ToolsError> {
    host.ask(op, &fields, patience).map_err(ToolsError::Page)
}

/// The name the page gave an element, or the selector the model used when it gave none.
fn named(said: String, or: &str) -> String {
    if said.is_empty() {
        or.to_string()
    } else {
        said
    }
}

/// A string field of a page's answer, or an empty one. The host fills these; an answer built by a
/// test or a newer host may leave one out, and an absent field is a blank line rather than a failed
/// call.
fn page_str(answer: &Value, key: &str) -> String {
    answer
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The first `max` characters of a page's text, and whether any were left.
fn page_text(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((cut, _)) => (text[..cut].to_string(), true),
        None => (text.to_string(), false),
    }
}

/// Whether a path names a page. The preview scheme will serve the file whatever it is, but this call
/// exists to put a *rendered* page in the panel, and a source file shown as text is not that.
fn is_html(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("html" | "htm")
    )
}

/// Read a text file, optionally a line range of one.
///
/// The whole allowed file is held while the range is picked out of it, which is what makes the answer
/// able to state the file's real line count and to continue from any line the cap stopped it on.
/// Record the plan. It touches no file: the list goes back to the model so the next round reads the
/// same states the user does, and onto the transcript row so the window can draw them. The result
/// says where the plan stands rather than repeating it, because the model just wrote the steps.
fn todo_write(arguments: &Value) -> Result<Executed, ToolsError> {
    let items = plan(arguments)?;
    let done = items
        .iter()
        .filter(|i| i.status == PlanStatus::Done)
        .count();
    let total = items.len();
    let mut content = String::from("Plan for this task:\n");
    for (index, item) in items.iter().enumerate() {
        content.push_str(&format!(
            "{}. {} {}\n",
            index + 1,
            item.status.mark(),
            item.text
        ));
    }
    content.push_str(
        "\nKeep it current: mark the step you are working on active, and a step done only once its \
         result is verified.",
    );
    Ok(Executed {
        summary: Summary::said(
            SummaryKey::PlanSteps,
            json!({ "done": done, "total": total }),
            format!("{done} of {total} steps done"),
        ),
        content,
        edits: Vec::new(),
    })
}

fn read_file(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
) -> Result<Executed, ToolsError> {
    let label = required(tool, arguments, "path")?.to_string();
    let path = resolve(policy, &label, Access::Read)?;
    let text = read_text(tool, &path)?.ok_or_else(|| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason: format!("{label} is not text this tool can read"),
    })?;
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total == 0 {
        return Ok(Executed {
            summary: Summary::said(SummaryKey::Empty, json!({}), "empty"),
            content: format!("{label} is empty."),
            edits: Vec::new(),
        });
    }

    let from = number_arg(arguments, "start_line").unwrap_or(1).max(1);
    if from > total {
        return Ok(Executed {
            summary: Summary::said(
                SummaryKey::Lines,
                json!({ "count": total }),
                format!("{total} lines"),
            ),
            content: format!(
                "Nothing to show: {label} ends at line {total} and the range asked for starts past line \
                 {total}."
            ),
            edits: Vec::new(),
        });
    }
    let to = number_arg(arguments, "end_line")
        .unwrap_or(total)
        .min(total);
    if to < from {
        return Ok(Executed {
            summary: Summary::said(
                SummaryKey::NoRange,
                json!({ "from": from, "to": to }),
                format!("nothing between {from} and {to}"),
            ),
            content: format!(
                "Nothing to show: the range ends at line {to}, before it starts at line {from}."
            ),
            edits: Vec::new(),
        });
    }

    // Cut on a whole line, and name the line it stopped on: `start_line` on the next call continues
    // exactly where this one ran out.
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0usize;
    let mut next = None;
    for (i, line) in lines[from - 1..to].iter().enumerate() {
        let cost = line.len() + 1;
        if used + cost > MAX_RESULT_BYTES {
            next = Some(from + i);
            break;
        }
        used += cost;
        kept.push(line);
    }
    let last = from + kept.len() - 1;

    if kept.is_empty() {
        return Ok(Executed {
            summary: Summary::said(
                SummaryKey::LineTooLong,
                json!({ "line": from }),
                format!("line {from} alone passes the output cap"),
            ),
            content: format!(
                "Line {from} of {label} is longer than the {} bytes this build sends back, so nothing of \
                 it is shown. Search for the words instead of reading the line.",
                MAX_RESULT_BYTES
            ),
            edits: Vec::new(),
        });
    }
    let whole = from == 1 && last >= total;
    let Some(stopped) = next else {
        return Ok(Executed {
            summary: if whole {
                Summary::said(
                    SummaryKey::Lines,
                    json!({ "count": total }),
                    format!("{total} lines"),
                )
            } else {
                Summary::said(
                    SummaryKey::LineRange,
                    json!({ "from": from, "to": last, "total": total }),
                    format!("lines {from}–{last} of {total}"),
                )
            },
            content: kept.join("\n"),
            edits: Vec::new(),
        });
    };
    Ok(Executed {
        summary: Summary::said(
            SummaryKey::LineRangeCapped,
            json!({ "from": from, "to": last, "total": total }),
            format!("lines {from}–{last} of {total}, capped here"),
        ),
        content: format!(
            "{}\n…output capped at {} bytes: line {stopped} of {total} and everything after it are not \
             shown. Ask again with start_line={stopped} to continue.",
            kept.join("\n"),
            MAX_RESULT_BYTES
        ),
        edits: Vec::new(),
    })
}

/// List a directory, one entry per line, directories with a trailing `/`.
///
/// What the sandbox protects and what this file skips are counted rather than dropped silently: a
/// listing that hides a secret with nothing said about it teaches the model that the workspace is
/// smaller than it is.
fn list_dir(tool: &str, arguments: &Value, policy: &SandboxPolicy) -> Result<Executed, ToolsError> {
    let label = required(tool, arguments, "path")?.to_string();
    let path = resolve(policy, &label, Access::Read)?;
    let show_hidden = flag_arg(arguments, "include_hidden");

    let mut entries = Vec::new();
    let mut held_back = 0usize;
    let dir = fs::read_dir(&path).map_err(|e| io_error(tool, &label, &e))?;
    for entry in dir {
        let entry = entry.map_err(|e| io_error(tool, &label, &e))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if (!show_hidden && name.starts_with('.'))
            || SKIP_DIRS.contains(&name.as_str())
            || policy.is_protected(&entry.path())
        {
            held_back += 1;
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let size = if is_dir {
            0
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or_default()
        };
        entries.push((name, is_dir, size));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let listed = entries.len().min(MAX_LISTED);
    let mut lines: Vec<String> = entries[..listed]
        .iter()
        .map(|(name, is_dir, size)| {
            if *is_dir {
                format!("{name}/")
            } else {
                format!("{name} ({})", human_bytes(*size))
            }
        })
        .collect();
    if listed < entries.len() {
        lines.push(format!(
            "…{} more entries are not listed.",
            entries.len() - listed
        ));
    }
    if held_back > 0 {
        lines.push(format!(
            "…{held_back} entries are held back (hidden, protected, or build trees)."
        ));
    }
    Ok(Executed {
        summary: Summary::said(
            SummaryKey::EntriesShown,
            json!({ "listed": listed, "total": entries.len() + held_back }),
            format!("{listed} of {} shown", entries.len() + held_back),
        ),
        content: if lines.is_empty() {
            format!("{label} holds nothing that may be listed.")
        } else {
            lines.join("\n")
        },
        edits: Vec::new(),
    })
}

/// Match file paths against a glob, under `path` or the whole workspace.
fn find_files(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
) -> Result<Executed, ToolsError> {
    let pattern = required(tool, arguments, "pattern")?.to_string();
    let glob = compile(tool, "pattern", &pattern, glob::Pattern::new)?;
    let root = optional_root(arguments, policy)?;
    // A pattern that names a separator has to match the whole relative path and respect its `/`s; one
    // that does not is a file name, and finds the file wherever it lives in the tree.
    let literal_separator = pattern.contains('/');
    let opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: literal_separator,
        require_literal_leading_dot: false,
    };

    let mut skipped = 0usize;
    let files = walk(policy, &root, &mut skipped);
    let mut found: Vec<String> = Vec::new();
    for file in &files {
        let rel = relative(policy, file);
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if glob.matches_with(&rel, opts) || (!literal_separator && glob.matches_with(&name, opts)) {
            found.push(rel);
        }
    }

    let count = found.len();
    let mut content = found[..count.min(MAX_LISTED)].join("\n");
    if count > MAX_LISTED {
        content.push_str(&format!(
            "\n…{} matches are not listed.",
            count - MAX_LISTED
        ));
    }
    Ok(Executed {
        summary: if count == 1 {
            Summary::said(
                SummaryKey::FileMatches,
                json!({ "pattern": pattern }),
                format!("1 file matches {pattern}"),
            )
        } else {
            Summary::said(
                SummaryKey::FilesMatch,
                json!({ "count": count, "pattern": pattern }),
                format!("{count} files match {pattern}"),
            )
        },
        content: if content.is_empty() {
            format!("No file in the workspace matches {pattern}.")
        } else {
            with_skips(content, skipped)
        },
        edits: Vec::new(),
    })
}

/// Search file contents by regular expression, with the file, the line number and the line.
fn search_content(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
) -> Result<Executed, ToolsError> {
    let pattern = required(tool, arguments, "pattern")?.to_string();
    let re = regex::RegexBuilder::new(&pattern)
        .case_insensitive(flag_arg(arguments, "ignore_case"))
        .build()
        .map_err(|e| ToolsError::InvalidArguments {
            tool: tool.to_string(),
            reason: format!("\"{pattern}\" is not a valid regular expression: {e}"),
        })?;
    let root = optional_root(arguments, policy)?;
    let filter = match text_arg(arguments, "glob") {
        Some(raw) => Some(compile(tool, "glob", raw, glob::Pattern::new)?),
        None => None,
    };
    let context = number_arg(arguments, "context_lines").unwrap_or(0).min(20);

    let mut skipped = 0usize;
    let files = walk(policy, &root, &mut skipped);
    let mut lines: Vec<String> = Vec::new();
    let mut matches = 0usize;
    let mut hit_files = 0usize;
    let mut capped = false;

    for file in &files {
        let rel = relative(policy, file);
        if filter.as_ref().is_some_and(|g| !g.matches(&rel)) {
            continue;
        }
        // A file this read cannot open — a permission, a socket, a bad UTF-8 boundary — is simply not
        // in the answer, rather than the reason a search of the whole workspace fails.
        let Ok(Some(text)) = read_text(tool, file) else {
            continue;
        };
        let owned: Vec<&str> = text.lines().collect();
        // The matching lines first, and only then how they are printed: neighbouring hits have to
        // share their context instead of repeating it, so a file with three close matches reads as one
        // block the way `grep -C` gives it.
        let hits: Vec<usize> = owned
            .iter()
            .enumerate()
            .filter(|(_, line)| re.is_match(line))
            .map(|(i, _)| i)
            .collect();
        if hits.is_empty() {
            continue;
        }
        hit_files += 1;
        let room = MAX_MATCHES - matches;
        let shown = hits.len().min(room);
        matches += shown;
        push_block(&mut lines, &rel, &owned, &hits[..shown], context);
        if shown < hits.len() {
            capped = true;
            break;
        }
    }

    let mut content = lines.join("\n");
    if capped {
        content.push_str(&format!(
            "\n…stopped at {MAX_MATCHES} matching lines. Narrow the pattern, the glob, or the path."
        ));
    }
    Ok(Executed {
        summary: match (matches, hit_files) {
            (0, _) => Summary::said(
                SummaryKey::NoMatches,
                json!({ "pattern": pattern }),
                format!("nothing matches {pattern}"),
            ),
            (1, 1) => Summary::said(SummaryKey::MatchInFile, json!({}), "1 match in 1 file"),
            (1, files) => Summary::said(
                SummaryKey::MatchInFiles,
                json!({ "files": files }),
                format!("1 match in {files} files"),
            ),
            (n, 1) => Summary::said(
                SummaryKey::MatchesInFile,
                json!({ "count": n }),
                format!("{n} matches in 1 file"),
            ),
            (n, files) => Summary::said(
                SummaryKey::MatchesInFiles,
                json!({ "count": n, "files": files }),
                format!("{n} matches in {files} files"),
            ),
        },
        content: if content.is_empty() {
            format!("No line in the workspace matches {pattern}.")
        } else {
            with_skips(content, skipped)
        },
        edits: Vec::new(),
    })
}

/// Replace an exact string in one workspace file, and report what it cost in lines.
///
/// Three things refuse the write before anything touches the disk: the file has to be text this tool
/// can read, the text named has to be present, and it has to be present once unless the call asked for
/// every copy. An ambiguous match is the failure a model makes most often and a person wants least,
/// so the call names which of the three it hit rather than writing its guess.
///
/// The sandbox is not rechecked here. [`crate::authorize`] resolves every path argument against the
/// policy before an executor runs, which is where a write outside the workspace, into `.git`, or into
/// a protected name is refused — out loud, in the sentence the row and the model both get.
fn edit_file(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
) -> Result<Executed, ToolsError> {
    let label = required(tool, arguments, "path")?.to_string();
    let path = resolve(policy, &label, Access::Write)?;
    let old = required(tool, arguments, "old_string")?;
    let new = required(tool, arguments, "new_string")?;
    let refuse = |reason: String| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason,
    };
    if old.is_empty() {
        return Err(refuse(
            "\"old_string\" is empty, which every position in the file matches.".to_string(),
        ));
    }
    let Some(text) = read_text(tool, &path)? else {
        return Err(refuse(format!(
            "{label} is not text this tool can read, so it was not edited."
        )));
    };

    let found = text.matches(old).count();
    let every = flag_arg(arguments, "replace_all");
    if found == 0 {
        return Err(refuse(format!(
            "{label} holds nothing matching \"old_string\", so nothing was written. Read the file and \
             copy the line as it stands."
        )));
    }
    if found > 1 && !every {
        return Err(refuse(format!(
            "{label} holds {found} copies of \"old_string\" and the call named one, so nothing was \
             written. Include enough of the lines around it to match once, or set replace_all."
        )));
    }

    let times = if every { found } else { 1 };
    let edited = if every {
        text.replace(old, new)
    } else {
        text.replacen(old, new, 1)
    };
    fs::write(&path, &edited).map_err(|e| io_error(tool, &label, &e))?;

    let (removed, added) = changed_lines(old, new);
    let (at, hunk) = hunk(&text, old, new);
    let edit = Edit {
        path: relative(policy, &path),
        added: (added * times) as u32,
        removed: (removed * times) as u32,
        occurrences: times as u32,
        hunks: vec![EditHunk {
            at: at as u32,
            lines: hunk.clone(),
        }],
    };
    let summary = if times > 1 {
        Summary::said(
            SummaryKey::CopiesReplaced,
            json!({ "count": times }),
            format!("{times} copies replaced"),
        )
    } else {
        Summary::said(SummaryKey::CopyReplaced, json!({}), "1 copy replaced")
    };
    Ok(Executed {
        content: format!(
            "Edited {} (+{} −{}): {}\n{label}, at line {at}:\n{hunk}",
            edit.path, edit.added, edit.removed, summary.text,
        ),
        summary,
        edits: vec![edit],
    })
}

/// How much of each stream a command may keep while it runs. Twice the result budget, because stdout
/// and stderr are merged and then cut to one: the tail of a build is where its errors are, so the
/// bytes kept have to be the last ones rather than the first.
const SHELL_KEEP_BYTES: usize = MAX_RESULT_BYTES * 2;

/// Run one command in the workspace, with the switch that admits it already checked in [`authorize`].
///
/// This is the one executor whose reach the path sandbox cannot bound: the command runs under the
/// app's own user, so the only honest statement about it is the toggle that turned it on. What this
/// function adds is the part the toggle does not: a fixed starting directory, a deadline that kills
/// the whole process group rather than its first process, and the tail of the output instead of all
/// of it. A nonzero exit is a result, not a tool failure — the model asked what the command would
/// say, and the exit status is part of the answer.
fn terminal(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
    watcher: Option<&Watcher>,
) -> Result<Executed, ToolsError> {
    let command = required(tool, arguments, "command")?.trim().to_string();
    let refuse = |reason: String| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason,
    };
    if command.is_empty() {
        return Err(refuse(
            "\"command\" is empty, so nothing was run".to_string(),
        ));
    }
    let cwd = match text_arg(arguments, "cwd") {
        Some(raw) => resolve(policy, raw, Access::Write)?,
        None => policy.workspace_root.clone(),
    };
    if !cwd.is_dir() {
        return Err(refuse(format!(
            "{} is not a directory in this workspace",
            relative(policy, &cwd)
        )));
    }
    let seconds = number_arg(arguments, "timeout_seconds")
        .unwrap_or(policy.shell_timeout_secs as usize)
        .clamp(1, 600) as u64;

    let mut child = shell_command(&command, &cwd)
        .spawn()
        .map_err(|e| io_error(tool, &command, &e))?;
    // Both pipes drain on their own thread: a command that fills one while the loop waits on the
    // other would deadlock before it ever reached its deadline. The watcher rides each thread rather
    // than the wait loop, so output is reported as it arrives instead of only once the command ends.
    let stdout = child
        .stdout
        .take()
        .map(|pipe| drain_mode(pipe, Capture::Tail, watcher.cloned()));
    let stderr = child
        .stderr
        .take()
        .map(|pipe| drain_mode(pipe, Capture::Tail, watcher.cloned()));
    let started = Instant::now();
    let ended = wait_limited(&mut child, Duration::from_secs(seconds), tool)?;
    let out = stdout.and_then(|t| t.join().ok()).unwrap_or_default();
    let err = stderr.and_then(|t| t.join().ok()).unwrap_or_default();
    let elapsed = started.elapsed();

    let mut body = String::from_utf8_lossy(&out.bytes).to_string();
    let err_text = String::from_utf8_lossy(&err.bytes);
    if !err_text.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&err_text);
    }
    let total = out.total + err.total;
    let (shown, held) = tail(&body, MAX_RESULT_BYTES);
    let held = held + (total - body.len().min(total));

    let summary = ended_summary(&ended, seconds);

    let mut content = format!("$ {command}\n");
    let dir = relative(policy, &cwd);
    if dir != "." {
        content.push_str(&format!("(in {dir})\n"));
    }
    if shown.is_empty() {
        content.push_str("(no output)\n");
    } else {
        content.push_str(&shown);
        if !shown.ends_with('\n') {
            content.push('\n');
        }
    }
    if held > 0 {
        content.push_str(&format!("…{held} earlier bytes of output are not shown.\n"));
    }
    content.push_str(&format!(
        "[{} in {:.1}s]",
        summary.text,
        elapsed.as_secs_f64()
    ));
    Ok(Executed {
        summary,
        content,
        edits: Vec::new(),
    })
}

/// The command as this platform spells a shell: `sh -c` on Unix, where the child is also made its own
/// group leader so a timeout can kill what it started; `cmd /C` elsewhere.
fn shell_command(command: &str, cwd: &Path) -> Command {
    #[cfg(unix)]
    let mut cmd = {
        use std::os::unix::process::CommandExt;
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c.process_group(0);
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c
    };
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// What the deadline decided.
enum Ended {
    Exited(ExitStatus),
    TimedOut,
}

fn wait_limited(child: &mut Child, limit: Duration, tool: &str) -> Result<Ended, ToolsError> {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Ended::Exited(status)),
            Ok(None) => {}
            Err(e) => {
                return Err(ToolsError::Io {
                    tool: tool.into(),
                    path: "command".into(),
                    reason: e.to_string(),
                })
            }
        }
        if Instant::now() >= deadline {
            kill_group(child);
            let _ = child.wait();
            return Ok(Ended::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The one sentence a run's exit leaves: a code, a deadline, or the signal that ended it. Shared so a
/// project runner and the shell cannot describe the same ending two different ways.
fn ended_summary(ended: &Ended, seconds: u64) -> Summary {
    match ended {
        Ended::TimedOut => Summary::said(
            SummaryKey::TerminalTimedOut,
            json!({ "seconds": seconds }),
            format!("stopped after {seconds}s"),
        ),
        Ended::Exited(status) => match status.code() {
            Some(code) => Summary::said(
                SummaryKey::TerminalExit,
                json!({ "code": code }),
                format!("exit {code}"),
            ),
            None => Summary::said(
                SummaryKey::TerminalStopped,
                json!({}),
                "stopped by a signal",
            ),
        },
    }
}

/// Take the whole process group down with the shell. The group id is the shell's own pid because
/// [`shell_command`] started it as a leader; killing only the shell would leave a `sleep` or a build
/// running after the run had already reported it stopped.
#[cfg(unix)]
fn kill_group(child: &mut Child) {
    let pid = child.id() as i32;
    // SAFETY: `kill` takes plain integers; the negative id names the process group, and a group that
    // has already exited fails harmlessly with ESRCH.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_group(child: &mut Child) {
    let _ = child.kill();
}

/// A pipe's bytes, with the head dropped as it overflows and the full count kept, so the answer can
/// say how much of the output it left out rather than pretending the command printed only this.
#[derive(Default)]
struct Drained {
    bytes: Vec<u8>,
    total: usize,
}

/// Which end of a stream the capture window keeps. A command's errors are at its end, so a terminal
/// or a build keeps the tail; a git payload is a list whose header names the files, so a read keeps
/// the head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    Head,
    Tail,
}

/// How a command's streams are read: which end the answer keeps, and the watcher that forwards the
/// bytes to the window while the command is still running. They are one choice at every call —
/// a watched run is a shell command or a check, which keeps its tail — so they travel together.
#[derive(Clone, Copy)]
struct Reporting<'w> {
    capture: Capture,
    watcher: Option<&'w Watcher>,
}

impl Reporting<'_> {
    /// A read whose payload starts with the part worth keeping: git's.
    fn head() -> Self {
        Self {
            capture: Capture::Head,
            watcher: None,
        }
    }
}

fn drain_mode(
    mut pipe: impl Read + Send + 'static,
    capture: Capture,
    watcher: Option<Watcher>,
) -> std::thread::JoinHandle<Drained> {
    std::thread::spawn(move || {
        let mut kept = Drained::default();
        let mut buf = [0u8; 8 * 1024];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    kept.total += n;
                    // The watcher sees every byte the pipe gave, including the ones the capture window
                    // is about to drop: a build's progress line is worth showing long before the
                    // window that keeps the error text would still hold it.
                    if let Some(watch) = watcher.as_ref() {
                        watch(&buf[..n]);
                    }
                    match capture {
                        Capture::Tail => {
                            kept.bytes.extend_from_slice(&buf[..n]);
                            if kept.bytes.len() > SHELL_KEEP_BYTES {
                                let cut = kept.bytes.len() - SHELL_KEEP_BYTES;
                                kept.bytes.drain(..cut);
                            }
                        }
                        Capture::Head if kept.bytes.len() >= SHELL_KEEP_BYTES => {}
                        Capture::Head => {
                            let room = SHELL_KEEP_BYTES - kept.bytes.len();
                            kept.bytes.extend_from_slice(&buf[..n.min(room)]);
                        }
                    }
                }
            }
        }
        kept
    })
}

/// The last `max` bytes of a string on a character boundary, and how many bytes were dropped.
fn tail(text: &str, max: usize) -> (String, usize) {
    if text.len() <= max {
        return (text.to_string(), 0);
    }
    let mut cut = text.len() - max;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    (text[cut..].to_string(), cut)
}

/// The first `max` bytes of a string on a character boundary, and how many bytes were dropped.
fn head(text: &str, max: usize) -> (String, usize) {
    if text.len() <= max {
        return (text.to_string(), 0);
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    (text[..cut].to_string(), text.len() - cut)
}

/// How long a project's own tests or build may run. A cold dependency build is minutes of work rather
/// than seconds, so this is sized for a first build; the deadline still takes the whole process group
/// down, and the answer says it was stopped rather than pretending it finished.
const PROJECT_BUILD_TIMEOUT_SECS: u64 = 300;
const PROJECT_LINT_TIMEOUT_SECS: u64 = 180;

/// How long a git read may take. These read the index and the object store rather than the network,
/// so a minute is a ceiling, not a budget to spend.
const GIT_TIMEOUT_SECS: u64 = 60;

/// What the three project runners ask a workspace to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectKind {
    Tests,
    Build,
    Lint,
}

impl ProjectKind {
    fn timeout(self) -> u64 {
        match self {
            Self::Lint => PROJECT_LINT_TIMEOUT_SECS,
            _ => PROJECT_BUILD_TIMEOUT_SECS,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Tests => "tests",
            Self::Build => "build",
            Self::Lint => "lint",
        }
    }
}

/// One argument set a manifest earned: the program, its argv, and the file that chose it. `argv` is
/// the whole point of this shape — nothing here is ever handed to `sh -c`, so a package name or a
/// filter the model typed remains one argument rather than becoming syntax.
#[derive(Debug)]
struct Project {
    manifest: &'static str,
    program: String,
    args: Vec<String>,
}

/// What the call asked the manifest for. A field the chosen toolchain cannot honour is refused rather
/// than dropped: a model that asked for a filtered run and silently got the whole suite would report
/// the wrong thing as checked.
struct Wanted<'a> {
    kind: ProjectKind,
    target: Option<&'a str>,
    filter: Option<&'a str>,
    release: bool,
    paths: &'a [String],
}

/// Run the workspace's tests, build or lint through the manifest at its root.
///
/// The command is derived, never typed by the model: the model asks for "the tests", and this reads
/// Cargo.toml, package.json, go.mod, the Python manifests or the Makefile to find what that means
/// here. The one switch that admits it is the user's Terminal access — declared code runs with their
/// permissions — and [`crate::authorize`] has already refused the call if it is off.
fn run_project(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
    kind: ProjectKind,
    watcher: Option<&Watcher>,
) -> Result<Executed, ToolsError> {
    let target = word(tool, arguments, "target")?;
    let filter = word(tool, arguments, "filter")?;
    let paths = paths_argument(tool, arguments, policy)?;
    let wanted = Wanted {
        kind,
        target: target.as_deref(),
        filter: filter.as_deref(),
        release: flag_arg(arguments, "release"),
        paths: &paths,
    };
    let project = detect(&policy.workspace_root, &wanted).map_err(ToolsError::NotConfigured)?;
    let shown = display_command(&project.program, &project.args);
    let seconds = kind.timeout();
    let finished = run_argv(
        tool,
        &project.program,
        &project.args,
        &policy.workspace_root,
        seconds,
        Reporting {
            capture: Capture::Tail,
            watcher,
        },
        &[],
    )?;
    let summary = ended_summary(&finished.ended, seconds);
    let (body, lost) = captured_text(&finished);
    let (output, held) = tail(&body, MAX_RESULT_BYTES);
    let content = command_report(
        &shown,
        project.manifest,
        &output,
        held + lost,
        finished.elapsed,
        &summary.text,
    );
    Ok(Executed {
        summary,
        content,
        edits: Vec::new(),
    })
}

/// The checks this workspace declares, in the order a run should try them: the tests when the
/// manifest names them, then the build, then the linter. A run that changed files is measured by the
/// first of these, because it is the one that says whether the change works rather than whether it
/// parses; the other two stay available to the model through their own calls.
pub fn declared_checks(root: &Path) -> Vec<&'static str> {
    [
        ("run_tests", ProjectKind::Tests),
        ("run_build", ProjectKind::Build),
        ("run_linter", ProjectKind::Lint),
    ]
    .into_iter()
    .filter(|(_, kind)| {
        detect(
            root,
            &Wanted {
                kind: *kind,
                target: None,
                filter: None,
                release: false,
                paths: &[],
            },
        )
        .is_ok()
    })
    .map(|(tool, _)| tool)
    .collect()
}

/// Choose the runner the workspace's own manifest names. Root manifests only: a workspace can hold a
/// dozen packages, and a command guessed from the wrong one is a round spent on the wrong answer.
fn detect(root: &Path, want: &Wanted) -> Result<Project, String> {
    if root.join("Cargo.toml").is_file() {
        return cargo_project(want);
    }
    if root.join("package.json").is_file() {
        return node_project(root, want);
    }
    if root.join("go.mod").is_file() {
        return go_project(want);
    }
    if python_manifest(root) {
        return python_project(root, want);
    }
    if root.join("Makefile").is_file() {
        return make_project(root, want);
    }
    Err(format!(
        "no Cargo.toml, package.json, go.mod, pyproject.toml or Makefile at the workspace root, so \
         nothing there says how to run its {}",
        want.kind.name()
    ))
}

fn cargo_project(want: &Wanted) -> Result<Project, String> {
    let mut args = match want.kind {
        ProjectKind::Tests => vec!["test".to_string()],
        ProjectKind::Build => vec!["build".to_string()],
        ProjectKind::Lint => vec!["clippy".to_string(), "--all-targets".to_string()],
    };
    if let Some(target) = want.target {
        args.push("-p".into());
        args.push(target.to_string());
    }
    match want.kind {
        ProjectKind::Tests => {
            refuse_option(
                want.release,
                "\"release\" applies to run_build; tests run in the profile \
                                         the manifest names",
            )?;
            refuse_paths(
                want,
                "the Cargo test runner takes a `filter`, not file paths",
            )?;
            if let Some(filter) = want.filter {
                args.push(filter.to_string());
            }
        }
        ProjectKind::Build => {
            refuse_paths(want, "the Cargo build runner takes no file paths")?;
            if want.release {
                args.push("--release".into());
            }
            if want.filter.is_some() {
                return Err("\"filter\" picks a test, and run_build runs no tests".into());
            }
        }
        ProjectKind::Lint => {
            refuse_paths(
                want,
                "the Cargo linter checks the whole workspace package by package; leave \"paths\" out",
            )?;
            if want.filter.is_some() || want.release {
                return Err(
                    "\"filter\" and \"release\" are for tests and builds; the linter takes \
                            neither"
                        .into(),
                );
            }
        }
    }
    Ok(Project {
        manifest: "Cargo.toml",
        program: "cargo".into(),
        args,
    })
}

fn node_project(root: &Path, want: &Wanted) -> Result<Project, String> {
    let raw = fs::read_to_string(root.join("package.json"))
        .map_err(|e| format!("package.json could not be read ({e})"))?;
    let manifest: Value =
        serde_json::from_str(&raw).map_err(|e| format!("package.json is not valid JSON ({e})"))?;
    let script = match want.kind {
        ProjectKind::Tests => "test",
        ProjectKind::Build => "build",
        ProjectKind::Lint => "lint",
    };
    let declared = manifest
        .get("scripts")
        .and_then(|scripts| scripts.get(script))
        .and_then(Value::as_str)
        .is_some();
    if !declared {
        return Err(format!(
            "package.json declares no \"{script}\" script, so there is no command to run"
        ));
    }
    if want.target.is_some() {
        return Err(
            "\"target\" names a Cargo package; a package.json project runs the script it \
                    declares"
                .into(),
        );
    }
    refuse_option(
        want.release,
        "\"release\" is a Cargo profile; a package.json project builds what its script builds",
    )?;
    let mut args = vec!["run".to_string(), script.to_string()];
    match want.kind {
        ProjectKind::Tests => {
            if !want.paths.is_empty() {
                return Err("\"paths\" narrows a lint run; a test script takes a `filter`".into());
            }
            if let Some(filter) = want.filter {
                args.push("--".into());
                args.push(filter.to_string());
            }
        }
        ProjectKind::Lint => {
            if want.filter.is_some() {
                return Err("\"filter\" narrows a test run, and this call is not one".into());
            }
            if !want.paths.is_empty() {
                args.push("--".into());
                args.extend(want.paths.iter().cloned());
            }
        }
        ProjectKind::Build => {
            if want.filter.is_some() || !want.paths.is_empty() {
                return Err(
                    "\"filter\" and \"paths\" narrow a test or lint run; run_build takes \
                            neither"
                        .into(),
                );
            }
        }
    }
    Ok(Project {
        manifest: "package.json",
        program: program(package_manager(root)),
        args,
    })
}

fn go_project(want: &Wanted) -> Result<Project, String> {
    if want.target.is_some() {
        return Err(
            "\"target\" names a Cargo package; a Go module is named by its import path".into(),
        );
    }
    refuse_option(
        want.release,
        "\"release\" is a Cargo profile; go build has no release flag",
    )?;
    let mut args: Vec<String> = match want.kind {
        ProjectKind::Tests => vec!["test".to_string()],
        ProjectKind::Build => vec!["build".to_string()],
        ProjectKind::Lint => vec!["vet".to_string()],
    };
    match want.kind {
        ProjectKind::Tests => {
            if let Some(filter) = want.filter {
                args.push("-run".into());
                args.push(filter.to_string());
            }
            args.push("./...".into());
        }
        ProjectKind::Build => {
            if want.filter.is_some() || !want.paths.is_empty() {
                return Err(
                    "\"filter\" and \"paths\" are for tests and lint; go build takes \
                            neither"
                        .into(),
                );
            }
            args.push("./...".into());
        }
        ProjectKind::Lint => {
            if want.filter.is_some() {
                return Err("\"filter\" narrows a test run, and this call is not one".into());
            }
            if want.paths.is_empty() {
                args.push("./...".into());
            } else {
                args.extend(want.paths.iter().cloned());
            }
        }
    }
    Ok(Project {
        manifest: "go.mod",
        program: "go".into(),
        args,
    })
}

fn python_manifest(root: &Path) -> bool {
    [
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "requirements.txt",
        "pytest.ini",
        "tox.ini",
        "conftest.py",
    ]
    .iter()
    .any(|name| root.join(name).is_file())
}

fn ruff_configured(root: &Path) -> bool {
    if root.join("ruff.toml").is_file() || root.join(".ruff.toml").is_file() {
        return true;
    }
    fs::read_to_string(root.join("pyproject.toml"))
        .map(|text| text.contains("[tool.ruff]"))
        .unwrap_or(false)
}

fn python_project(root: &Path, want: &Wanted) -> Result<Project, String> {
    if want.target.is_some() {
        return Err("\"target\" names a Cargo package; a pytest run takes a `filter`".into());
    }
    refuse_option(
        want.release,
        "\"release\" is a Cargo profile; pytest has no release flag",
    )?;
    match want.kind {
        ProjectKind::Tests => {
            let mut args = vec!["-m".to_string(), "pytest".to_string()];
            if let Some(filter) = want.filter {
                args.push("-k".into());
                args.push(filter.to_string());
            }
            args.extend(want.paths.iter().cloned());
            Ok(Project {
                manifest: "pyproject.toml",
                program: python_program().into(),
                args,
            })
        }
        ProjectKind::Build => Err(
            "this workspace is Python, which has no single build command to \
                                   run"
            .into(),
        ),
        ProjectKind::Lint => {
            if want.filter.is_some() {
                return Err("\"filter\" narrows a test run, and this call is not one".into());
            }
            if !ruff_configured(root) {
                return Err(
                    "this workspace is Python and configures no linter: no ruff.toml and no \
                            [tool.ruff] in pyproject.toml"
                        .into(),
                );
            }
            let mut args = vec!["-m".to_string(), "ruff".to_string(), "check".to_string()];
            if want.paths.is_empty() {
                args.push(".".into());
            } else {
                args.extend(want.paths.iter().cloned());
            }
            Ok(Project {
                manifest: "pyproject.toml",
                program: python_program().into(),
                args,
            })
        }
    }
}

fn make_project(root: &Path, want: &Wanted) -> Result<Project, String> {
    if want.target.is_some() || want.release || want.filter.is_some() || !want.paths.is_empty() {
        return Err(
            "a Makefile target takes no filter, target, release or paths; its recipe decides \
                    what to run"
                .into(),
        );
    }
    let text = fs::read_to_string(root.join("Makefile"))
        .map_err(|e| format!("Makefile could not be read ({e})"))?;
    let targets = make_targets(&text);
    let target = match want.kind {
        ProjectKind::Tests => "test",
        ProjectKind::Lint => "lint",
        ProjectKind::Build => {
            if targets.iter().any(|name| name == "build") {
                "build"
            } else {
                "all"
            }
        }
    };
    if !targets.iter().any(|name| name == target) {
        return Err(format!("the Makefile declares no \"{target}\" target"));
    }
    Ok(Project {
        manifest: "Makefile",
        program: "make".into(),
        args: vec![target.to_string()],
    })
}

/// The names a Makefile declares as targets. A recipe line is indented with a tab and is not one, and
/// a variable assignment is not one either — which is all the certainty a runner needs to avoid
/// invoking `make` on a target that does not exist.
fn make_targets(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| !line.starts_with('\t') && !line.trim_start().starts_with('#'))
        .filter_map(|line| line.split_once(':'))
        .map(|(head, _)| head.trim())
        .filter(|name| {
            !name.is_empty() && !name.contains(char::is_whitespace) && !name.contains('=')
        })
        .map(str::to_string)
        .collect()
}

fn package_manager(root: &Path) -> &'static str {
    let has = |name: &str| root.join(name).is_file();
    if has("pnpm-lock.yaml") {
        "pnpm"
    } else if has("yarn.lock") {
        "yarn"
    } else if has("bun.lockb") || has("bun.lock") {
        "bun"
    } else {
        "npm"
    }
}

/// The program name as this platform spells it. The JavaScript package managers are `.cmd` shims on
/// Windows, and `Command` does not go through a shell to find them.
fn program(name: &str) -> String {
    #[cfg(windows)]
    if matches!(name, "npm" | "pnpm" | "yarn" | "bun") {
        return format!("{name}.cmd");
    }
    name.to_string()
}

fn python_program() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
}

/// One argument in the model's own words. It has to stay one argument: a value that starts with `-`
/// would become a flag, and whitespace or a quote in a name would only matter to a shell this never
/// uses. The set is the characters a package, test or path name actually needs.
fn word(tool: &str, arguments: &Value, key: &str) -> Result<Option<String>, ToolsError> {
    let Some(value) = arguments.get(key) else {
        return Ok(None);
    };
    let Some(raw) = value.as_str() else {
        return Err(bad_arguments(tool, format!("\"{key}\" must be a string")));
    };
    let word = raw.trim();
    if word.is_empty() {
        return Ok(None);
    }
    if word.len() > 120
        || word.starts_with('-')
        || !word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '/'))
    {
        return Err(bad_arguments(
            tool,
            format!(
                "\"{key}\" must be one plain word — letters, digits, `_ - . : /` — and may not start \
                 with `-`"
            ),
        ));
    }
    Ok(Some(word.to_string()))
}

/// The `paths` array, each entry resolved against the sandbox before it can reach an argv. The paths
/// are handed back workspace-relative, which is the spelling a linter run from the root expects.
fn paths_argument(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
) -> Result<Vec<String>, ToolsError> {
    let Some(value) = arguments.get("paths") else {
        return Ok(Vec::new());
    };
    let Some(list) = value.as_array() else {
        return Err(bad_arguments(tool, "\"paths\" must be an array of strings"));
    };
    if list.len() > 32 {
        return Err(bad_arguments(
            tool,
            format!(
                "\"paths\" names {} entries; one call may name at most 32",
                list.len()
            ),
        ));
    }
    list.iter()
        .map(|entry| {
            let Some(raw) = entry.as_str() else {
                return Err(bad_arguments(tool, "\"paths\" must be an array of strings"));
            };
            let raw = raw.trim();
            if raw.is_empty() || raw.starts_with('-') {
                return Err(bad_arguments(
                    tool,
                    "\"paths\" entries must name a path and may not start with `-`",
                ));
            }
            let resolved = policy.resolve_path(Path::new(raw), Access::Read)?;
            Ok(policy.display_path(&resolved))
        })
        .collect()
}

fn bad_arguments(tool: &str, reason: impl Into<String>) -> ToolsError {
    ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason: reason.into(),
    }
}

fn refuse_option(asked: bool, reason: &str) -> Result<(), String> {
    if asked {
        Err(reason.to_string())
    } else {
        Ok(())
    }
}

fn refuse_paths(want: &Wanted, reason: &str) -> Result<(), String> {
    if want.paths.is_empty() {
        Ok(())
    } else {
        Err(reason.to_string())
    }
}

/// A program, its argv, and the environment every runner shares: no pager, no colour escapes, and
/// `CI=1` so a test script that watches by default exits instead of hanging until its deadline.
fn structured_command(
    program: &str,
    args: &[String],
    cwd: &Path,
    envs: &[(&str, &str)],
) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own group, so a deadline takes the compiler and the test binaries down with the runner.
        cmd.process_group(0);
    }
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CI", "1")
        .env("NO_COLOR", "1");
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd
}

/// What one structured command left behind.
struct Finished {
    ended: Ended,
    out: Drained,
    err: Drained,
    elapsed: Duration,
}

fn run_argv(
    tool: &str,
    program: &str,
    args: &[String],
    cwd: &Path,
    seconds: u64,
    reporting: Reporting,
    envs: &[(&str, &str)],
) -> Result<Finished, ToolsError> {
    let shown = display_command(program, args);
    let mut child = structured_command(program, args, cwd, envs)
        .spawn()
        .map_err(|e| {
            ToolsError::NotConfigured(format!(
                "{shown} could not be started ({e}); {program} may not be installed on this machine"
            ))
        })?;
    let out = child
        .stdout
        .take()
        .map(|pipe| drain_mode(pipe, reporting.capture, reporting.watcher.cloned()));
    let err = child
        .stderr
        .take()
        .map(|pipe| drain_mode(pipe, reporting.capture, reporting.watcher.cloned()));
    let started = Instant::now();
    let ended = wait_limited(&mut child, Duration::from_secs(seconds), tool)?;
    let out = out
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();
    let err = err
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();
    Ok(Finished {
        ended,
        out,
        err,
        elapsed: started.elapsed(),
    })
}

/// A command line for the model and the row: the program and each argument, with an argument that
/// carries a space quoted so the line reads the way it ran rather than as two arguments.
fn display_command(program: &str, args: &[String]) -> String {
    let mut parts = vec![program.to_string()];
    for arg in args {
        if arg.chars().any(char::is_whitespace) {
            parts.push(format!("\"{arg}\""));
        } else {
            parts.push(arg.clone());
        }
    }
    parts.join(" ")
}

/// Both streams as one text, and how many bytes the capture window dropped. The two are merged
/// because a compiler puts its errors on one and its progress on the other, and the model has to see
/// them in one answer.
fn captured_text(finished: &Finished) -> (String, usize) {
    let mut body = String::from_utf8_lossy(&finished.out.bytes).to_string();
    let err = String::from_utf8_lossy(&finished.err.bytes);
    if !err.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&err);
    }
    let total = finished.out.total + finished.err.total;
    let dropped = total.saturating_sub(body.len());
    (body, dropped)
}

/// The report a structured command leaves: the line that ran, what chose it, its output, what was
/// left out, and the exit that ended it.
fn command_report(
    command: &str,
    note: &str,
    output: &str,
    held: usize,
    elapsed: Duration,
    status: &str,
) -> String {
    let mut text = format!("$ {command}\n");
    if !note.is_empty() {
        text.push_str(&format!("({note})\n"));
    }
    if output.is_empty() {
        text.push_str("(no output)\n");
    } else {
        text.push_str(output);
        if !output.ends_with('\n') {
            text.push('\n');
        }
    }
    if held > 0 {
        text.push_str(&format!("…{held} bytes of output are not shown.\n"));
    }
    text.push_str(&format!("[{status} in {:.1}s]", elapsed.as_secs_f64()));
    text
}

/// The three reads of the repository: what is changed, what changed, and what was committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitRead {
    Status,
    Diff,
    Log,
}

/// Read the repository with a fixed argv and no switch of its own.
///
/// None of these three can write: `status`, `diff` and `log` report the index and the object store.
/// The flags are the ones that keep a read a read — no external diff driver, no textconv filter, no
/// pager, no optional index lock — so what comes back is git's own text rather than a program the
/// repository named.
fn git_read(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
    kind: GitRead,
) -> Result<Executed, ToolsError> {
    let path = match text_arg(arguments, "path") {
        Some(raw) => {
            let resolved = resolve(policy, raw, Access::Read)?;
            Some(policy.display_path(&resolved))
        }
        None => None,
    };
    let global = [
        "--no-pager",
        "-c",
        "color.ui=false",
        "-c",
        "core.quotepath=false",
    ];
    let mut args: Vec<String> = global.iter().map(|s| s.to_string()).collect();
    match kind {
        GitRead::Status => args.extend(["status", "--porcelain=v1", "--branch"].map(String::from)),
        GitRead::Diff | GitRead::Log => {}
    }
    let envs = [
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_PAGER", "cat"),
    ];
    let detail = match kind {
        GitRead::Status => None,
        GitRead::Diff => {
            let mut diff = args.clone();
            diff.extend(["diff", "--no-ext-diff", "--no-textconv"].map(String::from));
            if flag_arg(arguments, "staged") {
                diff.push("--cached".into());
            }
            if let Some(path) = &path {
                diff.push("--".into());
                diff.push(path.clone());
            }
            Some(diff)
        }
        GitRead::Log => {
            let limit = number_arg(arguments, "limit").unwrap_or(20).clamp(1, 200);
            let mut log = args.clone();
            log.extend(["log", "--oneline", "--decorate", "-n"].map(String::from));
            log.push(limit.to_string());
            if let Some(path) = &path {
                log.push("--".into());
                log.push(path.clone());
            }
            Some(log)
        }
    };
    let (run_args, summary_kind) = match &detail {
        Some(diff) => (diff.clone(), kind),
        None => {
            if let Some(path) = &path {
                args.push("--".into());
                args.push(path.clone());
            }
            (args.clone(), kind)
        }
    };
    // A diff's counts come from a second, tiny read: the patch is capped to what fits in an answer,
    // and counting the visible lines would under-report a change the answer had to cut.
    let counts = if summary_kind == GitRead::Diff {
        let mut numstat = run_args.clone();
        // `--numstat` sits with the flags, before any `--` separator a path introduced.
        let at = numstat
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(numstat.len());
        numstat.insert(at, "--numstat".into());
        Some(run_argv(
            tool,
            "git",
            &numstat,
            &policy.workspace_root,
            GIT_TIMEOUT_SECS,
            Reporting::head(),
            &envs,
        )?)
    } else {
        None
    };
    let shown = display_command("git", &run_args);
    let finished = run_argv(
        tool,
        "git",
        &run_args,
        &policy.workspace_root,
        GIT_TIMEOUT_SECS,
        Reporting::head(),
        &envs,
    )?;
    let stdout = String::from_utf8_lossy(&finished.out.bytes).to_string();
    let (body, lost) = captured_text(&finished);

    if let Ended::Exited(status) = &finished.ended {
        // What git prints is localised, so the workspace's state is asked for directly instead of
        // being read out of stderr: a read that failed is explained only after we know what it was
        // reading. Both probes are asked the way the read itself is, and only after a failure.
        if !status.success() && !git_answers(tool, policy, &["rev-parse", "--is-inside-work-tree"])
        {
            return Ok(Executed {
                summary: Summary::said(SummaryKey::GitNotRepo, json!({}), "not a git repository"),
                content: format!(
                    "{shown}\n\nThis workspace is not inside a git repository, so there is nothing \
                     for git to report."
                ),
                edits: Vec::new(),
            });
        }
        // A repository with no commits has a status and an empty log, not a broken log.
        if !status.success()
            && kind == GitRead::Log
            && !git_answers(tool, policy, &["rev-parse", "--verify", "--quiet", "HEAD"])
        {
            return Ok(Executed {
                summary: Summary::said(SummaryKey::GitCommits, json!({ "count": 0 }), "0 commits"),
                content: format!("{shown}\n\nThe repository has no commits yet."),
                edits: Vec::new(),
            });
        }
        if !status.success() {
            let summary = ended_summary(&finished.ended, GIT_TIMEOUT_SECS);
            let (output, held) = head(&body, MAX_RESULT_BYTES);
            let content = command_report(
                &shown,
                "",
                &output,
                held + lost,
                finished.elapsed,
                &summary.text,
            );
            return Ok(Executed {
                summary,
                content,
                edits: Vec::new(),
            });
        }
    } else {
        let summary = ended_summary(&finished.ended, GIT_TIMEOUT_SECS);
        return Ok(Executed {
            summary: summary.clone(),
            content: format!(
                "{shown}\n\ngit did not answer within {GIT_TIMEOUT_SECS}s and was stopped. The \
                 repository may be very large; a narrower path can answer sooner."
            ),
            edits: Vec::new(),
        });
    }

    match kind {
        GitRead::Status => {
            let changed = stdout
                .lines()
                .filter(|line| !line.trim().is_empty() && !line.starts_with("## "))
                .count();
            let summary = match changed {
                0 => Summary::said(SummaryKey::GitClean, json!({}), "nothing uncommitted"),
                1 => Summary::said(
                    SummaryKey::ChangedFile,
                    json!({ "count": 1 }),
                    "1 file changed",
                ),
                n => Summary::said(
                    SummaryKey::ChangedFiles,
                    json!({ "count": n }),
                    format!("{n} files changed"),
                ),
            };
            let (output, held) = head(&stdout, MAX_RESULT_BYTES);
            let content = if changed == 0 {
                format!(
                    "{shown}\n\nWorking tree clean: no staged, unstaged or untracked changes.\n\
                     [{} in {:.1}s]",
                    summary.text,
                    finished.elapsed.as_secs_f64()
                )
            } else {
                format!(
                    "{}Columns: index then working tree — M modified, A added, D deleted, R \
                     renamed, ?? untracked.\n[{} in {:.1}s]",
                    command_report_body(&shown, &output, held + lost),
                    summary.text,
                    finished.elapsed.as_secs_f64()
                )
            };
            Ok(Executed {
                summary,
                content,
                edits: Vec::new(),
            })
        }
        GitRead::Diff => {
            let counts = counts.expect("a diff's counts were read before its patch");
            let (files, added, removed) = if counts.ended_is_failure() {
                (0, 0, 0)
            } else {
                numstat_counts(&String::from_utf8_lossy(&counts.out.bytes))
            };
            if files == 0 {
                let side = if flag_arg(arguments, "staged") {
                    "staged"
                } else {
                    "unstaged"
                };
                return Ok(Executed {
                    summary: Summary::said(SummaryKey::GitClean, json!({}), "nothing uncommitted"),
                    content: format!("{shown}\n\nNo {side} changes."),
                    edits: Vec::new(),
                });
            }
            let summary = if files == 1 {
                Summary::said(
                    SummaryKey::GitDiffFile,
                    json!({ "added": added, "removed": removed }),
                    format!("1 file, +{added} −{removed}"),
                )
            } else {
                Summary::said(
                    SummaryKey::GitDiff,
                    json!({ "files": files, "added": added, "removed": removed }),
                    format!("{files} files, +{added} −{removed}"),
                )
            };
            let (output, held) = head(&body, MAX_RESULT_BYTES);
            let content = command_report(
                &shown,
                "",
                &output,
                held + lost,
                finished.elapsed,
                &summary.text,
            );
            Ok(Executed {
                summary,
                content,
                edits: Vec::new(),
            })
        }
        GitRead::Log => {
            let count = stdout
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count();
            let summary = if count == 1 {
                Summary::said(SummaryKey::GitCommit, json!({}), "1 commit")
            } else {
                Summary::said(
                    SummaryKey::GitCommits,
                    json!({ "count": count }),
                    format!("{count} commits"),
                )
            };
            let (output, held) = head(&body, MAX_RESULT_BYTES);
            let content = command_report(
                &shown,
                "",
                &output,
                held + lost,
                finished.elapsed,
                &summary.text,
            );
            Ok(Executed {
                summary,
                content,
                edits: Vec::new(),
            })
        }
    }
}

/// A yes/no question for git, asked with the same flags and environment as the read it is explaining.
/// Only used after a read fails, so an ordinary read costs one process rather than two.
fn git_answers(tool: &str, policy: &SandboxPolicy, args: &[&str]) -> bool {
    let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let envs = [
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_PAGER", "cat"),
    ];
    run_argv(
        tool,
        "git",
        &argv,
        &policy.workspace_root,
        GIT_TIMEOUT_SECS,
        Reporting::head(),
        &envs,
    )
    .map(|finished| !finished.ended_is_failure())
    .unwrap_or(false)
}

/// The environment every git call runs in.
///
/// `GIT_TERMINAL_PROMPT=0` is the one that matters for the writes: a call that needs a password
/// must fail rather than wait for one, because a run that hangs on a prompt nobody can see is a run
/// that never finishes.
const GIT_ENV: [(&str, &str); 3] = [
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_PAGER", "cat"),
];

/// How long a push may take. The push is the one git call that talks to a server, and a first push
/// of a big repository is slow; the read deadline would cut it off half way through.
const GIT_PUSH_TIMEOUT_SECS: u64 = 180;

/// Stage the named paths.
///
/// The paths go through the sandbox first: a path outside the workspace is refused here, with the
/// rule that refused it, rather than by git's own complaint about a path outside the repository.
fn git_stage(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
) -> Result<Executed, ToolsError> {
    let named: Vec<&str> = arguments
        .get("paths")
        .and_then(Value::as_array)
        .map(|paths| paths.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if named.is_empty() {
        return Err(ToolsError::InvalidArguments {
            tool: tool.to_string(),
            reason: "\"paths\" must name at least one file to stage".into(),
        });
    }
    let mut argv: Vec<String> = vec!["add".into(), "--".into()];
    let mut shown: Vec<String> = Vec::new();
    for raw in named {
        let resolved = resolve(policy, raw, Access::Write)?;
        argv.push(resolved.display().to_string());
        shown.push(relative(policy, &resolved));
    }
    let command = format!("git add -- {}", shown.join(" "));
    let finished = run_argv(
        tool,
        "git",
        &argv,
        &policy.workspace_root,
        GIT_TIMEOUT_SECS,
        Reporting::head(),
        &GIT_ENV,
    )?;
    let (body, lost) = captured_text(&finished);
    if finished.ended_is_failure() {
        let (summary, content) = git_failed(&command, &body, lost, &finished, GIT_TIMEOUT_SECS);
        return Ok(Executed {
            summary,
            content,
            edits: Vec::new(),
        });
    }
    // The row counts what the index holds, not what was asked for: a path that was already staged
    // does not become a second file, and a path git ignored does not become one at all.
    let count = git_output(tool, policy, &["diff", "--cached", "--name-only"])
        .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or(shown.len());
    let summary = Summary::said(
        SummaryKey::GitStaged,
        json!({ "count": count }),
        format!("{count} files staged"),
    );
    let content = git_said(&command, &body, lost, &finished, &summary.text);
    Ok(Executed {
        summary,
        content,
        edits: Vec::new(),
    })
}

/// Commit what is staged.
///
/// The message goes to git as one argument, so a message with a newline in it stays one message and
/// a message that starts with a dash is still a message. An amend rewrites the last commit, which
/// is why the row says so.
fn git_commit(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
) -> Result<Executed, ToolsError> {
    let message = required(tool, arguments, "message")?.trim().to_string();
    if message.is_empty() {
        return Err(ToolsError::InvalidArguments {
            tool: tool.to_string(),
            reason: "a commit needs a message".into(),
        });
    }
    let amend = flag_arg(arguments, "amend");
    let mut argv: Vec<String> = vec!["commit".into()];
    if amend {
        argv.push("--amend".into());
    }
    argv.push("-m".into());
    argv.push(message.clone());
    let command = if amend {
        format!("git commit --amend -m \"{message}\"")
    } else {
        format!("git commit -m \"{message}\"")
    };
    let finished = run_argv(
        tool,
        "git",
        &argv,
        &policy.workspace_root,
        GIT_TIMEOUT_SECS,
        Reporting::head(),
        &GIT_ENV,
    )?;
    let (body, lost) = captured_text(&finished);
    if finished.ended_is_failure() {
        let (summary, content) = git_failed(&command, &body, lost, &finished, GIT_TIMEOUT_SECS);
        return Ok(Executed {
            summary,
            content,
            edits: Vec::new(),
        });
    }
    let sha = git_output(tool, policy, &["rev-parse", "--short", "HEAD"])
        .map(|text| text.trim().to_string())
        .unwrap_or_default();
    let summary = Summary::said(
        SummaryKey::GitCommitted,
        json!({ "sha": sha }),
        format!("commit {sha}"),
    );
    let content = git_said(&command, &body, lost, &finished, &summary.text);
    Ok(Executed {
        summary,
        content,
        edits: Vec::new(),
    })
}

/// Switch the checkout to a branch, creating it when it is not there yet.
///
/// An existing branch is switched to and a missing one is created from the current commit, so the
/// model does not have to know which of the two the workspace is in. The uncommitted changes travel
/// with the switch; git refuses the ones that cannot, and says so.
fn git_branch(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
) -> Result<Executed, ToolsError> {
    let name = required(tool, arguments, "name")?.trim().to_string();
    if !crate::github::valid_branch(&name) {
        return Err(ToolsError::InvalidArguments {
            tool: tool.to_string(),
            reason: format!("\"{name}\" is not a branch name this tool will hand to git"),
        });
    }
    let start = match text_arg(arguments, "start_point") {
        Some(start) => {
            let start = start.trim();
            if !crate::github::valid_branch(start) {
                return Err(ToolsError::InvalidArguments {
                    tool: tool.to_string(),
                    reason: format!("\"{start}\" is not a branch name this tool will hand to git"),
                });
            }
            Some(start.to_string())
        }
        None => None,
    };
    let reference = format!("refs/heads/{name}");
    let exists = git_answers(
        tool,
        policy,
        &["rev-parse", "--verify", "--quiet", reference.as_str()],
    );
    let argv: Vec<String> = if exists {
        vec!["switch".into(), name.clone()]
    } else {
        let mut argv = vec!["switch".into(), "-c".into(), name.clone()];
        if let Some(start) = &start {
            argv.push(start.clone());
        }
        argv
    };
    let command = display_command("git", &argv);
    let finished = run_argv(
        tool,
        "git",
        &argv,
        &policy.workspace_root,
        GIT_TIMEOUT_SECS,
        Reporting::head(),
        &GIT_ENV,
    )?;
    let (body, lost) = captured_text(&finished);
    if finished.ended_is_failure() {
        let (summary, content) = git_failed(&command, &body, lost, &finished, GIT_TIMEOUT_SECS);
        return Ok(Executed {
            summary,
            content,
            edits: Vec::new(),
        });
    }
    let summary = Summary::said(
        SummaryKey::GitSwitched,
        json!({ "branch": name }),
        format!("on {name}"),
    );
    let content = git_said(&command, &body, lost, &finished, &summary.text);
    Ok(Executed {
        summary,
        content,
        edits: Vec::new(),
    })
}

/// Push the current branch to its remote.
///
/// The credential is git's own: whatever this machine already holds for that remote — a keychain
/// entry, an SSH agent, a helper — is what answers. The app does not ask for a password it did not
/// need. A push that cannot authenticate fails with git's own words, and the row says where a
/// credential would have to be added.
fn git_push(tool: &str, arguments: &Value, policy: &SandboxPolicy) -> Result<Executed, ToolsError> {
    let remote = text_arg(arguments, "remote")
        .unwrap_or("origin")
        .trim()
        .to_string();
    if remote.is_empty() || remote.starts_with('-') {
        return Err(ToolsError::InvalidArguments {
            tool: tool.to_string(),
            reason: format!("\"{remote}\" is not a remote this tool will hand to git"),
        });
    }
    let branch = match text_arg(arguments, "branch") {
        Some(branch) => branch.trim().to_string(),
        None => git_output(tool, policy, &["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|text| text.trim().to_string())
            .unwrap_or_default(),
    };
    if branch.is_empty() || branch == "HEAD" {
        return Err(ToolsError::InvalidArguments {
            tool: tool.to_string(),
            reason: "there is no branch checked out to push; name one".into(),
        });
    }
    // A branch with no upstream of its own gets one, so the next push and the pull request both
    // know where the branch belongs.
    let tracked = git_output(
        tool,
        policy,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .is_some();
    let mut argv: Vec<String> = vec!["push".into()];
    if !tracked {
        argv.push("--set-upstream".into());
    }
    argv.push(remote.clone());
    argv.push(branch.clone());
    let command = display_command("git", &argv);
    let finished = run_argv(
        tool,
        "git",
        &argv,
        &policy.workspace_root,
        GIT_PUSH_TIMEOUT_SECS,
        Reporting::head(),
        &GIT_ENV,
    )?;
    let (body, lost) = captured_text(&finished);
    if finished.ended_is_failure() {
        let (summary, content) =
            git_failed(&command, &body, lost, &finished, GIT_PUSH_TIMEOUT_SECS);
        let content = format!(
            "{content}\n\nIf this is an authentication failure, this machine has no credential for \
             {remote}: add one to the macOS Keychain, or use an SSH remote."
        );
        return Ok(Executed {
            summary,
            content,
            edits: Vec::new(),
        });
    }
    let summary = Summary::said(
        SummaryKey::GitPushed,
        json!({ "branch": branch }),
        format!("pushed {branch}"),
    );
    let content = git_said(&command, &body, lost, &finished, &summary.text);
    Ok(Executed {
        summary,
        content,
        edits: Vec::new(),
    })
}

/// Open a pull request on GitHub.
///
/// Everything the call needs is read rather than asked for: the repository from the origin remote,
/// the branch from the checkout, and the branch to land on from the repository's own default. The
/// token is the one thing the app cannot do without, and a run without one is told so in words the
/// model can act on rather than failing on a request that was never sent.
fn github_pr(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
    token: Option<&dyn GitHubToken>,
) -> Result<Executed, ToolsError> {
    let token = token
        .and_then(|host| host.token())
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            ToolsError::NotConfigured(
                "no GitHub token is saved; paste one under Settings → Agent → GitHub token".into(),
            )
        })?;
    let title = required(tool, arguments, "title")?.trim().to_string();
    if title.is_empty() {
        return Err(ToolsError::InvalidArguments {
            tool: tool.to_string(),
            reason: "a pull request needs a title".into(),
        });
    }
    let body = text_arg(arguments, "body").unwrap_or("").trim().to_string();
    let repo = match text_arg(arguments, "repo") {
        Some(repo) => {
            let repo = repo.trim();
            if !crate::github::valid_repo(repo) {
                return Err(ToolsError::InvalidArguments {
                    tool: tool.to_string(),
                    reason: format!("\"{repo}\" is not a repository name; use \"owner/name\""),
                });
            }
            repo.to_string()
        }
        None => {
            let url =
                git_output(tool, policy, &["remote", "get-url", "origin"]).unwrap_or_default();
            crate::github::repo_from_remote(&url).ok_or_else(|| ToolsError::InvalidArguments {
                tool: tool.to_string(),
                reason: "the origin remote is not a GitHub repository; name the repository as \
                          \"owner/name\""
                    .into(),
            })?
        }
    };
    let head = match text_arg(arguments, "head") {
        Some(head) => head.trim().to_string(),
        None => git_output(tool, policy, &["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|text| text.trim().to_string())
            .unwrap_or_default(),
    };
    if !crate::github::valid_branch(&head) {
        return Err(ToolsError::InvalidArguments {
            tool: tool.to_string(),
            reason: "there is no branch checked out to open a pull request from; name one".into(),
        });
    }
    let base = match text_arg(arguments, "base") {
        Some(base) => base.trim().to_string(),
        None => crate::github::default_branch(&token, &repo)?,
    };
    if !crate::github::valid_branch(&base) {
        return Err(ToolsError::InvalidArguments {
            tool: tool.to_string(),
            reason: format!("\"{base}\" is not a branch name"),
        });
    }
    let draft = flag_arg(arguments, "draft");
    let pull = crate::github::open_pull_request(&token, &repo, &head, &base, &title, &body, draft)?;
    let summary = Summary::said(
        SummaryKey::GitPullRequest,
        json!({ "number": pull.number }),
        format!("pull request #{}", pull.number),
    );
    let content = format!(
        "Opened pull request #{} on {repo} from {head} into {base}.\n{}\n{}",
        pull.number, pull.title, pull.url
    );
    Ok(Executed {
        summary,
        content,
        edits: Vec::new(),
    })
}

/// One small read of git's own output, for the number a row wants to show.
fn git_output(tool: &str, policy: &SandboxPolicy, args: &[&str]) -> Option<String> {
    let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let finished = run_argv(
        tool,
        "git",
        &argv,
        &policy.workspace_root,
        GIT_TIMEOUT_SECS,
        Reporting::head(),
        &GIT_ENV,
    )
    .ok()?;
    if finished.ended_is_failure() {
        return None;
    }
    Some(String::from_utf8_lossy(&finished.out.bytes).to_string())
}

/// What a git call that worked leaves: the command as it ran, git's own words, and the row's line.
fn git_said(command: &str, body: &str, lost: usize, finished: &Finished, legend: &str) -> String {
    let (output, held) = head(body, MAX_RESULT_BYTES);
    command_report(command, "", &output, held + lost, finished.elapsed, legend)
}

/// What a git call that failed leaves: git's own error and the code it exited with.
///
/// A failure is a result rather than a refusal — nothing blocked the call, git ran and said no —
/// so the row shows the exit code and the message rather than an empty answer.
fn git_failed(
    command: &str,
    body: &str,
    lost: usize,
    finished: &Finished,
    seconds: u64,
) -> (Summary, String) {
    let (output, held) = head(body, MAX_RESULT_BYTES);
    let summary = ended_summary(&finished.ended, seconds);
    let content = command_report(
        command,
        "",
        &output,
        held + lost,
        finished.elapsed,
        &summary.text,
    );
    (summary, content)
}

/// The body of a report without its trailing exit line, for a caller that appends its own legend
/// before the same line.
fn command_report_body(command: &str, output: &str, held: usize) -> String {
    let mut text = format!("$ {command}\n\n");
    if output.is_empty() {
        text.push_str("(no output)\n");
    } else {
        text.push_str(output);
        if !output.ends_with('\n') {
            text.push('\n');
        }
    }
    if held > 0 {
        text.push_str(&format!("…{held} bytes of output are not shown.\n"));
    }
    text
}

impl Finished {
    fn ended_is_failure(&self) -> bool {
        matches!(&self.ended, Ended::Exited(status) if !status.success())
    }
}

/// Per-file line counts from `git diff --numstat`: `added<TAB>removed<TAB>path`, with `-` for a
/// binary file. Every line is one file, so the count is exact even when the patch itself was cut.
fn numstat_counts(output: &str) -> (usize, u32, u32) {
    let mut files = 0;
    let mut added = 0u32;
    let mut removed = 0u32;
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.splitn(3, '\t');
        let (Some(a), Some(r), Some(_path)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        files += 1;
        added = added.saturating_add(a.trim().parse().unwrap_or(0));
        removed = removed.saturating_add(r.trim().parse().unwrap_or(0));
    }
    (files, added, removed)
}

/// How much of one search hit's sentence survives. Eight hits at the provider's full length would
/// spend the result budget on prose the model is about to re-read from the page it opens.
const MAX_SNIPPET_CHARS: usize = 300;

/// Ask the provider what it knows about the words, and answer with a list that carries an address per
/// entry.
///
/// The addresses are the point of the call: an answer the model writes after this has to cite pages it
/// could name, and a hit without one is a title nobody can check.
fn web_search(tool: &str, arguments: &Value) -> Result<Executed, ToolsError> {
    let query = required(tool, arguments, "query")?.to_string();
    let limit = number_arg(arguments, "limit").unwrap_or(web::MAX_RESULTS);
    let hits = web::search(&query, limit)?;
    if hits.is_empty() {
        return Ok(Executed {
            summary: Summary::said(
                SummaryKey::NoResults,
                json!({ "query": query }),
                format!("nothing for {query}"),
            ),
            content: format!(
                "Nothing was found for “{query}”. Fewer, plainer words find more: an article is titled \
                 for one thing, not for a whole question."
            ),
            edits: Vec::new(),
        });
    }
    let list = hits
        .iter()
        .enumerate()
        .map(|(i, hit)| {
            let snippet: String = hit.snippet.chars().take(MAX_SNIPPET_CHARS).collect();
            format!("{}. {} — {snippet}\n   {}", i + 1, hit.title, hit.url)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let n = hits.len();
    let plural = if n == 1 { "" } else { "s" };
    Ok(Executed {
        summary: if n == 1 {
            Summary::said(SummaryKey::Result, json!({}), "1 result")
        } else {
            Summary::said(SummaryKey::Results, json!({ "count": n }), format!("{n} results"))
        },
        content: format!(
            "{n} result{plural} for “{query}”:\n\n{list}\n\nRead one in full with fetch_url and the \
             address under it, and name that address in the answer.",
        ),
        edits: Vec::new(),
    })
}

/// Read one page: [`crate::web`]'s guards, then the text with the markup taken out.
///
/// A cut page says it was cut. A model that is handed the head of an article and no reason for the
/// sentence ending mid-word will finish the thought out of its own weights, which is the one way this
/// build can produce an answer that looks sourced and is not.
fn fetch_url(tool: &str, arguments: &Value) -> Result<Executed, ToolsError> {
    let raw = required(tool, arguments, "url")?.to_string();
    let max_chars = number_arg(arguments, "max_chars").unwrap_or(web::DEFAULT_PAGE_CHARS);
    let page = web::read_page(&raw, max_chars)?;
    let web::Page {
        url,
        title,
        mut text,
        truncated,
    } = page;
    if truncated {
        text.push_str(&format!(
            "\n\n…{title} is longer than the {} characters one read sends back, so this is its opening. \
             Ask again with a larger max_chars (up to {}) for more.",
            max_chars.clamp(1, web::MAX_PAGE_CHARS),
            web::MAX_PAGE_CHARS
        ));
    }
    Ok(Executed {
        summary: if truncated {
            Summary::said(
                SummaryKey::PageCut,
                json!({ "title": title }),
                format!("{title}, cut short"),
            )
        } else {
            // The page's own title, which is data the site wrote rather than a sentence the app does.
            Summary::verbatim(title.clone())
        },
        content: format!("{title} — {url}\n\n{text}"),
        edits: Vec::new(),
    })
}

/// Create one workspace file, with the directories its path names.
///
/// The write is a create and nothing else: `create_new` fails on a name that is already taken, so the
/// call cannot replace a file the run never read. A run that wants an existing file rewritten has to
/// say so with `edit_file`, whose text has to be found in that file first — one call that overwrites a
/// whole file with unverified text is the failure this tool would otherwise be.
///
/// The directories come with it because a project is a tree rather than a file: "make a web app" names
/// `my-app/index.html`, and a create that refuses the missing `my-app/` sends the run looking for a
/// shell this build does not offer.
fn write_file(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
) -> Result<Executed, ToolsError> {
    let label = required(tool, arguments, "path")?.to_string();
    let path = resolve(policy, &label, Access::Write)?;
    let contents = required(tool, arguments, "contents")?;
    let refuse = |reason: String| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason,
    };
    if contents.len() as u64 > MAX_FILE_BYTES {
        return Err(refuse(format!(
            "{label} would be {}, past the {} this build reads, so it could not be read back.",
            human_bytes(contents.len() as u64),
            human_bytes(MAX_FILE_BYTES)
        )));
    }
    // Named before the directories are made, so a refusal cannot leave an empty tree behind.
    if path.exists() {
        return Err(refuse(format!(
            "{label} is already there, so nothing was written. Read it and change it with edit_file, \
             or name a path that is free."
        )));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error(tool, &label, &e))?;
    }
    let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(created) => created,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(refuse(format!(
                "{label} is already there, so nothing was written."
            )))
        }
        Err(e) => return Err(io_error(tool, &label, &e)),
    };
    file.write_all(contents.as_bytes())
        .map_err(|e| io_error(tool, &label, &e))?;

    let added = contents.lines().count();
    let edit = Edit {
        path: relative(policy, &path),
        added: added as u32,
        removed: 0,
        occurrences: 1,
        hunks: vec![EditHunk {
            at: 1,
            lines: fit(contents.lines().map(|line| format!("+{line}")).collect()),
        }],
    };
    if added == 0 {
        return Ok(Executed {
            summary: Summary::said(SummaryKey::FileCreated, json!({}), "1 file created"),
            content: format!("Created {}: an empty file.", edit.path),
            edits: vec![edit],
        });
    }
    Ok(Executed {
        summary: Summary::said(SummaryKey::FileCreated, json!({}), "1 file created"),
        content: format!(
            "Created {} (+{} −{}): new file\n{label}:\n{}",
            edit.path, edit.added, edit.removed, edit.hunks[0].lines
        ),
        edits: vec![edit],
    })
}

/// One hunk of a unified diff: the numbers its header gave and the body lines under it.
///
/// `body` keeps the ` `, `-` and `+` prefix because the review prints diff lines rather than file
/// lines. A blank context line is stored as the single space a diff should have written it with, so
/// the preview's first character is always a prefix the window can read a gutter from.
#[derive(Debug, PartialEq)]
struct PatchHunk {
    /// The hunk's first old-file line, 1-based. For an empty old range this is the line the insert
    /// follows, and 0 when it follows none.
    old_start: usize,
    /// How many lines the old side spans; the body's ` ` and `-` lines are exactly these.
    old_count: usize,
    body: Vec<String>,
    /// A `\ No newline at end of file` marker followed the new side, so its last line carries no
    /// newline. Consulted only when the hunk reaches the file's own end.
    new_no_newline: bool,
}

impl PatchHunk {
    /// Where the hunk's first old-side line sits in the file, 0-based.
    fn start_index(&self) -> usize {
        if self.old_count == 0 {
            self.old_start
        } else {
            self.old_start - 1
        }
    }

    /// The line the change opens on, 1-based in the file as it stood before the patch — the number
    /// [`EditHunk::at`] carries and the review's gutter counts back from.
    fn opening_line(&self) -> usize {
        if self.old_count == 0 {
            return self.old_start + 1;
        }
        let lead = self
            .body
            .iter()
            .take_while(|line| line.starts_with(' '))
            .count();
        self.old_start + lead
    }

    fn added(&self) -> usize {
        self.body
            .iter()
            .filter(|line| line.starts_with('+'))
            .count()
    }

    fn removed(&self) -> usize {
        self.body
            .iter()
            .filter(|line| line.starts_with('-'))
            .count()
    }
}

/// A file's unified diff, parsed and validated as a shape but not yet applied to anything.
struct ParsedPatch {
    /// Workspace-relative as the header wrote it, prefixes stripped; resolved through the sandbox
    /// before it is opened.
    path: String,
    hunks: Vec<PatchHunk>,
}

/// Read a file as the lines a patch addresses, plus the two facts a line splice would lose: which
/// ending its lines use, and whether the last one has a newline.
///
/// Without them a one-line change to a Windows checkout would rewrite every line's ending, and a
/// patch that adds a final newline would appear to apply while changing nothing. A file that mixes
/// endings is refused rather than normalized, because applying a patch to it would rewrite lines the
/// patch never mentioned.
struct Source {
    lines: Vec<String>,
    eol: &'static str,
    final_newline: bool,
}

fn split_source(tool: &str, label: &str, text: &str) -> Result<Source, ToolsError> {
    let refuse = |reason: String| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason,
    };
    if text.is_empty() {
        return Ok(Source {
            lines: Vec::new(),
            eol: "\n",
            final_newline: false,
        });
    }
    let final_newline = text.ends_with('\n');
    let body = text.strip_suffix('\n').unwrap_or(text);
    let parts: Vec<&str> = body.split('\n').collect();
    let mut lines: Vec<String> = Vec::with_capacity(parts.len());
    let mut eol: Option<&'static str> = None;
    for (i, part) in parts.iter().enumerate() {
        let followed_by_newline = i + 1 < parts.len() || final_newline;
        let (content, ending) = if !followed_by_newline {
            (*part, "")
        } else if let Some(without_cr) = part.strip_suffix('\r') {
            (without_cr, "\r\n")
        } else {
            (*part, "\n")
        };
        if !ending.is_empty() {
            match eol {
                Some(seen) if seen != ending => {
                    return Err(refuse(format!(
                        "{label} mixes line endings, and a patch cannot be applied to it without \
                         rewriting the lines it does not touch. Normalise its endings first."
                    )))
                }
                None => eol = Some(ending),
                Some(_) => {}
            }
        }
        lines.push(content.to_string());
    }
    Ok(Source {
        lines,
        eol: eol.unwrap_or("\n"),
        final_newline,
    })
}

/// Rebuild the file from the patched lines, in its own endings.
fn render_source(source: &Source, lines: &[String], final_newline: bool) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        out.push_str(line);
        if i + 1 < lines.len() || final_newline {
            out.push_str(source.eol);
        }
    }
    out
}

/// Parse a unified diff. Every hunk's numbers checked against its own body, and every line the body
/// is not a diff line refused rather than guessed at. A diff may name several files: each `---`/`+++`
/// pair opens a section of its own, and a path may appear in only one section.
///
/// The tolerated headers are the ones `git diff` and `diff -u` actually write: an optional
/// `diff --git`, `index` and mode lines, a tab-separated timestamp after either name, quoted names,
/// and the `a/`/`b/` prefixes. Everything that would change the shape of the call — a rename, a
/// copy, a binary patch, a create or a delete — is refused with the reason, because applying a
/// different change than the one asked for is worse than applying none.
fn parse_patch(tool: &str, raw: &str) -> Result<Vec<ParsedPatch>, ToolsError> {
    let refuse = |reason: String| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason,
    };
    if raw.trim().is_empty() {
        return Err(refuse(
            "\"patch\" is empty, so nothing was applied.".to_string(),
        ));
    }
    // A patch copied out of a CRLF file carries the same `\r` on every line; its content is still
    // the LF text the file holds after `split_source` has read the endings.
    let mut lines: Vec<&str> = raw
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    // `split` gives the text after a final newline as one empty line. It is punctuation, not a
    // context line a short hunk may count itself satisfied with, so it goes before parsing.
    while lines.last() == Some(&"") {
        lines.pop();
    }

    let mut files: Vec<ParsedPatch> = Vec::new();
    let mut target: Option<String> = None;
    let mut hunks: Vec<PatchHunk> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("diff --git ") {
            close_section(tool, &mut files, &mut target, &mut hunks)?;
            i += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            close_section(tool, &mut files, &mut target, &mut hunks)?;
            let old_raw = header_path(rest);
            if old_raw == "/dev/null" {
                return Err(refuse(
                    "the patch creates a file (its old side is /dev/null), and apply_patch changes \
                     files that already exist. Use write_file to create one."
                        .to_string(),
                ));
            }
            let Some(after) = lines.get(i + 1) else {
                return Err(refuse(format!(
                    "the +++ header for {old_raw} is missing, so the patch does not name a file to change."
                )));
            };
            let Some(new_raw) = after.strip_prefix("+++ ") else {
                return Err(refuse(format!(
                    "{old_raw} is not followed by a +++ header, so the patch does not name a file to change."
                )));
            };
            let new_raw = header_path(new_raw);
            if new_raw == "/dev/null" {
                return Err(refuse(
                    "the patch deletes a file (its new side is /dev/null), and this build does not \
                     let a run remove one. Nothing was removed."
                        .to_string(),
                ));
            }
            let old = strip_diff_prefix(old_raw);
            let new = strip_diff_prefix(new_raw);
            if old != new {
                return Err(refuse(format!(
                    "the patch's old name ({old}) and new name ({new}) differ, which is a rename; \
                     apply_patch changes files whose names stay put."
                )));
            }
            if new.is_empty() {
                return Err(refuse("the patch's +++ header names no file.".to_string()));
            }
            target = Some(new.to_string());
            i += 2;
            continue;
        }
        if line.starts_with("@@") {
            let Some(name) = &target else {
                return Err(refuse(format!(
                    "the hunk at line {} comes before any ---/+++ file header, so it names no file.",
                    i + 1
                )));
            };
            let (hunk, next) = parse_hunk(tool, &lines, i)?;
            if let Some(prev) = hunks.last() {
                let start = hunk.start_index();
                let prev_end = prev.start_index() + prev.old_count;
                if start < prev_end {
                    return Err(refuse(format!(
                        "a hunk starts at line {} of {name}, before the hunk above it ends at line \
                         {}. A patch's hunks are applied in order and cannot overlap.",
                        hunk.opening_line(),
                        prev_end
                    )));
                }
            }
            hunks.push(hunk);
            i = next;
            continue;
        }
        if patch_metadata(line) {
            i += 1;
            continue;
        }
        if let Some(what) = unsupported_patch(line) {
            return Err(refuse(format!(
                "the patch is a {what}, which apply_patch does not do: it changes the text of files \
                 whose names stay put."
            )));
        }
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        // A body line here means the hunk above stopped at its declared count while its body had
        // not: saying so is more use than reading the extra line as a broken file header.
        if !hunks.is_empty()
            && (line.starts_with(' ') || line.starts_with('+') || line.starts_with('-'))
        {
            return Err(refuse(format!(
                "line {} of the patch continues a hunk whose header already said how many lines it \
                 carries. Check the @@ counts above it.",
                i + 1
            )));
        }
        return Err(refuse(format!(
            "line {} of the patch is not part of a unified diff: {:?}.",
            i + 1,
            clip(line)
        )));
    }

    close_section(tool, &mut files, &mut target, &mut hunks)?;
    if files.is_empty() {
        return Err(refuse(
            "the patch carries no ---/+++ file header, so it names no file to change.".to_string(),
        ));
    }
    Ok(files)
}

/// Close the section a `---`/`+++` pair opened, or do nothing when no section is open. A section
/// with no hunks and a path that appears twice are both refused, because the first changes nothing
/// and the second would make one file's line numbers depend on another section's order.
fn close_section(
    tool: &str,
    files: &mut Vec<ParsedPatch>,
    target: &mut Option<String>,
    hunks: &mut Vec<PatchHunk>,
) -> Result<(), ToolsError> {
    let refuse = |reason: String| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason,
    };
    let Some(path) = target.take() else {
        return Ok(());
    };
    if hunks.is_empty() {
        return Err(refuse(format!(
            "the patch names {path} but carries no hunks, so nothing was changed."
        )));
    }
    if files.iter().any(|file| file.path == path) {
        return Err(refuse(format!(
            "the patch names {path} in more than one section. Put its hunks in one section so their \
             line numbers all refer to the file as it was before the patch."
        )));
    }
    files.push(ParsedPatch {
        path,
        hunks: std::mem::take(hunks),
    });
    Ok(())
}

/// A header path without the tab-separated timestamp `diff -u` appends or the quotes git puts
/// around a name that needs them.
fn header_path(raw: &str) -> &str {
    let path = raw.split('\t').next().unwrap_or(raw).trim();
    path.strip_prefix('"')
        .and_then(|quoted| quoted.strip_suffix('"'))
        .unwrap_or(path)
}

/// A header path without git's side prefix. A top-level directory genuinely named `a` or `b` is the
/// one case this reads differently, and it is git's own convention at fault rather than a guess.
fn strip_diff_prefix(path: &str) -> &str {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}

/// Header lines that describe the file rather than change it, so ignoring them cannot ignore a
/// change. The mode and index values are not used: a patch's job here is the text.
fn patch_metadata(line: &str) -> bool {
    [
        "index ",
        "old mode ",
        "new mode ",
        "new file mode ",
        "deleted file mode ",
        "similarity index ",
        "dissimilarity index ",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

/// Header lines that announce a change this call cannot make. Naming the shape is what lets the
/// model stop retrying it.
fn unsupported_patch(line: &str) -> Option<&'static str> {
    [
        ("rename from ", "rename"),
        ("rename to ", "rename"),
        ("copy from ", "copy"),
        ("copy to ", "copy"),
        ("Binary files ", "binary patch"),
        ("GIT binary patch", "binary patch"),
    ]
    .iter()
    .find(|(prefix, _)| line.starts_with(prefix))
    .map(|(_, what)| *what)
}

fn clip(line: &str) -> String {
    let mut out: String = line.chars().take(80).collect();
    if line.chars().count() > 80 {
        out.push('…');
    }
    out
}

/// One hunk, read against the counts its own header declares rather than against the next header:
/// a body with a line too many or too few is a patch written against a different file, and saying
/// which line broke is how the model fixes it.
fn parse_hunk(tool: &str, lines: &[&str], at: usize) -> Result<(PatchHunk, usize), ToolsError> {
    let refuse = |reason: String| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason,
    };
    let header = at + 1;
    let (old_start, old_count, _new_start, new_count) = hunk_header(tool, lines[at], header)?;
    let mut body: Vec<String> = Vec::new();
    let mut old_seen = 0usize;
    let mut new_seen = 0usize;
    let mut new_no_newline = false;
    let mut last: Option<char> = None;
    let mut i = at + 1;
    while old_seen < old_count || new_seen < new_count {
        let Some(&line) = lines.get(i) else {
            return Err(refuse(format!(
                "the hunk at line {header} says {old_count} old and {new_count} new lines, but the \
                 patch ends after {old_seen} and {new_seen}."
            )));
        };
        if line.starts_with('\\') {
            note_no_newline(tool, line, last, &mut new_no_newline, i + 1)?;
            i += 1;
            continue;
        }
        let prefix = line.chars().next().unwrap_or(' ');
        match prefix {
            ' ' | '-' | '+' => {}
            other => {
                return Err(refuse(format!(
                    "line {} of the patch starts with {other:?} inside the hunk at line {header}; \
                     a hunk's lines start with a space, - or +.",
                    i + 1
                )))
            }
        }
        if prefix != '+' {
            old_seen += 1;
        }
        if prefix != '-' {
            new_seen += 1;
        }
        if old_seen > old_count || new_seen > new_count {
            return Err(refuse(format!(
                "the hunk at line {header} says {old_count} old and {new_count} new lines, but \
                 line {} is one line too many.",
                i + 1
            )));
        }
        last = Some(prefix);
        body.push(if line.is_empty() {
            // A blank line in a hunk is an empty context line; the space is the prefix the review
            // reads, and without it the line would be mistaken for a held one.
            " ".to_string()
        } else {
            line.to_string()
        });
        i += 1;
    }
    // A marker after the counted lines names the new side's last line, so it belongs to this hunk
    // even though it is not one of its lines.
    while let Some(&line) = lines.get(i) {
        if !line.starts_with('\\') {
            break;
        }
        note_no_newline(tool, line, last, &mut new_no_newline, i + 1)?;
        i += 1;
    }
    // A body line here means the header stopped counting before the body did. Without this, a
    // hunk whose counts are short of its text would either be read as context-only or have its
    // extra lines handed to the next parse step as something they are not. File headers start with
    // the same dashes a removed line does, so they are not mistaken for one.
    if let Some(&line) = lines.get(i) {
        if (line.starts_with(' ') || line.starts_with('+') || line.starts_with('-'))
            && !line.starts_with("--- ")
            && !line.starts_with("+++ ")
        {
            return Err(refuse(format!(
                "the hunk at line {header} says {old_count} old and {new_count} new lines, but \
                 line {} is one line too many. Check the @@ counts.",
                i + 1
            )));
        }
    }
    if !body
        .iter()
        .any(|line| line.starts_with('+') || line.starts_with('-'))
    {
        return Err(refuse(format!(
            "the hunk at line {header} changes nothing: every line it carries is context."
        )));
    }
    Ok((
        PatchHunk {
            old_start,
            old_count,
            body,
            new_no_newline,
        },
        i,
    ))
}

/// `@@ -old_start[,old_count] +new_start[,new_count] @@`, with the section heading after the second
/// `@@` ignored. A missing count means one, as the format says.
fn hunk_header(
    tool: &str,
    header: &str,
    at: usize,
) -> Result<(usize, usize, usize, usize), ToolsError> {
    let refuse = |reason: String| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason,
    };
    let shape = || {
        refuse(format!(
            "the hunk header at line {at} is not \"@@ -old_start[,old_count] +new_start[,new_count] \
             @@\": {:?}.",
            clip(header)
        ))
    };
    let rest = header.strip_prefix("@@").ok_or_else(shape)?;
    let ranges = rest.split("@@").next().unwrap_or(rest).trim();
    let mut parts = ranges.split_whitespace();
    let old = parts
        .next()
        .and_then(|raw| raw.strip_prefix('-'))
        .ok_or_else(shape)?;
    let new = parts
        .next()
        .and_then(|raw| raw.strip_prefix('+'))
        .ok_or_else(shape)?;
    let (old_start, old_count) = hunk_range(tool, old, at)?;
    let (new_start, new_count) = hunk_range(tool, new, at)?;
    if old_count > 0 && old_start == 0 {
        return Err(refuse(format!(
            "the hunk at line {at} names old line 0 for a range that removes lines; unified diffs \
             count from 1, and only an empty range may start at 0."
        )));
    }
    Ok((old_start, old_count, new_start, new_count))
}

fn hunk_range(tool: &str, raw: &str, at: usize) -> Result<(usize, usize), ToolsError> {
    let refuse = |reason: String| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason,
    };
    let (start, count) = raw.split_once(',').unwrap_or((raw, "1"));
    let start = start.parse::<usize>().map_err(|_| {
        refuse(format!(
            "the hunk header at line {at} has {raw:?} where a line number and count belong."
        ))
    })?;
    let count = count.parse::<usize>().map_err(|_| {
        refuse(format!(
            "the hunk header at line {at} has {raw:?} where a line number and count belong."
        ))
    })?;
    Ok((start, count))
}

/// Read the `\ No newline at end of file` marker. It says nothing about a side it does not follow,
/// so a marker after a removed line only becomes a fact about the result when a marker after the new
/// side says so too.
fn note_no_newline(
    tool: &str,
    line: &str,
    after: Option<char>,
    flag: &mut bool,
    at: usize,
) -> Result<(), ToolsError> {
    let refuse = |reason: String| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason,
    };
    if !line.to_ascii_lowercase().contains("no newline") {
        return Err(refuse(format!(
            "line {at} of the patch is a marker this build does not know: {:?}.",
            clip(line)
        )));
    }
    match after {
        None => Err(refuse(format!(
            "line {at} of the patch says a line has no newline, but it follows no line."
        ))),
        Some('+') | Some(' ') => {
            *flag = true;
            Ok(())
        }
        Some(_) => Ok(()),
    }
}

/// Apply every hunk to the file's lines, atomically: each hunk's old side has to match the file
/// line for line before one byte is written, and a hunk that does not is refused with the line and
/// both texts, because "the context no longer matches" without the line is a fact the model cannot
/// act on.
fn apply_hunks(
    tool: &str,
    label: &str,
    source: &Source,
    hunks: &[PatchHunk],
) -> Result<(Vec<String>, bool), ToolsError> {
    let refuse = |reason: String| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason,
    };
    let mut out: Vec<String> = Vec::with_capacity(source.lines.len());
    let mut cursor = 0usize;
    let mut final_newline = source.final_newline;
    for (n, hunk) in hunks.iter().enumerate() {
        let number = n + 1;
        let start = hunk.start_index();
        let end = start + hunk.old_count;
        if end > source.lines.len() {
            return Err(refuse(if hunk.old_count == 0 {
                format!(
                    "hunk {number} inserts after line {} of {label}, which has {} lines. Read the \
                     file again and send a patch against what is there.",
                    hunk.old_start,
                    source.lines.len()
                )
            } else {
                format!(
                    "hunk {number} expects lines {}–{} of {label}, which has {} lines. Read the \
                     file again and send a patch against what is there.",
                    hunk.old_start,
                    hunk.old_start + hunk.old_count - 1,
                    source.lines.len()
                )
            }));
        }
        let mut at = start;
        for line in &hunk.body {
            let expected = match line.chars().next() {
                Some('-') => &line[1..],
                Some(' ') => &line[1..],
                _ => continue,
            };
            if source.lines[at] != expected {
                return Err(refuse(format!(
                    "hunk {number} does not match {label}: at line {} the patch says {:?} and the \
                     file says {:?}. Nothing was written; read the file again and send a patch \
                     against what is there.",
                    at + 1,
                    clip(expected),
                    clip(&source.lines[at])
                )));
            }
            at += 1;
        }
        out.extend(source.lines[cursor..start].iter().cloned());
        // The hunk's new side is its context lines as well as its added ones: a line the patch
        // passed over is still a line the result keeps, and the old side's copy is about to be
        // skipped by `cursor`.
        out.extend(
            hunk.body
                .iter()
                .filter_map(|line| match line.chars().next() {
                    Some('+' | ' ') => Some(line[1..].to_string()),
                    _ => None,
                }),
        );
        cursor = end;
        if end == source.lines.len() {
            final_newline = !hunk.new_no_newline;
        }
    }
    out.extend(source.lines[cursor..].iter().cloned());
    Ok((out, final_newline))
}

/// Apply a unified diff to files that are already in the workspace.
///
/// The third write, and the one bounded by the most checks: every path comes through the same
/// `Access::Write` sandbox as the other two, every file has to exist (creation is `write_file`'s job
/// and deletion is not offered at all), every hunk's context has to match the file exactly rather
/// than by fuzz, and nothing is written until every hunk of every file has passed. A patch may name
/// several files; each is reported separately so the row, the ledger and the review can fold it by
/// the file it touched.
fn apply_patch(
    tool: &str,
    arguments: &Value,
    policy: &SandboxPolicy,
) -> Result<Executed, ToolsError> {
    let raw = required(tool, arguments, "patch")?;
    let parsed = parse_patch(tool, raw)?;
    let refuse = |reason: String| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason,
    };
    let mut staged: Vec<StagedPatch> = Vec::with_capacity(parsed.len());
    for section in &parsed {
        let label = section.path.clone();
        let path = resolve(policy, &label, Access::Write)?;
        // Two spellings of one path are one file; applying to it twice would make the second
        // section's line numbers describe a file the first had already changed.
        if staged.iter().any(|item| item.path == path) {
            return Err(refuse(format!(
                "the patch opens {label} twice, once under another spelling. Put its hunks in one \
                 section so their line numbers all refer to the same file."
            )));
        }
        if !path.is_file() {
            return Err(refuse(format!(
                "{label} is not a file in this workspace, so nothing was applied. apply_patch changes \
                 a file that is already there; write_file creates one."
            )));
        }
        let Some(text) = read_text(tool, &path)? else {
            return Err(refuse(format!(
                "{label} is not text this tool can read, so nothing was applied."
            )));
        };
        let source = split_source(tool, &label, &text)?;
        let (lines, final_newline) = apply_hunks(tool, &label, &source, &section.hunks)?;
        let patched = render_source(&source, &lines, final_newline);
        if patched.len() as u64 > MAX_FILE_BYTES {
            return Err(refuse(format!(
                "{label} would be {}, past the {} this build reads, so nothing was written.",
                human_bytes(patched.len() as u64),
                human_bytes(MAX_FILE_BYTES)
            )));
        }
        let added: usize = section.hunks.iter().map(PatchHunk::added).sum();
        let removed: usize = section.hunks.iter().map(PatchHunk::removed).sum();
        let hunks: Vec<EditHunk> = section
            .hunks
            .iter()
            .map(|hunk| EditHunk {
                at: hunk.opening_line() as u32,
                lines: fit(hunk.body.clone()),
            })
            .collect();
        staged.push(StagedPatch {
            label,
            rendered: patched,
            edit: Edit {
                path: relative(policy, &path),
                added: added as u32,
                removed: removed as u32,
                occurrences: section.hunks.len() as u32,
                hunks,
            },
            path,
        });
    }

    // Every file has been read and every hunk matched; only now is a byte written. An I/O failure
    // part-way cannot be undone, so it names the file it stopped on rather than claiming the call
    // was refused.
    for item in &staged {
        fs::write(&item.path, &item.rendered).map_err(|e| io_error(tool, &item.label, &e))?;
    }

    let edits: Vec<Edit> = staged.into_iter().map(|item| item.edit).collect();
    let hunks: usize = edits.iter().map(|edit| edit.occurrences as usize).sum();
    let summary = if edits.len() == 1 {
        if hunks == 1 {
            Summary::said(SummaryKey::HunkApplied, json!({}), "1 hunk applied")
        } else {
            Summary::said(
                SummaryKey::HunksApplied,
                json!({ "count": hunks }),
                format!("{hunks} hunks applied"),
            )
        }
    } else {
        Summary::said(
            SummaryKey::FilesPatched,
            json!({ "count": edits.len() }),
            format!("{} files patched", edits.len()),
        )
    };
    let content = patch_content(&edits, &summary);
    Ok(Executed {
        summary,
        content,
        edits,
    })
}

/// One file's patched text, held until every file in the call has passed its checks.
struct StagedPatch {
    label: String,
    path: PathBuf,
    rendered: String,
    edit: Edit,
}

/// The result the model reads for a patch. One file keeps the shape an edit's copy has — the first
/// hunk in full, the rest named as applied. Several files are listed with their own counts, and the
/// whole result stays inside [`MAX_RESULT_BYTES`]: a patch that names more files than the budget can
/// show has them all named, with the ones whose hunks could not be shown counted at the end.
fn patch_content(edits: &[Edit], summary: &Summary) -> String {
    if edits.len() == 1 {
        let edit = &edits[0];
        let first = &edit.hunks[0];
        let mut preview = first.lines.clone();
        if edit.hunks.len() > 1 {
            let rest = edit.hunks.len() - 1;
            preview.push_str(&format!(
                "\n…{rest} more {} applied, not shown here.",
                if rest == 1 { "hunk" } else { "hunks" }
            ));
        }
        return format!(
            "Patched {} (+{} −{}): {}\n{}, at line {}:\n{}",
            edit.path, edit.added, edit.removed, summary.text, edit.path, first.at, preview
        );
    }
    let added: u32 = edits.iter().map(|edit| edit.added).sum();
    let removed: u32 = edits.iter().map(|edit| edit.removed).sum();
    let mut out = format!(
        "Patched {} files (+{added} −{removed}): {}\n",
        edits.len(),
        summary.text
    );
    // Room for the trailing notes so a budget cut speaks rather than truncating a sentence.
    const RESERVE: usize = 96;
    let mut shown = 0;
    for edit in edits {
        let first = &edit.hunks[0];
        let head = format!(
            "{} (+{} −{}), {} at line {}:\n",
            edit.path,
            edit.added,
            edit.removed,
            if edit.hunks.len() == 1 {
                "1 hunk".to_string()
            } else {
                format!("{} hunks", edit.hunks.len())
            },
            first.at
        );
        let rest = edit.hunks.len().saturating_sub(1);
        let tail = if rest > 0 {
            format!(
                "\n…{rest} more {} in this file, not shown here.",
                if rest == 1 { "hunk" } else { "hunks" }
            )
        } else {
            String::new()
        };
        if out.len() + head.len() + tail.len() + RESERVE > MAX_RESULT_BYTES {
            break;
        }
        out.push_str(&head);
        let room = MAX_RESULT_BYTES - out.len() - tail.len() - RESERVE;
        out.push_str(&clip_hunk(&first.lines, room));
        out.push_str(&tail);
        out.push('\n');
        shown += 1;
    }
    if shown < edits.len() {
        let rest = edits.len() - shown;
        out.push_str(&format!(
            "…{rest} more {} applied, not shown here.",
            if rest == 1 { "file" } else { "files" }
        ));
    } else {
        out.pop();
    }
    out
}

/// The leading lines of a hunk that fit in `room` bytes, with the rest counted rather than cut in
/// the middle of a line.
fn clip_hunk(text: &str, room: usize) -> String {
    if text.len() <= room {
        return text.to_string();
    }
    let mut out = String::new();
    let mut held = 0usize;
    let mut full = false;
    for (i, line) in text.split('\n').enumerate() {
        let cost = line.len() + usize::from(i > 0);
        if !full && out.len() + cost + 40 <= room {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(line);
        } else {
            full = true;
            held += 1;
        }
    }
    if held > 0 {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("…{held} lines of this hunk are not shown."));
    }
    out
}

/// How many lines at each end of a replacement the replacement shares with what it replaced. The two
/// are what makes "wrap this block in an `if`" read as two added lines rather than as the whole block
/// rewritten: a line the call carried over unchanged is not one it changed.
fn shared_edges(before: &[&str], after: &[&str]) -> (usize, usize) {
    let mut head = 0;
    while head < before.len() && head < after.len() && before[head] == after[head] {
        head += 1;
    }
    let mut tail = 0;
    while head + tail < before.len()
        && head + tail < after.len()
        && before[before.len() - 1 - tail] == after[after.len() - 1 - tail]
    {
        tail += 1;
    }
    (head, tail)
}

/// The lines a replacement takes out and puts back, as `(removed, added)`, with the shared edges
/// cancelled from both sides.
fn changed_lines(old: &str, new: &str) -> (usize, usize) {
    let before: Vec<&str> = old.lines().collect();
    let after: Vec<&str> = new.lines().collect();
    let (head, tail) = shared_edges(&before, &after);
    (before.len() - head - tail, after.len() - head - tail)
}

/// The change as the diff lines it is drawn from: the lines it took out and put back, with two of the
/// file on either side, in the `+`/`−` shape a diff uses — and the same shared edges [`changed_lines`]
/// cancels printed as context, so a hunk and the counts standing over it cannot disagree.
///
/// Returns the 1-based line the hunk opens on with the body, because the caption the model's copy
/// carries names that line and the window's row already names the file.
///
/// Only the first change gets a hunk: a `replace_all` over forty copies would spend the whole result
/// budget saying the same forty lines forty times. A hunk past the budget is cut, and says how much of
/// itself the cut left out.
fn hunk(text: &str, old: &str, new: &str) -> (usize, String) {
    let lines: Vec<&str> = text.lines().collect();
    let start = text
        .find(old)
        .map_or(0, |offset| text[..offset].lines().count());
    let before: Vec<&str> = old.lines().collect();
    let after: Vec<&str> = new.lines().collect();
    let (head, tail) = shared_edges(&before, &after);
    let end = start + before.len();

    let mut body: Vec<String> = Vec::new();
    for i in start.saturating_sub(CTX_LINES)..(start + head) {
        body.push(format!(" {}", lines.get(i).copied().unwrap_or("")));
    }
    for line in &before[head..before.len() - tail] {
        body.push(format!("-{line}"));
    }
    for line in &after[head..after.len() - tail] {
        body.push(format!("+{line}"));
    }
    // The unchanged lines inside the block, printed as context, then the file's own lines after it.
    let changed_end = end.min(lines.len());
    for line in &lines[(end - tail).max(start + head).min(changed_end)..changed_end] {
        body.push(format!(" {line}"));
    }
    for line in &lines[changed_end..(end + CTX_LINES).min(lines.len())] {
        body.push(format!(" {line}"));
    }

    (start + 1, fit(body))
}

/// The lines a change may carry into the result, with the number it had to leave out named at the end
/// rather than dropped quietly. Both writes report their hunk through here, so a change too big for
/// the budget says how much of itself the cut left out the same way in either case.
fn fit(body: Vec<String>) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0usize;
    let mut held = 0usize;
    for line in body {
        let cost = line.len() + 1;
        if used + cost > MAX_RESULT_BYTES {
            held += 1;
            continue;
        }
        used += cost;
        kept.push(line);
    }
    if held > 0 {
        kept.push(format!("…{held} lines of the change are not shown."));
    }
    kept.join("\n")
}

/// One file's matches, printed with their context merged between neighbours: a hit's line carries
/// `path:NN: text`, the lines around it carry `path:NN| text`, and two hits whose windows touch print
/// as one block rather than repeating the lines they share.
fn push_block(out: &mut Vec<String>, rel: &str, lines: &[&str], hits: &[usize], context: usize) {
    let last = lines.len().saturating_sub(1);
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for &hit in hits {
        let from = hit.saturating_sub(context);
        let to = (hit + context).min(last);
        match groups.last_mut() {
            // Touching or overlapping windows become one group.
            Some(prev) if from <= prev.1.saturating_add(1) => prev.1 = prev.1.max(to),
            _ => groups.push((from, to)),
        }
    }
    for (g, &(from, to)) in groups.iter().enumerate() {
        if g > 0 {
            out.push(format!("{rel}- …"));
        }
        for (i, line) in lines.iter().enumerate().take(to + 1).skip(from) {
            let mark = if hits.binary_search(&i).is_ok() {
                ':'
            } else {
                '|'
            };
            out.push(format!("{rel}:{}{mark} {}", i + 1, line.trim_end()));
        }
    }
}

/// Every readable file under `root`, in walk order, with the trees and sizes a read could not answer
/// from counted in `skipped`.
///
/// Symlinks are not followed, so a link out of the workspace cannot make the walk leave it — the same
/// rule [`SandboxPolicy::resolve_path`] applies to a path the model names. `root` itself has already
/// been through that check by the time this runs, so anything under it only has to be tested against
/// the protected names.
fn walk(policy: &SandboxPolicy, root: &Path, skipped: &mut usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if root.is_file() {
        out.push(root.to_path_buf());
        return out;
    }
    let mut iter = WalkDir::new(root)
        .follow_links(false)
        .max_depth(MAX_DEPTH)
        .into_iter();
    while let Some(entry) = iter.next() {
        let Ok(entry) = entry else {
            *skipped += 1;
            continue;
        };
        if entry.file_type().is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if SKIP_DIRS.contains(&name.as_str()) || policy.is_protected(entry.path()) {
                iter.skip_current_dir();
            }
            continue;
        }
        if !entry.file_type().is_file()
            || policy.is_protected(entry.path())
            || entry
                .metadata()
                .map(|m| m.len() > MAX_FILE_BYTES)
                .unwrap_or(true)
        {
            *skipped += 1;
            continue;
        }
        if out.len() >= MAX_WALK_ENTRIES {
            *skipped += 1;
            break;
        }
        out.push(entry.path().to_path_buf());
    }
    out
}

/// A file as text, whole: the walk already refused anything past [`MAX_FILE_BYTES`], so this read has
/// a bounded allocation and the answer gets an exact line count out of it. `None` when the bytes are
/// not text.
fn read_text(tool: &str, path: &Path) -> Result<Option<String>, ToolsError> {
    let shown = path.display().to_string();
    let meta = fs::metadata(path).map_err(|e| io_error(tool, &shown, &e))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(ToolsError::InvalidArguments {
            tool: tool.to_string(),
            reason: format!(
                "{shown} is {}, past the {} this build reads",
                human_bytes(meta.len()),
                human_bytes(MAX_FILE_BYTES)
            ),
        });
    }
    let mut file = File::open(path).map_err(|e| io_error(tool, &shown, &e))?;
    let mut buf = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut buf)
        .map_err(|e| io_error(tool, &shown, &e))?;
    Ok(String::from_utf8(buf).ok())
}

/// The one sentence both walkers owe: what they left out of the tree and why.
fn with_skips(content: String, skipped: usize) -> String {
    if skipped == 0 {
        return content;
    }
    format!(
        "{content}\n({skipped} paths were deeper than {MAX_DEPTH} levels, larger than {}, protected, \
         or unreadable.)",
        human_bytes(MAX_FILE_BYTES)
    )
}

fn required<'a>(tool: &str, arguments: &'a Value, key: &str) -> Result<&'a str, ToolsError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolsError::InvalidArguments {
            tool: tool.to_string(),
            reason: format!("\"{key}\" is required and must be a string"),
        })
}

fn resolve(policy: &SandboxPolicy, raw: &str, access: Access) -> Result<PathBuf, ToolsError> {
    Ok(policy.resolve_path(Path::new(raw), access)?)
}

/// A directory to walk: the `path` argument, or the whole workspace when it is left out.
fn optional_root(arguments: &Value, policy: &SandboxPolicy) -> Result<PathBuf, ToolsError> {
    resolve(
        policy,
        text_arg(arguments, "path").unwrap_or("."),
        Access::Read,
    )
}

fn compile(
    tool: &str,
    key: &str,
    raw: &str,
    build: fn(&str) -> Result<glob::Pattern, glob::PatternError>,
) -> Result<glob::Pattern, ToolsError> {
    build(raw).map_err(|e| ToolsError::InvalidArguments {
        tool: tool.to_string(),
        reason: format!("\"{raw}\" is not a valid glob in \"{key}\": {e}"),
    })
}

/// The workspace-relative path, in forward slashes: what the model passed to `glob` and what it can
/// pass straight back to `read_file`.
fn relative(policy: &SandboxPolicy, path: &Path) -> String {
    policy
        .display_path(path)
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn text_arg<'a>(arguments: &'a Value, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(Value::as_str)
}

fn number_arg(arguments: &Value, key: &str) -> Option<usize> {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .map(|n| n as usize)
}

fn flag_arg(arguments: &Value, key: &str) -> bool {
    arguments.get(key).and_then(Value::as_bool) == Some(true)
}

fn io_error(tool: &str, path: &str, error: &std::io::Error) -> ToolsError {
    ToolsError::Io {
        tool: tool.to_string(),
        path: path.to_string(),
        reason: error.to_string(),
    }
}

/// A size in the unit a person compares, because a raw byte count in a transcript row is a number
/// that means nothing next to another.
fn human_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < KB * KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{:.1} MB", bytes as f64 / (KB * KB) as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn policy(root: &Path) -> SandboxPolicy {
        SandboxPolicy {
            workspace_root: root.to_path_buf(),
            ..Default::default()
        }
    }

    /// A workspace with one secret the sandbox protects, one vendor tree, and two readable files.
    fn fixture() -> (tempfile::TempDir, SandboxPolicy) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(dir.path().join("src/big.rs"), "x").unwrap();
        std::fs::write(dir.path().join(".env"), "one TOKEN=secret\n").unwrap();
        std::fs::write(dir.path().join("node_modules/pkg/index.js"), "one\n").unwrap();
        let p = policy(dir.path());
        (dir, p)
    }

    fn ran(id: &str, args: &Value, p: &SandboxPolicy) -> Result<Executed, ToolsError> {
        execute(id, args, p)
    }

    /// The one file a single-file write reports.
    fn only_edit(got: &Executed) -> &Edit {
        assert_eq!(got.edits.len(), 1, "one file written");
        &got.edits[0]
    }

    /// That file's one hunk, for a write that made a single replacement.
    fn only_hunk(edit: &Edit) -> &EditHunk {
        assert_eq!(edit.hunks.len(), 1, "one hunk written");
        &edit.hunks[0]
    }

    /// A file of `lines` readable lines, each labelled with its 1-based number and padded to `width`.
    fn long_file(dir: &Path, lines: usize, width: usize) -> PathBuf {
        let path = dir.join("long.txt");
        let mut f = File::create(&path).unwrap();
        for i in 1..=lines {
            let mut line = format!("line {i} of the long file,");
            while line.len() < width {
                line.push_str(" padded out");
            }
            writeln!(f, "{line}").unwrap();
        }
        path
    }

    /// A workspace with nothing in it the sandbox protects, so an exact answer can be asserted.
    fn plain() -> (tempfile::TempDir, SandboxPolicy) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "one\ntwo\nthree\n").unwrap();
        let p = policy(dir.path());
        (dir, p)
    }

    #[test]
    fn a_read_answers_the_whole_file_when_no_range_is_asked_for() {
        let (_d, p) = fixture();
        let got =
            ran("read_file", &json!({ "path": "src/lib.rs" }), &p).expect("a workspace read runs");
        assert_eq!(got.content, "one\ntwo\nthree");
        assert_eq!(got.summary.text, "3 lines");
    }

    #[test]
    fn a_range_answers_that_range_and_says_how_much_of_the_file_it_was() {
        let (_d, p) = fixture();
        let got = ran(
            "read_file",
            &json!({ "path": "src/lib.rs", "start_line": 2, "end_line": 3 }),
            &p,
        )
        .unwrap();
        assert_eq!(got.content, "two\nthree");
        assert_eq!(got.summary.text, "lines 2–3 of 3");
        // A range that starts past the end is an answer, not a failure: the model needs to learn the
        // file is shorter than it thought.
        let past = ran(
            "read_file",
            &json!({ "path": "src/lib.rs", "start_line": 99 }),
            &p,
        )
        .unwrap();
        assert!(past.content.contains("past line 3"), "{}", past.content);
        // And an empty range says so rather than showing line `from`.
        let backwards = ran(
            "read_file",
            &json!({ "path": "src/lib.rs", "start_line": 3, "end_line": 1 }),
            &p,
        )
        .unwrap();
        assert!(
            backwards.content.contains("before it starts"),
            "{}",
            backwards.content
        );
    }

    #[test]
    fn a_long_file_is_cut_on_a_line_and_says_where_to_ask_from() {
        let dir = tempfile::tempdir().unwrap();
        long_file(dir.path(), 5_000, 55);
        let p = policy(dir.path());
        let got = ran("read_file", &json!({ "path": "long.txt" }), &p).unwrap();
        assert!(
            got.content.len() <= MAX_RESULT_BYTES + 200,
            "{} bytes of answer",
            got.content.len()
        );
        assert!(got.summary.text.contains("capped"), "{}", got.summary.text);
        assert!(
            got.content.contains("Ask again with start_line="),
            "a cut is only useful if it names where to continue: {}",
            got.summary.text
        );
        // And the count it quotes is the file's real length, not the number of lines it happened to
        // hold in the answer.
        assert!(got.summary.text.contains("of 5000"), "{}", got.summary.text);
        assert!(!got.content.ends_with('\n'), "the cut stays on a line");
    }

    #[test]
    fn a_range_past_the_cap_still_answers_the_tail_of_a_long_file() {
        let dir = tempfile::tempdir().unwrap();
        long_file(dir.path(), 2_000, 45);
        let p = policy(dir.path());
        let got = ran(
            "read_file",
            &json!({ "path": "long.txt", "start_line": 1_900 }),
            &p,
        )
        .unwrap();
        assert!(got.content.starts_with("line 1900"), "{}", got.content);
        assert!(got.summary.text.contains("of 2000"), "{}", got.summary.text);
    }

    /// The line number a capped answer told the model to ask for next.
    fn resume_line(content: &str) -> u64 {
        content
            .rsplit("start_line=")
            .next()
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    #[test]
    fn continuing_where_the_last_answer_stopped_leaves_no_line_out() {
        let dir = tempfile::tempdir().unwrap();
        long_file(dir.path(), 2_000, 55);
        let p = policy(dir.path());
        let first = ran("read_file", &json!({ "path": "long.txt" }), &p).unwrap();
        let stopped = resume_line(&first.content);
        let last_shown = first
            .content
            .split("\n…output capped")
            .next()
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .to_string();
        assert!(
            last_shown.starts_with(&format!("line {} of the long file,", stopped - 1)),
            "{last_shown}"
        );
        let second = ran(
            "read_file",
            &json!({ "path": "long.txt", "start_line": stopped }),
            &p,
        )
        .unwrap();
        assert!(
            second.content.starts_with(&format!("line {stopped} of")),
            "{}",
            second.content
        );
        assert!(
            second.summary.text.contains("of 2000"),
            "{}",
            second.summary.text
        );
    }

    #[test]
    fn a_listing_holds_back_the_secret_and_the_vendor_tree_and_says_so() {
        let (_d, p) = fixture();
        let got = ran("list_dir", &json!({ "path": "." }), &p).unwrap();
        assert!(got.content.contains("src/"), "{}", got.content);
        assert!(!got.content.contains(".env"), "protected: {}", got.content);
        assert!(
            !got.content.contains("node_modules"),
            "vendor: {}",
            got.content
        );
        assert!(
            got.content.contains("held back"),
            "the gap is named: {}",
            got.content
        );
        // Inside a real source directory the files are listed, with their sizes.
        let inside = ran("list_dir", &json!({ "path": "src" }), &p).unwrap();
        assert!(inside.content.contains("lib.rs ("), "{}", inside.content);
        assert!(inside.content.contains("big.rs ("), "{}", inside.content);
    }

    #[test]
    fn a_glob_answers_workspace_relative_paths_the_model_can_read_back() {
        let (_d, p) = fixture();
        let got = ran("glob", &json!({ "pattern": "src/*.rs" }), &p).unwrap();
        assert!(got.content.contains("src/big.rs"), "{}", got.content);
        assert!(got.content.contains("src/lib.rs"), "{}", got.content);
        assert!(!got.content.contains("node_modules"), "{}", got.content);
        // Every path it prints is one `read_file` will accept.
        for line in got.content.lines().filter(|l| !l.starts_with('(')) {
            assert!(
                ran("read_file", &json!({ "path": line }), &p).is_ok(),
                "{line} is not readable back"
            );
        }
        // A bare name pattern finds the file wherever it is, and a separator-bound one does not cross
        // directories.
        let bare = ran("glob", &json!({ "pattern": "*.rs" }), &p).unwrap();
        assert!(bare.content.contains("src/lib.rs"), "{}", bare.content);
        let wide = ran("glob", &json!({ "pattern": "src/*" }), &p).unwrap();
        assert_eq!(
            wide.content.lines().filter(|l| l.contains('/')).count(),
            2,
            "`src/*` reached past one directory: {}",
            wide.content
        );
    }

    #[test]
    fn a_search_answers_file_line_number_and_line() {
        let (_d, p) = plain();
        let got = ran("grep_search", &json!({ "pattern": "^two" }), &p).unwrap();
        assert_eq!(got.content, "src/lib.rs:2: two");
        assert_eq!(got.summary.text, "1 match in 1 file");
        // Nothing found is an answer too, and says what it looked for.
        let none = ran("grep_search", &json!({ "pattern": "zzzz" }), &p).unwrap();
        assert!(none.content.starts_with("No line"), "{}", none.content);
        assert!(
            none.summary.text.contains("nothing matches"),
            "{}",
            none.summary.text
        );
    }

    #[test]
    fn a_search_never_reads_the_secret_or_the_vendor_tree() {
        let (_d, p) = fixture();
        // `one` is in `.env`, in `node_modules` and in `src/lib.rs`: only the last may appear.
        let got = ran("grep_search", &json!({ "pattern": "one|TOKEN" }), &p).unwrap();
        assert!(!got.content.contains("TOKEN"), "protected: {}", got.content);
        assert!(
            !got.content.contains("node_modules"),
            "vendor tree searched: {}",
            got.content
        );
        assert!(got.content.contains("src/lib.rs:1: one"), "{}", got.content);
    }

    #[test]
    fn a_search_finds_a_match_past_what_one_read_of_a_file_returns() {
        let dir = tempfile::tempdir().unwrap();
        // Line 4_000 is far past MAX_RESULT_BYTES: a search that only read the head of each file would
        // answer "no line matches" about a file it never looked at.
        long_file(dir.path(), 4_000, 55);
        std::fs::write(dir.path().join("needle.txt"), "top\nneedle here\nbottom\n").unwrap();
        let mut contents = std::fs::read_to_string(dir.path().join("long.txt")).unwrap();
        contents.push_str("the needle is at the end of the long file\n");
        std::fs::write(dir.path().join("long.txt"), contents).unwrap();
        let got = ran(
            "grep_search",
            &json!({ "pattern": "needle", "glob": "*.txt" }),
            &policy(dir.path()),
        )
        .unwrap();
        assert!(
            got.content.contains("long.txt:4001: the needle"),
            "{}",
            got.content
        );
    }

    #[test]
    fn context_lines_come_off_the_file_around_the_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "1\n2\nhit\n4\n5\n").unwrap();
        let got = ran(
            "grep_search",
            &json!({ "pattern": "hit", "context_lines": 1 }),
            &policy(dir.path()),
        )
        .unwrap();
        assert!(got.content.contains("a.txt:2| 2"), "{}", got.content);
        assert!(got.content.contains("a.txt:3: hit"), "{}", got.content);
        assert!(got.content.contains("a.txt:4| 4"), "{}", got.content);
    }

    #[test]
    fn a_search_respects_its_glob_and_its_case_flag() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/a.md"), "Needed here\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "NEEDED there\n").unwrap();
        let p = policy(dir.path());
        let only_md = ran(
            "grep_search",
            &json!({ "pattern": "NEEDED", "glob": "docs/*", "ignore_case": true }),
            &p,
        )
        .unwrap();
        assert_eq!(only_md.content, "docs/a.md:1: Needed here");
        let strict = ran("grep_search", &json!({ "pattern": "Needed" }), &p).unwrap();
        assert!(
            !strict.content.contains("b.rs"),
            "case folded without the flag"
        );
    }

    #[test]
    fn a_call_outside_the_workspace_is_refused_before_anything_is_read() {
        let (_d, p) = fixture();
        let err = ran("read_file", &json!({ "path": "../outside.txt" }), &p)
            .expect_err("a read that escapes must be refused");
        assert!(matches!(err, ToolsError::Sandbox(_)), "{err:?}");
        let walk = ran("grep_search", &json!({ "pattern": "x", "path": ".." }), &p)
            .expect_err("a walk that starts outside must be refused");
        assert!(matches!(walk, ToolsError::Sandbox(_)), "{walk:?}");
        // And naming the protected file itself is refused, not answered with a quiet "nothing found".
        let err = ran("read_file", &json!({ "path": ".env" }), &p).expect_err("secret");
        assert!(matches!(err, ToolsError::Sandbox(_)), "{err:?}");
    }

    /// The registry is exactly what runs, so the only way a call reaches this file naming a capability
    /// this build does not have is a model inventing one. It is refused at the name, before the sandbox
    /// is consulted and before a row opens, because a row would promise work that never happens.
    #[test]
    fn a_name_this_build_does_not_declare_is_refused_before_anything_runs() {
        let (_d, p) = fixture();
        for id in [
            "codebase_search",
            "delete_path",
            "move_path",
            "format_code",
            "install_extension",
            "mcp_call",
            "ask_user",
        ] {
            let err = ran(id, &json!({ "path": "src/lib.rs" }), &p)
                .expect_err("nothing is declared under this name");
            assert!(matches!(err, ToolsError::UnknownTool(_)), "{id}: {err:?}");
        }
    }

    #[test]
    fn a_missing_or_uncompilable_argument_says_which_one() {
        let (_d, p) = fixture();
        let err = ran("read_file", &json!({}), &p).expect_err("path is required");
        assert!(err.to_string().contains("\"path\""), "{err}");
        let err = ran("glob", &json!({ "path": 3 }), &p).expect_err("pattern is required");
        assert!(err.to_string().contains("\"pattern\""), "{err}");
        // A pattern the matcher cannot compile is the model's mistake, and it has to be able to fix it.
        let err =
            ran("grep_search", &json!({ "pattern": "(unbalanced" }), &p).expect_err("bad regex");
        assert!(err.to_string().contains("regular expression"), "{err}");
        let err = ran("glob", &json!({ "pattern": "**[a" }), &p).expect_err("bad glob");
        assert!(err.to_string().contains("not a valid glob"), "{err}");
    }

    #[test]
    fn a_binary_file_is_not_answered_with_replacement_characters() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bin.dat"), [0u8, 159, 146, 150]).unwrap();
        let err = ran(
            "read_file",
            &json!({ "path": "bin.dat" }),
            &policy(dir.path()),
        )
        .expect_err("not text");
        assert!(err.to_string().contains("not text"), "{err}");
    }

    #[test]
    fn a_file_past_the_size_a_read_answers_is_named_and_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.bin");
        let f = File::create(&path).unwrap();
        f.set_len(MAX_FILE_BYTES + 1).unwrap();
        let err = ran(
            "read_file",
            &json!({ "path": "huge.bin" }),
            &policy(dir.path()),
        )
        .expect_err("too big to read");
        assert!(err.to_string().contains("MB"), "{err}");
    }

    #[test]
    fn an_empty_file_is_answered_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("nothing.txt"), "").unwrap();
        let got = ran(
            "read_file",
            &json!({ "path": "nothing.txt" }),
            &policy(dir.path()),
        )
        .unwrap();
        assert!(got.content.contains("is empty"), "{}", got.content);
    }

    #[test]
    fn an_absolute_path_inside_the_root_is_the_same_answer_as_a_relative_one() {
        let (_d, p) = fixture();
        let abs = p.workspace_root.join("src/lib.rs");
        let got = ran("read_file", &json!({ "path": abs }), &p).unwrap();
        assert_eq!(got.content, "one\ntwo\nthree");
        // And a listing that walked an absolute root prints relative paths, so no answer carries the
        // home directory into the prompt.
        let walk = ran(
            "glob",
            &json!({ "pattern": "*.rs", "path": abs.parent().unwrap() }),
            &p,
        )
        .unwrap();
        assert!(!walk.content.contains("private/var"), "{}", walk.content);
    }

    #[test]
    fn sizes_are_printed_in_the_unit_a_person_compares() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    /// The bytes on disk after a call, which is the only evidence that counts for a write.
    fn on_disk(dir: &Path, rel: &str) -> String {
        std::fs::read_to_string(dir.join(rel)).unwrap()
    }

    #[test]
    fn an_edit_writes_the_file_and_counts_the_lines_it_cost() {
        let (d, p) = plain();
        let got = ran(
            "edit_file",
            &json!({ "path": "src/lib.rs", "old_string": "two", "new_string": "TWO\nand a half" }),
            &p,
        )
        .expect("an edit of a workspace file runs");
        assert_eq!(
            on_disk(d.path(), "src/lib.rs"),
            "one\nTWO\nand a half\nthree\n"
        );
        let edit = only_edit(&got);
        assert_eq!((edit.added, edit.removed, edit.occurrences), (2, 1, 1));
        // The path is the spelling a follow-up `read_file` takes back.
        assert_eq!(edit.path, "src/lib.rs");
        assert_eq!(got.summary.text, "1 copy replaced");
    }

    #[test]
    fn a_replacement_that_carries_a_line_over_costs_only_the_line_it_added() {
        let (d, p) = plain();
        let got = ran(
            "edit_file",
            &json!({ "path": "src/lib.rs", "old_string": "two", "new_string": "two\nfour" }),
            &p,
        )
        .expect("an edit runs");
        // The line the call kept is not a line it rewrote, so the edit is one line rather than two.
        let edit = only_edit(&got);
        assert_eq!((edit.added, edit.removed), (1, 0));
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\ntwo\nfour\nthree\n");
    }

    #[test]
    fn wrapping_a_block_costs_its_own_lines_rather_than_the_whole_block() {
        let (d, p) = plain();
        // The file's first and last lines are shared and cancel out; what is left is the edit a diff
        // would print — one line out, three in — rather than "the whole block was rewritten".
        let got = ran(
            "edit_file",
            &json!({
                "path": "src/lib.rs",
                "old_string": "one\ntwo\nthree",
                "new_string": "one\nif ready {\n  two\n}\nthree",
            }),
            &p,
        )
        .expect("a multi-line edit runs");
        let edit = only_edit(&got);
        assert_eq!((edit.added, edit.removed), (3, 1));
        assert_eq!(
            on_disk(d.path(), "src/lib.rs"),
            "one\nif ready {\n  two\n}\nthree\n"
        );
    }

    #[test]
    fn an_edit_that_matches_nothing_writes_nothing_and_says_so() {
        let (d, p) = plain();
        let why = ran(
            "edit_file",
            &json!({ "path": "src/lib.rs", "old_string": "four", "new_string": "4" }),
            &p,
        )
        .expect_err("a match the file does not hold cannot be applied");
        assert!(why.to_string().contains("holds nothing matching"), "{why}");
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\ntwo\nthree\n");
    }

    #[test]
    fn an_ambiguous_edit_is_refused_rather_than_applied_to_the_wrong_copy() {
        let (d, p) = plain();
        std::fs::write(d.path().join("src/lib.rs"), "two\ntwo\n").unwrap();
        let why = ran(
            "edit_file",
            &json!({ "path": "src/lib.rs", "old_string": "two", "new_string": "ONE" }),
            &p,
        )
        .expect_err("two copies of the text is not one match");
        let reason = why.to_string();
        assert!(reason.contains("2 copies"), "{reason}");
        // The sentence names the way out, because the model is the reader that can take it.
        assert!(reason.contains("replace_all"), "{reason}");
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "two\ntwo\n");
    }

    #[test]
    fn replace_all_changes_every_copy_and_counts_the_lines_once_each() {
        let (d, p) = plain();
        std::fs::write(d.path().join("src/lib.rs"), "old\nold\nold\n").unwrap();
        let got = ran(
            "edit_file",
            &json!({
                "path": "src/lib.rs",
                "old_string": "old",
                "new_string": "new",
                "replace_all": true,
            }),
            &p,
        )
        .expect("a call that asks for every copy gets it");
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "new\nnew\nnew\n");
        let edit = only_edit(&got);
        assert_eq!((edit.added, edit.removed, edit.occurrences), (3, 3, 3));
        assert_eq!(got.summary.text, "3 copies replaced");
        // One hunk rather than three: the copies are the same change said three times.
        assert_eq!(got.content.matches("at line").count(), 1, "{}", got.content);
    }

    #[test]
    fn a_hunk_shows_the_changed_line_between_the_lines_around_it() {
        let (_d, p) = plain();
        let got = ran(
            "edit_file",
            &json!({ "path": "src/lib.rs", "old_string": "two", "new_string": "TWO" }),
            &p,
        )
        .expect("an edit runs");
        assert_eq!(
            got.content,
            "Edited src/lib.rs (+1 −1): 1 copy replaced\nsrc/lib.rs, at line 2:\n one\n-two\n+TWO\n three"
        );
    }

    #[test]
    fn the_edit_carries_the_same_hunk_the_model_was_shown_without_its_caption() {
        let (_d, p) = plain();
        let got = ran(
            "edit_file",
            &json!({ "path": "src/lib.rs", "old_string": "two", "new_string": "TWO" }),
            &p,
        )
        .expect("an edit runs");
        // The window's row already names the file and the line the caption counts, so the copy that
        // crosses the boundary is the change alone — and it is the same text, not a second rendering
        // of it, which is what keeps the review and the answer from disagreeing.
        let content = got.content.clone();
        let edit = only_edit(&got);
        assert_eq!(only_hunk(edit).lines, " one\n-two\n+TWO\n three");
        assert!(
            content.ends_with(&format!("at line 2:\n{}", only_hunk(edit).lines)),
            "{content}"
        );
        // The gutter the window numbers its diff lines from. It names the *changed* line rather than
        // the first printed one, which is what the caption says too — so the two cannot drift, and the
        // panel steps back over its own context lines to find where the block starts.
        assert_eq!(only_hunk(edit).at, 2);
        assert!(content.contains(&format!("at line {}:\n", only_hunk(edit).at)));
    }

    #[test]
    fn an_edit_outside_the_workspace_is_refused_before_it_can_touch_a_file() {
        let (d, p) = plain();
        let outside = d.path().parent().unwrap().join("escaped.rs");
        let _ = std::fs::remove_file(&outside);
        let why = ran(
            "edit_file",
            &json!({
                "path": outside,
                "old_string": "x",
                "new_string": "y",
            }),
            &p,
        )
        .expect_err("the workspace root is the boundary of a write");
        assert!(matches!(why, ToolsError::Sandbox(_)), "{why}");
        assert!(!outside.exists(), "the refusal wrote the file anyway");
    }

    #[test]
    fn a_protected_name_is_refused_for_a_write_as_it_is_for_a_read() {
        let (d, p) = fixture();
        let why = ran(
            "edit_file",
            &json!({ "path": ".env", "old_string": "TOKEN", "new_string": "token" }),
            &p,
        )
        .expect_err("a secret the sandbox hides cannot be edited through it");
        assert!(matches!(why, ToolsError::Sandbox(_)), "{why}");
        assert_eq!(on_disk(d.path(), ".env"), "one TOKEN=secret\n");
    }

    #[test]
    fn an_empty_old_string_is_refused_rather_than_matching_every_position() {
        let (_d, p) = plain();
        let why = ran(
            "edit_file",
            &json!({ "path": "src/lib.rs", "old_string": "", "new_string": "x" }),
            &p,
        )
        .expect_err("an empty match is not an edit");
        assert!(why.to_string().contains("empty"), "{why}");
    }

    #[test]
    fn a_create_writes_the_bytes_it_was_given_and_counts_every_line_as_added() {
        let (d, p) = plain();
        let got = ran(
            "write_file",
            &json!({ "path": "page.html", "contents": "<!doctype html>\n<title>hi</title>\n" }),
            &p,
        )
        .expect("a new file in the workspace is created");
        // The contents go down exactly as they came: no newline added, none taken away.
        assert_eq!(
            on_disk(d.path(), "page.html"),
            "<!doctype html>\n<title>hi</title>\n"
        );
        let edit = only_edit(&got);
        assert_eq!((edit.added, edit.removed, edit.occurrences), (2, 0, 1));
        assert_eq!(
            only_hunk(edit).lines,
            "+<!doctype html>\n+<title>hi</title>"
        );
        assert_eq!(only_hunk(edit).at, 1);
        assert_eq!(got.summary.text, "1 file created");
    }

    #[test]
    fn a_create_makes_the_directories_its_path_names() {
        let (d, p) = plain();
        ran(
            "write_file",
            &json!({ "path": "my-app/pages/index.html", "contents": "<p>hello</p>\n" }),
            &p,
        )
        .expect("a project is a tree, not one file");
        assert_eq!(
            on_disk(d.path(), "my-app/pages/index.html"),
            "<p>hello</p>\n"
        );
    }

    /// The host holds each target of a write before the call runs, so the pairing has to name
    /// exactly the files the call will touch — and nothing a refusal or a read would touch.
    #[test]
    fn a_write_target_names_the_file_the_call_changes_and_a_read_names_none() {
        let (d, p) = plain();
        // The sandbox resolves every path, so the answer is in the root's canonical spelling — on
        // macOS `/tmp` is a link to `/private/tmp`.
        let root = d.path().canonicalize().unwrap();
        assert_eq!(
            write_targets(
                "edit_file",
                &json!({ "path": "src/lib.rs", "old_string": "one", "new_string": "1" }),
                &p,
            ),
            vec![root.join("src/lib.rs")]
        );
        assert_eq!(
            write_targets(
                "write_file",
                &json!({ "path": "new.md", "contents": "x\n" }),
                &p,
            ),
            vec![root.join("new.md")]
        );
        assert_eq!(
            write_targets(
                "apply_patch",
                &json!({
                    "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,1 @@\n-one\n+1\n"
                }),
                &p,
            ),
            vec![root.join("src/lib.rs")]
        );
        // A read changes nothing, so there is nothing to hold.
        assert!(write_targets("read_file", &json!({ "path": "src/lib.rs" }), &p).is_empty());
        // The patch that creates a file belongs to `write_file`: this call is refused, and a file a
        // refused call does not touch is not one the review may offer to revert.
        assert!(write_targets(
            "apply_patch",
            &json!({ "patch": "--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1 @@\n+x\n" }),
            &p
        )
        .is_empty());
        // A target the sandbox refuses resolves to nothing rather than to a path outside the root.
        assert!(write_targets(
            "write_file",
            &json!({ "path": "../escape.md", "contents": "x\n" }),
            &p
        )
        .is_empty());
    }

    #[test]
    fn a_create_never_writes_over_a_file_that_is_already_there() {
        let (d, p) = plain();
        let why = ran(
            "write_file",
            &json!({ "path": "src/lib.rs", "contents": "everything replaced\n" }),
            &p,
        )
        .expect_err("an existing file is not this call's to overwrite");
        let reason = why.to_string();
        assert!(reason.contains("already there"), "{reason}");
        // The sentence names the tool that can do it, because the model is the reader that can.
        assert!(reason.contains("edit_file"), "{reason}");
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\ntwo\nthree\n");
    }

    #[test]
    fn a_refused_create_leaves_nothing_behind() {
        let (d, p) = fixture();
        // The directories are made after the name is known to be free, so a refusal cannot leave an
        // empty tree the listing would then have to explain.
        let why = ran(
            "write_file",
            &json!({ "path": "../escaped.html", "contents": "x\n" }),
            &p,
        )
        .expect_err("the workspace root bounds a create");
        assert!(matches!(why, ToolsError::Sandbox(_)), "{why}");
        assert!(!d.path().parent().unwrap().join("escaped.html").exists());
        let why = ran(
            "write_file",
            &json!({ "path": ".env", "contents": "TOKEN=x\n" }),
            &p,
        )
        .expect_err("a protected name cannot be written through the sandbox either");
        assert!(matches!(why, ToolsError::Sandbox(_)), "{why}");
        assert_eq!(on_disk(d.path(), ".env"), "one TOKEN=secret\n");
    }

    #[test]
    fn a_file_too_big_to_read_back_is_too_big_to_create() {
        let (d, p) = plain();
        let why = ran(
            "write_file",
            &json!({ "path": "big.txt", "contents": "x".repeat(MAX_FILE_BYTES as usize + 1) }),
            &p,
        )
        .expect_err("a write has the same ceiling as a read");
        assert!(why.to_string().contains("MB"), "{why}");
        assert!(!d.path().join("big.txt").exists(), "the refusal wrote it");
    }

    #[test]
    fn an_empty_create_makes_an_empty_file_and_says_so() {
        let (d, p) = plain();
        let got = ran(
            "write_file",
            &json!({ "path": "notes.md", "contents": "" }),
            &p,
        )
        .expect("an empty file is a file");
        assert_eq!(on_disk(d.path(), "notes.md"), "");
        assert_eq!(only_edit(&got).added, 0);
        assert!(got.content.contains("empty"), "{}", got.content);
    }

    #[test]
    fn a_long_create_shows_its_opening_and_names_the_lines_it_left_out() {
        let (d, p) = plain();
        let body = "line of the new file, padded out to a sensible width\n".repeat(2_000);
        let got = ran(
            "write_file",
            &json!({ "path": "long.txt", "contents": body }),
            &p,
        )
        .unwrap();
        assert!(
            got.content.len() <= MAX_RESULT_BYTES + 200,
            "{} bytes of answer",
            got.content.len()
        );
        assert!(
            got.content.contains("lines of the change are not shown"),
            "the cut is named: {}",
            &got.content[..120.min(got.content.len())]
        );
        // The counts stay the file's real ones even though the hunk is a window onto it.
        assert_eq!(only_edit(&got).added, 2_000);
        assert_eq!(
            on_disk(d.path(), "long.txt").lines().count(),
            2_000,
            "the hunk is a window onto the file, not the file"
        );
    }

    /// A patch call with the file named in both headers, so a test states only the hunk it is about.
    fn patch_of(file: &str, hunks: &str) -> Value {
        json!({ "patch": format!("--- a/{file}\n+++ b/{file}\n{hunks}") })
    }

    #[test]
    fn a_patch_applies_its_hunk_and_counts_the_lines_it_changed() {
        let (d, p) = plain();
        let got = ran(
            "apply_patch",
            &patch_of("src/lib.rs", "@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n"),
            &p,
        )
        .expect("a patch against the file as it stands applies");
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\nTWO\nthree\n");
        let edit = only_edit(&got);
        assert_eq!((edit.added, edit.removed, edit.occurrences), (1, 1, 1));
        assert_eq!(edit.path, "src/lib.rs");
        // The review's contract is the same as an edit's: the body is diff lines only (no `@@`
        // header, which its numbering would read as context), and `at` names the changed line.
        let hunk = only_hunk(edit);
        assert_eq!(hunk.lines, " one\n-two\n+TWO\n three");
        assert_eq!(hunk.at, 2);
        assert_eq!(got.summary.text, "1 hunk applied");
        assert_eq!(
            got.content,
            "Patched src/lib.rs (+1 −1): 1 hunk applied\nsrc/lib.rs, at line 2:\n one\n-two\n+TWO\n three"
        );
    }

    #[test]
    fn every_hunk_of_a_patch_is_applied_and_the_ones_the_review_cannot_draw_are_named() {
        let (d, p) = plain();
        std::fs::write(d.path().join("src/lib.rs"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        let got = ran(
            "apply_patch",
            &patch_of(
                "src/lib.rs",
                "@@ -1,2 +1,2 @@\n one\n-two\n+TWO\n@@ -4,2 +4,3 @@\n four\n+four and a half\n five\n",
            ),
            &p,
        )
        .expect("both hunks match the file");
        assert_eq!(
            on_disk(d.path(), "src/lib.rs"),
            "one\nTWO\nthree\nfour\nfour and a half\nfive\n"
        );
        let edit = only_edit(&got);
        assert_eq!((edit.added, edit.removed, edit.occurrences), (2, 1, 2));
        assert_eq!(got.summary.text, "2 hunks applied");
        // Both hunks are kept, each with the line its gutter starts on, so the review can draw the
        // change in two places rather than one hunk holding a line that is not in the file.
        assert_eq!(edit.hunks.len(), 2);
        assert_eq!(edit.hunks[0].at, 2);
        assert_eq!(edit.hunks[0].lines, " one\n-two\n+TWO");
        assert_eq!(edit.hunks[1].at, 5);
        assert_eq!(edit.hunks[1].lines, " four\n+four and a half\n five");
        // The model reads the first hunk in full and is told the rest were applied.
        assert!(got
            .content
            .contains("…1 more hunk applied, not shown here."));
    }

    #[test]
    fn a_hunk_whose_context_no_longer_matches_writes_nothing_and_names_the_line() {
        let (d, p) = plain();
        let why = ran(
            "apply_patch",
            &patch_of("src/lib.rs", "@@ -1,3 +1,3 @@\n one\n-TWO\n+TWO!\n three\n"),
            &p,
        )
        .expect_err("a patch written against other text cannot apply");
        let reason = why.to_string();
        assert!(reason.contains("does not match src/lib.rs"), "{reason}");
        assert!(reason.contains("line 2"), "{reason}");
        assert!(
            reason.contains("\"TWO\"") && reason.contains("\"two\""),
            "{reason}"
        );
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\ntwo\nthree\n");
    }

    #[test]
    fn a_patch_may_change_several_files_and_reports_each_of_them() {
        let (d, p) = plain();
        std::fs::write(d.path().join("other.txt"), "x\n").unwrap();
        let got = ran(
            "apply_patch",
            &json!({ "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n\
                                --- a/other.txt\n+++ b/other.txt\n@@ -1 +1 @@\n-x\n+y\n" }),
            &p,
        )
        .expect("one call may change several files");
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\nTWO\nthree\n");
        assert_eq!(on_disk(d.path(), "other.txt"), "y\n");
        assert_eq!(got.summary.text, "2 files patched");
        assert_eq!(got.edits.len(), 2);
        let lib = &got.edits[0];
        assert_eq!(
            (lib.path.as_str(), lib.added, lib.removed, lib.occurrences),
            ("src/lib.rs", 1, 1, 1)
        );
        assert_eq!(lib.hunks[0].at, 2);
        let other = &got.edits[1];
        assert_eq!(
            (
                other.path.as_str(),
                other.added,
                other.removed,
                other.occurrences
            ),
            ("other.txt", 1, 1, 1)
        );
        assert_eq!(other.hunks[0].at, 1);
        // The model reads every file the call changed, with that file's own counts.
        assert!(got
            .content
            .starts_with("Patched 2 files (+2 −2): 2 files patched\n"));
        assert!(
            got.content
                .contains("src/lib.rs (+1 −1), 1 hunk at line 2:"),
            "{}",
            got.content
        );
        assert!(
            got.content.contains("other.txt (+1 −1), 1 hunk at line 1:"),
            "{}",
            got.content
        );
    }

    #[test]
    fn a_patch_whose_last_file_does_not_match_writes_none_of_them() {
        let (d, p) = plain();
        std::fs::write(d.path().join("other.txt"), "x\n").unwrap();
        let why = ran(
            "apply_patch",
            &json!({ "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-one\n+ONE\n\
                                --- a/other.txt\n+++ b/other.txt\n@@ -1 +1 @@\n-wrong\n+y\n" }),
            &p,
        )
        .expect_err("every file is checked before any is written");
        assert!(
            why.to_string().contains("does not match other.txt"),
            "{why}"
        );
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\ntwo\nthree\n");
        assert_eq!(on_disk(d.path(), "other.txt"), "x\n");
    }

    #[test]
    fn a_patch_that_opens_one_file_in_two_sections_is_refused() {
        let (d, p) = plain();
        std::fs::write(d.path().join("other.txt"), "x\n").unwrap();
        let same_path = ran(
            "apply_patch",
            &json!({ "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-one\n+ONE\n\
                                --- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -3 +3 @@\n-three\n+THREE\n" }),
            &p,
        )
        .expect_err("the second section's line numbers would describe a changed file");
        assert!(
            same_path.to_string().contains("more than one section"),
            "{same_path}"
        );
        // `./src/lib.rs` and `src/lib.rs` are one file; the sandbox settles that, not the spelling.
        let two_spellings = ran(
            "apply_patch",
            &json!({ "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-one\n+ONE\n\
                                --- a/./src/lib.rs\n+++ b/./src/lib.rs\n@@ -3 +3 @@\n-three\n+THREE\n" }),
            &p,
        )
        .expect_err("two spellings of one path are one file");
        assert!(
            two_spellings.to_string().contains("another spelling"),
            "{two_spellings}"
        );
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\ntwo\nthree\n");
        assert_eq!(on_disk(d.path(), "other.txt"), "x\n");
    }

    #[test]
    fn a_patch_section_without_hunks_is_refused_and_writes_nothing() {
        let (d, p) = plain();
        let why = ran(
            "apply_patch",
            &json!({ "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n\
                               --- a/other.txt\n+++ b/other.txt\n@@ -1 +1 @@\n-one\n+ONE\n" }),
            &p,
        )
        .expect_err("a section with no hunks changes nothing and is a mistake");
        assert!(why.to_string().contains("carries no hunks"), "{why}");
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\ntwo\nthree\n");
    }

    #[test]
    fn a_patch_that_creates_or_deletes_a_file_is_refused_with_the_tool_that_does_it() {
        let (d, p) = plain();
        let create = ran(
            "apply_patch",
            &json!({ "patch": "--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1,1 @@\n+x\n" }),
            &p,
        )
        .expect_err("creation is the create tool's job");
        assert!(create.to_string().contains("write_file"), "{create}");
        assert!(!d.path().join("new.rs").exists());
        let delete = ran(
            "apply_patch",
            &json!({ "patch": "--- a/src/lib.rs\n+++ /dev/null\n@@ -1,3 +0,0 @@\n-one\n-two\n-three\n" }),
            &p,
        )
        .expect_err("a run cannot remove a file through a patch");
        assert!(delete.to_string().contains("remove"), "{delete}");
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\ntwo\nthree\n");
    }

    #[test]
    fn a_patch_for_a_file_that_does_not_exist_names_the_call_that_creates_one() {
        let (_d, p) = plain();
        let why = ran(
            "apply_patch",
            &patch_of("src/missing.rs", "@@ -0,0 +1,1 @@\n+first\n"),
            &p,
        )
        .expect_err("apply_patch changes an existing file, it does not make one");
        let reason = why.to_string();
        assert!(reason.contains("not a file"), "{reason}");
        assert!(reason.contains("write_file"), "{reason}");
    }

    #[test]
    fn a_hunk_that_adds_a_line_opens_on_the_line_it_lands_on() {
        let (d, p) = plain();
        let got = ran(
            "apply_patch",
            &patch_of("src/lib.rs", "@@ -2,0 +3,1 @@\n+inserted\n"),
            &p,
        )
        .expect("an empty old range is where an insertion lives");
        assert_eq!(
            on_disk(d.path(), "src/lib.rs"),
            "one\ntwo\ninserted\nthree\n"
        );
        let edit = only_edit(&got);
        // The change lands after line 2, so the review's gutter has to start numbering at 3 — not
        // at 2, which is the line the header names as the insert's anchor.
        let hunk = only_hunk(edit);
        assert_eq!(hunk.at, 3);
        assert_eq!((edit.added, edit.removed), (1, 0));
        assert_eq!(hunk.lines, "+inserted");
    }

    #[test]
    fn a_patch_keeps_a_windows_files_endings_and_a_missing_final_newline() {
        let (d, p) = plain();
        std::fs::write(d.path().join("src/lib.rs"), "one\r\ntwo\r\nthree\r\n").unwrap();
        ran(
            "apply_patch",
            &patch_of("src/lib.rs", "@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n"),
            &p,
        )
        .expect("a patch written with LF applies to a CRLF file");
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\r\nTWO\r\nthree\r\n");

        // The old side's last line has no newline and the new side's has none either: the result
        // must carry that fact rather than quietly adding one.
        std::fs::write(d.path().join("src/end.rs"), "one\ntwo").unwrap();
        ran(
            "apply_patch",
            &patch_of(
                "src/end.rs",
                "@@ -1,2 +1,2 @@\n one\n-two\n\\ No newline at end of file\n+TWO\n\\ No newline at end of file\n",
            ),
            &p,
        )
        .expect("a marker states what the last line does");
        assert_eq!(on_disk(d.path(), "src/end.rs"), "one\nTWO");

        // And a patch that only touches the old side's marker adds the newline the new side lacks.
        std::fs::write(d.path().join("src/end.rs"), "one\ntwo").unwrap();
        ran(
            "apply_patch",
            &patch_of(
                "src/end.rs",
                "@@ -1,2 +1,2 @@\n one\n-two\n\\ No newline at end of file\n+TWO\n",
            ),
            &p,
        )
        .expect("an unmarked new side ends with a newline");
        assert_eq!(on_disk(d.path(), "src/end.rs"), "one\nTWO\n");
    }

    #[test]
    fn a_file_whose_lines_end_two_ways_is_refused_rather_than_normalized() {
        let (d, p) = plain();
        std::fs::write(d.path().join("src/lib.rs"), "one\r\ntwo\nthree\n").unwrap();
        let why = ran(
            "apply_patch",
            &patch_of("src/lib.rs", "@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n"),
            &p,
        )
        .expect_err("applying to mixed endings would rewrite lines the patch never named");
        assert!(why.to_string().contains("mixes line endings"), "{why}");
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\r\ntwo\nthree\n");
    }

    #[test]
    fn a_hunk_header_whose_counts_do_not_match_its_body_is_refused() {
        let (d, p) = plain();
        let short = ran(
            "apply_patch",
            &patch_of("src/lib.rs", "@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n"),
            &p,
        )
        .expect_err("three old lines were declared and two carried");
        assert!(short.to_string().contains("patch ends after"), "{short}");
        let long = ran(
            "apply_patch",
            &patch_of("src/lib.rs", "@@ -1,1 +1,1 @@\n one\n-two\n+TWO\n"),
            &p,
        )
        .expect_err("one old line was declared and two carried");
        assert!(long.to_string().contains("one line too many"), "{long}");
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\ntwo\nthree\n");
    }

    #[test]
    fn a_patch_path_is_resolved_through_the_sandbox_before_the_file_is_opened() {
        let (d, p) = plain();
        let outside = d.path().parent().unwrap().join("escaped.rs");
        let _ = std::fs::remove_file(&outside);
        let why = ran(
            "apply_patch",
            &patch_of("../escaped.rs", "@@ -0,0 +1,1 @@\n+x\n"),
            &p,
        )
        .expect_err("the workspace root bounds a patch as it bounds an edit");
        assert!(matches!(why, ToolsError::Sandbox(_)), "{why}");
        assert!(!outside.exists(), "the refusal wrote the file anyway");
    }

    #[test]
    fn a_patch_onto_a_protected_name_is_refused_as_an_edit_is() {
        let (d, p) = fixture();
        let why = ran(
            "apply_patch",
            &patch_of(
                ".env",
                "@@ -1 +1 @@\n-one TOKEN=secret\n+one TOKEN=public\n",
            ),
            &p,
        )
        .expect_err("a secret the sandbox hides cannot be changed through a patch");
        assert!(matches!(why, ToolsError::Sandbox(_)), "{why}");
        assert_eq!(on_disk(d.path(), ".env"), "one TOKEN=secret\n");
    }

    #[test]
    fn a_patch_that_is_not_a_diff_is_refused_without_touching_anything() {
        let (d, p) = plain();
        for (patch, says) in [
            ("", "empty"),
            ("replace line two with TWO\n", "not part of a unified diff"),
            ("--- a/src/lib.rs\n+++ b/src/lib.rs\n", "carries no hunks"),
            (
                "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n one\n same\n",
                "changes nothing",
            ),
        ] {
            let why = ran("apply_patch", &json!({ "patch": patch }), &p)
                .expect_err("text that is not a change is not applied");
            assert!(why.to_string().contains(says), "{patch:?}: {why}");
        }
        assert_eq!(on_disk(d.path(), "src/lib.rs"), "one\ntwo\nthree\n");
    }

    /// The switch is the whole gate: with it off, `authorize` refuses the call before [`crate::web`]
    /// is reached, so no packet leaves and no round is spent finding that out.
    #[test]
    fn a_web_call_is_refused_while_the_network_gate_is_closed() {
        let (_d, p) = fixture();
        for (id, args) in [
            ("web_search", json!({ "query": "Grace Hopper" })),
            ("fetch_url", json!({ "url": "https://example.com" })),
        ] {
            let err = ran(id, &args, &p).expect_err("{id} cannot run with the gate closed");
            assert!(
                matches!(err, ToolsError::NetworkNotConfigured(_)),
                "{err:?}"
            );
        }
    }

    /// The workspace with Terminal access on, as the host builds it from the user's switch.
    fn shell_policy(root: &Path) -> SandboxPolicy {
        SandboxPolicy {
            workspace_root: root.to_path_buf(),
            shell_allowed: true,
            shell_timeout_secs: 120,
            ..Default::default()
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_command_starts_in_the_workspace_and_answers_with_its_output() {
        let (d, _) = plain();
        let p = shell_policy(d.path());
        // The write lands in the workspace root without being told a directory: the cwd is the whole
        // reason a run's build does not scatter artefacts somewhere else.
        let got = ran(
            "terminal",
            &json!({ "command": "printf hi > made-by-shell.txt" }),
            &p,
        )
        .unwrap();
        assert_eq!(got.summary.text, "exit 0");
        assert_eq!(
            std::fs::read_to_string(d.path().join("made-by-shell.txt")).unwrap(),
            "hi"
        );
        assert!(got.content.starts_with("$ printf hi"), "{}", got.content);
        assert!(got.content.contains("[exit 0 in "), "{}", got.content);
        // A silent command says so rather than leaving a blank result.
        let silent = ran("terminal", &json!({ "command": "true" }), &p).unwrap();
        assert!(silent.content.contains("(no output)"), "{}", silent.content);
        // And a `cwd` inside the workspace is where the command starts instead.
        ran(
            "terminal",
            &json!({ "command": "printf x > nested.txt", "cwd": "src" }),
            &p,
        )
        .unwrap();
        assert!(d.path().join("src/nested.txt").exists());
    }

    #[test]
    fn a_command_is_refused_while_terminal_access_is_off() {
        let (d, p) = plain();
        assert!(!p.shell_allowed, "the fixture is the default policy");
        let err = ran("terminal", &json!({ "command": "echo hi" }), &p)
            .expect_err("a switched-off shell cannot run");
        assert!(matches!(err, ToolsError::ShellDisabled), "{err:?}");
        // The refusal is the same one `authorize` gives every other caller of `execute`.
        assert!(!d.path().join("made-by-shell.txt").exists());
    }

    #[test]
    fn a_command_cannot_start_outside_the_workspace() {
        let (d, p) = plain();
        let p = SandboxPolicy {
            shell_allowed: true,
            ..p
        };
        let err = ran("terminal", &json!({ "command": "ls", "cwd": ".." }), &p)
            .expect_err("the workspace is the command's boundary too");
        assert!(matches!(err, ToolsError::Sandbox(_)), "{err:?}");
        // A path inside the workspace that is not a directory names the mistake instead of running.
        let err = ran(
            "terminal",
            &json!({ "command": "ls", "cwd": "src/lib.rs" }),
            &p,
        )
        .expect_err("a file is not a starting directory");
        assert!(err.to_string().contains("not a directory"), "{err}");
        let err = ran("terminal", &json!({ "command": "  " }), &p)
            .expect_err("an empty command is not a command");
        assert!(err.to_string().contains("empty"), "{err}");
        let _ = d;
    }

    #[cfg(unix)]
    #[test]
    fn a_command_reports_its_output_while_it_is_still_running() {
        let (d, _) = plain();
        let p = shell_policy(d.path());
        let started = Instant::now();
        let first_at = Arc::new(std::sync::Mutex::new(None::<Duration>));
        let seen = Arc::new(std::sync::Mutex::new(String::new()));
        let (at, text) = (first_at.clone(), seen.clone());
        // The command writes, waits, then writes again. A watcher fed only the final capture would see
        // both lines at once, and the call would return before "first" was ever reported.
        let watcher: Watcher = Arc::new(move |bytes: &[u8]| {
            let mut at = at.lock().unwrap();
            if at.is_none() {
                *at = Some(started.elapsed());
            }
            text.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(bytes));
        });
        let got = execute_watched(
            "terminal",
            &json!({ "command": "printf first; sleep 0.4; printf second" }),
            &p,
            Some(&watcher),
            None,
            None,
        )
        .unwrap();
        let done = started.elapsed();
        let first = first_at.lock().unwrap().expect("output was reported");
        assert!(
            done.saturating_sub(first) >= Duration::from_millis(150),
            "output arrived only at the end: first at {first:?}, returned at {done:?}"
        );
        assert_eq!(seen.lock().unwrap().as_str(), "firstsecond");
        // The watcher does not change what the call answers with: the same report, from the same tail.
        assert!(got.content.contains("$ printf first"), "{}", got.content);
        assert!(got.summary.text == "exit 0", "{}", got.summary.text);
        // And a call that runs no command reports nothing, even when one is watching.
        let quiet = Arc::new(std::sync::Mutex::new(String::new()));
        let sink = quiet.clone();
        let watcher: Watcher = Arc::new(move |bytes: &[u8]| {
            sink.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(bytes));
        });
        execute_watched(
            "read_file",
            &json!({ "path": "src/lib.rs" }),
            &p,
            Some(&watcher),
            None,
            None,
        )
        .unwrap();
        assert!(quiet.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_command_answers_its_exit_code_and_its_stderr() {
        let (d, _) = plain();
        let p = shell_policy(d.path());
        let got = ran(
            "terminal",
            &json!({ "command": "echo oops >&2; exit 3" }),
            &p,
        )
        .unwrap();
        // A nonzero exit is the command's answer, not a failed call: the model asked what it would say.
        assert_eq!(got.summary.text, "exit 3");
        assert!(got.content.contains("oops"), "{}", got.content);
        assert!(got.content.contains("[exit 3 in "), "{}", got.content);
    }

    #[cfg(unix)]
    #[test]
    fn a_command_past_its_timeout_is_stopped_and_says_after_how_long() {
        let (d, _) = plain();
        let p = shell_policy(d.path());
        let started = Instant::now();
        let got = ran(
            "terminal",
            &json!({ "command": "sleep 5", "timeout_seconds": 1 }),
            &p,
        )
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the deadline did not stop the command: {:?}",
            started.elapsed()
        );
        assert_eq!(got.summary.text, "stopped after 1s");
        assert!(
            got.content.contains("[stopped after 1s in"),
            "{}",
            got.content
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_the_tail_of_a_flood_is_kept_and_the_gap_is_named() {
        let (d, _) = plain();
        let p = shell_policy(d.path());
        let got = ran("terminal", &json!({ "command": "seq 1 20000" }), &p).unwrap();
        assert!(
            got.content.len() <= MAX_RESULT_BYTES + 300,
            "{} bytes of answer",
            got.content.len()
        );
        assert!(
            got.content.contains("are not shown"),
            "the cut is named: {}",
            &got.content[..120.min(got.content.len())]
        );
        // The tail is what a build's errors are in, so the last number survives the cut.
        assert!(got.content.contains("20000"), "{}", got.content);
        assert!(!got.content.contains("\n1\n"), "the head was kept instead");
    }

    /// The three project runners are the same consent as the shell: they execute code the workspace
    /// already carries, so while Terminal access is off they are refused before a manifest is read.
    #[test]
    fn a_project_runner_is_refused_while_terminal_access_is_off() {
        let (d, _) = plain();
        let p = policy(d.path());
        assert!(!p.shell_allowed);
        for id in ["run_tests", "run_build", "run_linter"] {
            let err = ran(id, &json!({}), &p).expect_err("a runner executes project code");
            assert!(matches!(err, ToolsError::ShellDisabled), "{id}: {err:?}");
        }
        // The repository reads are not commands: they run a fixed argv and stay available.
        let status = ran("git_status", &json!({}), &p).expect("a git read needs no switch");
        assert_eq!(status.summary.key, Some(SummaryKey::GitNotRepo));
    }

    #[test]
    fn the_checks_a_workspace_declares_are_listed_in_the_order_a_run_tries_them() {
        let cargo = tempfile::tempdir().unwrap();
        std::fs::write(cargo.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        assert_eq!(
            declared_checks(cargo.path()),
            ["run_tests", "run_build", "run_linter"]
        );

        // A manifest that declares only a build has no test to run, so the build is the first check
        // rather than a test call the host would have to fail its way through.
        let node = tempfile::tempdir().unwrap();
        std::fs::write(
            node.path().join("package.json"),
            r#"{"scripts":{"build":"vite build"}}"#,
        )
        .unwrap();
        std::fs::write(node.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(declared_checks(node.path()), ["run_build"]);

        let empty = tempfile::tempdir().unwrap();
        assert!(declared_checks(empty.path()).is_empty());
    }

    #[test]
    fn only_a_command_that_did_not_succeed_reports_a_failure() {
        assert!(!Summary::said(SummaryKey::TerminalExit, json!({ "code": 0 }), "exit 0").failed());
        assert!(Summary::said(SummaryKey::TerminalExit, json!({ "code": 1 }), "exit 1").failed());
        assert!(Summary::said(
            SummaryKey::TerminalTimedOut,
            json!({ "seconds": 5 }),
            "stopped after 5s"
        )
        .failed());
        // Every other line describes a read or a write that returned, so it cannot say a check failed.
        assert!(!Summary::verbatim("src/main.ts").failed());
    }

    #[test]
    fn a_workspace_with_no_manifest_says_what_the_runner_looked_for() {
        let (d, _) = plain();
        let p = shell_policy(d.path());
        let err = ran("run_tests", &json!({}), &p).expect_err("nothing says how to test this");
        assert!(matches!(err, ToolsError::NotConfigured(_)), "{err:?}");
        let text = err.to_string();
        for name in [
            "Cargo.toml",
            "package.json",
            "go.mod",
            "pyproject.toml",
            "Makefile",
        ] {
            assert!(text.contains(name), "{name} missing from: {text}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_makefile_workspace_runs_the_target_it_declares() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Makefile"),
            "test:\n\t@printf 'SPECIALS PASS\\n'\n",
        )
        .unwrap();
        let p = shell_policy(dir.path());
        let got = ran("run_tests", &json!({}), &p).unwrap();
        assert_eq!(got.summary.text, "exit 0");
        assert!(got.content.contains("SPECIALS PASS"), "{}", got.content);
        // The report names the file that chose the command, so the model can change it.
        assert!(got.content.contains("(Makefile)"), "{}", got.content);
        // A target the file does not declare is a refusal, not a `make` error the model has to decode.
        let err = ran("run_linter", &json!({}), &p).expect_err("no lint target");
        assert!(err.to_string().contains("no \"lint\" target"), "{err}");
    }

    #[test]
    fn each_manifest_names_the_command_its_project_runs() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let tests = Wanted {
            kind: ProjectKind::Tests,
            target: Some("core"),
            filter: Some("auth"),
            release: false,
            paths: &[],
        };
        let cargo = detect(dir, &tests).unwrap();
        assert_eq!(cargo.program, "cargo");
        assert_eq!(cargo.args, ["test", "-p", "core", "auth"]);
        let lint = Wanted {
            kind: ProjectKind::Lint,
            target: None,
            filter: None,
            release: false,
            paths: &[],
        };
        assert_eq!(
            detect(dir, &lint).unwrap().args,
            ["clippy", "--all-targets"]
        );
        // A field the chosen toolchain cannot honour is refused rather than dropped, so a model that
        // asked for a narrow run cannot be told the whole suite passed.
        let release = Wanted {
            kind: ProjectKind::Tests,
            release: true,
            ..tests
        };
        assert!(detect(dir, &release).unwrap_err().contains("run_build"));

        let node = tempfile::tempdir().unwrap();
        std::fs::write(
            node.path().join("package.json"),
            r#"{"scripts":{"test":"x","build":"y","lint":"z"}}"#,
        )
        .unwrap();
        std::fs::write(node.path().join("pnpm-lock.yaml"), "").unwrap();
        let node_lint = detect(
            node.path(),
            &Wanted {
                kind: ProjectKind::Lint,
                target: None,
                filter: None,
                release: false,
                paths: &["src".to_string()],
            },
        )
        .unwrap();
        assert_eq!(node_lint.program, "pnpm");
        assert_eq!(node_lint.args, ["run", "lint", "--", "src"]);

        let go = tempfile::tempdir().unwrap();
        std::fs::write(go.path().join("go.mod"), "module x\n").unwrap();
        assert_eq!(
            detect(go.path(), &lint).unwrap().args,
            ["vet", "./..."],
            "go lint is vet over the module"
        );

        let python = tempfile::tempdir().unwrap();
        std::fs::write(
            python.path().join("pyproject.toml"),
            "[tool.ruff]\nline-length = 88\n",
        )
        .unwrap();
        let ruff = detect(python.path(), &lint).unwrap();
        assert_eq!(ruff.program, python_program());
        assert_eq!(ruff.args, ["-m", "ruff", "check", "."]);

        let make = tempfile::tempdir().unwrap();
        std::fs::write(make.path().join("Makefile"), "all:\n\t@true\n").unwrap();
        let build = Wanted {
            kind: ProjectKind::Build,
            target: None,
            filter: None,
            release: false,
            paths: &[],
        };
        assert_eq!(detect(make.path(), &build).unwrap().args, ["all"]);
    }

    /// A workspace in a git repository, with the files committed so a later change is a real diff.
    #[cfg(unix)]
    fn committed_workspace(files: &[(&str, &str)]) -> (tempfile::TempDir, SandboxPolicy) {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        let home = dir.path().join(".home");
        std::fs::create_dir_all(&home).unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("HOME", &home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q"]);
        // The identity is the repository's own, so a commit made by the tool under test is the same
        // commit the fixture's first one is, whatever the machine running the test has configured.
        run(&["config", "user.name", "T"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["add", "-A"]);
        run(&[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "first",
        ]);
        let p = shell_policy(dir.path());
        (dir, p)
    }

    #[cfg(unix)]
    #[test]
    fn a_git_status_says_clean_or_how_many_files_changed() {
        let (d, p) = committed_workspace(&[("a.txt", "one\ntwo\n")]);
        let clean = ran("git_status", &json!({}), &p).unwrap();
        assert_eq!(clean.summary.key, Some(SummaryKey::GitClean));
        assert!(
            clean.content.contains("Working tree clean"),
            "{}",
            clean.content
        );
        std::fs::write(d.path().join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(d.path().join("b.txt"), "new\n").unwrap();
        let dirty = ran("git_status", &json!({}), &p).unwrap();
        assert_eq!(dirty.summary.key, Some(SummaryKey::ChangedFiles));
        assert_eq!(dirty.summary.params["count"], json!(2));
        assert!(dirty.content.contains("?? b.txt"), "{}", dirty.content);
    }

    #[cfg(unix)]
    #[test]
    fn a_git_diff_counts_lines_from_the_numstat_read() {
        let (d, p) = committed_workspace(&[("a.txt", "one\ntwo\nthree\n")]);
        let clean = ran("git_diff", &json!({}), &p).unwrap();
        assert_eq!(clean.summary.key, Some(SummaryKey::GitClean));
        std::fs::write(d.path().join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        let got = ran("git_diff", &json!({}), &p).unwrap();
        assert_eq!(got.summary.key, Some(SummaryKey::GitDiffFile));
        assert_eq!(got.summary.params["added"], json!(2));
        assert_eq!(got.summary.params["removed"], json!(1));
        assert!(got.content.contains("@@"), "{}", got.content);
        // The staged view is a different read: nothing is staged yet.
        let staged = ran("git_diff", &json!({ "staged": true }), &p).unwrap();
        assert_eq!(staged.summary.key, Some(SummaryKey::GitClean));
    }

    #[cfg(unix)]
    #[test]
    fn a_git_log_counts_the_commits_and_says_when_there_are_none() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".home");
        std::fs::create_dir_all(&home).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("HOME", &home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let p = shell_policy(dir.path());
        let none = ran("git_log", &json!({}), &p).unwrap();
        assert_eq!(none.summary.key, Some(SummaryKey::GitCommits));
        assert_eq!(none.summary.params["count"], json!(0));
        assert!(none.content.contains("no commits yet"), "{}", none.content);
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "first",
        ]);
        let one = ran("git_log", &json!({}), &p).unwrap();
        assert_eq!(one.summary.key, Some(SummaryKey::GitCommit));
        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "second",
        ]);
        let two = ran("git_log", &json!({ "limit": 1 }), &p).unwrap();
        assert_eq!(
            two.summary.key,
            Some(SummaryKey::GitCommit),
            "the limit bounds the read"
        );
        let both = ran("git_log", &json!({}), &p).unwrap();
        assert_eq!(both.summary.params["count"], json!(2));
    }

    #[cfg(unix)]
    #[test]
    fn a_stage_puts_the_named_file_in_the_index_and_counts_what_the_index_holds() {
        let (d, p) = committed_workspace(&[("a.txt", "one\n")]);
        std::fs::write(d.path().join("a.txt"), "two\n").unwrap();
        std::fs::write(d.path().join("b.txt"), "new\n").unwrap();
        let staged = ran("git_stage", &json!({ "paths": ["a.txt"] }), &p).unwrap();
        assert_eq!(staged.summary.key, Some(SummaryKey::GitStaged));
        assert_eq!(staged.summary.params["count"], json!(1));
        // Only what was named is staged: the file that was left alone is still unstaged.
        let unstaged = ran("git_status", &json!({}), &p).unwrap();
        assert!(
            unstaged.content.contains("?? b.txt"),
            "{}",
            unstaged.content
        );
        // A path outside the workspace is refused before git is handed it.
        let outside = ran("git_stage", &json!({ "paths": ["../elsewhere.txt"] }), &p);
        assert!(
            matches!(outside, Err(ToolsError::Sandbox(_))),
            "{outside:?}"
        );
        // And a call that names no file at all is told what is missing.
        let none = ran("git_stage", &json!({ "paths": [] }), &p);
        assert!(
            matches!(none, Err(ToolsError::InvalidArguments { .. })),
            "{none:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_commit_writes_the_staged_change_and_names_the_commit_it_made() {
        let (d, p) = committed_workspace(&[("a.txt", "one\n")]);
        std::fs::write(d.path().join("a.txt"), "two\n").unwrap();
        ran("git_stage", &json!({ "paths": ["a.txt"] }), &p).unwrap();
        let committed = ran("git_commit", &json!({ "message": "second" }), &p).unwrap();
        assert_eq!(committed.summary.key, Some(SummaryKey::GitCommitted));
        let sha = committed.summary.params["sha"].as_str().unwrap();
        assert!(!sha.is_empty(), "the row names the commit it made");
        // The commit is a real one: the history now holds two.
        let log = ran("git_log", &json!({}), &p).unwrap();
        assert_eq!(log.summary.params["count"], json!(2));
        // A commit with nothing in it says so rather than claiming a commit it did not make. Git
        // words the refusal in its own language, so the check is the history and not the sentence.
        let empty = ran("git_commit", &json!({ "message": "third" }), &p).unwrap();
        let after = ran("git_log", &json!({}), &p).unwrap();
        assert_eq!(after.summary.params["count"], json!(2));
        assert!(
            empty.content.to_lowercase().contains("nothing to commit")
                || empty.content.to_lowercase().contains("нечего коммитить"),
            "{}",
            empty.content
        );
        // And a commit with no message is refused before git is asked.
        let blank = ran("git_commit", &json!({ "message": "  " }), &p);
        assert!(
            matches!(blank, Err(ToolsError::InvalidArguments { .. })),
            "{blank:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_branch_is_created_when_it_is_missing_and_switched_to_when_it_is_there() {
        let (d, p) = committed_workspace(&[("a.txt", "one\n")]);
        let made = ran("git_branch", &json!({ "name": "feature/one" }), &p).unwrap();
        assert_eq!(made.summary.key, Some(SummaryKey::GitSwitched));
        assert_eq!(made.summary.params["branch"], json!("feature/one"));
        let head = ran("git_log", &json!({}), &p).unwrap();
        assert!(head.content.contains("feature/one"), "{}", head.content);
        // A second call switches to the branch that is already there.
        ran("git_branch", &json!({ "name": "main" }), &p).unwrap();
        let again = ran("git_branch", &json!({ "name": "feature/one" }), &p).unwrap();
        assert_eq!(again.summary.key, Some(SummaryKey::GitSwitched));
        // A name git would read as something other than a branch is refused here, not by git.
        let odd = ran("git_branch", &json!({ "name": "--upload-pack=evil" }), &p);
        assert!(
            matches!(odd, Err(ToolsError::InvalidArguments { .. })),
            "{odd:?}"
        );
        let _ = d;
    }

    #[cfg(unix)]
    #[test]
    fn a_pull_request_asks_for_a_token_rather_than_failing_on_a_call_it_never_sent() {
        let (d, p) = committed_workspace(&[("a.txt", "one\n")]);
        // The Privacy switch off is refused first: nothing leaves the machine.
        let local = ran("github_pr", &json!({ "title": "a change" }), &p);
        assert!(
            matches!(local, Err(ToolsError::NetworkNotConfigured(_))),
            "{local:?}"
        );
        // With it on, the call reaches the door and finds no token behind it.
        let online = SandboxPolicy {
            network_allowed: true,
            ..p.clone()
        };
        let no_token = ran("github_pr", &json!({ "title": "a change" }), &online);
        match no_token {
            Err(ToolsError::NotConfigured(why)) => assert!(
                why.contains("Settings") && why.contains("GitHub token"),
                "{why}"
            ),
            other => panic!("a call with no token must say so, not try: {other:?}"),
        }
        let _ = d;
    }

    /// The values a runner passes on are argv entries, never shell syntax: a flag-shaped filter, a
    /// path that climbs out, and a name with a space are all refused before a process exists.
    #[test]
    fn a_runner_argument_that_could_become_syntax_never_reaches_an_argv() {
        let (d, _) = plain();
        let p = shell_policy(d.path());
        let err = ran("run_tests", &json!({ "filter": "--all" }), &p).expect_err("a flag");
        assert!(
            matches!(err, ToolsError::InvalidArguments { .. }),
            "{err:?}"
        );
        let err = ran("run_tests", &json!({ "paths": ["../outside"] }), &p).expect_err("escape");
        assert!(matches!(err, ToolsError::Sandbox(_)), "{err:?}");
        let err = ran("run_linter", &json!({ "paths": ["--force"] }), &p)
            .expect_err("a path shaped like a flag");
        assert!(
            matches!(err, ToolsError::InvalidArguments { .. }),
            "{err:?}"
        );
        let err = ran("run_build", &json!({ "target": "my crate" }), &p).expect_err("a space");
        assert!(
            matches!(err, ToolsError::InvalidArguments { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn an_address_the_web_tools_may_not_reach_is_refused_rather_than_tried() {
        let (_d, p) = fixture();
        let p = SandboxPolicy {
            network_allowed: true,
            ..p
        };
        // Nothing here resolves a name or opens a connection: the guard runs first, and these are the
        // two shapes it exists to stop — a path on this machine, and one of its own addresses.
        for url in ["file:///etc/passwd", "http://127.0.0.1:8080/health"] {
            let err = ran("fetch_url", &json!({ "url": url }), &p)
                .expect_err("{url} is not a public page");
            assert!(matches!(err, ToolsError::Web(_)), "{url}: {err:?}");
        }
        let err = ran("web_search", &json!({}), &p).expect_err("a search needs words");
        assert!(
            matches!(err, ToolsError::InvalidArguments { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn a_plan_is_recorded_and_its_count_is_the_summary() {
        let (_d, p) = fixture();
        let args = json!({
            "items": [
                { "text": "Read the manifest", "status": "done" },
                { "text": "Add the flag", "status": "active" },
                { "text": "Run the tests", "status": "pending" }
            ]
        });
        let got = ran("todo_write", &args, &p).expect("a plan touches no file");
        assert!(got.edits.is_empty(), "a plan writes nothing");
        assert_eq!(got.summary.text, "1 of 3 steps done");
        assert_eq!(
            got.summary.params,
            json!({ "done": 1, "total": 3 }),
            "the sentence's two blanks"
        );
        // The marks are the model's own echo: a step it marked done reads done.
        assert!(
            got.content.contains("1. [x] Read the manifest"),
            "{}",
            got.content
        );
        assert!(
            got.content.contains("2. [~] Add the flag"),
            "{}",
            got.content
        );
        assert!(
            got.content.contains("3. [ ] Run the tests"),
            "{}",
            got.content
        );
    }

    #[test]
    fn a_plan_the_run_could_not_follow_is_refused_with_the_step_that_broke_it() {
        let (_d, p) = fixture();
        let two_actives = json!({ "items": [
            { "text": "one", "status": "active" },
            { "text": "two", "status": "active" }
        ] });
        let err = ran("todo_write", &two_actives, &p).expect_err("one step at a time");
        assert!(
            err.to_string().contains("only one step may be active"),
            "{err}"
        );

        let empty = json!({ "items": [] });
        assert!(matches!(
            ran("todo_write", &empty, &p),
            Err(ToolsError::InvalidArguments { .. })
        ));

        let blank = json!({ "items": [{ "text": "   ", "status": "pending" }] });
        assert!(ran("todo_write", &blank, &p)
            .expect_err("a step needs words")
            .to_string()
            .contains("step 1 has no text"));

        let unknown = json!({ "items": [{ "text": "one", "status": "started" }] });
        assert!(ran("todo_write", &unknown, &p)
            .expect_err("three states only")
            .to_string()
            .contains("pending, active or done"));

        let long = "x".repeat(MAX_PLAN_TEXT + 1);
        let too_long = json!({ "items": [{ "text": long, "status": "pending" }] });
        assert!(ran("todo_write", &too_long, &p)
            .expect_err("a step is a phrase")
            .to_string()
            .contains("longer than"));
    }

    /// The window's panel, as the executors see it: it records each operation and the fields it was
    /// sent, and answers with one canned reply.
    struct FakePage {
        seen: std::sync::Mutex<Vec<(PageOp, Value, Duration)>>,
        answer: Value,
    }

    impl FakePage {
        fn saying(answer: Value) -> Self {
            Self {
                seen: std::sync::Mutex::new(Vec::new()),
                answer,
            }
        }

        fn seen(&self) -> Vec<(PageOp, Value, Duration)> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl PageHost for FakePage {
        fn ask(&self, op: PageOp, arguments: &Value, patience: Duration) -> Result<Value, String> {
            self.seen
                .lock()
                .unwrap()
                .push((op, arguments.clone(), patience));
            Ok(self.answer.clone())
        }
    }

    /// A workspace with one page and one source file, which is the difference `browser_open` judges.
    fn page_fixture() -> (tempfile::TempDir, SandboxPolicy) {
        let (dir, p) = plain();
        std::fs::write(
            dir.path().join("index.html"),
            "<!doctype html><title>Mine</title><h1>Hello</h1>",
        )
        .unwrap();
        (dir, p)
    }

    #[test]
    fn a_page_open_asks_the_panel_for_the_sandboxed_path_and_shows_what_it_answered() {
        let (_d, p) = page_fixture();
        let host = FakePage::saying(json!({
            "url": "preview://localhost/index.html",
            "title": "Mine",
            "text": "Hello"
        }));
        let got = page_tool(
            "browser_open",
            &json!({ "path": "index.html" }),
            &p,
            Some(&host),
        )
        .expect("a workspace page opens");
        assert_eq!(got.summary.key, Some(SummaryKey::BrowserOpened));
        assert_eq!(got.summary.params, json!({ "file": "index.html" }));
        assert!(got.content.contains("preview://localhost/index.html"));
        assert!(got.content.contains("Title: Mine"));
        assert!(got.content.contains("Hello"));
        let seen = host.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, PageOp::Open);
        // The host is told the workspace-relative path, not the absolute one the sandbox resolved.
        assert_eq!(seen[0].1, json!({ "path": "index.html" }));
    }

    #[test]
    fn a_page_open_refuses_a_source_file_and_a_path_outside_the_workspace() {
        let (d, p) = page_fixture();
        let host = FakePage::saying(json!({}));
        let why = page_tool(
            "browser_open",
            &json!({ "path": "src/lib.rs" }),
            &p,
            Some(&host),
        )
        .expect_err("a source file is not a page");
        assert!(why.to_string().contains("not an HTML page"), "{why}");
        assert!(host.seen().is_empty(), "nothing reached the panel");

        let outside = d.path().parent().unwrap().join("elsewhere.html");
        let why = page_tool(
            "browser_open",
            &json!({ "path": outside.to_str().unwrap() }),
            &p,
            Some(&host),
        )
        .expect_err("the sandbox refuses a path outside the workspace");
        assert!(host.seen().is_empty(), "{why}");
    }

    #[test]
    fn a_read_sends_a_selector_only_when_one_was_given_and_caps_the_text() {
        let (_d, p) = page_fixture();
        let long = "x".repeat(MAX_PAGE_TEXT + 37);
        let host = FakePage::saying(json!({
            "url": "preview://localhost/index.html",
            "title": "Mine",
            "text": long
        }));
        let got = page_tool("browser_read", &json!({}), &p, Some(&host)).expect("a read runs");
        assert_eq!(got.summary.key, Some(SummaryKey::BrowserText));
        assert_eq!(
            got.summary.params,
            json!({ "count": MAX_PAGE_TEXT + 37 }),
            "the count is the whole page, not the part that fit"
        );
        assert!(got.content.contains("longer than"), "{}", got.content);
        assert_eq!(host.seen()[0].1, json!({}));

        page_tool(
            "browser_read",
            &json!({ "selector": "h1" }),
            &p,
            Some(&host),
        )
        .expect("a read of one element runs");
        assert_eq!(host.seen()[1].1, json!({ "selector": "h1" }));
    }

    #[test]
    fn a_wait_that_ran_out_is_a_failed_row_that_quotes_the_page() {
        let (_d, p) = page_fixture();
        let host = FakePage::saying(json!({
            "found": false,
            "elapsed_ms": 3000,
            "text": "Nothing here yet"
        }));
        let got = page_tool(
            "browser_wait",
            &json!({ "selector": "#ready" }),
            &p,
            Some(&host),
        )
        .expect("the wait itself finished");
        assert_eq!(got.summary.key, Some(SummaryKey::BrowserNotFound));
        assert!(got.summary.failed());
        assert!(got.content.contains("did not appear"), "{}", got.content);
        assert!(got.content.contains("Nothing here yet"), "{}", got.content);
        let seen = host.seen();
        assert_eq!(seen[0].0, PageOp::Wait);
        assert_eq!(seen[0].1["timeout_ms"], json!(3000));
        assert!(
            seen[0].2 > Duration::from_secs(15),
            "the panel gets the longest timeout plus travel: {:?}",
            seen[0].2
        );
    }

    #[test]
    fn the_page_calls_refuse_a_run_with_no_panel() {
        let (_d, p) = page_fixture();
        let why = page_tool("browser_click", &json!({ "selector": "button" }), &p, None)
            .expect_err("a subagent has no panel");
        assert!(matches!(why, ToolsError::Page(_)));
        assert!(why.to_string().contains("no browser panel"), "{why}");
    }

    #[test]
    fn a_console_with_nothing_logged_is_clean_and_one_with_lines_names_them() {
        let (_d, p) = page_fixture();
        let quiet = FakePage::saying(json!({ "lines": [] }));
        let got = page_tool("browser_console", &json!({}), &p, Some(&quiet)).expect("a read runs");
        assert_eq!(got.summary.key, Some(SummaryKey::BrowserClean));
        assert!(got.content.contains("clean"), "{}", got.content);

        let noisy = FakePage::saying(json!({ "lines": [
            { "level": "error", "text": "Uncaught TypeError: draw is not a function" },
            { "level": "warn", "text": "deprecated" }
        ] }));
        let got = page_tool("browser_console", &json!({}), &p, Some(&noisy)).expect("a read runs");
        assert_eq!(got.summary.key, Some(SummaryKey::BrowserProblems));
        assert_eq!(got.summary.params, json!({ "count": 2 }));
        assert!(
            got.content.contains("[error] Uncaught TypeError"),
            "{}",
            got.content
        );
    }

    #[test]
    fn a_capture_names_the_file_it_kept_and_the_size_it_drew() {
        let (_d, p) = page_fixture();
        let host = FakePage::saying(json!({
            "path": "/Users/someone/Library/Application Support/freecode/captures/page-1.png",
            "width": 1440,
            "height": 900,
            "bytes": 94208
        }));
        let got =
            page_tool("browser_capture", &json!({}), &p, Some(&host)).expect("a capture runs");
        assert_eq!(seen_ops(&host), vec![PageOp::Capture]);
        assert_eq!(got.summary.key, Some(SummaryKey::BrowserCaptured));
        // The size is the row's whole subject: a run comparing two captures reads those two numbers.
        assert_eq!(got.summary.params, json!({ "size": "1440×900" }));
        assert!(got.content.contains("1440×900"), "{}", got.content);
        assert!(got.content.contains("page-1.png"), "{}", got.content);
        assert!(got.content.contains("92 KB"), "{}", got.content);
        // The model is told where the file is and that nothing it can call will change it, so it does
        // not spend a round trying to read or edit a PNG with the workspace tools.
        assert!(
            got.content.contains("rather than in the project folder"),
            "{}",
            got.content
        );
    }

    #[test]
    fn a_capture_that_came_back_without_a_picture_is_a_refusal_not_an_empty_page() {
        let (_d, p) = page_fixture();
        for answer in [
            json!({}),
            json!({ "path": "/tmp/page.png" }),
            json!({ "path": "/tmp/page.png", "width": 0, "height": 900 }),
        ] {
            let host = FakePage::saying(answer.clone());
            let why = page_tool("browser_capture", &json!({}), &p, Some(&host))
                .expect_err("no picture means no capture");
            assert!(matches!(why, ToolsError::Page(_)), "{answer}");
            assert!(why.to_string().contains("without a picture"), "{why}");
        }
    }

    #[test]
    fn a_check_that_holds_names_the_assertion_and_the_file_it_measured() {
        let (_d, p) = page_fixture();
        let host = FakePage::saying(json!({
            "passed": true,
            "detail": "1024 sampled points hold 7 different colours, so the page drew something.",
            "path": "/Users/someone/Library/Application Support/freecode/captures/page-2.png",
            "width": 900,
            "height": 600,
            "bytes": 43008
        }));
        let got = page_tool(
            "browser_check",
            &json!({ "expect": "not-blank" }),
            &p,
            Some(&host),
        )
        .expect("a check runs");
        assert_eq!(seen_ops(&host), vec![PageOp::Check]);
        // The host is handed the assertion as the executor understood it, not the raw wording.
        assert_eq!(host.seen()[0].1, json!({ "expect": "not-blank" }));
        assert_eq!(got.summary.key, Some(SummaryKey::BrowserChecked));
        assert_eq!(
            got.summary.params,
            json!({ "check": "not-blank", "size": "900×600" })
        );
        assert!(!got.summary.failed());
        assert!(got.content.contains("passed"), "{}", got.content);
        assert!(
            got.content.contains("7 different colours"),
            "{}",
            got.content
        );
        assert!(got.content.contains("42 KB"), "{}", got.content);
        assert!(got.content.contains("page-2.png"), "{}", got.content);
    }

    #[test]
    fn a_colour_check_names_the_point_it_measured() {
        let (_d, p) = page_fixture();
        let host = FakePage::saying(json!({
            "passed": true,
            "detail": "The pixel at 50% × 50% (450, 300) is #2f6b3f, within 8 of #2f6b3f.",
            "path": "/tmp/page-3.png",
            "width": 900,
            "height": 600,
            "bytes": 43008
        }));
        let got = page_tool(
            "browser_check",
            &json!({
                "expect": "colour",
                "x": 0.5,
                "y": 0.5,
                "rgb": "#2f6b3f",
                "tolerance": 8
            }),
            &p,
            Some(&host),
        )
        .expect("a colour check runs");
        assert_eq!(
            host.seen()[0].1,
            json!({ "expect": "colour", "x": 0.5, "y": 0.5, "rgb": "#2f6b3f", "tolerance": 8 })
        );
        // The row names the exact point, so two checks of one page read as different steps.
        assert_eq!(
            got.summary.params,
            json!({ "check": "#2f6b3f at 50% × 50%", "size": "900×600" })
        );
        assert!(got.content.contains("(450, 300)"), "{}", got.content);
    }

    #[test]
    fn a_check_that_does_not_hold_is_a_failed_row_that_quotes_the_pixel() {
        let (_d, p) = page_fixture();
        let host = FakePage::saying(json!({
            "passed": false,
            "detail": "The pixel at 50% × 50% (450, 300) is #ffffff, not #2f6b3f: it is off by 47 \
                       in green, more than the 8 allowed.",
            "path": "/tmp/page-4.png",
            "width": 900,
            "height": 600,
            "bytes": 43008
        }));
        let got = page_tool(
            "browser_check",
            &json!({
                "expect": "colour",
                "x": 0.5,
                "y": 0.5,
                "rgb": "#2f6b3f",
                "tolerance": 8
            }),
            &p,
            Some(&host),
        )
        .expect("a failed check is an answer, not an error");
        assert_eq!(got.summary.key, Some(SummaryKey::BrowserCheckFailed));
        assert!(
            got.summary.failed(),
            "a check that did not hold is not done"
        );
        assert!(got.content.contains("did not pass"), "{}", got.content);
        assert!(got.content.contains("#ffffff"), "{}", got.content);
        assert!(
            got.content.contains("run browser_check again"),
            "{}",
            got.content
        );
    }

    #[test]
    fn a_malformed_check_is_refused_before_the_panel_takes_a_picture() {
        let (_d, p) = page_fixture();
        let host = FakePage::saying(json!({}));
        for (arguments, says) in [
            (json!({ "expect": "bright" }), "expect is"),
            (
                json!({ "expect": "colour", "x": 1.5, "y": 0.5, "rgb": "#ffffff" }),
                "between 0 and 1",
            ),
            (
                json!({ "expect": "colour", "x": 0.5, "y": 0.5, "rgb": "green" }),
                "#rrggbb",
            ),
            (
                json!({ "expect": "colour", "x": 0.5, "y": 0.5 }),
                "missing field",
            ),
        ] {
            let why = page_tool("browser_check", &arguments, &p, Some(&host))
                .expect_err("a malformed check is not a check");
            assert!(matches!(why, ToolsError::InvalidArguments { .. }), "{why}");
            assert!(why.to_string().contains(says), "{why}");
        }
        assert!(host.seen().is_empty(), "nothing reached the panel");
    }

    #[test]
    fn a_check_that_came_back_without_a_picture_is_a_refusal_not_a_pass() {
        let (_d, p) = page_fixture();
        for answer in [
            json!({ "passed": true }),
            json!({ "passed": true, "path": "/tmp/page.png", "width": 0, "height": 600 }),
            json!({ "path": "/tmp/page.png", "width": 900, "height": 600 }),
        ] {
            let host = FakePage::saying(answer.clone());
            let why = page_tool(
                "browser_check",
                &json!({ "expect": "not-blank" }),
                &p,
                Some(&host),
            )
            .expect_err("no picture means no verdict");
            assert!(matches!(why, ToolsError::Page(_)), "{answer}");
        }
    }

    /// The operations one call asked the fake panel for, in order.
    fn seen_ops(host: &FakePage) -> Vec<PageOp> {
        host.seen().into_iter().map(|(op, _, _)| op).collect()
    }
}
