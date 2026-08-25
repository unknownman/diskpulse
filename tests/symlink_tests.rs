//! Symlink semantics: quarantine (unlink the link, never follow) during
//! cleaning, and loop immunity in the visualizer scanner.

#![cfg(unix)]

mod common;

use std::fs;
use std::time::{Duration, Instant};

use common::{BinRun, TestWorkspace};
use diskpulse::cleaner::{create_clean_plan, execute_clean_plan, CleanOptions};
use diskpulse::scanner::{scan_path, ScanOptions, SortCriterion};

// ---------------------------------------------------------------------------
// S.1 — Quarantined deletion: link dies, target survives untouched
// ---------------------------------------------------------------------------

#[test]
fn apply_deletes_symlink_but_never_its_target() {
    let ws = TestWorkspace::new();

    ws.create_file("real_important_data/document.txt", 1_024);
    ws.create_dir("fake_cache");
    ws.create_symlink("../real_important_data", "fake_cache/symlink_dir")
        .expect("create symlink fixture");

    let target_before =
        fs::metadata(ws.join("real_important_data/document.txt")).expect("target metadata before");

    // Engine-level run: plan + apply + yes (headless).
    let options = CleanOptions {
        custom_path: Some(ws.join("fake_cache")),
        apply: true,
        yes: true,
        ..CleanOptions::default()
    };
    let plan = create_clean_plan(&options).expect("plan");
    assert_eq!(
        plan.total_items, 1,
        "exactly one deletion unit: the symlink itself"
    );

    let report = execute_clean_plan(&plan, false).expect("execute");
    assert_eq!(report.items_freed, 1);
    assert_eq!(report.errors_count, 0);

    // The LINK is gone…
    assert!(
        !ws.join("fake_cache/symlink_dir").exists(),
        "symlink was not removed"
    );
    // …the TARGET survived byte-for-byte with identical metadata.
    let doc = ws.join("real_important_data/document.txt");
    assert!(doc.exists(), "precious data was destroyed!");
    assert!(doc.is_file());
    let target_after = fs::metadata(&doc).expect("target metadata after");
    assert_eq!(target_before.len(), target_after.len(), "size changed");
    assert_eq!(
        target_before.modified().ok(),
        target_after.modified().ok(),
        "mtime changed"
    );

    assert!(
        ws.join("real_important_data").is_dir(),
        "parent dir of precious data vanished"
    );
}

#[test]
fn binary_apply_quarantine_end_to_end() {
    let ws = TestWorkspace::new();
    ws.create_file("real_important_data/irreplaceable.dat", 2_048);
    ws.create_dir("fake_cache");

    let link = ws
        .create_symlink("../../real_important_data", "fake_cache/link")
        .expect("create symlink");

    // Also drop a normal cache blob so the plan is not symlink-only.
    ws.create_file("fake_cache/blob.cache", 4_096);

    let before_snapshot = {
        // Fingerprint just the protected subtree.
        TestWorkspaceProbe::hash_tree(&ws.join("real_important_data"))
    };

    let run = BinRun::args(&[
        "clean",
        "--path",
        ws.join("fake_cache").to_str().unwrap(),
        "--apply",
        "--yes",
    ]);
    run.assert_success();

    assert!(!link.exists(), "symlink survived execution");
    assert!(!ws.join("fake_cache/blob.cache").exists());
    assert!(ws.join("real_important_data/irreplaceable.dat").exists());

    let after_snapshot = TestWorkspaceProbe::hash_tree(&ws.join("real_important_data"));
    assert_eq!(
        before_snapshot, after_snapshot,
        "protected tree changed through a symlink"
    );

    // The report must show both units deleted.
    let stdout = run.stdout();
    assert!(stdout.contains("Freed"), "{stdout}");
}

/// Minimal recursive hasher used to prove a subtree is bit-identical.
struct TestWorkspaceProbe;

