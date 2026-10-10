//! The provider contract, tested the way the agent loop will use it: several
//! steps, real tool schemas, and the awkward answers in between.

use agent_providers::{
    build, tool_calls, tool_calls_among, Capabilities, CompletionRequest, EchoProvider,
    MockProvider, Provider, Reply, ToolSchema,
};
use agent_tools::ToolCall;
use serde_json::json;
use smollm_core::chat::{chat_message, Role};

/// The tools a run would offer, few enough that the prompt still fits a 1.5B
/// model's window.
fn tools() -> Vec<ToolSchema> {
    vec![
        ToolSchema::new(
            "read_file",
            "Read one file from the repository.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        ),
        ToolSchema::new(
            "run_tests",
            "Run the project's test command.",
            json!({"type": "object", "properties": {}}),
        ),
    ]
}

/// A step's request: the system rules, the task, and what earlier steps learned.
fn step(observations: &[&str]) -> CompletionRequest {
    let mut messages = vec![
        chat_message(Role::System, "You are a coding agent for this repository."),
        chat_message(Role::User, "Why does the config test fail?"),
    ];
    for observation in observations {
        messages.push(chat_message(Role::Assistant, "I read it."));
        messages.push(chat_message(Role::User, *observation));
    }
    let mut request = CompletionRequest::new(messages);
    request.tools = tools();
    request
}

/// The completion of one step, with the answer's text for the rare test that
/// only cares about prose.
async fn answer<P: Provider + ?Sized>(
    provider: &P,
    request: &CompletionRequest,
) -> agent_providers::Completion {
    provider.complete(request).await.expect("an answer")
}

#[tokio::test]
async fn a_three_step_run_asks_once_per_step_and_carries_what_it_learned() {
    let mock = MockProvider::scripted([
        Reply::text("I will read the parser, then run the tests."),
        Reply::call("read_file", json!({"path": "src/parser.rs"})),
        Reply::text("The parser drops the last field. Fix that first."),
    ]);

    let plan = answer(&mock, &step(&[])).await;
    assert!(!plan.asks_for_tools(), "a plan is prose, not a call");

    let call = answer(&mock, &step(&["file contents"])).await;
    assert!(call.asks_for_tools());
    assert_eq!(call.calls[0].name, "read_file");

    let final_answer = answer(&mock, &step(&["file contents", "tests failed"])).await;
    assert_eq!(
        final_answer.text,
        "The parser drops the last field. Fix that first."
    );

    assert_eq!(mock.requests().len(), 3, "one request per step");
    assert_eq!(mock.remaining(), 0, "and not one reply more than scripted");
    assert!(
        mock.requests()[2].messages.len() > mock.requests()[0].messages.len(),
        "a later step must carry what the earlier one learned"
    );
}

#[tokio::test]
async fn a_prose_answer_with_a_fenced_call_becomes_the_same_call_as_a_native_one() {
    let request = step(&[]);
    let prose = answer(
        &MockProvider::scripted([Reply::text(
            "```json\n{\"tool\": \"read_file\", \"arguments\": {\"path\": \"src/main.rs\"}}\n```",
        )]),
        &request,
    )
    .await;
    let native = answer(
        &MockProvider::scripted([Reply::call("read_file", json!({"path": "src/main.rs"}))]),
        &request,
    )
    .await;

    assert_eq!(prose.calls, native.calls);
    assert_eq!(prose.calls[0].arguments["path"], "src/main.rs");
}

#[tokio::test]
async fn a_call_for_a_tool_that_was_never_offered_is_not_runnable() {
    let requested = answer(
        &MockProvider::scripted([Reply::text(
            "```json\n{\"tool\": \"delete_repo\", \"confirm\": true}\n```",
        )]),
        &step(&[]),
    )
    .await;
    assert!(
        requested.calls.is_empty(),
        "a name that is not a tool is not a call"
    );
    assert_eq!(tool_calls_among(&requested.text, &[]).len(), 0);
    assert_eq!(
        tool_calls(&requested.text, &|name| name == "delete_repo").len(),
        1,
        "the parser reads what is there; the tool list is what gates it"
    );
}

#[tokio::test]
async fn an_answer_cut_off_mid_json_is_repaired_into_its_call() {
    let broken = "```json\n{\"tool\": \"read_file\", \"path\": \"src/main";
    let cut_off = answer(&MockProvider::scripted([Reply::text(broken)]), &step(&[])).await;
    assert_eq!(cut_off.calls.len(), 1, "the cut-off object was closed");
    assert_eq!(cut_off.calls[0].arguments["path"], "src/main");
}

#[tokio::test]
async fn an_answer_with_no_call_in_it_gets_one_repair_then_a_real_answer() {
    let mock = MockProvider::scripted([
        Reply::text("I think we should look at the parser, which is where"),
        Reply::call("read_file", json!({"path": "src/parser.rs"})),
    ]);
    let request = step(&[]);
    let rambling = answer(&mock, &request).await;
    assert!(!rambling.asks_for_tools());

    let repaired = mock.complete_again(&request).await.expect("the second try");
    assert_eq!(repaired.calls[0].name, "read_file");
    assert_eq!(mock.requests().len(), 2, "a repair is one more request");
}

#[tokio::test]
async fn a_rate_limit_is_retryable_and_keeps_the_prompt_it_refused() {
    let mock = MockProvider::scripted([
        Reply::RateLimited {
            retry_after_seconds: Some(2),
        },
        Reply::text("Now I can answer."),
    ]);
    let request = step(&[]);
    let error = mock.complete(&request).await.unwrap_err();
    assert!(error.retryable());
    assert_eq!(error.backoff_hint(), Some(2));
    assert_eq!(mock.requests().len(), 1, "the refused prompt is on record");

    let retry = answer(&mock, &request).await;
    assert_eq!(retry.text, "Now I can answer.");
}

#[tokio::test]
async fn a_dead_server_says_which_one_and_is_worth_another_try() {
    let error = MockProvider::named("ollama", [Reply::Unreachable])
        .complete(&step(&[]))
        .await
        .unwrap_err();
    assert!(error.retryable());
    assert!(error.to_string().contains("ollama"), "{error}");
}

#[tokio::test]
async fn an_overlong_run_stops_at_the_script_instead_of_inventing_an_answer() {
    let mock = MockProvider::scripted([Reply::text("one step")]);
    answer(&mock, &step(&[])).await;
    let error = mock.complete(&step(&["and then"])).await.unwrap_err();
    assert!(error.to_string().contains("ran out"), "{error}");
    assert!(!error.retryable(), "a missing reply is not a flaky server");
}

#[tokio::test]
async fn an_echo_shows_the_exact_prompt_a_run_would_send() {
    let request = step(&[]);
    let echoed = answer(&EchoProvider::new(), &request).await;
    assert!(echoed.text.contains("system: "), "{}", echoed.text);
    assert!(echoed.text.contains("user: "), "{}", echoed.text);
    assert!(echoed.text.contains("read_file"), "{}", echoed.text);
    assert!(echoed.text.contains("path:string*"), "{}", echoed.text);
    assert!(echoed.calls.is_empty(), "a dry run proposes nothing");
    assert_eq!(echoed.usage.prompt_tokens, request.prompt_tokens());
}

#[tokio::test]
async fn a_tiny_window_refuses_a_big_prompt_before_any_work_starts() {
    let weak = MockProvider::scripted([Reply::text("x")])
        .with_capabilities(Capabilities::basic().window(256));
    let mut crowded = CompletionRequest::new(vec![chat_message(
        Role::User,
        format!("summarise this file:\n{}", "fn main() {{}}\n".repeat(200)),
    )]);
    crowded.tools = tools();
    assert!(
        !crowded.fits(&weak.capabilities(), 64),
        "the loop must trim the context, not send it and hope"
    );

    let mut small = CompletionRequest::new(vec![chat_message(Role::User, "summarise this file")]);
    small.tools = tools();
    assert!(small.fits(&weak.capabilities(), 64));
}

#[tokio::test]
async fn a_tool_that_needs_no_arguments_survives_the_flat_shape() {
    let mut request = CompletionRequest::new(vec![chat_message(Role::User, "test it")]);
    request.tools = tools();
    let called = answer(
        &MockProvider::scripted([Reply::text("```\n{\"tool\": \"run_tests\"}\n```")]),
        &request,
    )
    .await;
    assert_eq!(called.calls, vec![ToolCall::new("run_tests", json!({}))]);
}

#[tokio::test]
async fn a_configuration_section_builds_a_provider_or_explains_why_not() {
    // What `[providers.demo]` with `type = "mock"` gives a run.
    let provider = build(
        "demo",
        &agent_config::ProviderConfig::new(agent_config::ProviderKind::Mock),
    )
    .expect("the mock builds");
    assert_eq!(provider.name(), "demo");
    let unscripted = provider
        .complete(&step(&[]))
        .await
        .expect("an honest answer");
    assert!(
        unscripted.text.contains("no scripted replies"),
        "{}",
        unscripted.text
    );
    assert!(
        !unscripted.asks_for_tools(),
        "an unscripted run does nothing"
    );

    // And what `[providers.local] type = "gguf"` gives it while the engine
    // adapter is still missing: an error naming the thing to build.
    let error = build(
        "local",
        &agent_config::ProviderConfig::new(agent_config::ProviderKind::Gguf),
    )
    .err()
    .expect("no gguf adapter yet");
    assert!(error.to_string().contains("gguf"), "{error}");
    assert!(!error.retryable(), "asking again does not build an adapter");
}
