//! Daily-rotating text log files for `logs/`.
//!
//! A `tracing` layer, so the app, engine, download and server streams all land
//! in the same file. One file per UTC day is the rotation policy: simple, no
//! background thread, and nothing to configure.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Layer appending one line per event to `logs/smollm-<date>.log`.
pub struct FileLogLayer {
    dir: Arc<Mutex<PathBuf>>,
}

impl FileLogLayer {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir: Arc::new(Mutex::new(dir)),
        }
    }

    fn path_for(&self, day: &str) -> Option<PathBuf> {
        let dir = self.dir.lock().ok()?.clone();
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join(format!("smollm-{day}.log")))
    }
}

impl<S: Subscriber> Layer<S> for FileLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        // Debug and trace lines would drown the file; keep info and above.
        if *metadata.level() > Level::INFO {
            return;
        }
        let mut visitor = LineVisitor::default();
        event.record(&mut visitor);

        let now = std::time::SystemTime::now();
        let day = format_day(&now);
        let Some(path) = self.path_for(&day) else {
            return;
        };
        let line = format!(
            "{} {:<5} {}: {} {}\n",
            format_timestamp(&now),
            metadata.level().as_str(),
            metadata.target(),
            visitor.message,
            visitor
                .extras
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(" ")
                .trim(),
        );
        let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
            return;
        };
        // A log file that cannot be written must never take the app down.
        let _ = file.write_all(line.as_bytes());
    }
}

#[derive(Default)]
struct LineVisitor {
    message: String,
    extras: Vec<(String, String)>,
}

impl Visit for LineVisitor {
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
        self.store(field.name(), format!("{value:.3}"));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.store(field.name(), format!("{value:?}"));
    }
}

impl LineVisitor {
    fn store(&mut self, name: &str, value: String) {
        if name == "message" {
            self.message = value;
        } else {
            self.extras.push((name.to_string(), value));
        }
    }
}

fn seconds_since_epoch(at: &std::time::SystemTime) -> i64 {
    at.duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// `YYYY-MM-DD` from epoch seconds, without pulling in a date crate.
fn format_day(at: &std::time::SystemTime) -> String {
    let (year, month, day) = civil_from_days(seconds_since_epoch(at) / 86_400);
    format!("{year:04}-{month:02}-{day:02}")
}

fn format_timestamp(at: &std::time::SystemTime) -> String {
    let seconds = seconds_since_epoch(at);
    let millis = at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_millis())
        .unwrap_or_default();
    let (year, month, day) = civil_from_days(seconds / 86_400);
    let time = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{millis:03}",
        time / 3_600,
        (time / 60) % 60,
        time % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since the Unix epoch to UTC Y/M/D.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn at(seconds: u64) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn day_labels_rotate_on_midnight_utc() {
        assert_eq!(format_day(&at(0)), "1970-01-01");
        assert_eq!(format_day(&at(86_399)), "1970-01-01");
        assert_eq!(format_day(&at(86_400)), "1970-01-02");
        // 2026-10-04, the release window this app targets.
        assert_eq!(format_day(&at(1_791_072_000)), "2026-10-04");
    }

    #[test]
    fn leap_years_are_handled() {
        // 2024-02-29 exists; the next day is March 1.
        assert_eq!(format_day(&at(1_709_164_800)), "2024-02-29");
        assert_eq!(format_day(&at(1_709_251_200)), "2024-03-01");
    }

    #[test]
    fn timestamps_carry_time_and_millis() {
        let moment = at(3_723) + Duration::from_millis(456);
        assert_eq!(format_timestamp(&moment), "1970-01-01 01:02:03.456");
    }

    #[test]
    fn files_land_in_the_requested_directory() {
        let dir =
            std::env::temp_dir().join(format!("smollm-logs-{}", uuid::Uuid::new_v4().simple()));
        let layer = FileLogLayer::new(dir.clone());
        let path = layer.path_for("1970-01-01").expect("path");
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string());
        assert_eq!(name, Some("smollm-1970-01-01.log".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }
}
