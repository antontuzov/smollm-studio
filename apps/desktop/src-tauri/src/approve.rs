//! The window's half of an approval: a call the policy will not settle on its own waits here for the
//! answer to its prompt.
//!
//! `freecode_core::approvals` decides everything it can from the settings a run started under, and
//! hands back `Pending::Prompt` for the rest. This module is what a prompt costs the host: the ask goes
//! out on the run's own event channel, the call waits on its answer, and the answer arrives as
//! [`resolve_approval`] — the press of a button.
//!
//! There are two ways to wait because there are two kinds of thread that ask. A tool call runs on the
//! blocking pool, so it parks on its own receiver. A write on the free route is answered from inside a
//! stream that lives on the event loop, and a loop thread holding a receiver is a loop with nobody left
//! to deliver the command that answers it — so that one hands the same wait to the blocking pool and
//! awaits the result.
//!
//! A wait always ends. Its time running out is `TimedOut`, which the loop reads as a denial and never as
//! consent; a window that could not be reached at all comes back as an error naming the reason, because
//! a run that could not ask is a different fact from a run that asked and was refused, and the sentence
//! the model reads has to say which.

use std::collections::BTreeMap;
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use freecode_core::approvals::{ApprovalDecision, ApprovalRequest};
use freecode_core::AgentEvent;
use tauri::{AppHandle, Emitter, Manager, State};

/// The prompts waiting for an answer, keyed by the call id the engine minted them for.
///
/// The id is a string rather than the next number because a run can have several calls parked at once —
/// a round's calls go out together — and one card per call is only true if an answer can name which one
/// it is about.
#[derive(Default)]
pub struct ApprovalBridge {
    waiters: Mutex<BTreeMap<String, Sender<ApprovalDecision>>>,
}

type Waiters = BTreeMap<String, Sender<ApprovalDecision>>;

impl ApprovalBridge {
    fn waiters(&self) -> Result<MutexGuard<'_, Waiters>, String> {
        self.waiters
            .lock()
            .map_err(|_| "an approval is held by a thread that panicked".to_string())
    }
}

/// The window's answer to one prompt: the press, in the decision vocabulary the engine already has.
///
/// An id nothing is waiting on is not an error. Its wait ended a moment ago, its row said what happened,
/// and the card cannot always know which of the two it is closing — so a press that arrives late is a
/// press nobody should be told about.
#[tauri::command]
pub fn resolve_approval(
    bridge: State<'_, ApprovalBridge>,
    call_id: String,
    approved: bool,
) -> Result<(), String> {
    answer(&bridge, &call_id, approved)
}

/// The press, taken on the bridge rather than on the runtime's handle to it — which is what lets the
/// pairing of an answer with its wait be checked without a window.
fn answer(bridge: &ApprovalBridge, call_id: &str, approved: bool) -> Result<(), String> {
    let decision = if approved {
        ApprovalDecision::UserApproved
    } else {
        ApprovalDecision::UserDenied
    };
    if let Some(sender) = bridge.waiters()?.remove(call_id) {
        let _ = sender.send(decision);
    }
    Ok(())
}

/// Ask the window and wait for its answer, on a thread that is allowed to block for it.
///
/// The waiter is filed before the ask goes out, so an answer that somehow beats the render still lands;
/// and every path out of here takes the entry with it, so a run that waited on a hundred prompts does not
/// leave a hundred dead senders behind.
pub fn ask(app: &AppHandle, request: &ApprovalRequest) -> Result<ApprovalDecision, String> {
    let bridge = app.state::<ApprovalBridge>();
    let (sender, receiver) = std::sync::mpsc::channel();
    bridge.waiters()?.insert(request.call_id.clone(), sender);
    let sent = app.emit(
        crate::chat::EVENT,
        AgentEvent::ApprovalRequested {
            call_id: request.call_id.clone(),
            tool_id: request.tool_id.clone(),
            headline: request.headline.clone(),
            permission: request.permission,
            timeout_ms: request.timeout_ms,
            command: request.command.clone(),
        },
    );
    if let Err(why) = sent {
        take(&bridge, &request.call_id);
        return Err(format!("the window could not be asked: {why}"));
    }
    let decision = match receiver.recv_timeout(Duration::from_millis(request.timeout_ms)) {
        Ok(decision) => decision,
        // Still reported as a denial, and never as consent: nobody agreed to this call.
        Err(RecvTimeoutError::Timeout) => ApprovalDecision::TimedOut,
        Err(RecvTimeoutError::Disconnected) => {
            take(&bridge, &request.call_id);
            return Err("the prompt's answer channel closed before it was answered".into());
        }
    };
    // A prompt that ran out of time is still on the window's screen until this says otherwise, and this
    // is the only place that knows the wait ended on its own rather than being answered.
    take(&bridge, &request.call_id);
    emit_resolved(app, &request.call_id, decision);
    Ok(decision)
}

