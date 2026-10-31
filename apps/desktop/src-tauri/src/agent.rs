//! The tool round: run what one answer asked for, and say in the transcript what each call did.
//!
//! This is the half of the agent loop that touches the machine. [`freecode_tools::EXECUTABLE`] names the
//! calls that have executors — four reads of the workspace, three reads of its repository, three writes
//! inside it, two that reach the internet, one command, and the three project runners built on the same
//! consent; anything else the model asks for is answered with a notice that says so rather than run,
//! which is why the loop can keep the "Local" badge true. The web calls, the shell and its runners are
//! only *offered* while their own switches are on, so a run that can leave the machine, or run with
//! this user's permissions, is one the user asked for; the executors themselves are what bound them,
//! the way the sandbox bounds the six that touch disk.
//!
//! Every call takes the same road: [`freecode_core::approvals`] decides, [`freecode_tools::authorize`]
//! resolves its paths against the workspace sandbox inside [`freecode_tools::execute`], and only then
//! does anything touch the disk. The row opens before the call and closes after it, so a call that
//! walks a large tree shows up as work in progress rather than as a window that stopped answering.
//!
//! A write also leaves a number behind: the lines it cost go on the row, and into the run's ledger on
//! [`ChatState`], which is what the "+N −M" pill above the composer reads. A create counts every line it
//! wrote as added, since nothing stood there to remove. The ledger is per file, so a run that edits one
//! file four times says "1 file, +9 −3" rather than "4 files".
//!
//! A refusal is said twice, on purpose: the row carries it for the user, and the tool result carries
//! it for the model, which is the only reader that can act on it. A model handed an empty result
//! instead of "that path is protected" tries the same call again, and the run spends a round to learn
//! nothing.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

use freecode_config::Settings;
use freecode_core::approvals::{evaluate, ApprovalDecision, ApprovalRequest, Pending};
use freecode_core::events::{AgentEvent, NoticeCode, ToolOutcome};
use freecode_core::transcript::{
    tool_batch, DiffStat, EditedFile, Hunk, StepKind, StepStatus, TranscriptStep,
};
use freecode_llama::ToolCall;
use freecode_tools::{
    execute_watched, is_delegated, is_executable, is_runnable, patch_target, Edit, SandboxPolicy,
};

/// One call's answer to the model, with the id the replay has to name.
#[derive(Debug, Clone)]
pub struct Ran {
    pub call_id: String,
    /// The tool result text. A refusal is in here too, in words the model can act on.
    pub result: String,
}

/// The batch header, emitted once a round's calls have all closed. The count comes from the rows the
/// round opened, so a batch never claims a tool it did not run — and a round whose every call was for
/// something this build cannot do opens no rows and so claims no header.
pub fn close_batch(app: &AppHandle, calls: &[ToolCall]) {
    let rows = rows_for(calls);
    if rows == 0 {
        return;
    }
    emit(
        app,
        AgentEvent::ToolBatchClosed {
            step: tool_batch(rows),
        },
    );
}

/// How many of these calls open a row: exactly the ones [`run_one`] or the loop's delegation path
/// will answer, which is the same test applied before emitting anything. A call for a tool with no
/// runner is answered with a notice and never becomes a row, so counting it here would put a number
/// in the header that the batch below it cannot show rows for. A plan is the exception for the same
/// reason in the other direction: the window keeps `todo_write`'s row in the column rather than
/// folding it under the header, so counting it would name a child the batch does not hold.
fn rows_for(calls: &[ToolCall]) -> usize {
    calls
        .iter()
        .filter(|call| is_runnable(&call.name) && call.name != "todo_write")
        .count()
}

/// The sandbox every tool call in this build goes through: the project folder chosen in Settings — or
/// the root the host resolved when none was — with no reads outside it, the network gated on the one
/// privacy setting that could make the "Local" badge false, and the shell gated on its own switch. The
/// shell is the one call the path sandbox cannot bound — a command runs with this user's permissions —
/// so the switch, off by default, is the consent rather than a path rule that would only be a promise.
pub fn policy_for(app: &AppHandle, settings: &Settings) -> Result<SandboxPolicy, String> {
    Ok(SandboxPolicy {
        workspace_root: crate::paths::project_root(Some(app), settings)?,
        // A model that names `/etc/hosts` gets the sandbox's sentence about it, which is a better
        // answer than the file. The workspace is what the task is about.
        read_outside_workspace: false,
        network_allowed: settings.privacy.allow_remote_providers,
        shell_allowed: settings.approvals.shell_enabled,
        // The page already bounds the field at 5–600 s; a document edited by hand is bounded again by
        // the executor, which clamps to 1–600. A zero here would otherwise mean "kill immediately".
        shell_timeout_secs: (settings.approvals.shell_timeout_ms / 1000).max(1),
        extra_denied: Vec::new(),
    })
}

