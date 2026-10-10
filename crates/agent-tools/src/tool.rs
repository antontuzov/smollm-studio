//! What a tool is, in the terms the rest of the agent needs.
//!
//! A tool is four things: a name and description the model reads, a JSON Schema
//! its arguments are checked against, an *intent* — which paths and programs a
//! particular call touches — and the code that carries it out. The first two go
//! into a prompt, the third goes to the sandbox before any of the fourth runs.
//!
//! That order is the whole design. A tool never decides whether it should have
//! been called, and the sandbox never has to trust a tool's own answer to that
//! question: [`Tool::intent`] is a declaration the caller can inspect without
//! executing anything, and [`Intent::permission`] can only raise the class a
//! tool claims, never lower it.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use agent_sandbox::{classify, Permission};

use crate::call::{ToolCall, ToolResult};
use crate::context::Context;

/// The part of a tool a prompt and a policy both read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    /// Written for a small model: what it is for, and what it does not do.
    pub description: String,
    /// A JSON Schema object for the arguments.
    pub parameters: serde_json::Value,
    /// The highest class this tool can reach on its own. A command line in the
    /// intent can raise it; nothing lowers it.
    pub permission: Permission,
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        permission: Permission,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            permission,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECONDS),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_output_limit(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }

    /// The token cost of offering this tool, which is paid on every step.
    pub fn token_cost(&self) -> usize {
        smollm_core::chat::approx_token_count(&format!(
            "{} {} {}",
            self.name,
            self.description,
            serde_json::to_string(&self.parameters).unwrap_or_default()
        )) as usize
    }
}

/// A read is cheap and a command is not, so the two get different defaults.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 20;
/// Roughly 2,000 tokens of text, which is what a 4,096-token model can spare.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 8 * 1024;

/// What one call is about to do, as far as the policy is concerned.
///
/// Paths are expected to be absolute and inside the workspace where the
/// configuration says they must be; [`crate::Context::resolve`] is what produces
/// them from the strings a model wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Intent {
    /// Files or directories the call reads or writes.
    pub paths: Vec<std::path::PathBuf>,
    /// The program and its arguments, for a call that runs one. Never a shell
    /// command line: nothing here goes through a shell, so redirection and `&&`
    /// are not features a model can reach.
    pub command: Option<Vec<String>>,
    /// Hosts a call would contact.
    pub hosts: Vec<String>,
}

impl Intent {
    pub fn reading(paths: Vec<std::path::PathBuf>) -> Self {
        Self {
            paths,
            ..Self::default()
        }
    }

    pub fn running(argv: Vec<String>) -> Self {
        Self {
            command: Some(argv),
            ..Self::default()
        }
    }

    pub fn writing(paths: Vec<std::path::PathBuf>) -> Self {
        Self {
            paths,
            ..Self::default()
        }
    }

    /// The one string that says what this call is about: the command line, or
    /// the first path. It is what a human reads in an approval prompt and what
    /// an allowlist is matched against.
    pub fn subject(&self) -> Option<String> {
        if let Some(argv) = &self.command {
            return Some(argv.join(" "));
        }
        self.paths
            .first()
            .map(|path| path.to_string_lossy().into_owned())
    }

    /// The class of this particular call, which is the tool's own class raised
    /// by whatever the call actually involves.
    ///
    /// The one override is a destructive command: a tool that runs programs
    /// cannot make `rm -rf` look like `cargo test` by declaring itself
    /// [`Permission::Execute`].
    pub fn permission(&self, declared: Permission) -> Permission {
        let mut permission = declared;
        if let Some(argv) = &self.command {
            permission = permission.max(classify(argv));
        }
        if !self.hosts.is_empty() {
            permission = permission.max(Permission::Network);
        }
        permission
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    /// What the call would do, before anything does it. The `Err` arm is for
    /// arguments that cannot be read at all, which is a failure the model gets
    /// to try again rather than a refusal.
    fn intent(&self, call: &ToolCall, ctx: &Context) -> Result<Intent, String>;

    /// Carry out a call the policy has already passed.
    ///
    /// A tool returns a [`ToolResult`] and does not decide whether it was
    /// allowed to be called: the registry has that job, so the answer cannot
    /// depend on which tool was asked.
    async fn run(&self, call: &ToolCall, ctx: &Context) -> ToolResult;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(text)
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn a_read_stays_a_read() {
        let intent = Intent::reading(vec![path("/repo/src/lib.rs")]);
        assert_eq!(
            intent.permission(Permission::ReadOnly),
            Permission::ReadOnly
        );
    }

    #[test]
    fn a_destructive_command_raises_the_class_of_a_tool_that_called_itself_execute() {
        let intent = Intent::running(argv(&["rm", "-rf", "target"]));
        assert_eq!(
            intent.permission(Permission::Execute),
            Permission::Destructive,
            "the argv wins, not the declaration"
        );
        let intent = Intent::running(argv(&["cargo", "test"]));
        assert_eq!(intent.permission(Permission::Execute), Permission::Execute);
    }

    #[test]
    fn a_declaration_cannot_be_lowered_by_an_intent() {
        let intent = Intent::default();
        assert_eq!(
            intent.permission(Permission::Destructive),
            Permission::Destructive,
            "a tool that says it destroys things is treated as one"
        );
    }

    #[test]
    fn reaching_the_network_is_never_less_than_a_network_permission() {
        let intent = Intent {
            hosts: vec!["huggingface.co".to_owned()],
            ..Intent::default()
        };
        assert_eq!(intent.permission(Permission::ReadOnly), Permission::Network);
    }

    #[test]
    fn a_definition_carries_its_own_limits() {
        let definition = ToolDefinition::new(
            "read_file",
            "Read one file",
            serde_json::json!({}),
            Permission::ReadOnly,
        );
        assert_eq!(
            definition.timeout,
            Duration::from_secs(DEFAULT_TIMEOUT_SECONDS)
        );
        assert_eq!(definition.max_output_bytes, DEFAULT_MAX_OUTPUT_BYTES);
        assert!(definition.token_cost() > 0);
        let json = serde_json::to_string(&definition).expect("serialisable");
        assert!(json.contains("\"permission\":\"read-only\""), "{json}");
    }
}
