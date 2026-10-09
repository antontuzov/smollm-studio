//! Chat transcripts that outlive the window.
//!
//! A conversation is the only data in this app a user cannot recreate, so every
//! one is a JSON file under [`AppPaths::sessions_dir`] saved the same
//! write-then-rename way [`crate::Settings`] are. Nothing here knows about
//! Tauri or React: the desktop layer hands turns over, this decides what
//! survives a quit.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::chat::{ChatMessage, Role};
use crate::error::{AppError, AppResult};
use crate::paths::AppPaths;

pub const UNTITLED: &str = "New chat";

/// One stored turn of a conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredTurn {
    pub role: Role,
    pub content: String,
    pub created_at_ms: u64,
    /// Why an assistant turn failed, kept so the transcript explains itself
    /// instead of showing an empty bubble.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Measured decode rate for this answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_second: Option<f64>,
}

impl StoredTurn {
    #[must_use]
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            created_at_ms: now_ms(),
            error: None,
            tokens_per_second: None,
        }
    }
}

/// A conversation as stored on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub model_id: Option<String>,
    pub system_prompt: String,
    pub turns: Vec<StoredTurn>,
}

impl ChatSession {
    #[must_use]
    pub fn new(system_prompt: impl Into<String>) -> Self {
        let now = now_ms();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: UNTITLED.to_string(),
            created_at_ms: now,
            updated_at_ms: now,
            model_id: None,
            system_prompt: system_prompt.into(),
            turns: Vec::new(),
        }
    }

    /// Append a turn and take the edit, so callers can add stats afterwards.
    pub fn push(&mut self, turn: StoredTurn) {
        if self.title == UNTITLED && turn.role == Role::User {
            self.title = auto_title(&turn.content);
        }
        self.turns.push(turn);
        self.updated_at_ms = now_ms();
    }

    pub fn record_user(&mut self, content: &str) {
        self.push(StoredTurn::new(Role::User, content));
    }

    pub fn record_assistant(&mut self, content: &str, tokens_per_second: Option<f64>) {
        self.push(StoredTurn {
            tokens_per_second,
            ..StoredTurn::new(Role::Assistant, content)
        });
    }

    /// Record a failed answer. The turn keeps whatever text did arrive.
    pub fn record_failure(&mut self, content: &str, error: &str) {
        self.push(StoredTurn {
            error: Some(error.to_string()),
            ..StoredTurn::new(Role::Assistant, content)
        });
    }

    /// Name a transcript after its first question and date it as touched.
    ///
    /// [`Self::push`] already does both a turn at a time, but the interface
    /// layer hands a whole transcript back at each turn boundary, so this
    /// applies the same rules to what arrives in one piece.
    pub fn prepare_for_save(&mut self) {
        self.apply_auto_title();
        self.updated_at_ms = now_ms();
    }

    /// Name an untitled transcript after its first question.
    ///
    /// [`Self::push`] already does this, but a session handed over whole by the
    /// interface layer has been assembled in one step, so saving it needs the
    /// same rule applied once. Without it every restored chat reads "New chat".
    pub fn apply_auto_title(&mut self) {
        let untitled = self.title.trim().is_empty() || self.title == UNTITLED;
        if !untitled {
            return;
        }
        if let Some(question) = self
            .turns
            .iter()
            .find(|turn| turn.role == Role::User)
            .map(|turn| turn.content.clone())
        {
            self.title = auto_title(&question);
        }
    }

    pub fn rename(&mut self, title: &str) {
        let trimmed = title.trim();
        if !trimmed.is_empty() {
            self.title = trimmed.to_string();
            self.updated_at_ms = now_ms();
        }
    }

    /// History for the next request: everything except a turn that produced
    /// nothing, which would only make the model repeat itself.
    #[must_use]
    pub fn messages(&self) -> Vec<ChatMessage> {
        self.turns
            .iter()
            .filter(|turn| !turn.content.trim().is_empty())
            .map(|turn| ChatMessage {
                role: turn.role,
                content: turn.content.clone(),
            })
            .collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    /// Drop turns after `index`, the same way editing a message forks a chat.
    pub fn truncate_to(&mut self, index: usize) {
        self.turns.truncate(index);
        self.updated_at_ms = now_ms();
    }

    #[must_use]
    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            turn_count: self.turns.len(),
            model_id: self.model_id.clone(),
            preview: self
                .turns
                .iter()
                .find(|turn| turn.role == Role::User)
                .map(|turn| clip(&turn.content, 140))
                .unwrap_or_default(),
        }
    }

    /// A readable transcript: what a user expects to keep when they export.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("# {}\n\n", self.title));
        out.push_str(&format!("- Started: {}\n", format_utc(self.created_at_ms)));
        out.push_str(&format!("- Updated: {}\n", format_utc(self.updated_at_ms)));
        if let Some(model) = &self.model_id {
            out.push_str(&format!("- Model: `{model}`\n"));
        }
        if !self.system_prompt.trim().is_empty() {
            out.push_str(&format!("- System prompt: {}\n", self.system_prompt));
        }
        out.push_str(&format!("- Turns: {}\n", self.turns.len()));
        for turn in &self.turns {
            out.push_str(&format!("\n## {}\n\n", role_label(turn.role)));
            out.push_str(turn.content.trim());
            out.push('\n');
            if let Some(rate) = turn.tokens_per_second {
                out.push_str(&format!("\n> {rate:.1} tokens/s\n"));
            }
            if let Some(error) = &turn.error {
                out.push_str(&format!("\n> Failed: {error}\n"));
            }
        }
        out
    }

    pub fn to_json(&self) -> AppResult<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

