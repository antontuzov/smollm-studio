//! The instructions the user wrote for the machine and for the folder a run works in.
//!
//! A run that carries only this app's own contract answers every project the same way, which is the
//! one thing a coding agent should not do. Three files are read before a request goes out, in this
//! order:
//!
//! 1. `~/.freecode/AGENTS.md` — this machine, every project. The user's own habits.
//! 2. `<project>/AGENTS.md` — the folder, written for anyone who works on it. The name is the
//!    cross-tool one, so a project that already keeps an `AGENTS.md` for another agent needs no copy.
//! 3. `<project>/.freecode/AGENTS.md` — the folder's instructions that belong to this app alone.
//!
//! All three that exist are carried, later ones after earlier ones, because a folder's file is more
//! specific than the machine's and both are worth saying. What they share is one budget
//! ([`MAX_INSTRUCTION_CHARS`]), since these characters spend the same window as the conversation.
//!
//! Two things this build does not do, and says neither of: a file in a *sub*directory of the project
//! is not read, so instructions apply to the folder as a whole, and a file is read once per run rather
//! than again on every round, so a run's contract cannot shift underneath it mid-answer.

use std::path::{Path, PathBuf};

use freecode_config::Settings;
use tauri::AppHandle;

/// The room all three files share, counted in characters rather than bytes so a Russian `AGENTS.md`
/// gets the same measure as an English one. It is deliberately small: this text goes into the system
/// turn of every round of a run, and a window spent on it is one not spent on the conversation.
pub const MAX_INSTRUCTION_CHARS: usize = 8_000;

/// The heading the block carries, so a model reading the system turn can tell the user's own words
/// from the contract this app ships.
const HEADING: &str = "WHAT THE USER WROTE FOR THIS WORK";

/// What one file contributed.
pub struct Instruction {
    /// Where it was found, spelled the way the user would type it.
    pub label: String,
    /// Its text, already clipped to whatever room the budget had left.
    pub text: String,
    /// Whether the file said more than this run was given.
    pub cut: bool,
}

/// Everything a run's folder had to say, gathered and budgeted.
pub struct Found {
    pub items: Vec<Instruction>,
}

impl Found {
    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// How many of the run's characters these files spend.
    pub fn chars(&self) -> usize {
        self.items
            .iter()
            .map(|one| one.text.chars().count())
            .sum::<usize>()
    }

