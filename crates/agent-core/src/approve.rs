//! The human, expressed as a trait.
//!
//! The policy decides whether a call has to be asked about; the loop decides
//! that it must stop and ask; only this module decides what the answer is. In
//! the CLI that is a prompt on the terminal, in a TUI it is a dialog, and in a
//! test it is a script. All three are the same call at the same point in the
//! loop, which is the only reason the loop can be tested at all.
//!
//! An approver never decides *whether* to be asked. `suggest-only` is settled by
//! the policy before a tool is reached, and no implementation here can talk the
//! loop into writing a file it was going to refuse.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::types::{ApprovalDecision, ApprovalRequest};

/// What a person says about one call the policy wants asked about.
///
/// Implementations must be honest about not knowing: the default for anything
/// this crate ships is to refuse, because an agent that cannot reach a person
/// and proceeds anyway is the failure mode the approval modes exist to prevent.
#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, request: &ApprovalRequest) -> ApprovalDecision;
}

/// Yes, and stop asking about the class: the run was told to be autonomous by
/// someone who typed `--yes`, so asking again only costs a step.
pub struct AutoApprove;

#[async_trait]
impl Approver for AutoApprove {
    async fn approve(&self, _request: &ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::ApprovedForRun
    }
}

/// No, with the words that go into the transcript.
pub struct RefuseAll {
    reason: String,
}

impl Default for RefuseAll {
    fn default() -> Self {
        Self::new(
            "nobody was there to answer, and this run does not write on an unanswered question",
        )
    }
}

impl RefuseAll {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl Approver for RefuseAll {
    async fn approve(&self, _request: &ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::Rejected {
            reason: self.reason.clone(),
        }
    }
}

/// A script of answers, which is what an integration test hands the loop.
///
/// It keeps every request it was shown, so a test asserts on what the loop
/// actually asked rather than on what it meant to ask — the same discipline
/// `agent_providers::MockProvider` applies to prompts. Once the script is out it
/// repeats the last answer, because a run that asks four times about four files
/// should not fail on the fifth for want of a fifth scripted line. With nothing
/// scripted it refuses everything.
pub struct Scripted {
    answers: Mutex<VecDeque<ApprovalDecision>>,
    asked: Mutex<Vec<ApprovalRequest>>,
}

impl Default for Scripted {
    fn default() -> Self {
        Self::new([])
    }
}

impl Scripted {
    pub fn new(answers: impl IntoIterator<Item = ApprovalDecision>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
            asked: Mutex::new(Vec::new()),
        }
    }

    /// The one answer to give every time, however often the loop asks.
    pub fn always(answer: ApprovalDecision) -> Self {
        Self::new([answer])
    }

    /// Approved, and for the rest of the run too: what a test that only cares
    /// about the writes wants.
    pub fn yes() -> Self {
        Self::always(ApprovalDecision::ApprovedForRun)
    }

    /// Refused, every time.
    pub fn no(reason: impl Into<String>) -> Self {
        Self::always(ApprovalDecision::Rejected {
            reason: reason.into(),
        })
    }

    /// Every request shown to this approver, oldest first.
    pub fn asks(&self) -> Vec<ApprovalRequest> {
        lock(&self.asked).clone()
    }

    /// How many times the loop stopped to ask.
    pub fn times_asked(&self) -> usize {
        lock(&self.asked).len()
    }

    fn next_answer(&self) -> ApprovalDecision {
        let mut answers = lock(&self.answers);
        if answers.len() > 1 {
            return answers.pop_front().expect("just measured");
        }
        // One left: keep it, so the last scripted answer carries the rest of the
        // run instead of running out mid-write.
        answers
            .front()
            .cloned()
            .unwrap_or(ApprovalDecision::Rejected {
                reason: "this approver was never scripted".to_owned(),
            })
    }

    fn note(&self, request: &ApprovalRequest) {
        lock(&self.asked).push(request.clone());
    }
}

#[async_trait]
impl Approver for Scripted {
    async fn approve(&self, request: &ApprovalRequest) -> ApprovalDecision {
        self.note(request);
        self.next_answer()
    }
}

/// A lock this module never holds across an await, so contention is not a
/// concern and a poison means another test panicked while holding it.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_sandbox::Permission;
    use std::path::PathBuf;

    fn request() -> ApprovalRequest {
        ApprovalRequest::new(Permission::Write, "change src/lib.rs", "@@ one hunk @@")
            .for_paths([PathBuf::from("src/lib.rs")])
    }

    #[tokio::test]
    async fn an_autonomous_run_is_asked_once_and_answered_for_the_class() {
        let decision = AutoApprove.approve(&request()).await;
        assert_eq!(decision, ApprovalDecision::ApprovedForRun);
        assert!(decision.approved());
    }

    #[tokio::test]
    async fn a_run_with_nobody_in_front_of_it_does_not_write() {
        let decision = RefuseAll::default().approve(&request()).await;
        assert!(
            !decision.approved(),
            "silence is not consent, whatever the default says"
        );
        match decision {
            ApprovalDecision::Rejected { reason } => {
                assert!(reason.contains("unanswered"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_script_answers_in_order_and_keeps_what_it_was_shown() {
        let approver = Scripted::new([
            ApprovalDecision::Approved,
            ApprovalDecision::Rejected {
                reason: "not that file".to_owned(),
            },
        ]);
        assert!(approver.approve(&request()).await.approved());
        let second = approver.approve(&request()).await;
        assert!(!second.approved());

        let asks = approver.asks();
        assert_eq!(asks.len(), 2);
        assert_eq!(asks[0].summary, "change src/lib.rs");
        assert_eq!(asks[0].paths, vec![PathBuf::from("src/lib.rs")]);
        assert_eq!(approver.times_asked(), 2);
    }

    #[tokio::test]
    async fn the_last_scripted_answer_carries_the_rest_of_the_run() {
        let approver = Scripted::new([
            ApprovalDecision::Rejected {
                reason: "ask a person".to_owned(),
            },
            ApprovalDecision::ApprovedForRun,
        ]);
        assert!(
            !approver.approve(&request()).await.approved(),
            "the first question gets the first scripted answer"
        );
        for _ in 0..3 {
            assert!(approver.approve(&request()).await.approved(), "yes forever");
        }
        assert_eq!(approver.times_asked(), 4);
    }

    #[tokio::test]
    async fn an_unscripted_approver_refuses_rather_than_inventing_consent() {
        let approver = Scripted::default();
        assert!(
            !approver.approve(&request()).await.approved(),
            "a test that forgot to script an approval must not get a write"
        );
    }
}
