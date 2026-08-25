//! Integration tests for the safe cleanup engine (`clean` subcommand).
//!
//! Everything runs inside `tempfile` sandboxes; no real cache directories or
//! personal data are touched.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use diskpulse::cleaner::{
    create_clean_plan, execute_clean_plan, validate_path_safety, CleanOptions,
};
use diskpulse::models::CleanItemStatus;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn touch(path: &Path, size: usize) -> PathBuf {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(path, vec![b'x'; size]).expect("write fixture file");
    path.to_path_buf()
}

/// A fake "cache root" containing one populated directory and one loose file.
/// Deletion units are the direct children of the root, so this yields exactly
/// two planned items: `pkg-a` (sized recursively) and `loose.bin`.
fn sandbox_cache() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("cache");
    touch(&root.join("pkg-a").join("lib.rlib"), 4_096);
    touch(&root.join("pkg-a").join("meta.json"), 128);
    touch(&root.join("loose.bin"), 4_096);
    (tmp, root)
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("create symlink");
}

// ---------------------------------------------------------------------------
// Safety jail
// ---------------------------------------------------------------------------

#[test]
fn safety_jail_rejects_protected_locations() {
    // Home root and personal folders must always be refused.
    if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
        assert!(validate_path_safety(&home).is_err(), "home root refused");
        assert!(
            validate_path_safety(&home.join("Documents")).is_err(),
            "Documents refused"
        );
        assert!(
            validate_path_safety(&home.join("Documents").join("a.txt")).is_err(),
            "contents of Documents refused"
        );
        // Cache dirs under $HOME remain allowed.
        assert!(validate_path_safety(&home.join(".cache").join("anything")).is_ok());
    }

    // System roots are refused, but their temporary subtrees are fine.
    #[cfg(unix)]
    {
        assert!(validate_path_safety(Path::new("/var")).is_err());
        assert!(validate_path_safety(Path::new("/usr")).is_err());
        let deep = std::env::temp_dir().join("diskpulse_jail_probe");
        assert!(validate_path_safety(&deep).is_ok());
    }
    #[cfg(windows)]
    {
        assert!(validate_path_safety(Path::new("C:\\Windows\\System32")).is_err());
    }
}

// ---------------------------------------------------------------------------
// Dry-run behaviour
// ---------------------------------------------------------------------------

#[test]
fn dry_run_plan_does_not_mutate_filesystem() {
    let (_guard, root) = sandbox_cache();

    let options = CleanOptions {
        custom_path: Some(root.clone()),
        apply: false,
        ..CleanOptions::default()
    };

    let plan = create_clean_plan(&options).expect("dry-run plan");
    assert!(plan.is_dry_run);
    assert_eq!(plan.total_items, 2, "pkg-a dir + one loose file");
    let planned_dir = plan
        .items
        .iter()
        .find(|item| item.path == root.join("pkg-a"))
        .expect("pkg-a planned as one unit");
    assert!(planned_dir.is_dir);

    // Planning must not have removed anything.
    assert!(root.join("pkg-a").is_dir());
    assert!(root.join("loose.bin").is_file());

    let report = execute_clean_plan(&plan, false).expect("dry-run execution");
    assert_eq!(report.items_freed, 0);
    assert_eq!(report.bytes_freed, 0);
    assert!(report
        .results
        .iter()
        .all(|item| matches!(item.status, CleanItemStatus::SkippedDryRun)));

    // Nothing was deleted.
    assert!(root.join("pkg-a").is_dir());
    assert!(root.join("loose.bin").is_file());
}

// ---------------------------------------------------------------------------
// Symlink quarantine
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn plan_quarantines_symlinks_pointing_at_precious_data() {
    let _documents_guard = TempDir::new().expect("tempdir");
    let precious = _documents_guard.path().join("thesis.pdf");
    touch(&precious, 512);

    let (_cache_guard, root) = sandbox_cache();
    symlink(&precious, &root.join("evil-link"));

    let options = CleanOptions {
        custom_path: Some(root.clone()),
        apply: false,
        ..CleanOptions::default()
    };
    let plan = create_clean_plan(&options).expect("plan");

    assert!(
        !plan
            .items
            .iter()
            .any(|item| item.path == root.join("evil-link")),
        "symlinked entries must never be planned for deletion"
    );

    // The precious target survived planning untouched.
    assert!(precious.is_file());

    // Even a forced execution attempt cannot follow the link.
    let report = execute_clean_plan(&plan, true).expect("execution");
    assert!(precious.is_file(), "target of quarantined link survives");
    let _ = report;
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

#[test]
fn age_filter_excludes_fresh_entries() {
    use filetime::{set_file_mtime, FileTime};

    let (_guard, root) = sandbox_cache();
    let stale = root.join("stale.bin");
    touch(&stale, 256);

    // Three hours ago.
    let old = FileTime::from_unix_time(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64
            - 3 * 3600,
        0,
    );
    set_file_mtime(&stale, old).expect("backdate mtime");

    let options = CleanOptions {
        custom_path: Some(root.clone()),
        older_than: Some(chrono::Duration::hours(1)),
        apply: false,
        ..CleanOptions::default()
    };
    let plan = create_clean_plan(&options).expect("plan");

    let planned_paths: Vec<_> = plan.items.iter().map(|item| item.path.clone()).collect();
    assert!(planned_paths.contains(&stale), "stale entry selected");
    assert!(
        !planned_paths.contains(&root.join("loose.bin")),
        "fresh entry excluded"
    );
}

#[test]
fn size_filter_excludes_small_entries() {
    let (_guard, root) = sandbox_cache();

    let big = touch(&root.join("big.cache"), 8_192);

    let options = CleanOptions {
        custom_path: Some(root.clone()),
        // Above one APFS allocation block (4 KiB) so the small file loses,
        // but below `big.cache`.
        min_size: Some(6_000),
        apply: false,
        ..CleanOptions::default()
    };
    let plan = create_clean_plan(&options).expect("plan");

    let planned_paths: Vec<_> = plan.items.iter().map(|item| item.path.clone()).collect();
    assert!(planned_paths.contains(&big), "large entry selected");
    assert!(
        !planned_paths.contains(&root.join("loose.bin")),
        "entry below --min-size excluded"
    );
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

#[test]
fn execute_plan_deletes_items_when_apply_is_set() {
    let (_guard, root) = sandbox_cache();

    let options = CleanOptions {
        custom_path: Some(root.clone()),
        apply: true,
        yes: true,
        ..CleanOptions::default()
    };

    let plan = create_clean_plan(&options).expect("apply-mode plan");
    let expected_bytes = plan.total_bytes;
    let expected_items = plan.total_items;
    assert_eq!(expected_items, 2);

    let report = execute_clean_plan(&plan, false).expect("execution");
    for item in &report.results {
        if let CleanItemStatus::Failed(reason) = &item.status {
            panic!("deletion failed for {}: {reason}", item.path.display());
        }
    }
    assert!(!report.dry_run);
    assert_eq!(report.items_freed, 2);
    assert_eq!(report.bytes_freed, expected_bytes);
    assert_eq!(report.errors_count, 0);
    assert!(report
        .results
        .iter()
        .all(|item| matches!(item.status, CleanItemStatus::Deleted)));

    // Items are gone; the sandbox cache root itself remains.
    assert!(!root.join("pkg-a").exists());
    assert!(!root.join("loose.bin").exists());
    assert!(root.is_dir());
}
