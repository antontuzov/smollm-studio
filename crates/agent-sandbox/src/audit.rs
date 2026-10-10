//! An append-only record of what the agent asked to do and what was said.
//!
//! The audit log is the answer to the question a user asks after a run: *what
//! actually happened*. Every decision the policy made is one JSON line, in the
//! order it was reached, with the reason attached — including the refusals,
//! which are the part worth reading afterwards.
//!
//! Two properties are load-bearing:
//!
//! - **Append-only.** The file is opened with `append(true)` and never rewritten,
//!   so a run cannot lose the record of itself, and a crash leaves the lines it
//!   did write.
//! - **Redacted on the way in.** A tool's subject is a path or a command line,
//!   and command lines are where a model puts a token it found in a `.env` file.
//!   Redaction happens here rather than at every call site so that forgetting it
//!   at one of them is not possible.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::permission::Permission;
use crate::policy::Decision;
use crate::redact::redact;

/// Where a run puts its audit log when nothing says otherwise.
pub const AUDIT_FILE: &str = ".smoll/audit.jsonl";

/// One line of the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Milliseconds since the Unix epoch, so a reader can order runs as well as
    /// lines.
    pub at_ms: u64,
    pub tool: String,
    pub permission: Permission,
    /// Flattened, so a line reads `…"decision":"block","reason":"…"` and a
    /// reader can `jq` it without reaching into a nested object.
    #[serde(flatten)]
    pub decision: Decision,
    /// The path or command line the action was about, with any secret in it
    /// already removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// How long the action took once it ran; absent when it never ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Bytes of output the action produced, capped or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<usize>,
}

impl Entry {
    pub fn new(
        tool: impl Into<String>,
        permission: Permission,
        decision: Decision,
        subject: Option<String>,
    ) -> Self {
        let subject = subject.map(|text| redact(&text).0);
        Self {
            at_ms: now_ms(),
            tool: tool.into(),
            permission,
            decision,
            subject,
            duration_ms: None,
            output_bytes: None,
        }
    }

    pub fn with_timing(mut self, duration_ms: u64, output_bytes: usize) -> Self {
        self.duration_ms = Some(duration_ms);
        self.output_bytes = Some(output_bytes);
        self
    }

    /// Whether this line records something that actually happened, as opposed to
    /// a refusal or a question that is still open.
    pub fn ran(&self) -> bool {
        matches!(self.decision, Decision::Allow | Decision::Warn { .. })
    }
}

/// The file itself, or the fact that this run was told not to keep one.
#[derive(Debug, Clone)]
pub struct AuditLog {
    path: Option<PathBuf>,
}

impl AuditLog {
    /// Open (creating if needed) the log at `path`.
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Touch it so an empty run still leaves a file a reader can find.
        OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { path: Some(path) })
    }

    /// `<workspace>/.smoll/audit.jsonl`.
    pub fn in_workspace(root: impl AsRef<Path>) -> io::Result<Self> {
        Self::open(root.as_ref().join(AUDIT_FILE))
    }

    /// A log that records nothing, for `audit_log = false`.
    pub fn disabled() -> Self {
        Self { path: None }
    }

    pub fn is_enabled(&self) -> bool {
        self.path.is_some()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Append one line. A failure to write the log is returned rather than
    /// swallowed, but a caller should not abort a run over it — the action has
    /// already happened by then.
    pub fn record(&self, entry: &Entry) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let line = serde_json::to_string(entry)
            .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))?;
        let mut file = OpenOptions::new().append(true).open(path)?;
        writeln!(file, "{line}")?;
        file.flush()
    }

    /// Read the whole log back, for `smoll rollback` and for a human asking what
    /// a run did. A line that does not parse is skipped rather than fatal: a log
    /// that cannot be read at all is worse than one missing an entry.
    pub fn read(&self) -> io::Result<Vec<Entry>> {
        let Some(path) = &self.path else {
            return Ok(Vec::new());
        };
        read_entries(path)
    }
}

