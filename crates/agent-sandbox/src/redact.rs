//! Masking credentials before anything is logged, echoed or shown in a diff.
//!
//! A coding agent reads files and command output that land in a transcript it
//! also writes to disk. One accidentally recorded `HF_TOKEN=…` in a session
//! file is the failure this module exists to prevent, and it has to work
//! without a list anyone needs to keep current: so it looks for the shapes real
//! providers ship, plus any `key = value` whose key says it is secret.
//!
//! This is a mask, not a detection engine. It will miss a password in a field
//! named `pw`, and it never refuses to log something because it is unsure.

use serde::{Deserialize, Serialize};

/// What the mask changed, so a caller can report "3 secrets were masked"
/// instead of silently rewriting what the tool printed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redaction {
    pub masked: usize,
}

impl Redaction {
    pub fn nothing(&self) -> bool {
        self.masked == 0
    }

    fn add(&mut self, count: usize) {
        self.masked += count;
    }
}

/// Prefixes that identify a credential on sight.
const SHAPES: [&str; 8] = [
    "sk-ant-",     // Anthropic
    "github_pat_", // GitHub fine-grained
    "sk-",         // OpenAI and anything that copied it
    "hf_",         // Hugging Face
    "ghp_",        // GitHub personal access
    "glpat-",      // GitLab
    "AKIA",        // AWS access key id
    "xox",         // Slack
];

/// A key means its value is a secret if the normalised name contains one of
/// these, so `HF_API_TOKEN`, `api_key` and `DB_PASSWORD` all count.
const SECRET_KEY_PARTS: [&str; 6] = ["token", "secret", "password", "passwd", "api_key", "apikey"];

const MASK: &str = "«redacted»";

fn is_key_separator(character: char) -> bool {
    matches!(character, '=' | ' ' | '\t')
}

/// A value shorter than this is not a credential, and masking it would only
/// mangle ordinary text like `a=b`.
const MIN_SECRET_LEN: usize = 8;

/// Replace anything that looks like a credential. The first character of a
/// masked value survives: "is this the token I just pasted" is a question people
/// ask, and a fully opaque mask makes it unanswerable.
pub fn redact(text: &str) -> (String, Redaction) {
    let mut report = Redaction::default();
    let mut out = String::with_capacity(text.len());

    for line in text.split_inclusive('\n') {
        let (body, terminator) = split_line_ending(line);
        // `key=value` and `key = "value"` are both ways a secret arrives, and
        // the quoted form is the one a config file or a .env dump uses.
        if let Some(index) = body.find('=') {
            let (key, after) = body.split_at(index);
            let skip = after.len() - after.trim_start_matches(is_key_separator).len();
            let value_area = &after[skip..];
            let opener = value_area
                .chars()
                .next()
                .filter(|c| *c == '"' || *c == '\'');
            let (offset, value) = match opener {
                Some(quote) => {
                    let inner = &value_area[1..];
                    let end = inner.find(quote).unwrap_or(inner.len());
                    (1, &inner[..end])
                }
                None => (0, take_value(value_area)),
            };
            if value.len() >= MIN_SECRET_LEN && (key_says_secret(key) || has_shape(value)) {
                let value_at = index + skip + offset;
                out.push_str(&body[..value_at]);
                out.push_str(&mask(value));
                out.push_str(&body[value_at + value.len()..]);
                report.add(1);
                out.push_str(terminator);
                continue;
            }
        }
        let (masked_line, count) = mask_shapes(body);
        report.add(count);
        out.push_str(&masked_line);
        out.push_str(terminator);
    }

    (out, report)
}