/// Run one call, emitting the rows as it opens and closes.
pub(crate) fn run_one(
    app: &AppHandle,
    call: &ToolCall,
    settings: &Settings,
    policy: &SandboxPolicy,
) -> Ran {
    // A call whose arguments never parsed is answered as an empty object: the executor's own
    // "path is required" names the field to fix, which a JSON error does not.
    let arguments: Value = serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
    let name = call.name.as_str();

    // Delegation is the loop's own call, answered in `chat::run_rounds`; a call that reaches this
    // function is outside that road and must not run a second model request here.
    if is_delegated(name) {
        emit(
            app,
            AgentEvent::notice(NoticeCode::ToolNotOffered, &[("name", name)]),
        );
        return Ran {
            call_id: call.id.clone(),
            result: format!(
                "{name} is answered by the run loop, not by a tool executor. Ask for it from a Coding \
                 run with Subagents on."
            ),
        };
    }

    // Not a call this build has. The registry lists nothing without a runner behind it, so reaching
    // here means the model named a capability on its own — and no row opens either way, because a row
    // would promise work that never happens.
    if !is_executable(name) {
        emit(
            app,
            AgentEvent::notice(NoticeCode::ToolNotOffered, &[("name", name)]),
        );
        return Ran {
            call_id: call.id.clone(),
            result: format!(
                "{name} is not a call this build has. The calls it can make \
                  are read_file, list_dir, glob, grep_search, write_file, edit_file and apply_patch, \
                 which reach nothing outside this workspace; git_status, git_diff and git_log, which \
                 only read the repository; git_stage and git_branch, which change the checkout; \
                 git_commit, which rewrites history and always asks; git_push and github_pr, which \
                 leave the machine and need the Privacy switch on as well as an approval; \
                 web_search and fetch_url, which reach the internet only \
                 while the Privacy switch is on; terminal, run_tests, run_build and run_linter, which \
                 run only while Terminal access is on; the browser calls, which act on the page open \
                 in the preview panel; and spawn_subagent, which delegates only while Subagents is \
                 on."
            ),
        };
    }

    let mut row = started_row(name, &arguments);
    emit(app, AgentEvent::ToolStarted { step: row.clone() });

    let staged = match cleared(app, name, &arguments, &call.id, settings, policy) {
        Ok(()) => {
            // Before the executor touches disk: these are the texts a revert puts back, and reading
            // them here is what lets the pair of the old and the new state be recorded together.
            let captured = crate::undo::capture(name, &arguments, policy);
            let watcher = watches(name).then(|| watcher_for(app, &row.id));
            // The page calls reach the window's preview panel through the bridge in `preview`: a host
            // that is built per call so it borrows this run's own app handle.
            let page = crate::preview::Page::new(app);
            match execute_watched(
                name,
                &arguments,
                policy,
                watcher.as_ref(),
                Some(&page),
                Some(&crate::github::Token),
            ) {
                Ok(done) => {
                    // The executor's own report of what it wrote. Its paths are workspace-relative by
                    // construction, which is what makes two calls that named one file — one by its
                    // absolute path, one relatively — land on the same ledger row.
                    app.state::<crate::chat::ChatState>()
                        .remember(crate::undo::entries(
                            &captured,
                            &done.edits,
                            &policy.workspace_root,
                        ));
                    let written = written(&done.edits);
                    let picture = done.summary.picture.clone();
                    (
                        ToolOutcome {
                            diff: written.as_ref().map(|written| written.diff),
                            ..ToolOutcome::ok(done.summary)
                        },
                        done.content,
                        written,
                        picture,
                    )
                }
                Err(why) => {
                    let reason = why.to_string();
                    (
                        ToolOutcome::blocked(&reason),
                        format!("{name} did not run: {reason}"),
                        None,
                        None,
                    )
                }
            }
        }
        Err(reason) => (
            ToolOutcome::blocked(&reason),
            format!("{name} did not run: {reason}"),
            None,
            None,
        ),
    };
    let (outcome, result, written, picture) = staged;
    // A call that drew a picture leaves the frame on its row, small enough to hang in the run report
    // beside the check that judged it. The file stays where the host keeps it; the row holds an image
    // a reopened conversation can still draw, not a path into this user's home.
    if let Some(file) = picture {
        row.frame = frame(&file);
    }
    row.finish(if outcome.ok {
        StepStatus::Done
    } else {
        StepStatus::Blocked
    });
    let mut total = None;
    if let Some(written) = written {
        row.diff = Some(written.diff);
        row.detail = written.detail;
        row.hunk = written.hunk;
        for file in &written.files {
            total = Some(app.state::<crate::chat::ChatState>().stage(
                &file.path,
                file.added,
                file.removed,
            ));
        }
        // The row lists its files only when one hunk cannot describe the call. A single block keeps
        // the shape every filed row already has; two files or two places get the breakdown beside it.
        if row.hunk.is_none() {
            row.files = written.files;
        }
    }
    emit(app, AgentEvent::ToolFinished { step: row, outcome });
    // The pill moves while the run is still going rather than arriving only once it ends: an agent
    // that has changed six files is a different thing to watch than one that has changed none.
    if let Some(diff) = total {
        emit(app, AgentEvent::DiffStaged { diff });
    }
    Ran {
        call_id: call.id.clone(),
        result,
    }
}

