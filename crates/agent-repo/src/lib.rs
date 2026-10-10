//! Knowing the repository well enough to talk about it in 2,000 tokens.
//!
//! A small model cannot hold a codebase in context, so this crate does the part
//! a large model would do by reading everything: walk the tree honouring
//! `.gitignore` and `.agentignore`, detect what kind of project this is and
//! which commands validate it, build a compact repo map (crates, modules,
//! public surface, where the tests live), score files against a task so the
//! few most relevant ones are the only ones quoted, and apply or produce
//! unified diffs.
//!
//! Diffs are the format a small model is least likely to get wrong, so
//! patch application lives here and is tested against files that do not match
//! the hunk headers.