/// What a list row needs, without shipping whole transcripts over the IPC
/// boundary for every row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub turn_count: usize,
    pub model_id: Option<String>,
    pub preview: String,
}

/// A search match, with the line that matched rather than the first message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHit {
    #[serde(flatten)]
    pub session: SessionSummary,
    pub snippet: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIndex {
    pub sessions: Vec<SessionSummary>,
    /// Files that exist but do not parse. Reported so a damaged transcript is
    /// visible in the UI instead of silently missing from the list.
    pub unreadable: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Markdown,
    Json,
}

/// Reads and writes the transcript files.
#[derive(Debug, Clone)]
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn from_paths(paths: &AppPaths) -> Self {
        Self::new(paths.sessions_dir.clone())
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn save(&self, session: &ChatSession) -> AppResult<()> {
        let path = self.path_for(&session.id)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(session)?;
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, &text)?;
        std::fs::rename(&temp, &path)?;
        Ok(())
    }

    pub fn load(&self, id: &str) -> AppResult<ChatSession> {
        let path = self.path_for(id)?;
        let text = std::fs::read_to_string(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                AppError::InvalidRequest(format!("no conversation saved with id {id}"))
            } else {
                AppError::from(source)
            }
        })?;
        serde_json::from_str(&text).map_err(|source| {
            AppError::Config(format!(
                "{} is not a readable conversation: {source}",
                path.display()
            ))
        })
    }

    pub fn delete(&self, id: &str) -> AppResult<()> {
        let path = self.path_for(id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(AppError::from(source)),
        }
    }

    pub fn index(&self) -> AppResult<SessionIndex> {
        let mut out = SessionIndex::default();
        for session in self.all(&mut out.unreadable)? {
            out.sessions.push(session.summary());
        }
        out.sessions
            .sort_by_key(|session| Reverse(session.updated_at_ms));
        Ok(out)
    }

    /// Case-insensitive match over titles and every turn's text.
    pub fn search(&self, query: &str) -> AppResult<Vec<SessionHit>> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let mut ignored = Vec::new();
        let mut hits = Vec::new();
        for session in self.all(&mut ignored)? {
            let matched = session
                .turns
                .iter()
                .find(|turn| turn.content.to_lowercase().contains(&needle));
            let snippet = match matched {
                Some(turn) => clip_around(&turn.content, &needle, 90),
                None => {
                    if session.title.to_lowercase().contains(&needle) {
                        clip(&session.title, 90)
                    } else {
                        continue;
                    }
                }
            };
            hits.push(SessionHit {
                session: session.summary(),
                snippet,
            });
        }
        hits.sort_by_key(|hit| Reverse(hit.session.updated_at_ms));
        Ok(hits)
    }

    /// Most recently used conversation, which is the one to reopen at launch.
    pub fn newest(&self) -> AppResult<Option<ChatSession>> {
        let mut ignored = Vec::new();
        let mut best: Option<ChatSession> = None;
        for session in self.all(&mut ignored)? {
            let newer = match best.as_ref() {
                Some(current) => session.updated_at_ms > current.updated_at_ms,
                None => true,
            };
            if newer {
                best = Some(session);
            }
        }
        Ok(best)
    }

    /// Write a transcript out, in the format the user picked.
    pub fn export(&self, id: &str, dest: &Path, format: ExportFormat) -> AppResult<PathBuf> {
        let session = self.load(id)?;
        let text = match format {
            ExportFormat::Markdown => session.to_markdown(),
            ExportFormat::Json => session.to_json()?,
        };
        let path = with_extension(dest, format);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, text)?;
        Ok(path)
    }

    /// Every readable transcript; unparseable files are pushed into `unreadable`
    /// rather than failing the whole listing.
    fn all(&self, unreadable: &mut Vec<String>) -> AppResult<Vec<ChatSession>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut sessions = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            match serde_json::from_str::<ChatSession>(&text) {
                Ok(session) => sessions.push(session),
                Err(source) => {
                    unreadable.push(format!("{}: {source}", path.display()));
                }
            }
        }
        Ok(sessions)
    }

    fn path_for(&self, id: &str) -> AppResult<PathBuf> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(AppError::InvalidRequest(format!(
                "{id:?} cannot be used as a conversation name"
            )));
        }
        Ok(self.root.join(format!("{id}.json")))
    }
}