/// What a call wrote, as the row and the ledger need it: the call's totals, the chip when every
/// change landed in one file, and the per-file list every write is staged from.
pub(crate) struct Written {
    pub(crate) diff: DiffStat,
    pub(crate) detail: String,
    pub(crate) hunk: Option<Hunk>,
    pub(crate) files: Vec<EditedFile>,
}

/// `None` for a call that wrote nothing. A single file with a single hunk keeps the one-`hunk` form
/// the row has always had; a file changed in two places, or a patch that named two files, is listed
/// per file so the review can number each block against the file it belongs to.
pub(crate) fn written(edits: &[Edit]) -> Option<Written> {
    let first = edits.first()?;
    let diff = DiffStat {
        lines_added: edits.iter().map(|edit| edit.added).sum(),
        lines_removed: edits.iter().map(|edit| edit.removed).sum(),
        files_changed: edits.len() as u32,
    };
    let files: Vec<EditedFile> = edits
        .iter()
        .map(|edit| EditedFile {
            path: edit.path.clone(),
            added: edit.added,
            removed: edit.removed,
            hunks: edit
                .hunks
                .iter()
                .map(|hunk| Hunk {
                    at: hunk.at,
                    lines: hunk.lines.clone(),
                })
                .collect(),
        })
        .collect();
    let single = (edits.len() == 1 && first.hunks.len() == 1).then(|| Hunk {
        at: first.hunks[0].at,
        lines: first.hunks[0].lines.clone(),
    });
    Some(Written {
        diff,
        // A patch that named several files has no one path to show; the summary and the list say
        // what it changed, where a chip naming whichever file came first would say only part of it.
        detail: if edits.len() == 1 {
            first.path.clone()
        } else {
            String::new()
        },
        hunk: single,
        files,
    })
}

/// After a run that changed files, measure the change with the project's own check — the tests when
/// the manifest names them, otherwise the build, otherwise the lint — unless the model already ran
/// one itself. The row opens and closes like any other call, so the wait and its result appear in the
/// transcript the user is already reading instead of behind a claim that the change works.
///
/// The consent is Terminal access, the same switch the model's own runners answer to: a check is a
/// command run with this user's permissions. With it off the run says its change is unverified rather
/// than quietly skipping the measurement. The free-model route denies its model the shell, so this is
/// the only place a run on that route can be checked at all.
///
/// Runs on the blocking pool: a test suite takes minutes, and the window cannot wait on it in the
/// event loop.
pub(crate) async fn verify_run(
    app: &AppHandle,
    settings: &Settings,
    policy: &SandboxPolicy,
    already_checked: bool,
) {
    if already_checked {
        return;
    }
    // Read after the run's last row closed: a run that only read files has nothing to measure, and a
    // check on a folder it never touched would be a command the user did not ask for.
    if app
        .state::<crate::chat::ChatState>()
        .staged_diff()
        .is_none()
    {
        return;
    }
    if !settings.approvals.shell_enabled {
        emit(app, AgentEvent::notice(NoticeCode::ChecksNotRun, &[]));
        return;
    }
    let Some(tool) = freecode_tools::declared_checks(&policy.workspace_root)
        .into_iter()
        .next()
    else {
        emit(app, AgentEvent::notice(NoticeCode::ChecksUnavailable, &[]));
        return;
    };
    let (host, policy) = (app.clone(), policy.clone());
    // A join error means the executor panicked, which no notice can word better than the absence it
    // leaves: the run's rows, its diff and its answer still stand.
    let _ = tauri::async_runtime::spawn_blocking(move || run_check(&host, tool, &policy)).await;
}

/// Run one host-initiated check and emit its row. Failing to run at all is the same shape as a call
/// the loop could not make: a blocked row, with the executor's sentence as the reason.
fn run_check(app: &AppHandle, tool: &str, policy: &SandboxPolicy) {
    let arguments = json!({});
    let mut row = started_row(tool, &arguments);
    emit(app, AgentEvent::ToolStarted { step: row.clone() });
    // A check is a command the user waits through, so it reports into the same panel a call does.
    let watcher = watcher_for(app, &row.id);
    // A host-initiated check has no preview panel of its own: the browser calls are never checks.
    match execute_watched(tool, &arguments, policy, Some(&watcher), None, None) {
        Ok(done) => {
            let failed = done.summary.failed();
            let code = done.summary.params.get("code").and_then(Value::as_i64);
            row.finish(if failed {
                StepStatus::Blocked
            } else {
                StepStatus::Done
            });
            let mut outcome = ToolOutcome::ok(done.summary);
            outcome.ok = !failed;
            outcome.exit_code = code.map(|code| code as i32);
            emit(app, AgentEvent::ToolFinished { step: row, outcome });
            // The row says which command and how it ended; the notice says what that means for the
            // change the answer just claimed. A timeout has no exit code to quote, and its own summary
            // already carries the seconds it ran.
            if let Some(code) = code.filter(|_| failed) {
                let code = code.to_string();
                emit(
                    app,
                    AgentEvent::notice(NoticeCode::ChecksFailed, &[("code", code.as_str())]),
                );
            }
        }
        Err(why) => {
            row.finish(StepStatus::Blocked);
            emit(
                app,
                AgentEvent::ToolFinished {
                    step: row,
                    outcome: ToolOutcome::blocked(&why.to_string()),
                },
            );
        }
    }
}

