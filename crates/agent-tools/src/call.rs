//! A tool call, what came back from it, and the limit that keeps a small
//! model's context usable.
//!
//! These are the two structures the loop trades in, and both are shaped by what
//! a 1.5B model actually produces rather than by what a well-behaved API
//! promises. Arguments arrive as a JSON value that may be malformed, so
//! accessors return `Option` and the repair step gets a chance to fix them;
//! output arrives as text that may be enormous, so [`ToolOutput`] carries
//! whether it was cut down.

use serde::{Deserialize, Serialize};

use agent_sandbox::Permission;

/// A tool the model asked for, with its arguments as written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

impl ToolCall {
    pub fn new(name: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            arguments,
        }
    }

    /// An argument that should be a string. A small model writes `1` where a
    /// path belongs, so a number is read as its text rather than refused: the
    /// tool will still complain about the path itself, which is a better error.
    pub fn str_arg(&self, key: &str) -> Option<String> {
        match self.arguments.get(key)? {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Number(number) => Some(number.to_string()),
            serde_json::Value::Bool(flag) => Some(flag.to_string()),
            _ => None,
        }
    }

    pub fn usize_arg(&self, key: &str) -> Option<usize> {
        match self.arguments.get(key)? {
            serde_json::Value::Number(number) => number.as_u64().map(|value| value as usize),
            // `"40"` is how a model that spells everything as a string answers.
            serde_json::Value::String(text) => text.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn bool_arg(&self, key: &str) -> Option<bool> {
        match self.arguments.get(key)? {
            serde_json::Value::Bool(flag) => Some(*flag),
            serde_json::Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" => Some(true),
                "false" | "no" | "0" => Some(false),
                _ => None,
            },
            serde_json::Value::Number(number) => number.as_u64().map(|value| value != 0),
            _ => None,
        }
    }

    /// A list of strings, tolerating the single value a model writes where an
    /// array belongs.
    pub fn str_list_arg(&self, key: &str) -> Option<Vec<String>> {
        match self.arguments.get(key)? {
            serde_json::Value::Array(items) => Some(
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect(),
            ),
            serde_json::Value::String(text) => Some(vec![text.clone()]),
            _ => None,
        }
    }
}

/// What a tool produced, already limited to a size a small model can read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub text: String,
    /// True when the tool had more to say and this is only the start of it.
    pub truncated: bool,
    /// Bytes before truncation, so the reader knows how much was dropped.
    pub full_bytes: usize,
}

impl ToolOutput {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let full_bytes = text.len();
        Self {
            text,
            truncated: false,
            full_bytes,
        }
    }

    pub fn empty() -> Self {
        Self::new(String::new())
    }

    /// Keep at most `max_bytes`, cut on a character boundary, and say what was
    /// dropped. A model that cannot tell the difference between a complete file
    /// and a cut one will edit the missing half out of existence.
    pub fn capped(text: impl Into<String>, max_bytes: usize) -> Self {
        let text = text.into();
        let full_bytes = text.len();
        if text.len() <= max_bytes {
            return Self {
                text,
                truncated: false,
                full_bytes,
            };
        }
        let mut cut = max_bytes;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let dropped = full_bytes - cut;
        let mut kept = text[..cut].to_owned();
        kept.push_str(&format!(
            "\n[… {dropped} bytes omitted; the tool saw {full_bytes} in total]"
        ));
        Self {
            text: kept,
            truncated: true,
            full_bytes,
        }
    }
}

/// How a call ended, kept separate from the text so a failure never looks like
/// an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolStatus {
    Completed,
    /// The tool ran and the thing it tried did not work: no such file, a test
    /// that failed, a command that exited non-zero.
    Failed {
        message: String,
    },
    /// The policy said no. This is not an error to retry; it is an answer.
    Refused {
        reason: String,
    },
    /// Nothing ran because a human has not said yes yet.
    AwaitingApproval,
}

impl ToolStatus {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Completed)
    }
}

/// One finished call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call: ToolCall,
    pub permission: Permission,
    pub status: ToolStatus,
    pub output: ToolOutput,
    pub duration_ms: u64,
}

impl ToolResult {
    pub fn completed(
        call: ToolCall,
        permission: Permission,
        output: ToolOutput,
        duration_ms: u64,
    ) -> Self {
        Self {
            call,
            permission,
            status: ToolStatus::Completed,
            output,
            duration_ms,
        }
    }

    pub fn failed(
        call: ToolCall,
        permission: Permission,
        message: impl Into<String>,
        output: ToolOutput,
    ) -> Self {
        Self {
            call,
            permission,
            status: ToolStatus::Failed {
                message: message.into(),
            },
            output,
            duration_ms: 0,
        }
    }

