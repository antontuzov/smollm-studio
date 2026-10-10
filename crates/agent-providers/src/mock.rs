//! The scripted provider every test in this workspace runs against.
//!
//! A mock that only returns a string cannot test an agent: what is worth
//! checking is what the loop does when the model asks for a tool, gets a rate
//! limit, or stops mid-sentence. So a [`MockProvider`] replays a *script* of
//! those shapes, remembers every request it received, and refuses to invent
//! answers once the script runs out — an under-scripted test then fails with
//! "ran out after 2 replies" instead of looping until the step budget kills it.
//! [`MockProvider::repeat_last`] is the opt-out for demos.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use smollm_core::chat::{approx_token_count, TokenUsage};

use agent_tools::ToolCall;

use crate::error::ProviderError;
use crate::parse::tool_calls_among;
use crate::provider::{Capabilities, Completion, CompletionRequest, Provider, StopReason};

/// One answer in a [`MockProvider`] script.
#[derive(Debug, Clone)]
pub enum Reply {
    /// Prose. If it contains a tool call in a shape [`crate::parse`] recognises,
    /// the completion carries that call too, so a script written as plain text
    /// exercises the same parsing path a real small model's answer does.
    Text(String),
    /// A native tool call, the shape a function-calling model produces.
    Calls(Vec<ToolCall>),
    /// An answer cut off mid-token, which is what a reply budget does.
    Truncated(String),
    /// An answer that is not usable at all: no text, no call.
    Empty,
    /// The connection drops. Retryable, so the loop should back off and ask again.
    Unreachable,
    /// A 429, with the wait the server asked for.
    RateLimited { retry_after_seconds: Option<u64> },
}

impl Reply {
    /// A prose reply.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// A reply that asks for one tool with these arguments.
    pub fn call(name: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self::Calls(vec![ToolCall::new(name, arguments)])
    }

    /// A reply that stops at `max_tokens` with this much of the answer present.
    pub fn truncated(text: impl Into<String>) -> Self {
        Self::Truncated(text.into())
    }
}

impl From<&str> for Reply {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<String> for Reply {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

/// What an unscripted mock answers, forever.
pub const UNSCRIPTED: &str = "This mock provider has no scripted replies.";

/// A provider that says what it was told to say, in order, and takes notes.
pub struct MockProvider {
    label: String,
    capabilities: Capabilities,
    /// What to answer once the script is exhausted, if the run may keep going.
    on_empty: Option<Reply>,
    script: Mutex<Script>,
}

#[derive(Debug, Default)]
struct Script {
    pending: VecDeque<Reply>,
    received: Vec<CompletionRequest>,
    answered: usize,
}

impl MockProvider {
    /// A provider that replays `replies`, one per request.
    pub fn scripted(replies: impl IntoIterator<Item = Reply>) -> Self {
        let pending: VecDeque<Reply> = replies.into_iter().collect();
        Self {
            label: "mock".to_owned(),
            capabilities: Capabilities::basic().with_tool_calling(),
            on_empty: None,
            script: Mutex::new(Script {
                pending,
                received: Vec::new(),
                answered: 0,
            }),
        }
    }

    /// The same, answering to the name the loop prints.
    pub fn named(label: impl Into<String>, replies: impl IntoIterator<Item = Reply>) -> Self {
        Self::scripted(replies).with_label(label)
    }

    /// A mock nobody scripted: it keeps explaining that it has no replies, asks
    /// for no tool, and so cannot damage a repository. This is what a
    /// configuration section with `type = "mock"` builds.
    pub fn unscripted() -> Self {
        Self::scripted([Reply::text(UNSCRIPTED)]).with_repeated(Reply::text(UNSCRIPTED))
    }