/// Whether a call runs a command whose output the terminal panel can show. The four are the registry's
/// own shell runners — the free-form one and the three the project's manifest decides — and nothing
/// else in this build starts a process whose output would belong in a terminal.
fn watches(name: &str) -> bool {
    matches!(name, "terminal" | "run_tests" | "run_build" | "run_linter")
}

/// How much output one command may report before the panel stops hearing from it. A command can print
/// for its whole deadline; the panel is a window, not a log store, and the row's own result still
/// carries the tail. Nothing is said when this bound is reached, because the answer is: the panel
/// stopped watching, not the command stopped printing.
pub(crate) const TERMINAL_EVENT_BYTES: usize = 256 * 1024;

/// The sink a running command's output goes to: one `TerminalOutput` event per pipe read, against the
/// row that opened the command. The bytes are decoded across reads so a character the pipe split in
/// two is completed rather than sent as a replacement mark; a read that is still mid-character waits
/// for the next one, and a stream that never becomes text is dropped instead of held.
pub(crate) fn watcher_for(app: &AppHandle, step_id: &str) -> freecode_tools::Watcher {
    let app = app.clone();
    let step_id = step_id.to_string();
    let pending: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sent = Arc::new(AtomicUsize::new(0));
    Arc::new(move |bytes: &[u8]| {
        let text = {
            let mut buf = match pending.lock() {
                Ok(buf) => buf,
                // A panic on the other pipe's thread poisoned the lock; the reader still gets its
                // output through the call's own result, so the panel losing a chunk is not worth a
                // panic on this thread too.
                Err(_) => return,
            };
            buf.extend_from_slice(bytes);
            match std::str::from_utf8(&buf) {
                Ok(text) => {
                    let text = text.to_string();
                    buf.clear();
                    text
                }
                Err(invalid) => {
                    let valid = invalid.valid_up_to();
                    if valid == 0 {
                        // Binary output would otherwise grow this buffer for as long as the command
                        // runs. Once it is clearly not text, it is dropped: the panel shows text.
                        if buf.len() > 4096 {
                            buf.clear();
                        }
                        return;
                    }
                    let text = String::from_utf8_lossy(&buf[..valid]).to_string();
                    buf.drain(..valid);
                    text
                }
            }
        };
        if text.is_empty() || sent.fetch_add(text.len(), Ordering::Relaxed) >= TERMINAL_EVENT_BYTES
        {
            return;
        }
        emit(
            &app,
            AgentEvent::TerminalOutput {
                step_id: step_id.clone(),
                chunk: text,
            },
        );
    })
}

/// Approval, then the sandbox. `Ok(())` is the only answer that lets a call run, and everything else
/// comes back as the sentence the row and the model both get.
///
/// A call the policy will not settle on its own is put to the window and the thread waits for the answer.
/// This runs on the blocking pool, which is the one place a wait like this costs nothing: the event loop
/// stays free to deliver the press that ends it.
pub(crate) fn cleared(
    app: &AppHandle,
    name: &str,
    arguments: &Value,
    call_id: &str,
    settings: &Settings,
    policy: &SandboxPolicy,
) -> Result<(), String> {
    let Some(request) = gated(name, arguments, call_id, settings, policy)? else {
        return Ok(());
    };
    approved(&request, crate::approve::ask(app, &request))
}

/// The same check for a caller that is on the event loop rather than on the blocking pool, where the
/// wait has to be handed off rather than held.
pub(crate) async fn cleared_on_loop(
    app: &AppHandle,
    name: &str,
    arguments: &Value,
    call_id: &str,
    settings: &Settings,
    policy: &SandboxPolicy,
) -> Result<(), String> {
    let Some(request) = gated(name, arguments, call_id, settings, policy)? else {
        return Ok(());
    };
    approved(&request, crate::approve::ask_await(app, &request).await)
}

/// What the policy makes of a call: nothing to answer, the sentence it is refused in, or the prompt the
/// window has to answer before anything runs.
///
/// The free route reads this too, so a call the service asks about is decided by the same dial, the
/// same switches and the same allow/deny lists as one the loop runs itself — and a prompt that means the
/// same thing on both routes says the same thing in the window.
pub(crate) fn gated(
    name: &str,
    arguments: &Value,
    call_id: &str,
    settings: &Settings,
    policy: &SandboxPolicy,
) -> Result<Option<ApprovalRequest>, String> {
    let tool = freecode_tools::find(name).ok_or_else(|| format!("no tool named {name}"))?;
    match evaluate(&tool, arguments, &settings.approvals, policy, call_id) {
        Pending::Decided(decision) if decision.is_allowed() => Ok(None),
        Pending::Decided(ApprovalDecision::UserDenied)
        | Pending::Decided(ApprovalDecision::TimedOut) => {
            Err("the call was not approved".to_string())
        }
        Pending::Decided(_) => Err(format!(
            "{} is refused by the approval policy in Settings",
            tool.title
        )),
        Pending::Prompt(request) => Ok(Some(request)),
    }
}

