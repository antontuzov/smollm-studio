//! Plain-text output: aligned tables and an in-place download progress line.

use std::io::{IsTerminal, Write};

/// Format decimal megabytes the way the catalog states them.
pub fn size_label(size_mb: u64) -> String {
    if size_mb >= 1_000 {
        format!("{:.1} GB", size_mb as f64 / 1_000.0)
    } else {
        format!("{size_mb} MB")
    }
}

/// Render `header` plus `rows` as a left-aligned ASCII table.
pub fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let columns = header.len();
    let mut widths = vec![0usize; columns];
    for (index, title) in header.iter().enumerate() {
        widths[index] = title.len();
    }
    for row in rows {
        for (index, cell) in row.iter().take(columns).enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    out.push_str(&row_line(header, &widths));
    out.push('\n');
    out.push_str(
        &widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>()
            .join("  "),
    );
    out.push('\n');
    for row in rows {
        let cells: Vec<&str> = header
            .iter()
            .enumerate()
            .map(|(index, _)| row.get(index).map(String::as_str).unwrap_or(""))
            .collect();
        out.push_str(&row_line(&cells, &widths));
        out.push('\n');
    }
    out
}

fn row_line(cells: &[&str], widths: &[usize]) -> String {
    cells
        .iter()
        .enumerate()
        .map(|(index, cell)| format!("{cell:<width$}", width = widths[index]))
        .collect::<Vec<_>>()
        .join("  ")
        .trim_end()
        .to_string()
}

/// Redraw the download progress line in place.
pub fn progress_line(percent: f64, downloaded: u64, total: u64, bytes_per_second: f64) {
    let bar_width = 28;
    let filled = ((percent / 100.0).clamp(0.0, 1.0) * bar_width as f64).round() as usize;
    let bar = format!("{}{}", "#".repeat(filled), "-".repeat(bar_width - filled));
    let rate = if bytes_per_second > 0.0 {
        format!("{:.1} MB/s", bytes_per_second / 1_000_000.0)
    } else {
        "starting".to_string()
    };
    write(&format!(
        "\r  [{bar}] {percent:5.1}%  {} / {}  {rate}",
        human_size(downloaded),
        human_size(total)
    ));
}

/// Render `label: value` pairs without a table frame.
pub fn key_values(pairs: &[(&str, String)]) -> String {
    let width = pairs
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    pairs
        .iter()
        .map(|(label, value)| format!("{label:<width$}  {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn human_size(bytes: u64) -> String {
    const GB: f64 = 1_000_000_000.0;
    const MB: f64 = 1_000_000.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.2} GB", value / GB)
    } else {
        format!("{:.1} MB", value / MB)
    }
}

/// Close a progress line so later output starts on a fresh line.
pub fn finish_line() {
    write("\n");
}

fn write(text: &str) {
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

pub fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
}
