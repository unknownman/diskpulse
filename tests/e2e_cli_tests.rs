//! End-to-end CLI integration tests exercising the real binary.
//!
//! Every test spawns `target/debug/diskpulse` via [`std::process::Command`]
//! and asserts on exit codes, stdout/stderr content and filesystem effects,
//! per the documented exit-code matrix (0 success / 1 runtime / 2 usage).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_diskpulse")
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn diskpulse binary")
}

fn code(output: &std::process::Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

fn stdout_lossy(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_lossy(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A mock cache directory with two deletion units: a populated subdirectory
/// and one loose file.
fn mock_cache() -> (TempDir, PathBuf, PathBuf, PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("cache-root");
    fs::create_dir_all(root.join("pkg")).expect("mkdir pkg");
    let dir_item = root.join("pkg");
    let file_item = root.join("loose.bin");
    fs::write(&file_item, vec![b'x'; 4_096]).expect("write loose.bin");
    fs::write(dir_item.join("blob.bin"), vec![b'y'; 4_096]).expect("write blob.bin");
    (tmp, root, dir_item, file_item)
}

// ---------------------------------------------------------------------------
// Exit code 2 — CLI usage / validation errors
// ---------------------------------------------------------------------------

#[test]
fn yes_without_apply_exits_two_with_exact_message() {
    let out = run(&["clean", "--yes"]);
    assert_eq!(code(&out), 2, "stderr={}", stderr_lossy(&out));
    let stderr = stderr_lossy(&out);
    assert!(
        stderr.contains("--yes requires --apply"),
        "unexpected stderr: {stderr}"
    );
    // Actionable remediation hint accompanies the error.
    assert!(stderr.contains("--apply"), "hint missing: {stderr}");
}

#[test]
fn invalid_min_size_string_exits_two() {
    let out = run(&["clean", "--min-size", "100XYZ"]);
    assert_eq!(code(&out), 2, "stderr={}", stderr_lossy(&out));
    assert!(
        stderr_lossy(&out).contains("invalid byte size"),
        "unexpected stderr: {}",
        stderr_lossy(&out)
    );
}

#[test]
fn invalid_sort_criterion_exits_two() {
    let out = run(&["viz", ".", "--sort", "bogus"]);
    assert_eq!(code(&out), 2, "stderr={}", stderr_lossy(&out));
    let stderr = stderr_lossy(&out);
    assert!(
        stderr.contains("invalid sort field") && stderr.contains("size, count, name"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn unknown_clean_target_exits_two_with_hint() {
    let out = run(&["clean", "not-a-target"]);
    assert_eq!(code(&out), 2, "stderr={}", stderr_lossy(&out));
    let stderr = stderr_lossy(&out);
    assert!(
        stderr.contains("not-a-target") && stderr.contains("targets --all"),
        "unexpected stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Viz execution & JSON contract
// ---------------------------------------------------------------------------

#[test]
fn viz_json_is_valid_and_pollution_free() {
    let out = run(&["viz", ".", "--depth", "1", "--json"]);
    assert_eq!(code(&out), 0, "stderr={}", stderr_lossy(&out));

    let stdout = stdout_lossy(&out);
    assert!(!stdout.contains("\x1b["), "ANSI escape leaked into JSON");

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("viz --json must emit valid JSON");
    assert!(parsed.get("summary").is_some(), "summary key missing");
    assert!(parsed.get("root").is_some(), "root key missing");
}

#[test]
fn viz_missing_path_exits_one() {
    let out = run(&["viz", "/definitely/not/here-diskpulse-xyz"]);
    assert_eq!(code(&out), 1, "stderr={}", stderr_lossy(&out));
    assert!(
        stderr_lossy(&out).starts_with("error:"),
        "errors must use the friendly format"
    );
}

// ---------------------------------------------------------------------------
// Clean safety gates
// ---------------------------------------------------------------------------

#[test]
fn dry_run_never_touches_the_filesystem() {
    let (_guard, root, dir_item, file_item) = mock_cache();

    let out = run(&["clean", "--path", root.to_str().unwrap()]);
    assert_eq!(code(&out), 0, "stderr={}", stderr_lossy(&out));

    assert!(dir_item.exists(), "dry-run deleted a directory!");
    assert!(file_item.exists(), "dry-run deleted a file!");

    let stdout = stdout_lossy(&out);
    assert!(
        stdout.contains("[DRY-RUN MODE]"),
        "dry-run banner missing: {stdout}"
    );
    assert!(
        stdout.contains("clean --apply"),
        "next-step guidance missing: {stdout}"
    );
}

#[test]
fn non_interactive_stdin_aborts_apply_cleanly() {
    let (_guard, root, dir_item, file_item) = mock_cache();

    // EOF on stdin (Stdio::null): the prompt cannot be answered, so the
    // operation must be cancelled with exit code 0 and zero deletions.
    let out = Command::new(bin())
        .args(["clean", "--path", root.to_str().unwrap(), "--apply"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn diskpulse binary");

    assert_eq!(code(&out), 0, "stderr={}", stderr_lossy(&out));
    assert!(dir_item.exists());
    assert!(file_item.exists());

    let stdout = stdout_lossy(&out);
    assert!(
        stdout.contains("Operation cancelled by user."),
        "cancellation notice missing: {stdout}"
    );
}

#[test]
fn headless_apply_deletes_items() {
    let (_guard, root, dir_item, file_item) = mock_cache();

    let out = run(&[
        "clean",
        "--path",
        root.to_str().unwrap(),
        "--apply",
        "--yes",
    ]);
    assert_eq!(code(&out), 0, "stderr={}", stderr_lossy(&out));

    assert!(!dir_item.exists(), "pkg/ was not deleted");
    assert!(!file_item.exists(), "loose.bin was not deleted");
    assert!(root.is_dir(), "the cache root itself must survive");

    let stdout = stdout_lossy(&out);
    assert!(stdout.contains("Freed"), "freed summary missing: {stdout}");
}

#[test]
fn apply_on_protected_path_fails_without_deleting() {
    let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) else {
        return;
    };
    let documents = home.join("Documents");
    if !documents.is_dir() {
        return; // platform without Documents — nothing to prove here
    }

    let out = run(&[
        "clean",
        "--path",
        documents.to_str().unwrap(),
        "--apply",
        "--yes",
    ]);
    assert_eq!(code(&out), 1, "protected paths are runtime errors");
    assert!(documents.exists(), "Documents must never be touched");
    assert!(
        stderr_lossy(&out).contains("personal data"),
        "unexpected stderr: {}",
        stderr_lossy(&out)
    );
}

// ---------------------------------------------------------------------------
// Targets listing & help screens
// ---------------------------------------------------------------------------

#[test]
fn targets_lists_default_catalog() {
    let out = run(&["targets"]);
    assert_eq!(code(&out), 0, "stderr={}", stderr_lossy(&out));

    let stdout = stdout_lossy(&out);
    for expected in ["Cargo Cache", "System Temp", "npm Cache", "Go Build Cache"] {
        assert!(stdout.contains(expected), "{expected} missing from listing");
    }
}

#[test]
fn targets_all_includes_opt_in_entries() {
    let out = run(&["targets", "--all"]);
    assert_eq!(code(&out), 0);
    let stdout = stdout_lossy(&out);
    for expected in [
        "Xcode Derived Data",
        "Docker Buildx Cache",
        "Browser Caches",
    ] {
        assert!(stdout.contains(expected), "{expected} missing from --all");
    }
}

#[test]
fn help_screens_show_examples_for_every_command() {
    // Root: command overview instead of examples.
    let out = run(&["--help"]);
    assert_eq!(code(&out), 0);
    let stdout = stdout_lossy(&out);
    for expected in ["viz", "clean", "targets", "completions"] {
        assert!(stdout.contains(expected), "root help missing {expected}");
    }

    // Subcommands: practical EXAMPLES blocks.
    for args in [
        vec!["viz", "--help"],
        vec!["clean", "--help"],
        vec!["targets", "--help"],
    ] {
        let out = run(&args);
        assert_eq!(code(&out), 0, "help failed for {args:?}");
        let stdout = stdout_lossy(&out);
        assert!(stdout.contains("EXAMPLES:"), "no EXAMPLES in {args:?}");
        assert!(stdout.contains("diskpulse"), "no usage in {args:?}");
    }

    // Completions: installation guidance instead of EXAMPLES.
    let out = run(&["completions", "--help"]);
    assert_eq!(code(&out), 0);
    let stdout = stdout_lossy(&out);
    assert!(stdout.contains("bash") && stdout.contains("zsh"));
}

#[test]
fn aliases_resolve_to_subcommands() {
    for alias_args in [
        vec!["v", ".", "--depth", "1", "--json"],
        vec!["scan", ".", "--depth", "1", "--json"],
        vec!["c", "--help"],
        vec!["prune", "--help"],
        vec!["sweep", "--help"],
        vec!["rules"],
    ] {
        let out = run(&alias_args);
        assert_eq!(code(&out), 0, "alias {alias_args:?} failed");
    }
}

// ---------------------------------------------------------------------------
// Completions generation
// ---------------------------------------------------------------------------

#[test]
fn completions_emit_scripts_for_every_shell() {
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let out = run(&["completions", shell]);
        assert_eq!(code(&out), 0, "completions {shell} failed");
        let stdout = stdout_lossy(&out);
        assert!(!stdout.trim().is_empty(), "{shell} completions were empty");
        assert!(
            stdout.to_lowercase().contains("diskpulse"),
            "{shell} completions lack the binary name"
        );
    }
}

#[test]
fn completions_reject_unknown_shell_with_usage_error() {
    let out = run(&["completions", "tcsh"]);
    assert_eq!(code(&out), 2, "clap usage errors exit with 2");
}

// ---------------------------------------------------------------------------
// JSON purity under piping
// ---------------------------------------------------------------------------

#[test]
fn json_mode_has_zero_spinner_or_ansi_artifacts() {
    // Simulate a slow-ish scan target so a spinner *would* have been drawn
    // had JSON mode not suppressed it.
    let out = Command::new(bin())
        .args(["viz", "..", "--json"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdin(Stdio::null())
        .output()
        .expect("spawn binary");

    assert_eq!(code(&out), 0);
    let stdout = stdout_lossy(&out);
    assert!(!stdout.contains("\x1b["));
    assert!(!stdout.contains("⠋") && !stdout.contains("Scanning"));

    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(parsed.get("summary").is_some());
}

// ---------------------------------------------------------------------------
// Quiet mode contract
// ---------------------------------------------------------------------------

#[test]
fn quiet_suppresses_tables_but_not_exit_semantics() {
    let (_guard, root, _dir, _file) = mock_cache();
    let out = run(&["clean", "--quiet", "--path", root.to_str().unwrap()]);
    assert_eq!(code(&out), 0);
    assert!(stdout_lossy(&out).trim().is_empty(), "quiet printed tables");
}
