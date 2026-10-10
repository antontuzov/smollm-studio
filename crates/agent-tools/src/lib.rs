//! What the agent can actually do, as data.
//!
//! A tool is a name, a description the model reads, a JSON schema its arguments
//! are validated against, a permission class the sandbox enforces, a timeout and
//! an output ceiling. The registry is the same list the CLI prints, the schema
//! the provider embeds in its prompt, and the gate a tool call passes through
//! before any code runs — so "what this agent is capable of" has exactly one
//! answer.
//!
//! This crate owns the implementations (file reads, search, git, test and lint
//! runners) and nothing about the agent's reasoning: a tool never decides
//! whether it should have been called.

mod call;

pub use agent_sandbox::Permission;
pub use call::{ToolCall, ToolOutput, ToolResult, ToolStatus};
