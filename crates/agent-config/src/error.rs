//! Why a configuration was refused, said precisely enough to fix.

use std::fmt;
use std::path::PathBuf;

/// A single thing wrong with a configuration, named by the key that caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub key: String,
    pub message: String,
}

impl Problem {
    pub fn new(key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.message)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A file that is already there and was not asked to be replaced.
    #[error("{path} already exists; pass --force to replace it")]
    Exists { path: PathBuf },

    /// The file is not TOML at all. The parser's own message already names the
    /// line, so it is passed through rather than rewritten into something less
    /// useful.
    #[error("{path} is not valid TOML\n  {message}")]
    Syntax { path: PathBuf, message: String },

    /// The TOML parses but does not match the configuration shape: a typo in a
    /// key, or a value of the wrong type. Reported instead of ignored, because
    /// `[aegnt]` silently read as an empty section would silently lower the
    /// approval mode.
    #[error(
        "the configuration does not match what this build expects\n  {message}\n  from: {sources}"
    )]
    Shape { message: String, sources: String },

    /// The shape is fine but the values contradict each other or are outside
    /// their allowed range. Every problem found is listed, not only the first.
    #[error("the configuration has {} problem(s):\n{}", .problems.len(), render_problems(.problems))]
    Invalid { problems: Vec<Problem> },
}

fn render_problems(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(|problem| format!("  - {problem}"))
        .collect::<Vec<_>>()
        .join("\n")
}

impl ConfigError {
    pub fn syntax(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self::Syntax {
            path: path.into(),
            message: message.into(),
        }
    }

    pub fn invalid(problems: Vec<Problem>) -> Self {
        Self::Invalid { problems }
    }

    /// The problems, when this error is a validation failure.
    pub fn problems(&self) -> &[Problem] {
        match self {
            Self::Invalid { problems } => problems,
            _ => &[],
        }
    }
}
