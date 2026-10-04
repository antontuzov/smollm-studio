//! In-memory log store backing the Logs page.
//!
//! A `tracing` layer keeps the last N events in a ring buffer so the UI can show
//! app/engine/download/server logs without touching the filesystem. Log files are
//! still written by the app layer when the user exports diagnostics.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::system::{LogEntry, LogFilter, LogStream};

pub const DEFAULT_CAPACITY: usize = 2_000;

/// Ring buffer of recent log entries, cheap to clone and share.
#[derive(Debug, Clone)]
pub struct LogStore {
    inner: Arc<Mutex<VecDeque<LogEntry>>>,
    capacity: usize,
}

impl LogStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(capacity.min(128)))),
            capacity: capacity.max(1),
        }
    }

    pub fn push(&self, entry: LogEntry) {
        if let Ok(mut buffer) = self.inner.lock() {
            if buffer.len() == self.capacity {
                buffer.pop_front();
            }
            buffer.push_back(entry);
        }
    }

    pub fn snapshot(&self) -> Vec<LogEntry> {
        self.inner
            .lock()
            .map(|buffer| buffer.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn query(&self, filter: &LogFilter) -> Vec<LogEntry> {
        let mut entries: Vec<LogEntry> = self
            .snapshot()
            .into_iter()
            .filter(|entry| {
                filter.levels.is_empty()
                    || filter
                        .levels
                        .iter()
                        .any(|level| level.eq_ignore_ascii_case(&entry.level))
            })
            .filter(|entry| filter.stream.is_none() || filter.stream == Some(entry.stream))
            .collect();

        if let Some(needle) = filter.contains.as_deref().map(str::to_lowercase) {
            entries.retain(|entry| {
                entry.message.to_lowercase().contains(&needle)
                    || entry.target.to_lowercase().contains(&needle)
            });
        }

        let limit = filter.limit.unwrap_or(entries.len());
        if entries.len() > limit {
            // Newest last: keep the tail the user actually wants to read.
            entries.drain(0..entries.len() - limit);
        }
        entries
    }

    pub fn clear(&self) {
        if let Ok(mut buffer) = self.inner.lock() {
            buffer.clear();
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map_or(0, |buffer| buffer.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for LogStore {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

static GLOBAL_STORE: OnceLock<LogStore> = OnceLock::new();

/// Process-wide store used by [`CaptureLayer`].
pub fn global_store() -> &'static LogStore {
    GLOBAL_STORE.get_or_init(LogStore::default)
}

/// `tracing` layer that mirrors events into a [`LogStore`].
#[derive(Debug, Clone)]
pub struct CaptureLayer {
    store: Arc<Mutex<VecDeque<LogEntry>>>,
    capacity: usize,
}

impl CaptureLayer {
    pub fn new(store: &LogStore) -> Self {
        Self {
            store: Arc::clone(&store.inner),
            capacity: store.capacity,
        }
    }

    /// Layer writing into [`global_store`].
    pub fn global() -> Self {
        let store = global_store();
        Self::new(store)
    }

    fn push(&self, entry: LogEntry) {
        if let Ok(mut buffer) = self.store.lock() {
            if buffer.len() == self.capacity {
                buffer.pop_front();
            }
            buffer.push_back(entry);
        }
    }
}

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        self.push(LogEntry {
            timestamp_ms: now_millis(),
            level: metadata.level().as_str().to_string(),
            target: metadata.target().to_string(),
            stream: stream_for(metadata.target()),
            message: visitor.message(),
        });
    }
}

fn stream_for(target: &str) -> LogStream {
    if target.starts_with("smollm_engine") {
        LogStream::Engine
    } else if target.starts_with("smollm_models") {
        LogStream::Download
    } else if target.starts_with("smollm_server") {
        LogStream::Server
    } else {
        LogStream::App
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[derive(Debug, Default)]
struct MessageVisitor {
    message: Option<String>,
    extras: Vec<(String, String)>,
}

impl MessageVisitor {
    fn message(self) -> String {
        let base = self.message.unwrap_or_default();
        if self.extras.is_empty() {
            return base;
        }
        let rendered = self
            .extras
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(" ");
        if base.is_empty() {
            rendered
        } else {
            format!("{base} {rendered}")
        }
    }
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.store(field.name(), value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.store(field.name(), value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.store(field.name(), value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.store(field.name(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        self.store(field.name(), rendered.trim_matches('"').to_string());
    }
}

impl MessageVisitor {
    fn store(&mut self, name: &str, value: String) {
        if name == "message" {
            self.message = Some(value);
        } else {
            self.extras.push((name.to_string(), value));
        }
    }
}

/// Render a filter as JSON-ish debug text, used by diagnostics export.
pub fn describe_filter(filter: &LogFilter) -> String {
    Value::String(format!(
        "levels={:?} stream={:?} contains={:?} limit={:?}",
        filter.levels, filter.stream, filter.contains, filter.limit
    ))
    .to_string()
}

/// Highest level a filter accepts, for log file headers.
pub fn level_order(level: &str) -> u8 {
    match level.to_ascii_uppercase().as_str() {
        "ERROR" => 4,
        "WARN" => 3,
        "INFO" => 2,
        "DEBUG" => 1,
        _ => 0,
    }
}

pub fn parse_level(level: &str) -> Option<Level> {
    match level.to_ascii_uppercase().as_str() {
        "ERROR" => Some(Level::ERROR),
        "WARN" => Some(Level::WARN),
        "INFO" => Some(Level::INFO),
        "DEBUG" => Some(Level::DEBUG),
        "TRACE" => Some(Level::TRACE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::info;

    #[test]
    fn capture_layer_records_messages() {
        use tracing_subscriber::layer::SubscriberExt;

        let store = LogStore::new(10);
        let filter = tracing_subscriber::filter::Targets::new()
            .with_target("smollm_engine", tracing::Level::TRACE);
        let subscriber =
            tracing_subscriber::registry().with(CaptureLayer::new(&store).with_filter(filter));
        let guard = tracing::subscriber::set_default(subscriber);
        info!(target: "smollm_engine::mock", request = "req-1", "generated {} tokens", 12);
        drop(guard);

        let entries = store.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].stream, LogStream::Engine);
        assert_eq!(entries[0].level, "INFO");
        assert!(entries[0].message.contains("generated 12 tokens"));
        assert!(entries[0].message.contains("request=req-1"));
    }

    #[test]
    fn ring_buffer_keeps_newest_entries() {
        let store = LogStore::new(3);
        for index in 0..5 {
            store.push(entry("app", format!("message {index}")));
        }
        let entries = store.snapshot();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].message, "message 2");
        assert_eq!(entries[2].message, "message 4");
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn filter_matches_level_stream_and_text() {
        let store = LogStore::new(10);
        store.push(entry("app", "starting up"));
        let mut download = entry("download", "Downloading Qwen");
        download.level = "WARN".to_string();
        store.push(download);

        let only_warn = store.query(&LogFilter {
            levels: vec!["warn".to_string()],
            ..Default::default()
        });
        assert_eq!(only_warn.len(), 1);

        let by_stream = store.query(&LogFilter {
            stream: Some(LogStream::Download),
            ..Default::default()
        });
        assert_eq!(by_stream.len(), 1);
        assert_eq!(by_stream[0].stream, LogStream::Download);

        let by_text = store.query(&LogFilter {
            contains: Some("qwen".to_string()),
            ..Default::default()
        });
        assert_eq!(by_text.len(), 1);

        let limited = store.query(&LogFilter {
            limit: Some(1),
            ..Default::default()
        });
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].message, "Downloading Qwen");
    }

    #[test]
    fn level_helpers_round_trip() {
        assert_eq!(parse_level("warn"), Some(Level::WARN));
        assert_eq!(parse_level("nope"), None);
        assert!(level_order("ERROR") > level_order("INFO"));
        assert!(describe_filter(&LogFilter::default()).contains("levels=[]"));
    }

    fn entry(stream: &str, message: impl Into<String>) -> LogEntry {
        let stream = match stream {
            "download" => LogStream::Download,
            "engine" => LogStream::Engine,
            "server" => LogStream::Server,
            _ => LogStream::App,
        };
        LogEntry {
            timestamp_ms: 1,
            level: "INFO".to_string(),
            target: "test".to_string(),
            stream,
            message: message.into(),
        }
    }
}