fn with_extension(path: &Path, format: ExportFormat) -> PathBuf {
    let wanted = match format {
        ExportFormat::Markdown => "md",
        ExportFormat::Json => "json",
    };
    match path.extension() {
        Some(ext) if ext == wanted => path.to_path_buf(),
        _ => path.with_extension(wanted),
    }
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::User => "You",
        Role::Assistant => "Assistant",
        Role::System => "System",
    }
}

/// Title a conversation from its first line, on a character boundary.
#[must_use]
pub fn auto_title(text: &str) -> String {
    let first_line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let collapsed = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return UNTITLED.to_string();
    }
    clip(&collapsed, 60)
}

fn clip(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.trim().to_string();
    }
    let mut out: String = chars[..max_chars.saturating_sub(1)].iter().collect();
    out.push('…');
    out
}

/// A window of text centred on the first case-insensitive hit.
fn clip_around(text: &str, needle_lower: &str, radius: usize) -> String {
    let lower = text.to_lowercase();
    let Some(byte_at) = lower.find(needle_lower) else {
        return clip(text, radius);
    };
    let start = text[..byte_at].chars().count().saturating_sub(radius / 2);
    let chars: Vec<char> = text.chars().collect();
    let end = (start + radius).min(chars.len());
    let mut out: String = chars[start..end].iter().collect();
    if start > 0 {
        out = format!("…{out}");
    }
    if end < chars.len() {
        out.push('…');
    }
    out
}

#[must_use]
pub fn now_ms() -> u64 {
    std::time::UNIX_EPOCH
        .elapsed()
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or_default()
}

