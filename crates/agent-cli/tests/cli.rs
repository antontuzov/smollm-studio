//! The binary's own contract: what it prints, and which exit code it leaves
//! behind, because those two are what a script or a CI job reads.

use std::path::Path;
use std::process::{Command, Output};

fn smoll(dir: &Path, args: &[&str]) -> Output {
    // Hermetic: a developer's real ~/.config/smoll must not change what these
    // assertions see.
    Command::new(env!("CARGO_BIN_EXE_smoll"))
        .args(["-C"])
        .arg(dir)
        .args(args)
        .env("HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("home/.config"))
        .env_remove("SMOLL_CONFIG")
        .env_remove("SMOLL_APPROVAL_MODE")
        .env_remove("SMOLL_PROVIDER")
        .env_remove("SMOLL_MODEL")
        .output()
        .expect("the binary runs")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

#[test]
fn help_lists_every_subcommand_the_readme_promises() {
    let dir = tempfile::tempdir().expect("temp dir");
    let output = smoll(dir.path(), &["--help"]);
    let help = text(&output);
    for command in ["init", "doctor", "config", "tools", "task"] {
        assert!(
            help.contains(command),
            "{command} missing from --help:\n{help}"
        );
    }
    assert!(
        help.contains("--json"),
        "the machine-readable flag must be discoverable"
    );
    assert!(
        help.contains("-C"),
        "the working directory flag must be discoverable"
    );
}

#[test]
fn init_writes_a_file_and_exits_zero() {
    let dir = tempfile::tempdir().expect("temp dir");
    let output = smoll(dir.path(), &["init"]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        dir.path().join("smoll.toml").exists(),
        "the file is written"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("approve-edits"),
        "it says what will happen:\n{stdout}"
    );
    assert!(
        stdout.contains("mock"),
        "it says which provider answers:\n{stdout}"
    );
}

#[test]
fn init_twice_refuses_unless_forced() {
    let dir = tempfile::tempdir().expect("temp dir");
    smoll(dir.path(), &["init"]);
    let second = smoll(dir.path(), &["init"]);
    assert_eq!(
        second.status.code(),
        Some(2),
        "a refused overwrite is a usage error"
    );
    assert!(
        text(&second).contains("--force"),
        "it tells you the way out: {}",
        text(&second)
    );
    assert!(smoll(dir.path(), &["init", "--force"]).status.success());
}

#[test]
fn config_prints_toml_for_a_human_and_json_for_a_script() {
    let dir = tempfile::tempdir().expect("temp dir");
    smoll(dir.path(), &["init"]);

    let human = smoll(dir.path(), &["config"]);
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(stdout.contains("[agent]"), "{stdout}");
    assert!(stdout.contains("approval_mode"), "{stdout}");

    let machine = smoll(dir.path(), &["config", "--json"]);
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&machine.stdout)).expect("valid json");
    assert_eq!(value["config"]["agent"]["approval_mode"], "approve-edits");
    assert_eq!(value["config"]["privacy"]["telemetry"], false);
    assert!(
        !value["sources"].as_array().expect("a list").is_empty(),
        "the file that supplied the values is named"
    );
}

#[test]
fn environment_overrides_are_visible_in_the_resolved_output() {
    let dir = tempfile::tempdir().expect("temp dir");
    smoll(dir.path(), &["init"]);
    let output = Command::new(env!("CARGO_BIN_EXE_smoll"))
        .args(["-C"])
        .arg(dir.path())
        .args(["config", "--json"])
        .env("HOME", dir.path().join("home"))
        .env("XDG_CONFIG_HOME", dir.path().join("home/.config"))
        .env("SMOLL_APPROVAL_MODE", "suggest-only")
        .output()
        .expect("the binary runs");
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("valid json");
    assert_eq!(value["config"]["agent"]["approval_mode"], "suggest-only");
}

#[test]
fn a_broken_config_is_error_exit_two_with_the_offending_key() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("smoll.toml"), "[agent]\nmax_stepz = 4\n").expect("write");
    let output = smoll(dir.path(), &["config"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output).contains("max_stepz"), "{}", text(&output));
}

#[test]
fn a_cwd_that_is_not_a_directory_is_refused_before_anything_reads() {
    let dir = tempfile::tempdir().expect("temp dir");
    let output = Command::new(env!("CARGO_BIN_EXE_smoll"))
        .args(["-C", "no-such-directory", "config"])
        .current_dir(dir.path())
        .env("HOME", dir.path().join("home"))
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        text(&output).contains("not a directory"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_command_that_is_not_built_yet_says_so_rather_than_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    for command in ["doctor", "tools"] {
        let output = smoll(dir.path(), &[command]);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{command} must not pretend to work"
        );
        assert!(text(&output).contains("not wired up"), "{}", text(&output));
    }
}

#[test]
fn running_with_no_subcommand_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    let output = Command::new(env!("CARGO_BIN_EXE_smoll"))
        .env("HOME", dir.path().join("home"))
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output).contains("--help"), "{}", text(&output));
}