    pub fn refused(call: ToolCall, permission: Permission, reason: impl Into<String>) -> Self {
        Self {
            call,
            permission,
            status: ToolStatus::Refused {
                reason: reason.into(),
            },
            output: ToolOutput::empty(),
            duration_ms: 0,
        }
    }

    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    /// One line for a transcript or a status bar, always naming the tool.
    pub fn summary(&self) -> String {
        let detail = match &self.status {
            ToolStatus::Completed => format!(
                "{} bytes{}",
                self.output.full_bytes,
                if self.output.truncated {
                    ", truncated"
                } else {
                    ""
                }
            ),
            ToolStatus::Failed { message } => message.clone(),
            ToolStatus::Refused { reason } => format!("refused: {reason}"),
            ToolStatus::AwaitingApproval => "waiting for approval".to_owned(),
        };
        format!("{}: {detail}", self.call.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(json: &str) -> ToolCall {
        ToolCall::new("test", serde_json::from_str(json).expect("valid json"))
    }

    #[test]
    fn arguments_are_read_leniently_and_never_panics() {
        let c = call(r#"{"path": "src/main.rs", "limit": 40, "all": "yes"}"#);
        assert_eq!(c.str_arg("path").as_deref(), Some("src/main.rs"));
        assert_eq!(c.usize_arg("limit"), Some(40));
        assert_eq!(c.usize_arg("all"), None, "a yes is not a count");
        assert_eq!(c.bool_arg("all"), Some(true));
        assert_eq!(c.str_arg("missing"), None);
    }

    #[test]
    fn a_number_where_a_string_belongs_is_still_a_path() {
        let c = call(r#"{"line": 12}"#);
        assert_eq!(
            c.str_arg("line").as_deref(),
            Some("12"),
            "read it, then let the tool judge"
        );
    }

    #[test]
    fn a_single_value_where_an_array_belongs_is_one_item() {
        let c = call(r#"{"stop": "EOF"}"#);
        assert_eq!(c.str_list_arg("stop"), Some(vec!["EOF".to_owned()]));
        let c = call(r#"{"stop": ["a", 3, "b"]}"#);
        assert_eq!(
            c.str_list_arg("stop"),
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
    }

    #[test]
    fn truncation_never_splits_a_character() {
        // 6 bytes of two three-byte characters.
        // 24 bytes of three-byte characters asked down to 7: the cut has to
        // retreat to 6, which is the end of the second character, not slice the
        // third one open.
        let output = ToolOutput::capped("日本語のテキスト", 7);
        assert!(output.truncated);
        assert!(output.text.starts_with("日本\n"), "{:?}", output.text);
        assert!(
            !output.text.starts_with("日本語"),
            "the third character was dropped"
        );
        assert!(output.text.contains("18 bytes omitted"), "{}", output.text);
        assert_eq!(output.full_bytes, 24);
    }

    #[test]
    fn a_short_output_is_returned_unchanged() {
        let output = ToolOutput::capped("hello", 100);
        assert_eq!(output.text, "hello");
        assert!(!output.truncated);
        assert_eq!(output.full_bytes, 5);
    }

    #[test]
    fn a_refusal_is_not_the_same_as_a_failure() {
        let c = call("{}");
        let refused = ToolResult::refused(c.clone(), Permission::Execute, "rm -rf is denylisted");
        assert!(!refused.is_success());
        assert!(
            refused.summary().contains("denylisted"),
            "{}",
            refused.summary()
        );
        let failed = ToolResult::failed(
            c.clone(),
            Permission::Execute,
            "cargo test exited 101",
            ToolOutput::new("3 tests failed"),
        );
        assert!(
            failed.summary().contains("exited 101"),
            "{}",
            failed.summary()
        );
        assert_ne!(refused.summary(), failed.summary());
    }

    #[test]
    fn a_result_round_trips_through_json_for_the_session_file() {
        let result = ToolResult::completed(
            call(r#"{"path":"a.rs"}"#),
            Permission::ReadOnly,
            ToolOutput::new("fn main() {}"),
            12,
        );
        let json = serde_json::to_string(&result).expect("serialisable");
        let back: ToolResult = serde_json::from_str(&json).expect("deserialisable");
        assert_eq!(back, result);
        assert!(
            json.contains("\"read-only\""),
            "the class is on the wire: {json}"
        );
        assert!(json.contains("\"status\":\"completed\""), "{json}");
    }
}