/// The same wait, handed to the blocking pool and awaited: for a prompt asked from inside a stream.
pub async fn ask_await(
    app: &AppHandle,
    request: &ApprovalRequest,
) -> Result<ApprovalDecision, String> {
    let (host, request) = (app.clone(), request.clone());
    tauri::async_runtime::spawn_blocking(move || ask(&host, &request))
        .await
        .map_err(|why| format!("the wait for the prompt stopped: {why}"))?
}

/// Answer every prompt still waiting as a call that will not run, and say so to the window.
///
/// Stop was pressed. The thread parked on a prompt cannot notice that until its wait ends, and the wait
/// is long by design — so stopping a run settles its open questions rather than leaving the window
/// holding a card for a run that is over. Nothing the parked call would have done is done; the row it
/// closes says only that it was not approved.
pub fn cancel_pending(app: &AppHandle) {
    let bridge = app.state::<ApprovalBridge>();
    let Ok(ids) = settle_all(&bridge) else {
        return;
    };
    for id in ids {
        emit_resolved(app, &id, ApprovalDecision::UserDenied);
    }
}

/// Answer every open prompt as a call that will not run, and take it off the list. The ids come back so
/// the window's cards can be closed one by one, after the lock is let go.
fn settle_all(bridge: &ApprovalBridge) -> Result<Vec<String>, String> {
    let mut waiters = bridge.waiters()?;
    let ids: Vec<String> = waiters.keys().cloned().collect();
    for id in &ids {
        if let Some(sender) = waiters.remove(id) {
            let _ = sender.send(ApprovalDecision::UserDenied);
        }
    }
    Ok(ids)
}

/// Takes a prompt off the list: the wait is over, whether or not anything answered it.
fn take(bridge: &ApprovalBridge, call_id: &str) {
    let _ = bridge.waiters().map(|mut waiters| {
        waiters.remove(call_id);
    });
}

