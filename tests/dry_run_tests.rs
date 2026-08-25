//! Dry-run invariants: planning and reporting must NEVER mutate the
//! filesystem, and CLI misuse gates must fail closed.

mod common;

use common::{BinRun, MockCacheBuilder, TestWorkspace};

// ---------------------------------------------------------------------------
// D.1 — Zero-mutation invariant across a realistic multi-cache workspace
// ---------------------------------------------------------------------------

#[test]
fn dry_run_leaves_every_byte_and_timestamp_untouched() {
    let ws = TestWorkspace::new();
    let mut caches = MockCacheBuilder::new(&ws);
    caches.cargo_cache(12);
    caches.npm_cache(10);
    caches.pip_cache(10);
    caches.system_temp(18, 30 * 24 * 3600);
    assert!(
        caches.files().len() >= 50,
        "fixture should fabricate at least 50 files, got {}",
        caches.files().len()
    );

    let before = ws.snapshot();
    let entry_count = before.entry_count();

    let run = BinRun::args(&["clean", "--path", ws.path().to_str().unwrap()]);
    run.assert_success();
    run.assert_stdout_clean();

    let stdout = run.stdout();
    assert!(
        stdout.contains("[DRY-RUN MODE]"),
        "dry-run banner missing:\n{stdout}"
    );
    assert!(
        stdout.contains("clean --apply"),
        "next-step guidance missing:\n{stdout}"
    );
    // The proposal table must be present.
    assert!(stdout.contains("Reclaimable"), "plan table missing");

    let after = ws.snapshot();
    assert_eq!(
        before.entry_count(),
        after.entry_count(),
        "entry count changed"
    );
    let problems = before.diff(&after);
    assert!(
        problems.is_empty(),
        "dry-run mutated the workspace ({entry_count} entries):\n{}",
        problems.join("\n")
    );
}

#[test]
fn dry_run_report_lists_items_but_marks_them_skipped() {
    let ws = TestWorkspace::new();
    ws.create_file("cache/blob.bin", 8_192);

    // JSON dry-run exposes the machine-readable contract.
    let run = BinRun::args(&[
        "clean",
        "--path",
        ws.path().join("cache").to_str().unwrap(),
        "--json",
    ]);
    run.assert_success();

    let parsed: serde_json::Value =
        serde_json::from_str(&run.stdout()).expect("dry-run --json is valid JSON");
    assert_eq!(parsed["is_dry_run"], serde_json::json!(true));
    assert!(!parsed["items"].as_array().expect("items").is_empty());
    // Plans carry the reclaimable estimate; a plan never reports freed bytes.
    assert!(parsed["total_bytes"].as_u64().expect("total_bytes") > 0);
    assert!(parsed.get("bytes_freed").is_none());

    assert!(
        ws.join("cache/blob.bin").exists(),
        "file deleted by dry run!"
    );
}

// ---------------------------------------------------------------------------
// D.2 — Flag validation gates fail closed (exit 2, zero mutation)
// ---------------------------------------------------------------------------

#[test]
fn yes_without_apply_is_rejected_before_any_io() {
    let ws = TestWorkspace::new();
    let mut caches = MockCacheBuilder::new(&ws);
    caches.cargo_cache(4);
    let before = ws.snapshot();

    let run = BinRun::args(&["clean", "--yes"]);
    assert_eq!(
        run.code(),
        2,
        "--yes without --apply must be a usage error\nstderr: {}",
        run.stderr()
    );

    let stderr = run.stderr();
    assert!(stderr.contains("--yes requires --apply"), "{stderr}");
    // Nothing was even scanned: no tables on stdout.
    assert!(run.stdout().trim().is_empty());

    let problems = before.diff(&ws.snapshot());
    assert!(
        problems.is_empty(),
        "workspace mutated: {}",
        problems.join("\n")
    );
}

#[test]
fn invalid_min_size_is_a_usage_error_not_a_crash() {
    let run = BinRun::args(&["clean", "--min-size", "100XYZ"]);
    assert_eq!(run.code(), 2);
    assert!(run.stderr().contains("invalid byte size"));
}

#[test]
fn path_and_explicit_targets_is_rejected() {
    let run = BinRun::args(&["clean", "cargo-cache", "--path", "/tmp"]);
    assert_eq!(
        run.code(),
        2,
        "mixed --path + target IDs must be a usage error\nstderr: {}",
        run.stderr()
    );
    let stderr = run.stderr();
    assert!(
        stderr.contains("--path cannot be combined with explicit target IDs"),
        "{stderr}"
    );
}

#[test]
fn empty_plan_apply_json_has_report_shape_not_plan_shape() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let run = BinRun::args(&[
        "clean",
        "--apply",
        "--yes",
        "--json",
        "--path",
        tmp.path().to_str().unwrap(),
    ]);
    assert_eq!(run.code(), 0, "stderr: {}", run.stderr());

    let parsed: serde_json::Value =
        serde_json::from_str(&run.stdout()).expect("empty apply run must emit report JSON");
    assert!(
        parsed.get("bytes_freed").is_some(),
        "report shape requires top-level bytes_freed: {parsed}"
    );
    assert!(
        parsed.get("is_dry_run").is_none(),
        "is_dry_run belongs nested under plan, not at top level: {parsed}"
    );
    assert_eq!(parsed["dry_run"], false);
    assert_eq!(parsed["bytes_freed"], 0);
    assert_eq!(parsed["plan"]["is_dry_run"], false);
}