    /// One answer, kept forever: the provider for a demo that decides nothing.
    pub fn fixed(text: impl Into<String>) -> Self {
        let reply = Reply::text(text);
        Self::scripted([reply.clone()]).with_repeated(reply)
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Keep giving this answer after the script runs out.
    pub fn with_repeated(mut self, reply: Reply) -> Self {
        self.on_empty = Some(reply);
        self
    }

    /// Answer the last scripted reply forever, which is what a demo that ends in
    /// a summary wants. Panics on an empty script: there is nothing to repeat.
    pub fn repeat_last(mut self) -> Self {
        let last = self
            .script
            .lock()
            .expect("the mock is not shared across a panic")
            .pending
            .back()
            .cloned()
            .expect("repeat_last needs at least one scripted reply");
        self.on_empty = Some(last);
        self
    }

    /// Every request received so far, oldest first. This is how a test asserts on
    /// the context the loop actually built rather than on what it meant to build.
    pub fn requests(&self) -> Vec<CompletionRequest> {
        self.script().received.clone()
    }

    /// The most recent request, or `None` before the first.
    pub fn last_request(&self) -> Option<CompletionRequest> {
        self.script().received.last().cloned()
    }

    /// How many answers have been handed out.
    pub fn answers_given(&self) -> usize {
        self.script().answered
    }

    /// Replies still queued — the way to check a run stopped early.
    pub fn remaining(&self) -> usize {
        self.script().pending.len()
    }

    fn script(&self) -> std::sync::MutexGuard<'_, Script> {
        self.script
            .lock()
            .expect("the mock is not shared across a panic")
    }

    /// The next scripted reply, or the exhaustion error.
    fn take_reply(&self) -> Result<Reply, ProviderError> {
        let mut script = self.script();
        if let Some(reply) = script.pending.pop_front() {
            script.answered += 1;
            return Ok(reply);
        }
        if let Some(reply) = self.on_empty.clone() {
            script.answered += 1;
            return Ok(reply);
        }
        Err(ProviderError::Malformed {
            message: format!(
                "the mock ran out of scripted replies after {}; script another one",
                script.answered
            ),
        })
    }