/// The window's answer, as the loop's: run, or the words the row and the model both get.
///
/// A prompt that never reached a window is told as that rather than as a denial. "You did not approve
/// this" about a question that was never asked would put the user's refusal in the model's mouth and in
/// the transcript, and it is the one kind of sentence this app has to be exact about.
///
/// The free route folds the same sentence into the verdict it sends back to the service, so a call the
/// user refused reads alike however it was offered.
pub(crate) fn approved(
    request: &ApprovalRequest,
    answer: Result<ApprovalDecision, String>,
) -> Result<(), String> {
    match answer {
        Ok(decision) if decision.is_allowed() => Ok(()),
        Ok(ApprovalDecision::TimedOut) => Err(format!(
            "{} waited {} s for an approval that never came, so it did not run. Answer the approval \
             prompt in the window while the run is waiting, or set the approval control to Auto.",
            request.headline,
            (request.timeout_ms / 1000).max(1)
        )),
        Ok(_) => Err(format!(
            "{} was not approved, so nothing it would have changed happened.",
            request.headline
        )),
        Err(why) => Err(format!("{} could not be put to you: {why}", request.headline)),
    }
}

/// How many characters of a delegated question the transcript shows. A task can be a paragraph; the
/// row is a line, and the full text the model wrote is in the run's own history rather than in a chip.
const TASK_CHARS: usize = 80;

fn task_subject(task: &str) -> String {
    let line = task.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= TASK_CHARS {
        return line.to_string();
    }
    let mut clipped: String = line.chars().take(TASK_CHARS).collect();
    clipped.push('…');
    clipped
}

/// How wide a frame is drawn on the row it came from. The run report's column is a few dozen pixels
/// wide there, and a page's shape is what the picture has to say at that size.
const FRAME_WIDTH: u32 = 96;

/// The frame a call took, cut down to something a row can draw.
///
/// The file itself stays in the host's own captures folder — it is the run's evidence, and nothing in
/// the workspace may change it. What rides on the row is a copy: a `data:` URL the window draws
/// directly, so a reopened conversation shows the picture the run was judged on even though the
/// panel that took it is long gone. A capture this build cannot read leaves the row without a frame
/// rather than failing the call: the capture succeeded, and its sentence still names the file.
fn frame(file: &str) -> Option<String> {
    let png = std::fs::read(file).ok()?;
    let small = freecode_tools::pixels::thumbnail(&png, FRAME_WIDTH).ok()?;
    Some(format!("data:image/png;base64,{}", BASE64.encode(small)))
}