/// Whether any known credential shape appears, for a caller that wants to warn
/// rather than rewrite. Returns the prefixes found, sorted and deduplicated.
pub fn find_secret_shapes(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for word in split_words(text) {
        if word.len() >= MIN_SECRET_LEN && SHAPES.iter().any(|shape| word.starts_with(shape)) {
            if let Some(shape) = SHAPES.iter().find(|shape| word.starts_with(**shape)) {
                found.push((*shape).to_owned());
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

fn mask_shapes(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut masked = 0usize;
    for chunk in text.split_inclusive(is_separator) {
        let trailing: usize = chunk
            .chars()
            .rev()
            .take_while(|c| is_separator(*c))
            .map(char::len_utf8)
            .sum();
        let leading: usize = chunk
            .chars()
            .take_while(|c| is_separator(*c))
            .map(char::len_utf8)
            .sum();
        let word_end = chunk.len() - trailing;
        // A chunk of nothing but separators has no word in it.
        if leading >= word_end {
            out.push_str(chunk);
            continue;
        }
        let (before, word_and_after) = chunk.split_at(leading);
        let word = &word_and_after[..word_end - leading];
        let separators = &chunk[word_end..];
        if word.len() >= MIN_SECRET_LEN && has_shape(word) {
            out.push_str(before);
            out.push_str(&mask(word));
            out.push_str(separators);
            masked += 1;
        } else {
            out.push_str(chunk);
        }
    }
    (out, masked)
}

fn is_separator(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            ',' | ';' | ':' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '='
        )
}

fn split_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(is_separator)
}

fn has_shape(value: &str) -> bool {
    SHAPES.iter().any(|shape| value.starts_with(shape))
}

fn key_says_secret(key: &str) -> bool {
    let normalised: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect::<String>()
        .to_ascii_lowercase();
    SECRET_KEY_PARTS
        .iter()
        .any(|part| normalised.contains(part))
}

/// The value up to the first character that cannot be part of one.
fn take_value(text: &str) -> &str {
    let end = text
        .find(|c: char| {
            c == ' '
                || c == '\t'
                || c == ','
                || c == ';'
                || c == '"'
                || c == '\''
                || c == '\n'
                || c == '\r'
        })
        .unwrap_or(text.len());
    &text[..end]
}

fn mask(value: &str) -> String {
    match value.chars().next() {
        Some(first) => format!("{first}{MASK}"),
        None => MASK.to_owned(),
    }
}

fn split_line_ending(line: &str) -> (&str, &str) {
    match line.strip_suffix('\n') {
        Some(body) => (body, "\n"),
        None => (line, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_that_admits_it_is_a_secret_gets_its_value_masked() {
        let (out, report) = redact("HF_API_TOKEN=hf_AbCdEfGhIjKlMnOpQrSt\n");
        assert!(out.starts_with("HF_API_TOKEN=h«redacted»"), "{out}");
        assert!(
            !out.contains("AbCdEfGhIjKlMnOpQrSt"),
            "the middle is gone: {out}"
        );
        assert_eq!(report.masked, 1);
    }

    #[test]
    fn a_bare_token_is_masked_without_a_key_at_all() {
        // This is the shape that appears in `Authorization: Bearer …` and in a
        // log line that quotes a curl command.
        let (out, report) =
            redact("curl -H \"Authorization: Bearer sk-ant-ABCDEFGHIJKLMNOPQRSTUVWXYZ\"");
        assert!(out.contains("s«redacted»\""), "{out}");
        assert!(!out.contains("BCDEFGHIJKLMNOPQRSTUVWXYZ"), "{out}");
        assert_eq!(report.masked, 1);
    }

    #[test]
    fn ordinary_settings_survive_untouched() {
        for line in [
            "approval_mode = \"approve-edits\"",
            "max_steps = 20",
            "context_length = 8192",
            "path = \"~/Models/qwen.gguf\"",
            "let key = compute(key_index);",
            "https://huggingface.co/org/model",
        ] {
            let (out, report) = redact(line);
            assert_eq!(out, line.to_owned(), "must not mangle: {line}");
            assert!(report.nothing(), "nothing masked in {line}");
        }
    }

    #[test]
    fn short_values_are_left_alone() {
        // `a=b` and `token=1` are not credentials, and masking them would make
        // config output unreadable.
        let (out, report) = redact("token=1\napi_key=no");
        assert_eq!(out, "token=1\napi_key=no");
        assert!(report.nothing());
    }

    #[test]
    fn several_secrets_in_one_blob_are_all_counted() {
        let text = "password = \"supersecretvalue\"\nand hf_AbCdEfGhIjKlMn alone\n";
        let (out, report) = redact(text);
        assert_eq!(report.masked, 2, "{out}");
        assert!(!out.contains("supersecretvalue"), "{out}");
        assert!(!out.contains("CdEfGhIjKlMn"), "{out}");
        assert!(out.ends_with('\n'), "line endings are preserved");
    }

    #[test]
    fn detection_reports_the_shapes_it_matched() {
        let found = find_secret_shapes("use sk-ABCDEFGHIJKLMNOP and hf_AAAAAAAAAA");
        assert_eq!(found, vec!["hf_".to_owned(), "sk-".to_owned()], "{found:?}");
        assert!(find_secret_shapes("nothing to see here").is_empty());
    }
}
