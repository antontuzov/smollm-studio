//! Layered configuration for the agent: a user TOML file, an optional
//! project-level file that can override it, and environment variables on top.
//!
//! Everything the agent can do is decided by this crate's [`Config`], so that
//! the loop, the tools and the sandbox never read a file or an environment
//! variable themselves — they are handed one validated value. Loading is
//! forgiving about a missing file and strict about a broken one: a config that
//! cannot be parsed is reported with its path and the offending key rather than
//! silently falling back to defaults, because a silently ignored `sandbox =
//! "strict"` is a security failure.