impl TestWorkspaceProbe {
    fn hash_tree(root: &std::path::Path) -> u64 {
        use std::collections::BTreeMap;
        fn fnv(bytes: &[u8]) -> u64 {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in bytes {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            h
        }
        let mut files: BTreeMap<std::path::PathBuf, (u64, Option<u64>)> = BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).expect("walk protected tree") {
                let entry = entry.expect("entry");
                let meta = entry.metadata().expect("meta"); // follows nothing here: no links inside
                if meta.is_dir() {
                    stack.push(entry.path());
                } else {
                    let mtime = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos() as u64);
                    files.insert(
                        entry.path(),
                        (fnv(&fs::read(entry.path()).expect("read")), mtime),
                    );
                }
            }
        }
        let mut combined: u64 = 0x1234_5678;
        for (path, (content, mtime)) in &files {
            combined ^= fnv(path.to_string_lossy().as_bytes());
            combined = combined.rotate_left(13) ^ content ^ mtime.unwrap_or(0);
        }
        combined
    }
}

// ---------------------------------------------------------------------------
// S.2 — Scanner loop prevention
// ---------------------------------------------------------------------------

#[test]
fn circular_symlinks_do_not_hang_or_double_count() {
    let ws = TestWorkspace::new();
    ws.create_file("a/payload-a.bin", 4_096);
    ws.create_file("b/payload-b.bin", 4_096);
    // Cycle: a/link_to_b -> ../b and b/link_to_a -> ../a
    ws.create_symlink("../b", "a/link_to_b").expect("link a->b");
    ws.create_symlink("../a", "b/link_to_a").expect("link b->a");

    let options = ScanOptions {
        max_depth: 10,
        min_size: None,
        top_n: Some(50),
        apparent_size: false,
        no_ignore: true,
        include_hidden: true,
        one_file_system: false,
        sort_by: SortCriterion::Size,
    };

    let started = Instant::now();
    let result = scan_path(ws.path(), &options).expect("scan completes");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(1),
        "scanner took {elapsed:?} on a cyclic graph"
    );

    // Each real payload file counted exactly once, plus each link ENTRY
    // counted as a (non-traversed) file-type node: 2 payloads + 2 links = 4.
    assert_eq!(result.summary.total_files, 4, "{result:#?}");
    // Sizes are summed once per physical file (no exponential blow-up):
    // only the two 4 KiB payloads carry blocks.
    assert!(result.summary.total_size >= 8_192);
    assert!(result.summary.total_size < 8_192 * 1024);
}

#[test]
fn self_referential_symlink_is_survivable() {
    let ws = TestWorkspace::new();
    ws.create_file("data/file.bin", 1_024);
    ws.create_symlink(".", "data/self-loop").expect("self link");

    let options = ScanOptions {
        max_depth: 5,
        min_size: None,
        top_n: Some(20),
        apparent_size: false,
        no_ignore: true,
        include_hidden: false,
        one_file_system: false,
        sort_by: SortCriterion::Name,
    };
    let result = scan_path(&ws.join("data"), &options).expect("scan completes");
    // One real file + the self-loop link entry (never traversed).
    assert_eq!(result.summary.total_files, 2);
}

#[test]
fn dangling_symlink_does_not_break_scan_or_clean() {
    let ws = TestWorkspace::new();
    // `keep.bin` lives OUTSIDE the cleaned root so it must survive; the
    // cleaned cache contains a real blob plus a dangling link.
    ws.create_file("keep.bin", 512);
    ws.create_file("cache/blob.cache", 4_096);
    ws.create_symlink("/nonexistent/target-diskpulse-xyz", "cache/dangling")
        .expect("dangling link");

    // Scan tolerates it.
    let options = ScanOptions {
        max_depth: 2,
        min_size: None,
        top_n: Some(10),
        apparent_size: false,
        no_ignore: true,
        include_hidden: false,
        one_file_system: false,
        sort_by: SortCriterion::Size,
    };
    scan_path(ws.path(), &options).expect("scan handles dangling links");

    // Clean removes only the real sibling; the dangling link is unlinked.
    let run = BinRun::args(&[
        "clean",
        "--path",
        ws.join("cache").to_str().unwrap(),
        "--apply",
        "--yes",
    ]);
    run.assert_success();
    assert!(!ws.join("cache/dangling").exists());
    assert!(!ws.join("cache/blob.cache").exists());
    assert!(ws.join("keep.bin").exists());
}
