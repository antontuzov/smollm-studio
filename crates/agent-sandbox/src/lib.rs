//! The boundary the agent is not allowed to cross.
//!
//! Three things live here and they are deliberately in one crate rather than
//! spread across the loop: the *policy* (which permission class a tool needs,
//! what the current approval mode allows, whether a path is inside the
//! workspace and whether a command is on the deny list), the *redactor* (which
//! turns secrets in model output, tool output and log lines into `sk-…c3f1`
//! before any of them is written down), and the *audit log* (an append-only
//! local record of every decision and every tool call, which is also what makes
//! `smoll rollback` possible).
//!
//! Nothing in this crate runs a command or opens a network connection. It
//! answers questions about whether something may be done.

mod audit;
mod permission;
mod policy;
mod redact;

pub use audit::{read_entries, AuditLog, Entry, AUDIT_FILE};
pub use permission::Permission;
pub use policy::{classify, Action, Decision, Policy};
pub use redact::{find_secret_shapes, redact, Redaction};
