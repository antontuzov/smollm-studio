//! From a configuration entry to the thing that answers.
//!
//! One function decides, for the whole workspace, what `[providers.small]` with
//! `type = "gguf"` means in this build. That question has three honest answers —
//! here is the provider, this build cannot make one because a feature was not
//! asked for, this repository does not have one yet — and a fourth answer that
//! would be a lie: quietly handing back the mock. An agent that runs a scripted
//! model in place of yours produces confident nonsense about your repository.

use std::sync::Arc;

use agent_config::{ProviderConfig, ProviderKind};

use crate::echo::EchoProvider;
use crate::error::ProviderError;
use crate::mock::MockProvider;
use crate::provider::{Capabilities, Provider};

/// Build the provider a `[providers.<name>]` section names.
///
/// `name` becomes the provider's own name, so a run's log, the config file and
/// the approval prompt all use the same word for what is answering.
pub fn build(name: &str, config: &ProviderConfig) -> Result<Arc<dyn Provider>, ProviderError> {
    let capabilities = capabilities_of(config);
    match config.kind {
        ProviderKind::Mock => Ok(Arc::new(
            MockProvider::unscripted()
                .with_label(name)
                .with_capabilities(capabilities),
        )),
        ProviderKind::Echo => Ok(Arc::new(
            EchoProvider::named(name).with_capabilities(capabilities),
        )),
        ProviderKind::Gguf => Err(if cfg!(feature = "gguf") {
            ProviderError::not_supported(
                "gguf",
                Some("the engine adapter for a local GGUF file is the next step".to_owned()),
            )
        } else {
            ProviderError::not_compiled("gguf", "gguf")
        }),
        ProviderKind::OpenaiCompatible => Err(if cfg!(feature = "openai-compatible") {
            ProviderError::not_supported(
                "openai-compatible",
                Some(
                    "the HTTP client for Ollama, LM Studio, vLLM and `smoll serve` is next"
                        .to_owned(),
                ),
            )
        } else {
            ProviderError::not_compiled("openai-compatible", "openai-compatible")
        }),
        ProviderKind::Candle => Err(ProviderError::not_supported(
            "candle",
            Some("no Candle runtime is linked in this build".to_owned()),
        )),
        ProviderKind::Onnx => Err(ProviderError::not_supported(
            "onnx",
            Some("no ONNX runtime is linked in this build".to_owned()),
        )),
        ProviderKind::Huggingface => Err(ProviderError::not_supported(
            "huggingface",
            Some(
                "a Hub download stays in the model layer; its inference API is not wired here"
                    .to_owned(),
            ),
        )),
    }
}

/// The window and abilities the configuration claims, taken at its word.
///
/// Validation belongs to `agent-config`: a context length under 256 tokens is
/// refused there, so a provider built from a loaded configuration always has
/// enough room for a prompt and an answer.
pub fn capabilities_of(config: &ProviderConfig) -> Capabilities {
    let mut capabilities = Capabilities::basic();
    if let Some(tokens) = config.context_length {
        capabilities = capabilities.window(tokens as usize);
    }
    // Only a model this repository knows can hold a tool list is asked to: a
    // GGUF file says nothing about function calling until its template does.
    if matches!(config.kind, ProviderKind::Mock) {
        capabilities = capabilities.with_tool_calling();
    }
    capabilities
}

#[cfg(test)]
mod tests {
    use super::*;
    use smollm_core::chat::{chat_message, Role};

    use crate::provider::CompletionRequest;

    fn config(kind: ProviderKind) -> ProviderConfig {
        ProviderConfig::new(kind)
    }

    fn request() -> CompletionRequest {
        CompletionRequest::new(vec![chat_message(Role::User, "what does this repo do?")])
    }

    #[tokio::test]
    async fn a_configured_mock_answers_that_it_has_no_script() {
        let provider = build("mock", &config(ProviderKind::Mock)).expect("a mock always builds");
        assert_eq!(provider.name(), "mock");
        let answer = provider.complete(&request()).await.expect("an answer");
        assert!(
            answer.text.contains("no scripted replies"),
            "{}",
            answer.text
        );
        assert!(
            !answer.asks_for_tools(),
            "an unscripted mock must not pretend to act"
        );
    }

    #[tokio::test]
    async fn a_configured_echo_shows_the_prompt_it_was_given() {
        let provider = build("inspector", &config(ProviderKind::Echo)).expect("an echo builds");
        assert_eq!(provider.name(), "inspector");
        let answer = provider.complete(&request()).await.expect("an answer");
        assert!(
            answer.text.contains("what does this repo do?"),
            "{}",
            answer.text
        );
    }

    #[tokio::test]
    async fn the_window_a_config_declares_is_the_window_the_provider_reports() {
        let mut config = config(ProviderKind::Echo);
        config.context_length = Some(2048);
        let provider = build("small", &config).expect("an echo builds");
        assert_eq!(provider.capabilities().context_tokens, 2048);
    }

    #[tokio::test]
    async fn a_local_model_file_is_not_silently_replaced_by_a_script() {
        let mut config = config(ProviderKind::Gguf);
        config.path = Some("/models/qwen2.5-1.5b-instruct-q4_k_m.gguf".to_owned());
        let error = build("local", &config)
            .err()
            .expect("the adapter does not exist yet");
        let message = error.to_string();
        assert!(message.contains("gguf"), "{message}");
        assert!(
            message.contains("--features") || message.contains("next step"),
            "the error should say what to do: {message}"
        );
        assert!(!error.retryable(), "asking again does not build an adapter");
    }

    #[tokio::test]
    async fn an_openai_compatible_server_is_named_as_the_next_step() {
        let mut config = config(ProviderKind::OpenaiCompatible);
        config.base_url = Some("http://localhost:11434/v1".to_owned());
        config.model = Some("qwen2.5:1.5b".to_owned());
        let error = build("ollama", &config)
            .err()
            .expect("the client does not exist yet");
        assert!(error.to_string().contains("openai"), "{error}");
    }

    #[tokio::test]
    async fn an_unwired_backend_says_which_one() {
        for kind in [
            ProviderKind::Candle,
            ProviderKind::Onnx,
            ProviderKind::Huggingface,
        ] {
            let error = build("future", &config(kind))
                .err()
                .expect("not implemented yet");
            let message = error.to_string();
            assert!(message.contains(kind.as_str()), "{message}");
            assert!(!error.retryable(), "{message}");
        }
    }
}