/// The row's label argument: the path or pattern the call worked on, shown as the chip beside
/// "Read file" rather than buried in a sentence about it.
fn subject(name: &str, arguments: &Value) -> String {
    // A patch carries no path argument; the file it names is in the diff's own `+++` header. A patch
    // that names several files has no single file to show, so the row is labelled by the tool alone
    // and the summary carries the count.
    if name == "apply_patch" {
        return arguments
            .get("patch")
            .and_then(Value::as_str)
            .and_then(patch_target)
            .unwrap_or_default();
    }
    // A delegated question is the row's subject: "Subagent" alone would say what kind of row it is
    // and nothing about what was handed over.
    if name == "spawn_subagent" {
        return task_subject(
            arguments
                .get("task")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
    }
    // A runner's label already says what ran, and its summary carries the measured line; the one
    // argument that reads as a subject is the filter or package the model narrowed to. A repository
    // status has no subject at all, and a chip repeating the tool name would only take up the row.
    match name {
        "run_tests" => {
            return arguments
                .get("filter")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        }
        "run_build" => {
            return arguments
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        }
        "run_linter" | "git_status" => return String::new(),
        "git_diff" | "git_log" => {
            return arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        }
        // A page call is named by what it acted on: the selector it reached for, the key it pressed.
        // A whole-page read and a console look have no single subject — their summaries carry the
        // counts — and a click's coordinates name nothing, so a selector is the only label.
        "browser_read" | "browser_click" | "browser_wait" => {
            return arguments
                .get("selector")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        }
        "browser_type" => {
            let selector = arguments
                .get("selector")
                .and_then(Value::as_str)
                .filter(|selector| !selector.is_empty());
            return match selector {
                Some(selector) => selector.to_string(),
                None => task_subject(
                    arguments
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
            };
        }
        "browser_press" => {
            return arguments
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        }
        "browser_console" => return String::new(),
        _ => {}
    }
    // The repository's own rows are named by what they acted on: the branch a switch moved to, the
    // message a commit carried, the title a pull request was opened with. A staging row names the
    // first path it was given, because the summary beside it carries the count.
    match name {
        "git_stage" => {
            let first = arguments
                .get("paths")
                .and_then(Value::as_array)
                .and_then(|paths| paths.first())
                .and_then(Value::as_str)
                .unwrap_or_default();
            return first.to_string();
        }
        "git_commit" => {
            return arguments
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        }
        "git_branch" => {
            return arguments
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        }
        "git_push" => {
            return arguments
                .get("branch")
                .and_then(Value::as_str)
                .filter(|branch| !branch.is_empty())
                .unwrap_or("origin")
                .to_string()
        }
        "github_pr" => {
            return arguments
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        }
        _ => {}
    }
    ["path", "pattern", "file", "query", "url", "command"]
        .iter()
        .find_map(|key| arguments.get(*key).and_then(Value::as_str))
        .unwrap_or(name)
        .to_string()
}

pub(crate) fn started_row(name: &str, arguments: &Value) -> TranscriptStep {
    let kind = match name {
        "read_file" => StepKind::ReadFile,
        "grep_search" => StepKind::SearchContent,
        "todo_write" => StepKind::Todo,
        "edit_file" => StepKind::EditFile,
        "write_file" => StepKind::CreateFile,
        "apply_patch" => StepKind::EditFile,
        "terminal" => StepKind::Terminal,
        "run_tests" => StepKind::Tests,
        "run_build" => StepKind::Build,
        "run_linter" => StepKind::Linter,
        "git_status" | "git_diff" | "git_log" => StepKind::Git,
        "git_stage" | "git_commit" | "git_branch" | "git_push" => StepKind::GitWrite,
        "github_pr" => StepKind::PullRequest,
        "web_search" => StepKind::WebSearch,
        "fetch_url" => StepKind::WebReading,
        _ if name.starts_with("browser_") => StepKind::Browser,
        "spawn_subagent" => StepKind::Subagent,
        _ => StepKind::Tool,
    };
    let shown = subject(name, arguments);
    let mut row = TranscriptStep::running_tool(name, kind, json!({ "subject": shown }));
    row.detail = shown;
    // The plan rides on the row so the window can draw it. An invalid call is left empty here and
    // fails in `execute` with the sentence that says which step is wrong.
    if name == "todo_write" {
        row.plan = freecode_tools::plan(arguments).unwrap_or_default();
        row.detail.clear();
    }
    row
}

pub(crate) fn emit(app: &AppHandle, event: AgentEvent) {
    let _ = app.emit(crate::chat::EVENT, event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_is_named_by_the_argument_it_acted_on() {
        assert_eq!(
            subject("read_file", &json!({ "path": "src/lib.rs" })),
            "src/lib.rs"
        );
        assert_eq!(subject("grep_search", &json!({ "pattern": "run" })), "run");
        // The two web calls name themselves by what they looked up, so the row shows the words the run
        // asked the internet with, and the page it opened.
        assert_eq!(
            subject("web_search", &json!({ "query": "Grace Hopper" })),
            "Grace Hopper"
        );
        assert_eq!(
            subject(
                "fetch_url",
                &json!({ "url": "https://en.wikipedia.org/wiki/Grace_Hopper" })
            ),
            "https://en.wikipedia.org/wiki/Grace_Hopper"
        );
        // A call that named neither still gets a label rather than an empty chip.
        assert_eq!(subject("list_dir", &json!({})), "list_dir");
        // A command is the row's subject too: "cargo test" reads as the thing that ran, where the tool's
        // own name would only say that something did.
        assert_eq!(
            subject("terminal", &json!({ "command": "cargo test" })),
            "cargo test"
        );
        // A patch has no path argument to read, so its subject comes from the file its own header
        // names — a row called "apply_patch" would say nothing about what was written.
        assert_eq!(
            subject(
                "apply_patch",
                &json!({
                    "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-one\n+ONE\n"
                })
            ),
            "src/lib.rs"
        );
        // A patch that names several files has no one path to show; the row is labelled by the tool
        // and the summary says how many files changed.
        assert_eq!(
            subject(
                "apply_patch",
                &json!({
                    "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-one\n+ONE\n\
                              --- a/page.html\n+++ b/page.html\n@@ -1 +1 @@\n-a\n+b\n"
                })
            ),
            ""
        );
        // A delegated question is its own subject, on one line and clipped to a chip's length.
        assert_eq!(
            subject(
                "spawn_subagent",
                &json!({ "task": "List the files\nand say which is largest" })
            ),
            "List the files"
        );
        let long = "x".repeat(TASK_CHARS + 10);
        let clipped = subject("spawn_subagent", &json!({ "task": long }));
        assert_eq!(clipped.chars().count(), TASK_CHARS + 1, "{clipped}");
        assert!(clipped.ends_with('…'), "{clipped}");
    }

    #[test]
    fn a_browser_call_is_named_by_the_element_it_reached_for() {
        // The chip says which control the call acted on; "browser_click" alone would say what kind of
        // row it is and nothing about the page. A whole-page read and a console look have no one
        // element, so their rows are labelled by the summary's count instead.
        let row = started_row("browser_click", &json!({ "selector": "#play" }));
        assert_eq!(row.kind, StepKind::Browser);
        assert_eq!(row.detail, "#play");
        assert_eq!(
            started_row("browser_wait", &json!({ "selector": "#score" })).detail,
            "#score"
        );
        assert_eq!(
            started_row("browser_press", &json!({ "key": "Enter" })).detail,
            "Enter"
        );
        // A typing call without a selector writes into the focused field, and the text is the only
        // thing that says which field that was.
        assert_eq!(
            started_row("browser_type", &json!({ "text": "Ada" })).detail,
            "Ada"
        );
        assert!(started_row("browser_read", &json!({})).detail.is_empty());
        assert!(started_row("browser_console", &json!({})).detail.is_empty());
        // Opening a page is named by the file it framed, which is the path argument like any read.
        assert_eq!(
            started_row("browser_open", &json!({ "path": "game.html" })).detail,
            "game.html"
        );
    }

    #[test]
    fn a_write_that_spanned_two_files_is_reported_per_file() {
        use freecode_tools::exec::EditHunk;
        let edits = vec![
            Edit {
                path: "a.rs".into(),
                added: 2,
                removed: 1,
                occurrences: 1,
                hunks: vec![EditHunk {
                    at: 1,
                    lines: "+x".into(),
                }],
            },
            Edit {
                path: "b.rs".into(),
                added: 3,
                removed: 0,
                occurrences: 2,
                hunks: vec![
                    EditHunk {
                        at: 4,
                        lines: "+y".into(),
                    },
                    EditHunk {
                        at: 9,
                        lines: "+z".into(),
                    },
                ],
            },
        ];
        let row = written(&edits).expect("two writes");
        assert_eq!(row.diff.files_changed, 2);
        assert_eq!((row.diff.lines_added, row.diff.lines_removed), (5, 1));
        assert_eq!(row.detail, "", "no single path can stand in a chip");
        assert!(row.hunk.is_none(), "one hunk cannot describe two files");
        assert_eq!(row.files.len(), 2);
        assert_eq!(row.files[1].path, "b.rs");
        assert_eq!(row.files[1].hunks.len(), 2);

        // One file and one block is the shape every filed edit row already has.
        let single = written(&edits[..1]).expect("one write");
        assert_eq!(single.detail, "a.rs");
        assert_eq!(single.hunk.expect("one hunk").at, 1);
        assert!(single.files.len() == 1);
        assert_eq!(single.diff.files_changed, 1);
        assert!(written(&[]).is_none());
    }

    #[test]
    fn the_offered_reads_the_writes_and_the_web_calls_open_a_row() {
        // The executors and the offer have to agree, or a call would open a row nothing closes.
        for id in freecode_tools::EXECUTABLE {
            assert!(is_executable(id), "{id} is offered but not runnable");
            let row = started_row(id, &json!({ "path": "a/b.rs" }));
            assert_eq!(row.tool_id.as_deref(), Some(*id));
            assert_eq!(row.status, StepStatus::Running);
        }
        assert_eq!(freecode_tools::EXECUTABLE.len(), 31);
        // The writes are the rows the pill is built from, so none can silently degrade to a generic
        // tool — and a create says create, because a row that reads "Edit file" over a file the run
        // invented would be the transcript contradicting the review under it.
        assert_eq!(
            started_row("edit_file", &json!({ "path": "a/b.rs" })).kind,
            StepKind::EditFile
        );
        // A patch changes a file rather than creating one, so it opens the same row an edit does.
        assert_eq!(
            started_row(
                "apply_patch",
                &json!({ "patch": "--- a/a/b.rs\n+++ b/a/b.rs\n@@ -1 +1 @@\n-x\n+y\n" })
            )
            .kind,
            StepKind::EditFile
        );
        assert_eq!(
            started_row("write_file", &json!({ "path": "a/b.rs" })).kind,
            StepKind::CreateFile
        );
        // Nor can the two web calls: a run that looked something up has to read as a lookup, not as
        // "Ran tool", which is the row shape that says nothing about what left the machine.
        assert_eq!(
            started_row("web_search", &json!({ "query": "rust" })).kind,
            StepKind::WebSearch
        );
        assert_eq!(
            started_row("fetch_url", &json!({ "url": "https://example.com" })).kind,
            StepKind::WebReading
        );
        // The browser calls act on the page in the panel, so they read as that and not as a lookup:
        // the two web calls left the machine, and these stayed inside the window.
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
            assert_eq!(started_row(id, &json!({})).kind, StepKind::Browser, "{id}");
        }
        // A command reads as a command: a row that said "Ran tool" over `rm -rf target` would hide the
        // one call whose subject is the exact words that ran.
        assert_eq!(
            started_row("terminal", &json!({ "command": "cargo test" })).kind,
            StepKind::Terminal
        );
        // The runners and the repository reads are steps of their own: a transcript that said "Ran
        // tool" over the call which decided whether the change works would hide the answer.
        for (id, kind) in [
            ("run_tests", StepKind::Tests),
            ("run_build", StepKind::Build),
            ("run_linter", StepKind::Linter),
            ("git_status", StepKind::Git),
            ("git_diff", StepKind::Git),
            ("git_log", StepKind::Git),
            // The repository's writes are not reads of it, so they cannot open the row that says
            // "Checked the repository" — a transcript that called a commit a check would be wrong in
            // the one place a reader looks for what the run did.
            ("git_stage", StepKind::GitWrite),
            ("git_commit", StepKind::GitWrite),
            ("git_branch", StepKind::GitWrite),
            ("git_push", StepKind::GitWrite),
            ("github_pr", StepKind::PullRequest),
        ] {
            assert_eq!(started_row(id, &json!({})).kind, kind, "{id}");
        }
        // Their chips carry the argument that narrowed the call, and stay empty when nothing did.
        assert_eq!(subject("run_tests", &json!({ "filter": "auth" })), "auth");
        assert_eq!(subject("run_build", &json!({ "target": "core" })), "core");
        assert_eq!(subject("git_status", &json!({})), "");
        assert_eq!(
            subject("git_diff", &json!({ "path": "src/lib.rs" })),
            "src/lib.rs"
        );
        // The repository's own rows name what they acted on, so a chip over a commit says what was
        // committed rather than repeating the tool's name.
        assert_eq!(
            subject(
                "git_stage",
                &json!({ "paths": ["src/lib.rs", "README.md"] })
            ),
            "src/lib.rs"
        );
        assert_eq!(
            subject("git_commit", &json!({ "message": "Fix the guard\n\nwhy" })),
            "Fix the guard"
        );
        assert_eq!(
            subject("git_branch", &json!({ "name": "fix/guard" })),
            "fix/guard"
        );
        assert_eq!(subject("git_push", &json!({ "branch": "main" })), "main");
        assert_eq!(subject("git_push", &json!({})), "origin");
        assert_eq!(
            subject("github_pr", &json!({ "title": "Fix the guard" })),
            "Fix the guard"
        );
        // Delegation has its own glyph in the inspector, so its row cannot be a generic tool.
        assert!(is_delegated("spawn_subagent"));
        let delegated = started_row("spawn_subagent", &json!({ "task": "Find the entry point" }));
        assert_eq!(delegated.kind, StepKind::Subagent);
        assert_eq!(delegated.detail, "Find the entry point");
        // The plan row carries the list itself, so the header can draw the steps rather than a count.
        let plan = started_row(
            "todo_write",
            &json!({ "items": [
                { "text": "Read the manifest", "status": "done" },
                { "text": "Run the tests", "status": "active" }
            ] }),
        );
        assert_eq!(plan.kind, StepKind::Todo);
        assert_eq!(plan.plan.len(), 2);
        assert_eq!(plan.plan[1].text, "Run the tests");
        assert_eq!(plan.plan[1].status, freecode_tools::PlanStatus::Active);
        assert_eq!(plan.detail, "", "the list is the row, not a chip");
    }

    #[test]
    fn a_refused_call_closes_its_row_as_blocked() {
        // The row's status is read off the outcome, so a refusal cannot leave a running row up.
        let mut row = started_row("read_file", &json!({ "path": ".env" }));
        row.finish(StepStatus::Blocked);
        assert_eq!(row.status, StepStatus::Blocked);
    }

    #[test]
    fn a_batch_counts_the_rows_it_opened() {
        let called = |names: &[&str]| {
            names
                .iter()
                .map(|name| ToolCall {
                    id: name.to_string(),
                    name: name.to_string(),
                    arguments: "{}".to_string(),
                })
                .collect::<Vec<_>>()
        };
        // A call for a name this build does not have gets a notice, not a row, so the header cannot
        // claim it.
        assert_eq!(rows_for(&called(&["read_file", "grep_search"])), 2);
        assert_eq!(rows_for(&called(&["read_file", "write_file"])), 2);
        // Delegation is run by the loop rather than an executor, and still opens the row it counts.
        assert_eq!(rows_for(&called(&["spawn_subagent"])), 1);
        assert_eq!(rows_for(&called(&["spawn_subagent", "read_file"])), 2);
        assert_eq!(rows_for(&called(&["read_file", "delete_path"])), 1);
        assert_eq!(rows_for(&called(&["delete_everything"])), 0);
    }

    #[test]
    fn a_picture_a_call_kept_becomes_a_frame_the_row_can_draw() {
        let dir = std::env::temp_dir().join(format!("freecode-frame-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder to write the capture in");
        let path = dir.join("page-1.png");
        // A page-shaped 400×300 capture: red on the left, white on the right.
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 400, 300);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("a PNG header");
            let mut data = Vec::with_capacity(400 * 300 * 4);
            for _ in 0..300 {
                for x in 0..400 {
                    data.extend_from_slice(if x < 200 {
                        &[220, 40, 40, 255]
                    } else {
                        &[255, 255, 255, 255]
                    });
                }
            }
            writer.write_image_data(&data).expect("pixels");
        }
        std::fs::write(&path, &bytes).expect("the capture on disk");

        let url =
            frame(path.to_str().expect("a UTF-8 path")).expect("a read capture becomes a frame");
        assert!(url.starts_with("data:image/png;base64,"), "{url}");
        let small = BASE64
            .decode(url.trim_start_matches("data:image/png;base64,"))
            .expect("the frame carries PNG bytes");
        let picture =
            freecode_tools::pixels::evaluate(&small, &freecode_tools::pixels::Expect::NotBlank)
                .expect("the frame is a picture this build reads");
        // The row holds the page, not the file: the frame is the capture at the row's width.
        assert_eq!((picture.width, picture.height), (FRAME_WIDTH, 72));
        assert!(picture.passed, "{}", picture.detail);

        // A capture that is not there is a row without a frame, not a failed call.
        assert!(frame("/nonexistent/freecode/page.png").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