/// The window's card is closed by the host, in the same words whatever settled it: an answer, a
/// timeout, or a stop.
fn emit_resolved(app: &AppHandle, call_id: &str, decision: ApprovalDecision) {
    let _ = app.emit(
        crate::chat::EVENT,
        AgentEvent::ApprovalResolved {
            call_id: call_id.to_string(),
            decision,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use freecode_config::agent::PermissionClass;

    fn bridge() -> ApprovalBridge {
        ApprovalBridge::default()
    }

    /// One waiting prompt, as the real wait files it: the sender in the list, the receiver held by the
    /// call that is parked.
    fn waiting(bridge: &ApprovalBridge, id: &str) -> std::sync::mpsc::Receiver<ApprovalDecision> {
        let (sender, receiver) = std::sync::mpsc::channel();
        bridge
            .waiters
            .lock()
            .unwrap()
            .insert(id.to_string(), sender);
        receiver
    }

    #[test]
    fn an_answer_lands_on_the_wait_that_asked_for_it_and_on_no_other() {
        let bridge = bridge();
        let first = waiting(&bridge, "a");
        let second = waiting(&bridge, "b");
        // An id nobody waits on is the second half of the same race: the window answered a prompt whose
        // wait had already ended.
        answer(&bridge, "gone", true).unwrap();
        answer(&bridge, "a", true).unwrap();
        assert_eq!(first.recv().unwrap(), ApprovalDecision::UserApproved);
        assert!(
            second.try_recv().is_err(),
            "a press for one call does not clear another's"
        );
        assert_eq!(
            bridge.waiters.lock().unwrap().len(),
            1,
            "answering a prompt takes it off the list, and nothing else moves"
        );
    }

    #[test]
    fn a_press_that_arrives_after_the_wait_is_not_an_error_anybody_has_to_explain() {
        let bridge = bridge();
        let _ = waiting(&bridge, "a");
        answer(&bridge, "a", false).unwrap();
        assert!(bridge.waiters.lock().unwrap().is_empty());
        // The same press again has nothing to answer, and still says nothing about it.
        answer(&bridge, "a", false).unwrap();
    }

    #[test]
    fn a_denial_answers_as_a_denial_and_not_as_the_absence_of_an_answer() {
        let bridge = bridge();
        let receiver = waiting(&bridge, "a");
        answer(&bridge, "a", false).unwrap();
        let decision = receiver.recv().unwrap();
        assert_eq!(decision, ApprovalDecision::UserDenied);
        assert!(!decision.is_allowed());
        assert!(
            !matches!(decision, ApprovalDecision::TimedOut),
            "a call the user refused and one nobody answered are two different facts"
        );
    }

    #[test]
    fn stopping_a_run_settles_every_prompt_it_left_open() {
        let bridge = bridge();
        let first = waiting(&bridge, "a");
        let second = waiting(&bridge, "b");
        assert_eq!(
            settle_all(&bridge).unwrap(),
            vec!["a".to_string(), "b".to_string()],
            "both waits are named, so both cards can be closed"
        );
        for receiver in [first, second] {
            assert_eq!(
                receiver.recv().unwrap(),
                ApprovalDecision::UserDenied,
                "a stopped run answers its own open questions rather than leaving them parked"
            );
        }
        assert!(bridge.waiters.lock().unwrap().is_empty());
        assert!(
            settle_all(&bridge).unwrap().is_empty(),
            "nothing is left to settle"
        );
    }

    /// The contract the window reads this through: the field names are the ones `lib/ipc.ts` declares,
    /// so a rename on one side is a card that shows nothing on the other.
    #[test]
    fn a_prompt_is_sent_in_the_shape_the_window_names_in_its_answer() {
        let json = serde_json::to_value(AgentEvent::ApprovalRequested {
            call_id: "call-7".to_string(),
            tool_id: "terminal".to_string(),
            headline: "Run `cargo test`".to_string(),
            permission: PermissionClass::SideEffect,
            timeout_ms: 120_000,
            command: Some("cargo test".to_string()),
        })
        .unwrap();
        assert_eq!(json["type"], "approvalRequested");
        assert_eq!(json["callId"], "call-7");
        assert_eq!(json["toolId"], "terminal");
        assert_eq!(json["headline"], "Run `cargo test`");
        assert_eq!(json["permission"], "side_effect");
        assert_eq!(json["timeoutMs"], 120_000);
        assert_eq!(json["command"], "cargo test");
    }

    /// A prompt the policy settled on its own never reaches this module — but a run whose dial says
    /// "ask" has to reach it, which is the half of the engine the prompt exists for.
    #[test]
    fn only_a_call_the_policy_cannot_settle_becomes_a_prompt() {
        let dir = tempfile::tempdir().unwrap();
        // The shell's switch is on in both cases: switched off, the engine refuses the call before the
        // dial gets to speak, and there would be no prompt to test either way.
        let policy = freecode_tools::SandboxPolicy {
            workspace_root: dir.path().to_path_buf(),
            shell_allowed: true,
            ..Default::default()
        };
        let tool = freecode_tools::find("terminal").unwrap();
        let args = serde_json::json!({ "command": "cargo test" });
        // Auto, with its switch on: the switch is the consent the dial reads, so the policy answers and
        // there is nothing for the window to say. `shell_enabled` is the same switch on the dial's side;
        // with it off, Auto has nothing to settle a command with and a side effect becomes a prompt.
        let auto = freecode_config::agent::Approvals {
            shell_enabled: true,
            ..Default::default()
        };
        assert!(!matches!(
            freecode_core::approvals::evaluate(&tool, &args, &auto, &policy, "c"),
            freecode_core::approvals::Pending::Prompt(_)
        ));
        // Ask: the same call is the window's to answer.
        let asking = freecode_config::agent::Approvals {
            mode: freecode_config::agent::ApprovalMode::AskAlways,
            shell_enabled: true,
            ..Default::default()
        };
        assert!(matches!(
            freecode_core::approvals::evaluate(&tool, &args, &asking, &policy, "c"),
            freecode_core::approvals::Pending::Prompt(_)
        ));
    }
}