    fn replay(
        &self,
        reply: Reply,
        request: &CompletionRequest,
    ) -> Result<Completion, ProviderError> {
        let prompt_tokens = cost(request);
        match reply {
            Reply::Text(text) => {
                let calls = tool_calls_among(&text, &request.tools);
                let mut completion = with_prompt(Completion::text(text), prompt_tokens);
                if !calls.is_empty() {
                    completion = completion.with_calls(calls);
                }
                Ok(completion)
            }
            Reply::Calls(calls) => {
                let text = calls
                    .iter()
                    .map(|call| {
                        format!(
                            "{} {}",
                            call.name,
                            serde_json::to_string(&call.arguments).unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let completion =
                    with_prompt(Completion::text(text).with_calls(calls), prompt_tokens);
                Ok(completion)
            }
            Reply::Truncated(text) => {
                let budget = request.max_tokens.unwrap_or(256);
                let mut completion = with_prompt(Completion::text(text), prompt_tokens);
                completion.stop = StopReason::Length;
                // A cut-off answer spends the whole reply budget to get there.
                completion.usage.completion_tokens = budget;
                completion.usage.total_tokens = prompt_tokens + budget;
                Ok(completion)
            }
            Reply::Empty => Ok(with_prompt(Completion::text(String::new()), prompt_tokens)),
            Reply::Unreachable => Err(ProviderError::Unreachable {
                target: self.label.clone(),
                message: "the script said the server is down".to_owned(),
            }),
            Reply::RateLimited {
                retry_after_seconds,
            } => Err(ProviderError::RateLimited {
                message: "the script said 429".to_owned(),
                retry_after_seconds,
            }),
        }
    }
}

impl Default for MockProvider {
    /// An answer that asks for nothing, so an unconfigured mock produces a run
    /// that does nothing rather than one that invents tool calls.
    fn default() -> Self {
        Self::scripted([Reply::text("This mock provider has no scripted replies.")])
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        &self.label
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    fn model(&self) -> Option<&str> {
        Some("scripted")
    }

    async fn complete(&self, request: &CompletionRequest) -> Result<Completion, ProviderError> {
        // The request is recorded before the answer, so a test can see the prompt
        // of a step that then failed.
        {
            let mut script = self.script();
            script.received.push(request.clone());
        }
        let reply = self.take_reply()?;
        self.replay(reply, request)
    }
}

/// Set the prompt side of a usage line without losing the completion side,
/// which is what keeps `total_tokens` the sum of the two.
fn with_prompt(mut completion: Completion, prompt_tokens: u32) -> Completion {
    completion.usage = TokenUsage::new(prompt_tokens, completion.usage.completion_tokens);
    completion
}

/// The prompt's own cost, from the same heuristic the rest of the repo uses.
fn cost(request: &CompletionRequest) -> u32 {
    request
        .messages
        .iter()
        .map(|message| approx_token_count(&message.content))
        .sum()
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use smollm_core::chat::{chat_message, Role};

    use crate::provider::ToolSchema;

    fn request(text: &str) -> CompletionRequest {
        CompletionRequest::new(vec![chat_message(Role::User, text)])
    }

    fn tool(name: &str) -> ToolSchema {
        ToolSchema::new(name, "a tool for the test", json!({"type": "object"}))
    }

    #[tokio::test]
    async fn replies_come_in_order_and_every_request_is_kept() {
        let mock = MockProvider::scripted([Reply::text("first"), "second".into()]);
        assert_eq!(mock.complete(&request("one")).await.unwrap().text, "first");
        assert_eq!(mock.complete(&request("two")).await.unwrap().text, "second");
        assert_eq!(mock.answers_given(), 2);
        assert_eq!(mock.remaining(), 0);
        let prompts: Vec<String> = mock
            .requests()
            .iter()
            .map(|request| request.messages[0].content.clone())
            .collect();
        assert_eq!(prompts, ["one", "two"]);
        assert_eq!(mock.last_request().unwrap().messages[0].content, "two");
    }

    #[tokio::test]
    async fn a_text_answer_containing_a_call_is_parsed_like_a_real_one() {
        let mock = MockProvider::scripted([Reply::text(
            "Reading it now.\n```json\n{\"tool\": \"read_file\", \"path\": \"src/main.rs\"}\n```",
        )]);
        let mut request = request("what does main.rs do?");
        request.tools = vec![tool("read_file"), tool("list_files")];
        let answer = mock.complete(&request).await.unwrap();
        assert!(answer.asks_for_tools());
        assert_eq!(answer.calls.len(), 1);
        assert_eq!(answer.calls[0].name, "read_file");
        assert_eq!(answer.calls[0].arguments["path"], "src/main.rs");
    }

    #[tokio::test]
    async fn a_call_for_a_tool_that_was_not_offered_is_not_a_call() {
        let mock = MockProvider::scripted([Reply::text(
            "```json\n{\"tool\": \"delete_repo\", \"path\": \".\"}\n```",
        )]);
        let mut request = request("tidy up");
        request.tools = vec![tool("read_file")];
        let answer = mock.complete(&request).await.unwrap();
        assert!(!answer.asks_for_tools());
        assert!(!answer.text.is_empty(), "the prose is still worth showing");
    }

    #[tokio::test]
    async fn a_native_call_answer_carries_its_arguments_through() {
        let mock = MockProvider::scripted([Reply::call("list_files", json!({"dir": "src"}))]);
        let answer = mock.complete(&request("what is in src?")).await.unwrap();
        assert_eq!(answer.stop, StopReason::ToolCalls);
        assert_eq!(answer.calls[0].name, "list_files");
        assert_eq!(answer.calls[0].str_arg("dir"), Some("src".to_owned()));
    }

    #[tokio::test]
    async fn a_runaway_script_fails_loudly_instead_of_answering_forever() {
        let mock = MockProvider::scripted([Reply::text("only one")]);
        mock.complete(&request("one")).await.unwrap();
        let error = mock.complete(&request("two")).await.unwrap_err();
        assert!(
            error.to_string().contains("after 1"),
            "unexpected message: {error}"
        );
        assert!(!error.retryable(), "adding a reply is not a retry's fault");
    }

    #[tokio::test]
    async fn repeat_last_keeps_answering_the_same_thing() {
        let mock = MockProvider::scripted([Reply::text("done")]).repeat_last();
        for _ in 0..3 {
            assert_eq!(mock.complete(&request("x")).await.unwrap().text, "done");
        }
        assert_eq!(mock.answers_given(), 3);
    }

    #[tokio::test]
    async fn fixed_answers_never_run_out() {
        let mock = MockProvider::fixed("hello");
        assert_eq!(mock.complete(&request("a")).await.unwrap().text, "hello");
        assert_eq!(mock.complete(&request("b")).await.unwrap().text, "hello");
    }

    #[tokio::test]
    async fn a_scripted_connection_failure_is_the_retryable_kind() {
        let mock = MockProvider::scripted([
            Reply::Unreachable,
            Reply::RateLimited {
                retry_after_seconds: Some(4),
            },
            Reply::text("back up"),
        ]);
        let down = mock.complete(&request("x")).await.unwrap_err();
        assert!(down.retryable());
        assert!(down.to_string().contains("mock"), "{down}");

        let busy = mock.complete(&request("x")).await.unwrap_err();
        assert_eq!(busy.backoff_hint(), Some(4));

        assert_eq!(mock.complete(&request("x")).await.unwrap().text, "back up");
    }

    #[tokio::test]
    async fn a_failed_step_still_records_its_prompt() {
        let mock = MockProvider::scripted([Reply::Unreachable]);
        mock.complete(&request("the prompt that failed"))
            .await
            .unwrap_err();
        assert_eq!(mock.requests().len(), 1);
        assert_eq!(
            mock.requests()[0].messages[0].content,
            "the prompt that failed"
        );
    }

    #[tokio::test]
    async fn a_truncated_answer_says_it_ran_out_of_budget() {
        let mut request = request("explain the whole file");
        request.max_tokens = Some(64);
        let mock = MockProvider::scripted([Reply::truncated("the parser then")]);
        let answer = mock.complete(&request).await.unwrap();
        assert!(answer.stop.is_truncated());
        assert_eq!(answer.usage.completion_tokens, 64);
        assert!(!answer.asks_for_tools());
    }

    #[tokio::test]
    async fn an_empty_answer_is_not_a_call() {
        let mock = MockProvider::scripted([Reply::Empty]);
        let answer = mock.complete(&request("x")).await.unwrap();
        assert!(answer.text.is_empty());
        assert!(!answer.asks_for_tools());
        assert_eq!(answer.stop, StopReason::EndOfTurn);
    }

    #[tokio::test]
    async fn usage_counts_the_prompt_the_loop_built() {
        let mock = MockProvider::scripted([Reply::text("ok")]);
        let request = request("count these words exactly once please");
        let expected = cost(&request);
        let answer = mock.complete(&request).await.unwrap();
        assert_eq!(answer.usage.prompt_tokens, expected);
        assert!(expected > 0);
        assert_eq!(
            answer.usage.total_tokens,
            expected + answer.usage.completion_tokens
        );
    }

    #[tokio::test]
    async fn capabilities_are_the_ones_the_test_asked_for() {
        let mock = MockProvider::scripted([Reply::text("ok")])
            .with_capabilities(Capabilities::basic().window(2048));
        assert!(!mock.capabilities().tool_calling, "a weak model stays weak");
        assert_eq!(mock.capabilities().context_tokens, 2048);
        assert_eq!(mock.name(), "mock");
        assert_eq!(MockProvider::named("small", []).name(), "small");
    }

    #[tokio::test]
    async fn an_unconfigured_mock_asks_for_nothing() {
        let mock = MockProvider::default();
        let answer = mock.complete(&request("x")).await.unwrap();
        assert!(!answer.asks_for_tools());
        assert!(
            answer.text.contains("no scripted replies"),
            "{}",
            answer.text
        );
    }
}
