//! The provider that repeats the prompt back.
//!
//! Its answer is useless and its request is not: an echo makes the exact context
//! a run would send inspectable, which is how a prompt for a 1.5B model gets
//! tuned without a model in the loop. `smoll task --dry-run` prints this text,
//! and the documentation's prompt examples are copied from it.

use async_trait::async_trait;
use smollm_core::chat::{approx_token_count, Role, TokenUsage};

use crate::error::ProviderResult;
use crate::provider::{Capabilities, Completion, CompletionRequest, Provider, StopReason};

/// A provider whose completion is the rendered prompt.
pub struct EchoProvider {
    label: String,
    capabilities: Capabilities,
}

impl EchoProvider {
    pub fn new() -> Self {
        Self {
            label: "echo".to_owned(),
            // What a small local model really is: no native tool calling, and a
            // window small enough that the fit check fails in a test rather than
            // in a demo.
            capabilities: Capabilities::basic().window(4096),
        }
    }

    /// Call itself by a different name in the run's log.
    pub fn named(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            ..Self::new()
        }
    }

    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// The prompt as the model would receive it: a header naming the tools on
    /// offer, then each message under its role.
    pub fn render(request: &CompletionRequest) -> String {
        let mut out = String::new();
        if !request.tools.is_empty() {
            out.push_str("Tools available. To use one, answer with a JSON object naming it.\n");
            for tool in &request.tools {
                out.push_str(&format!(
                    "- {}: {}\n",
                    tool.name,
                    render_description(&tool.description, &tool.parameters)
                ));
            }
            out.push('\n');
        }
        for message in &request.messages {
            out.push_str(&format!(
                "{}: {}\n\n",
                role_label(&message.role),
                message.content.trim()
            ));
        }
        while out.ends_with("\n\n") {
            out.truncate(out.len() - 2);
        }
        out.push('\n');
        out
    }
}

impl Default for EchoProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// A tool line the model can act on: its description, then its arguments.
fn render_description(description: &str, parameters: &serde_json::Value) -> String {
    let mut line = description.trim().to_owned();
    if let Some(object) = parameters.as_object() {
        let properties = object
            .get("properties")
            .and_then(serde_json::Value::as_object);
        let required = object
            .get("required")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let rendered: Vec<String> = match properties {
            Some(fields) => fields
                .iter()
                .map(|(name, schema)| {
                    let kind = schema
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("any");
                    let is_required = required
                        .iter()
                        .any(|need| need.as_str() == Some(name.as_str()));
                    let note = schema
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(|text| format!(" — {text}"))
                        .unwrap_or_default();
                    format!("{name}:{kind}{}", if is_required { "*" } else { "" }) + &note
                })
                .collect(),
            None => Vec::new(),
        };
        if !rendered.is_empty() {
            line.push_str(&format!(" Arguments: {}.", rendered.join(", ")));
        }
    }
    line
}

#[async_trait]
impl Provider for EchoProvider {
    fn name(&self) -> &str {
        &self.label
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    fn model(&self) -> Option<&str> {
        Some("echo")
    }

    async fn complete(&self, request: &CompletionRequest) -> ProviderResult<Completion> {
        let text = Self::render(request);
        // An echo never asks for a tool: repeating a prompt is not an intent to
        // run something, and a dry run must not look like a plan of action.
        Ok(Completion {
            calls: Vec::new(),
            stop: StopReason::EndOfTurn,
            usage: TokenUsage::new(request.prompt_tokens(), approx_token_count(&text)),
            text,
        })
    }
}

/// The role as a chat template writes it, so an echo looks like the real prompt.
fn role_label(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use smollm_core::chat::{chat_message, Role};

    use crate::provider::ToolSchema;

    fn schema() -> ToolSchema {
        ToolSchema::new(
            "read_file",
            "Read a file from the repository.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Repo-relative path"},
                    "max_bytes": {"type": "integer"}
                },
                "required": ["path"]
            }),
        )
    }

    #[tokio::test]
    async fn the_prompt_comes_back_as_the_answer() {
        let request = CompletionRequest::new(vec![
            chat_message(Role::System, "You are a coding agent."),
            chat_message(Role::User, "Explain main.rs"),
        ]);
        let answer = EchoProvider::new().complete(&request).await.unwrap();
        assert!(
            answer.text.contains("You are a coding agent."),
            "{}",
            answer.text
        );
        assert!(answer.text.contains("Explain main.rs"), "{}", answer.text);
        assert!(!answer.asks_for_tools(), "an echo proposes nothing");
    }

    #[tokio::test]
    async fn roles_are_visible_so_a_bad_template_is_findable() {
        let request = CompletionRequest::new(vec![
            chat_message(Role::System, "rules"),
            chat_message(Role::User, "question"),
            chat_message(Role::Assistant, "an earlier answer"),
            chat_message(Role::User, "the tool result"),
        ]);
        let text = EchoProvider::render(&request);
        for role in ["system", "user", "assistant"] {
            assert!(
                text.contains(&format!("{role}: ")),
                "missing {role}: {text}"
            );
        }
        // Two user turns in a row is the shape a tool result takes; the order is
        // what the loop has to be able to see.
        assert!(text.find("rules").unwrap() < text.find("the tool result").unwrap());
    }

    #[tokio::test]
    async fn offered_tools_are_named_with_their_arguments() {
        let mut request = CompletionRequest::new(vec![chat_message(Role::User, "read it")]);
        request.tools = vec![schema()];
        let text = EchoProvider::render(&request);
        assert!(text.contains("read_file"), "{text}");
        assert!(text.contains("Read a file from the repository."), "{text}");
        assert!(text.contains("path:string*"), "{text}");
        assert!(text.contains("max_bytes:integer"), "{text}");
        assert!(text.contains("Repo-relative path"), "{text}");
    }

    #[tokio::test]
    async fn no_tools_means_no_tool_header() {
        let request = CompletionRequest::new(vec![chat_message(Role::User, "just answer")]);
        let text = EchoProvider::render(&request);
        assert!(!text.contains("Tools available"), "{text}");
    }

    #[tokio::test]
    async fn usage_counts_both_sides_of_the_exchange() {
        let request =
            CompletionRequest::new(vec![chat_message(Role::User, "a prompt of some length")]);
        let answer = EchoProvider::new().complete(&request).await.unwrap();
        assert_eq!(answer.usage.prompt_tokens, request.prompt_tokens());
        assert!(answer.usage.completion_tokens > 0);
        assert_eq!(
            answer.usage.total_tokens,
            answer.usage.prompt_tokens + answer.usage.completion_tokens
        );
    }

    #[tokio::test]
    async fn the_default_window_is_the_one_a_small_model_has() {
        let echo = EchoProvider::new();
        let capabilities = echo.capabilities();
        assert!(!capabilities.tool_calling);
        assert!(!capabilities.streaming);
        assert_eq!(capabilities.context_tokens, 4096);
        assert_eq!(echo.name(), "echo");
        assert_eq!(EchoProvider::named("inspect").name(), "inspect");
    }

    #[tokio::test]
    async fn a_prompt_over_the_window_does_not_fit() {
        let echo = EchoProvider::new().with_capabilities(Capabilities::basic().window(64));
        let request = CompletionRequest::new(vec![chat_message(Role::User, "word ".repeat(200))]);
        assert!(
            !request.fits(&echo.capabilities(), 32),
            "the fit check has to fail before the request is made"
        );
    }

    #[test]
    fn a_tool_without_a_properties_map_still_renders() {
        let line = render_description("Do the thing.", &json!({"type": "object"}));
        assert_eq!(line, "Do the thing.");
    }
}