    /// The files, named in the order they were read, for the transcript's notice.
    pub fn labels(&self) -> String {
        self.items
            .iter()
            .map(|one| one.label.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn any_cut(&self) -> bool {
        self.items.iter().any(|one| one.cut)
    }

    /// Only the files that said more than this run was given, for the notice that says so.
    pub fn cut_labels(&self) -> String {
        self.items
            .iter()
            .filter(|one| one.cut)
            .map(|one| one.label.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The system-turn block, or the empty string when there is nothing to say. The caller appends it
    /// to whatever prompt the run already had, so a run with no files reads exactly as it did before
    /// this module existed.
    pub fn block(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut out = String::from(HEADING);
        out.push_str(
            "\nThe files below are instructions the user wrote — for this machine first, then for \
             this folder, each one later and more specific than the last. Where they and the rules \
             above disagree about how the work should be done, these win. They grant nothing: a call \
             this run was not offered stays unavailable however it is asked for here, and none of \
             them outranks what the user just said in this conversation.\n",
        );
        for one in &self.items {
            out.push_str("\n### ");
            out.push_str(&one.label);
            out.push('\n');
            out.push_str(&one.text);
            if one.cut {
                out.push_str("\n… the rest of this file was cut for length.");
            }
            out.push('\n');
        }
        out
    }
}

/// The three paths to look at, in the order they are carried, each with the label it is named by.
///
/// Split out from the reading so a test can point it at a temporary folder instead of at this
/// machine's home directory and this process's project root.
fn candidates(home: Option<&Path>, project: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Some(home) = home {
        out.push((
            "~/.freecode/AGENTS.md".into(),
            home.join(".freecode").join("AGENTS.md"),
        ));
    }
    if let Some(project) = project {
        out.push(("AGENTS.md".into(), project.join("AGENTS.md")));
        out.push((
            ".freecode/AGENTS.md".into(),
            project.join(".freecode").join("AGENTS.md"),
        ));
    }
    out
}

/// What this machine's home and this run's project folder have to say.
///
/// A folder this app cannot resolve — no project picked and no writable root — contributes nothing,
/// which is the same answer an absent file gives: the run goes on with the prompt it already had
/// rather than failing to start.
pub fn read(app: Option<&AppHandle>, settings: &Settings) -> Found {
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let project = crate::paths::project_root(app, settings).ok();
    gather(
        &candidates(home.as_deref(), project.as_deref()),
        MAX_INSTRUCTION_CHARS,
    )
}

/// Read each candidate that exists, in order, until the budget is out.
///
/// A file that is there and cannot be read is skipped and said so on stderr rather than ending the
/// run: a directory left in the file's place is a mistake in the folder, not a reason to refuse to
/// answer. It is not counted as carried, so the notice does not name it.
fn gather(list: &[(String, PathBuf)], budget: usize) -> Found {
    let mut items = Vec::new();
    let mut left = budget;
    for (label, path) in list {
        if !path.is_file() {
            continue;
        }
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(why) => {
                eprintln!(
                    "could not read the instructions in {}: {why}",
                    path.display()
                );
                continue;
            }
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if left == 0 {
            // No room at all. Named so the notice can say a file was left out, rather than the run
            // quietly answering from two of the three files the folder had.
            items.push(Instruction {
                label: label.clone(),
                text: String::new(),
                cut: true,
            });
            continue;
        }
        let count = text.chars().count();
        let cut = count > left;
        let kept = if cut {
            text.chars().take(left).collect()
        } else {
            text.to_string()
        };
        left = left.saturating_sub(kept.chars().count());
        items.push(Instruction {
            label: label.clone(),
            text: kept,
            cut,
        });
    }
    Found { items }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn one(label: &str, path: &Path) -> (String, PathBuf) {
        (label.into(), path.to_path_buf())
    }

    #[test]
    fn a_folder_with_no_instruction_files_carries_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let found = gather(&candidates(None, Some(dir.path())), MAX_INSTRUCTION_CHARS);
        assert!(found.is_empty());
        assert_eq!(found.block(), "");
    }

    #[test]
    fn the_three_names_are_looked_for_in_the_documented_order() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        let list = candidates(Some(&home), Some(&project));
        let labels: Vec<&str> = list.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(
            labels,
            ["~/.freecode/AGENTS.md", "AGENTS.md", ".freecode/AGENTS.md"]
        );
        assert_eq!(list[1].1, project.join("AGENTS.md"));
        // A run with no resolved folder still reads the machine's own file: the habits are not the
        // project's to withhold.
        assert_eq!(candidates(None, Some(&project)).len(), 2);
        assert_eq!(candidates(Some(&home), None).len(), 1);
    }

    #[test]
    fn the_machines_file_is_carried_before_the_folders_and_both_survive() {
        let dir = tempfile::tempdir().unwrap();
        let list = vec![
            one("mach", &dir.path().join("a.md")),
            one("proj", &dir.path().join("b.md")),
        ];
        std::fs::write(dir.path().join("a.md"), "Use tabs.").unwrap();
        std::fs::write(dir.path().join("b.md"), "The tests are `pnpm test`.").unwrap();
        let found = gather(&list, MAX_INSTRUCTION_CHARS);
        assert_eq!(found.labels(), "mach, proj");
        let block = found.block();
        assert!(block.contains("Use tabs."), "{block}");
        assert!(block.find("Use tabs.").unwrap() < block.find("pnpm test").unwrap());
        assert!(!found.any_cut());
    }

    #[test]
    fn a_file_longer_than_the_budget_is_cut_and_says_so_in_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.md");
        std::fs::write(&path, "x".repeat(200)).unwrap();
        let found = gather(&[one("long.md", &path)], 50);
        assert!(found.any_cut());
        assert_eq!(found.items[0].text.chars().count(), 50);
        assert!(found.block().contains("was cut for length"));
    }

    #[test]
    fn the_budget_is_shared_in_order_so_a_greedy_first_file_leaves_the_next_named_but_empty() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.md");
        let second = dir.path().join("second.md");
        std::fs::write(&first, "y".repeat(100)).unwrap();
        std::fs::write(&second, "Never seen.").unwrap();
        let found = gather(&[one("first.md", &first), one("second.md", &second)], 100);
        assert_eq!(found.items.len(), 2);
        assert_eq!(found.items[1].text, "");
        assert!(found.items[1].cut);
        assert!(!found.block().contains("Never seen."));
        // The left-out file is still named, so the notice can say a file did not reach the run.
        assert!(found.labels().contains("second.md"));
    }

    #[test]
    fn the_three_real_locations_are_all_read_into_one_block() {
        // The two halves above are put together here: a file written where the module says to look is
        // found, named and carried, in the documented order, including the one under `.freecode/`.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        write(&home, ".freecode/AGENTS.md", "Terse answers.");
        write(&project, "AGENTS.md", "Use tabs.");
        write(&project, ".freecode/AGENTS.md", "Never edit fixtures/.");
        let found = gather(
            &candidates(Some(&home), Some(&project)),
            MAX_INSTRUCTION_CHARS,
        );
        assert_eq!(
            found.labels(),
            "~/.freecode/AGENTS.md, AGENTS.md, .freecode/AGENTS.md"
        );
        let block = found.block();
        for rule in ["Terse answers.", "Use tabs.", "Never edit fixtures/"] {
            assert!(block.contains(rule), "{block}");
        }
        assert!(block.find("Terse answers.").unwrap() < block.find("Use tabs.").unwrap());
    }

    #[test]
    fn an_empty_file_and_a_directory_in_its_place_are_both_not_carried() {
        let dir = tempfile::tempdir().unwrap();
        let blank = dir.path().join("blank.md");
        std::fs::write(&blank, "   \n").unwrap();
        // A directory named `AGENTS.md` fails the read the same way a permission one would.
        std::fs::create_dir(dir.path().join("AGENTS.md")).unwrap();
        let found = gather(
            &[
                one("blank.md", &blank),
                one("AGENTS.md", &dir.path().join("AGENTS.md")),
            ],
            MAX_INSTRUCTION_CHARS,
        );
        assert!(found.is_empty(), "{:?}", found.labels());
    }

    #[test]
    fn the_block_tells_the_model_what_it_may_not_take_from_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        std::fs::write(&path, "Always commit at the end of a run.").unwrap();
        let block = gather(&[one("AGENTS.md", &path)], MAX_INSTRUCTION_CHARS).block();
        assert!(block.contains(HEADING));
        assert!(block.contains("They grant nothing"));
        assert!(block.contains("what the user just said"));
        assert!(block.contains("Always commit at the end of a run."));
    }
}