/// Civil date from Unix milliseconds. A date crate is a large dependency for a
/// stamp in an exported file.
#[must_use]
pub fn format_utc(ms: u64) -> String {
    let seconds = (ms / 1000) as i64;
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        time / 3_600,
        (time % 3_600) / 60
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let shifted = days_since_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_part + 2) / 5 + 1) as u32;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with(question: &str, answer: &str) -> ChatSession {
        let mut session = ChatSession::new("Be brief.");
        session.record_user(question);
        session.record_assistant(answer, Some(41.5));
        session
    }

    #[test]
    fn a_transcript_round_trips_through_disk() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SessionStore::new(dir.path().join("sessions"));
        let session = session_with("what is a gguf?", "A tensor container.");

        store.save(&session).expect("saved");
        let loaded = store.load(&session.id).expect("loaded");

        assert_eq!(loaded, session);
        assert_eq!(loaded.system_prompt, "Be brief.");
        assert_eq!(
            loaded.turns[1].tokens_per_second,
            Some(41.5),
            "measured speed belongs to the answer it measured"
        );
        assert!(
            !store
                .path_for(&session.id)
                .expect("path")
                .with_extension("json.tmp")
                .exists(),
            "the temp file is renamed away, not left behind"
        );
    }

    #[test]
    fn the_first_question_names_the_conversation() {
        let mut session = ChatSession::new("");
        assert_eq!(session.title, UNTITLED);

        session.record_user("  Explain\nresumable downloads\nin one line ");
        assert_eq!(session.title, "Explain");

        session.rename("  Kept  ");
        assert_eq!(session.title, "Kept");
        // An empty rename must not wipe a real title.
        session.rename("   ");
        assert_eq!(session.title, "Kept");
    }

    #[test]
    fn a_transcript_handed_over_in_one_piece_is_titled_and_dated() {
        // The interface layer assembles turns instead of pushing them one at a
        // time, so saving has to apply the same two rules in a single step.
        let mut session = ChatSession::new("");
        session.turns = vec![
            StoredTurn::new(Role::User, "What is a gguf?"),
            StoredTurn::new(Role::Assistant, "A file format."),
        ];
        session.updated_at_ms = 1;

        session.prepare_for_save();

        assert_eq!(session.title, "What is a gguf?");
        assert!(session.updated_at_ms > 1);
        // A title the user chose is never overwritten by the first question.
        session.rename("Notes");
        session.prepare_for_save();
        assert_eq!(session.title, "Notes");
    }

    #[test]
    fn a_long_first_line_is_cut_on_a_character_boundary() {
        let title = auto_title(&"é".repeat(200));
        assert!(title.chars().count() <= 60, "{title}");
        assert!(title.ends_with('…'));
    }

    #[test]
    fn the_index_is_newest_first_and_carries_a_preview() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SessionStore::new(dir.path().join("sessions"));

        let mut older = session_with("older question", "answer");
        older.updated_at_ms = 1_000;
        let mut newer = session_with("newer question", "answer");
        newer.updated_at_ms = 2_000;
        store.save(&newer).expect("saved");
        store.save(&older).expect("saved");

        let index = store.index().expect("index");
        assert_eq!(index.unreadable, Vec::<String>::new());
        assert_eq!(index.sessions.len(), 2);
        assert_eq!(index.sessions[0].id, newer.id);
        assert_eq!(index.sessions[0].preview, "newer question");
        assert_eq!(index.sessions[0].turn_count, 2);
    }

    #[test]
    fn a_damaged_transcript_is_reported_rather_than_swallowed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("sessions");
        let store = SessionStore::new(root.clone());
        store.save(&session_with("kept", "yes")).expect("saved");
        std::fs::write(root.join("broken.json"), "{ not json").expect("written");

        let index = store.index().expect("the good ones still list");
        assert_eq!(index.sessions.len(), 1);
        assert_eq!(index.unreadable.len(), 1, "the bad one is named");
        assert!(index.unreadable[0].contains("broken.json"));
    }

    #[test]
    fn search_matches_answers_and_returns_the_line_it_hit() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SessionStore::new(dir.path().join("sessions"));
        store
            .save(&session_with(
                "why is my download slow?",
                "Resume uses a Range header, so it continues where it stopped.",
            ))
            .expect("saved");
        store
            .save(&session_with("unrelated", "nothing to see"))
            .expect("saved");

        let hits = store.search("range header").expect("searched");
        assert_eq!(hits.len(), 1);
        assert!(
            hits[0].snippet.contains("Range header"),
            "{}",
            hits[0].snippet
        );
        assert!(store
            .search("   ")
            .expect("empty is not a search")
            .is_empty());
    }

    #[test]
    fn an_id_that_could_escape_the_folder_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SessionStore::new(dir.path().join("sessions"));
        for id in ["../../settings", "a/b", "", &"x".repeat(65)] {
            let error = store.path_for(id).expect_err("refused");
            assert!(
                matches!(error, AppError::InvalidRequest(_)),
                "{id:?} was accepted"
            );
        }
    }

    #[test]
    fn deleting_is_idempotent_and_only_removes_that_transcript() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SessionStore::new(dir.path().join("sessions"));
        let first = session_with("first", "a");
        let second = session_with("second", "b");
        store.save(&first).expect("saved");
        store.save(&second).expect("saved");

        store.delete(&first.id).expect("deleted");
        store.delete(&first.id).expect("deleting twice is fine");
        assert!(store.load(&first.id).is_err());
        assert!(store.load(&second.id).is_ok());
    }

    #[test]
    fn export_writes_the_format_asked_for_under_the_name_given() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SessionStore::new(dir.path().join("sessions"));
        let session = session_with("what is a gguf?", "A tensor container.");
        store.save(&session).expect("saved");

        let target = dir.path().join("out").join("chat");
        let written = store
            .export(&session.id, &target, ExportFormat::Markdown)
            .expect("exported");
        assert_eq!(written.extension().and_then(|e| e.to_str()), Some("md"));
        let text = std::fs::read_to_string(&written).expect("readable");
        assert!(text.starts_with("# what is a gguf?"), "{text}");
        assert!(text.contains("## You"), "{text}");
        assert!(text.contains("41.5 tokens/s"), "{text}");
        assert!(text.contains("2026-"), "{text}");

        let json = store
            .export(&session.id, &target, ExportFormat::Json)
            .expect("exported");
        assert_eq!(json.extension().and_then(|e| e.to_str()), Some("json"));
        serde_json::from_str::<ChatSession>(&std::fs::read_to_string(&json).expect("readable"))
            .expect("an exported transcript reloads");
    }

    #[test]
    fn a_failed_answer_keeps_what_it_managed_to_say() {
        let mut session = ChatSession::new("");
        session.record_user("tell me something");
        session.record_failure("Here begins the", "engine died mid-stream");
        assert_eq!(session.messages().len(), 2, "partial text still replays");
        assert_eq!(
            session.turns[1].error.as_deref(),
            Some("engine died mid-stream")
        );
    }

    #[test]
    fn empty_turns_never_replay_to_the_model() {
        let mut session = ChatSession::new("");
        session.record_user("hello");
        session.record_assistant("   ", None);
        assert_eq!(session.messages().len(), 1);
    }

    #[test]
    fn utc_stamps_read_as_dates() {
        // 2026-10-08T13:44:00Z
        let ms = 1_791_417_600_000 + 13 * 3_600_000 + 44 * 60_000;
        assert_eq!(format_utc(ms), "2026-10-08 13:44 UTC");
        assert_eq!(format_utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_utc(2_147_483_648_000), "2038-01-19 03:14 UTC");
    }
}