/// Read a log file that is not necessarily open for writing.
pub fn read_entries(path: impl AsRef<Path>) -> io::Result<Vec<Entry>> {
    let path = path.as_ref();
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(source),
    };
    let mut entries = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<Entry>(&line) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_records_the_refusal_as_faithfully_as_the_action() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let log = AuditLog::open(dir.path().join(".smoll/audit.jsonl")).expect("opens");
        assert!(log.is_enabled());
        let entry = Entry::new(
            "run_command",
            Permission::Destructive,
            Decision::Block {
                reason: "`sudo` is not a program this agent will run".to_owned(),
            },
            Some("sudo rm -rf /".to_owned()),
        );
        log.record(&entry).expect("appends");
        let back = log.read().expect("reads");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0], entry);
        assert!(!back[0].ran(), "nothing ran");
    }

    #[test]
    fn a_second_record_appends_rather_than_rewrites() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("audit.jsonl");
        let log = AuditLog::open(&path).expect("opens");
        for name in ["read_file", "write_file"] {
            log.record(&Entry::new(
                name,
                Permission::ReadOnly,
                Decision::Allow,
                None,
            ))
            .expect("appends");
        }
        let text = fs::read_to_string(&path).expect("readable");
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(text.starts_with('{'), "the file is JSON lines");
        assert_eq!(read_entries(&path).expect("parses").len(), 2);
    }

    #[test]
    fn a_token_in_a_command_line_never_reaches_the_file() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let log = AuditLog::open(dir.path().join("audit.jsonl")).expect("opens");
        log.record(&Entry::new(
            "run_command",
            Permission::Execute,
            Decision::Allow,
            Some("gh api -H \"Authorization: Bearer sk-abcdef0123456789abcdef\"".to_owned()),
        ))
        .expect("appends");
        let text = fs::read_to_string(log.path().expect("a path")).expect("readable");
        assert!(
            !text.contains("abcdef0123456789"),
            "the raw secret is in the audit log: {text}"
        );
        assert!(
            text.contains("\u{ab}redacted\u{bb}"),
            "the mask is what landed instead: {text}"
        );
    }

    #[test]
    fn a_disabled_log_writes_nothing_and_reads_back_empty() {
        let log = AuditLog::disabled();
        assert!(!log.is_enabled());
        log.record(&Entry::new(
            "read_file",
            Permission::ReadOnly,
            Decision::Allow,
            None,
        ))
        .expect("a no-op is not an error");
        assert!(log.read().expect("empty").is_empty());
        assert_eq!(log.path(), None);
    }

    #[test]
    fn opening_a_log_creates_the_directory_it_needs() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let log = AuditLog::in_workspace(dir.path()).expect("creates .smoll");
        assert!(dir.path().join(AUDIT_FILE).exists());
        assert!(log.read().expect("empty file").is_empty());
    }

    #[test]
    fn a_hand_written_line_parses() {
        let line = "{\"at_ms\":1,\"tool\":\"read_file\",\"permission\":\"read-only\",\"decision\":\"allow\"}";
        let entry: Entry = serde_json::from_str(line).expect("a written-out line parses");
        assert_eq!(entry.decision, Decision::Allow);
    }

    #[test]
    fn a_line_that_does_not_parse_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("audit.jsonl");
        fs::write(
            &path,
            "{\"at_ms\":1,\"tool\":\"read_file\",\"permission\":\"read-only\",\"decision\":\"allow\"}\nnot json\n\n",
        )
        .expect("writable");
        let entries = read_entries(&path).expect("reads what it can");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tool, "read_file");
    }

    #[test]
    fn timing_is_only_recorded_for_something_that_ran() {
        let entry = Entry::new(
            "search_text",
            Permission::ReadOnly,
            Decision::Allow,
            Some("src".to_owned()),
        )
        .with_timing(42, 1024);
        assert!(entry.ran());
        assert_eq!(entry.duration_ms, Some(42));
        assert_eq!(entry.output_bytes, Some(1024));
    }
}
