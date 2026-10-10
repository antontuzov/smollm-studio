//! The tree as the agent sees it: what is here, what kind of file it is, and
//! what is deliberately not here.
//!
//! One walk, done once, feeds everything else in this crate: the project
//! detection reads manifests out of it, the repo map groups it by directory,
//! and the ranking scores it against a task. Walking twice would give two
//! answers about what is in the repository, and the two would drift.
//!
//! Two ignore files are honoured. `.gitignore` because a repository's own
//! decision about what is noise is the best available answer, and `.agentignore`
//! because there are paths a *model* should not see that a human still wants
//! git to track — a vendored corpus, a generated fixture, a directory of
//! screenshots. `.agentignore` uses gitignore syntax and is read in every
//! directory, so a sub-project can hide its own noise.
//!
//! What a model must never see is not left to either file: the names in
//! `SKIPPED_FILES` are refused wherever they appear, because a repository that
//! commits a `.env` for local convenience should not decide what the agent reads.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file a repository uses to say what the agent must not read.
pub const AGENT_IGNORE_FILE: &str = ".agentignore";

/// Directories that are never context, whether or not a `.gitignore` says so.
///
/// A fresh clone of a Rust project has a `target/` full of expanded macros and
/// a copy of every string in the crate; `node_modules` has a copy of every
/// README on npm. Indexing them is not "knowing the repository", it is drowning
/// in it — and a model that finds `.git/config` will report the remote URL as
/// part of the project's structure.
const SKIPPED_DIRECTORIES: [&str; 8] = [
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    ".idea",
];

/// File names that are never context, whatever a model thinks they are for.
///
/// A dot-env file is a secret by convention and a private key is a secret by
/// name. The redactor would mask most of what they contain on the way back to
/// the prompt, and an index that quotes them in full is not covered by that:
/// these paths are simply not part of the repository as far as this crate is
/// concerned, and no ignore file can put them back.
const SKIPPED_FILES: [&str; 5] = [".env", "id_rsa", "id_ed25519", ".netrc", ".npmrc"];

fn is_skipped_file(name: &str) -> bool {
    SKIPPED_FILES.contains(&name) || name.starts_with(".env.") || name.ends_with(".env")
}

/// The largest file this crate will read as text.
///
/// Anything bigger is indexed — a model should know the file exists — but is not
/// quoted into a prompt, because a single 40 MB lockfile would end the run.
pub const MAX_READ_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Toml,
    Json,
    Yaml,
    Markdown,
    Shell,
    Css,
    Html,
    Other,
}

impl Language {
    /// From the extension, which is the only signal a file name gives.
    pub fn from_path(path: &str) -> Self {
        let extension = Path::new(path)
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match extension.as_str() {
            "rs" => Self::Rust,
            "py" => Self::Python,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "ts" | "tsx" => Self::TypeScript,
            "toml" => Self::Toml,
            "json" | "jsonl" | "ndjson" => Self::Json,
            "yaml" | "yml" => Self::Yaml,
            "md" | "markdown" => Self::Markdown,
            "sh" | "bash" | "zsh" => Self::Shell,
            "css" | "scss" | "less" => Self::Css,
            "html" | "htm" => Self::Html,
            _ => Self::Other,
        }
    }

    /// Whether a file of this kind is worth quoting to a model as context.
    ///
    /// A lockfile, a JSON bundle or a generated client is data, not code: the
    /// ranking may name it, but its body does not belong in a 2,000-token
    /// window.
    pub fn is_code(self) -> bool {
        matches!(
            self,
            Self::Rust | Self::Python | Self::JavaScript | Self::TypeScript | Self::Shell
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Toml => "toml",
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Markdown => "markdown",
            Self::Shell => "shell",
            Self::Css => "css",
            Self::Html => "html",
            Self::Other => "other",
        }
    }
}

/// One file in the index, addressed the way a model addresses it: relative to
/// the repository root, with forward slashes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    /// How many directories below the root this file sits, which is what keeps a
    /// repo map shallow without counting separators.
    pub depth: usize,
}

impl FileEntry {
    pub fn language(&self) -> Language {
        Language::from_path(&self.path)
    }

    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// The directory this file is in, `""` for one at the root.
    pub fn directory(&self) -> &str {
        match self.path.rfind('/') {
            Some(index) => &self.path[..index],
            None => "",
        }
    }

    pub fn has_name(&self, name: &str) -> bool {
        self.file_name() == name
    }
}

