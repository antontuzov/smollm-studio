//! Domain foundation for SmolLLM Studio.
//!
//! Everything here is UI-agnostic: shared types, configuration, platform paths,
//! GGUF metadata reading and the in-memory log store.

pub mod chat;
pub mod config;
pub mod error;
pub mod gguf;
pub mod logs;
pub mod model;
pub mod paths;
pub mod session;
pub mod system;

pub use config::Settings;
pub use error::{AppError, AppResult, ErrorPayload};
pub use paths::AppPaths;