/// The set of files one repository contributed to one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub root: PathBuf,
    pub files: Vec<FileEntry>,
    /// Names the walk refused to enter or read, so a report can say "there is
    /// more you cannot see" rather than implying the tree is complete.
    pub skipped: Vec<String>,
}

impl Index {
    /// Walk the repository once.
    pub fn walk(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let mut files = Vec::new();
        let refused = std::sync::Arc::<Refused>::default();
        let mut builder = ignore::WalkBuilder::new(&root);
        builder
            // Hidden files are indexed: `.github/workflows`, `.cargo/config`
            // and the ignore files themselves all answer "what is this
            // project". What must not be seen is refused by name, not by
            // attribute.
            .hidden(false)
            // A .gitignore is a statement about noise, and it is true whether
            // or not this directory happens to be a working tree.
            .require_git(false)
            .add_custom_ignore_filename(AGENT_IGNORE_FILE);
        let counted = std::sync::Arc::clone(&refused);
        builder.filter_entry(move |entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                if entry.depth() > 0 && SKIPPED_DIRECTORIES.contains(&name.as_str()) {
                    counted.add(name);
                    return false;
                }
                return true;
            }
            if entry.file_type().is_some_and(|kind| kind.is_file()) && is_skipped_file(&name) {
                counted.add(name);
                return false;
            }
            true
        });

        for entry in builder.build().flatten() {
            if entry.depth() == 0 {
                // The root is not its own entry, and dropping it before its
                // ignore rules are read would drop the rules with it.
                continue;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(&root) else {
                continue;
            };
            files.push(FileEntry {
                path: relative.to_string_lossy().replace('\\', "/"),
                size: entry.metadata().map(|metadata| metadata.len()).unwrap_or(0),
                depth: entry.depth(),
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Self {
            root,
            files,
            skipped: refused.names(),
        }
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn contains(&self, path: &str) -> bool {
        self.get(path).is_some()
    }

    pub fn get(&self, path: &str) -> Option<&FileEntry> {
        let wanted = path.trim_start_matches("./").replace('\\', "/");
        self.files.iter().find(|entry| entry.path == wanted)
    }

    /// Every file in one directory, not counting what is below it.
    pub fn in_directory(&self, directory: &str) -> Vec<&FileEntry> {
        self.files
            .iter()
            .filter(|entry| entry.directory() == directory)
            .collect()
    }

    pub fn with_language(&self, language: Language) -> Vec<&FileEntry> {
        self.files
            .iter()
            .filter(|entry| entry.language() == language)
            .collect()
    }

    pub fn directories(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for entry in &self.files {
            let directory = entry.directory();
            if !directory.is_empty() && !seen.contains(&directory.to_owned()) {
                // A parent is added before a child because the walk is sorted by
                // path, so `crates` arrives with `crates/agent-core/src/lib.rs`.
                let mut parts: Vec<&str> = Vec::new();
                for part in directory.split('/') {
                    parts.push(part);
                    let prefix = parts.join("/");
                    if !seen.contains(&prefix) {
                        seen.push(prefix);
                    }
                }
            }
        }
        seen.sort();
        seen
    }

    /// The bytes of a file, if it is text and small enough to be worth reading.
    ///
    /// `None` is the honest answer for a binary, a missing path and a generated
    /// blob at once; the caller says which it meant because it knows what it
    /// asked for.
    pub fn read(&self, path: &str) -> Option<String> {
        let entry = self.get(path)?;
        if entry.size > MAX_READ_BYTES {
            return None;
        }
        let bytes = std::fs::read(self.root.join(&entry.path)).ok()?;
        String::from_utf8(bytes).ok()
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|entry| entry.size).sum()
    }

    /// A one-line description of the tree, for a report or a prompt header.
    pub fn describe(&self) -> String {
        let mut counts: Vec<(Language, usize)> = Vec::new();
        for entry in &self.files {
            let language = entry.language();
            match counts.iter_mut().find(|(known, _)| *known == language) {
                Some((_, count)) => *count += 1,
                None => counts.push((language, 1)),
            }
        }
        counts.sort_by(|(left, left_count), (right, right_count)| {
            right_count.cmp(left_count).then(left.cmp(right))
        });
        let top: Vec<String> = counts
            .into_iter()
            .take(4)
            .map(|(language, count)| format!("{count} {}", language.as_str()))
            .collect();
        format!(
            "{} file(s), {} directory(ies): {}",
            self.len(),
            self.directories().len(),
            top.join(", ")
        )
    }
}

/// The names a walk refused, kept somewhere the filter closure can reach.
///
/// A walker's filter must be `Send + Sync + 'static`, so it cannot borrow a
/// `Vec` from the stack of the function that built it; the list lives behind a
/// lock the closure holds a handle to instead. One thread does the walking, and
/// a lock this short-lived cannot realistically be contended.
#[derive(Default)]
struct Refused(std::sync::Mutex<Vec<String>>);

impl Refused {
    fn add(&self, name: String) {
        let mut seen = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !seen.contains(&name) {
            seen.push(name);
        }
    }

    fn names(&self) -> Vec<String> {
        let mut seen = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        seen.sort();
        seen.dedup();
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, path: &str, contents: &str) {
        let full = dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("a writable parent");
        }
        std::fs::write(full, contents).expect("writable");
    }

    fn sample() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp repo");
        write(dir.path(), "Cargo.toml", "[package]\nname = \"demo\"\n");
        write(dir.path(), "src/lib.rs", "pub fn demo() {}\n");
        write(dir.path(), "src/main.rs", "fn main() {}\n");
        write(dir.path(), "README.md", "# demo\n");
        write(dir.path(), "target/debug/build.rs", "generated\n");
        write(dir.path(), "node_modules/left-pad/index.js", "x\n");
        write(dir.path(), ".git/config", "[remote]\nurl = secret\n");
        write(dir.path(), ".agentignore", "secrets/\n");
        write(dir.path(), "secrets/token.txt", "sk-live\n");
        write(dir.path(), "docs/.hidden/note.md", "x\n");
        dir
    }

    #[test]
    fn a_walk_skips_the_directories_that_are_never_context() {
        let dir = sample();
        let index = Index::walk(dir.path());
        let paths: Vec<&str> = index
            .files
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert!(paths.contains(&"Cargo.toml"), "{paths:?}");
        assert!(paths.contains(&"src/lib.rs"), "{paths:?}");
        assert!(
            !paths.iter().any(|path| path.starts_with("target/")),
            "{paths:?}"
        );
        assert!(
            !paths.iter().any(|path| path.starts_with("node_modules/")),
            "{paths:?}"
        );
        assert!(
            !paths.iter().any(|path| path.starts_with(".git/")),
            "a model has no business reading the remote url: {paths:?}"
        );
        assert!(
            index.skipped.contains(&"target".to_owned()),
            "{:?}",
            index.skipped
        );
    }

    #[test]
    fn gitignore_and_agentignore_both_apply_outside_a_working_tree() {
        let dir = sample();
        write(dir.path(), ".gitignore", "generated/\n");
        write(dir.path(), "generated/data.json", "{}");
        let index = Index::walk(dir.path());
        let paths: Vec<&str> = index
            .files
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert!(
            !paths.iter().any(|path| path.starts_with("generated/")),
            "{paths:?}"
        );
        assert!(
            !paths.iter().any(|path| path.starts_with("secrets/")),
            ".agentignore is honoured: {paths:?}"
        );
    }

    #[test]
    fn a_nested_agentignore_hides_its_own_subtree() {
        let dir = tempfile::tempdir().expect("a temp repo");
        write(dir.path(), "src/lib.rs", "pub fn demo() {}");
        write(dir.path(), "src/.agentignore", "snapshots/\n");
        write(dir.path(), "src/snapshots/big.png", "x");
        write(dir.path(), "tests/one.rs", "fn one() {}");
        write(dir.path(), "tests/snapshots/big.png", "x");
        let index = Index::walk(dir.path());
        let paths: Vec<&str> = index
            .files
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert!(
            !paths.contains(&"src/snapshots/big.png"),
            "the rules of a directory apply below it: {paths:?}"
        );
        assert!(
            paths.contains(&"tests/snapshots/big.png"),
            "and only below it: {paths:?}"
        );
    }

    #[test]
    fn no_ignore_file_can_reveal_a_name_that_is_refused_by_definition() {
        let dir = tempfile::tempdir().expect("a temp repo");
        write(dir.path(), "src/lib.rs", "pub fn demo() {}");
        write(dir.path(), "id_rsa", "-----BEGIN OPENSSH PRIVATE KEY-----");
        // The one spelling of "please show this one to the model" an owner can
        // write. It does not work: the name is refused before any rule is read.
        write(dir.path(), ".agentignore", "!id_rsa\n");
        let index = Index::walk(dir.path());
        assert!(
            !index.contains("id_rsa"),
            "a private key is not context whatever the ignore files say"
        );
        assert!(
            index.skipped.contains(&"id_rsa".to_owned()),
            "and the report says it was hidden: {:?}",
            index.skipped
        );
    }

    #[test]
    fn a_dot_env_file_is_never_indexed() {
        let dir = tempfile::tempdir().expect("a temp repo");
        write(dir.path(), "src/lib.rs", "pub fn demo() {}");
        write(dir.path(), ".env", "API_KEY=sk-live");
        write(dir.path(), ".env.production", "API_KEY=sk-live");
        write(dir.path(), "staging.env", "API_KEY=sk-live");
        let index = Index::walk(dir.path());
        assert_eq!(index.len(), 1, "{:?}", index.files);
        assert_eq!(index.skipped.len(), 3, "{:?}", index.skipped);
    }

    #[test]
    fn entries_know_their_own_shape() {
        let entry = FileEntry {
            path: "crates/agent-core/src/lib.rs".to_owned(),
            size: 12,
            depth: 4,
        };
        assert_eq!(entry.file_name(), "lib.rs");
        assert_eq!(entry.directory(), "crates/agent-core/src");
        assert_eq!(entry.language(), Language::Rust);
        assert!(entry.has_name("lib.rs"));
        assert!(entry.language().is_code());

        let root = FileEntry {
            path: "README.md".to_owned(),
            size: 4,
            depth: 1,
        };
        assert_eq!(root.directory(), "");
        assert!(
            !root.language().is_code(),
            "a readme is quoted by other means"
        );
    }

    #[test]
    fn a_spelling_a_model_wrote_still_finds_the_file() {
        let dir = sample();
        let index = Index::walk(dir.path());
        assert!(index.contains("./src/lib.rs"));
        assert!(index.contains("src/lib.rs"));
        assert_eq!(index.get("src/nope.rs"), None);
    }

    #[test]
    fn directories_include_every_ancestor() {
        let dir = sample();
        let index = Index::walk(dir.path());
        let directories = index.directories();
        assert!(directories.contains(&"src".to_owned()), "{directories:?}");
        for directory in &directories {
            let mut parent = directory.as_str();
            while let Some(index) = parent.rfind('/') {
                parent = &parent[..index];
                assert!(
                    directories.iter().any(|known| known.as_str() == parent),
                    "{directory} is listed but its parent {parent} is not: {directories:?}"
                );
            }
        }
    }

    #[test]
    fn a_large_or_binary_file_is_indexed_but_not_read() {
        let dir = tempfile::tempdir().expect("a temp repo");
        write(dir.path(), "small.rs", "fn small() {}");
        std::fs::write(dir.path().join("blob.bin"), [0u8, 255, 128, 3]).expect("writable");
        let big = "x".repeat(2 * 1024 * 1024);
        write(dir.path(), "huge.txt", &big);
        let index = Index::walk(dir.path());
        assert!(
            index.contains("huge.txt"),
            "it exists; it is just not context"
        );
        assert_eq!(index.read("huge.txt"), None);
        assert_eq!(index.read("blob.bin"), None);
        assert_eq!(index.read("small.rs").as_deref(), Some("fn small() {}"));
        assert_eq!(index.read("nowhere.rs"), None);
    }

    #[test]
    fn a_description_counts_without_inventing_anything() {
        let dir = sample();
        let index = Index::walk(dir.path());
        let text = index.describe();
        assert!(
            text.starts_with("6 file(s), 3 directory(ies): 2 rust, 2 markdown"),
            "{text}"
        );
        assert!(index.total_bytes() > 0);
        assert_eq!(index.in_directory("src").len(), 2);
        assert_eq!(index.with_language(Language::Rust).len(), 2);
    }

    #[test]
    fn a_language_comes_from_its_extension_and_says_what_it_is() {
        for (path, language) in [
            ("a.rs", Language::Rust),
            ("a.py", Language::Python),
            ("a.tsx", Language::TypeScript),
            ("a.JS", Language::JavaScript),
            ("Cargo.toml", Language::Toml),
            ("a.jsonl", Language::Json),
            ("a.yaml", Language::Yaml),
            ("a.md", Language::Markdown),
            ("a.sh", Language::Shell),
            ("a.css", Language::Css),
            ("a.html", Language::Html),
            ("Makefile", Language::Other),
        ] {
            assert_eq!(Language::from_path(path), language, "{path}");
        }
        assert_eq!(Language::Rust.as_str(), "rust");
        assert!(!Language::Toml.is_code());
    }
}
