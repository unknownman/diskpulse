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
fn execution_unlinks_symlinks_without_touching_their_target() {
    let _documents_guard = TempDir::new().expect("tempdir");
    let precious = _documents_guard.path().join("thesis.pdf");
    touch(&precious, 512);

    let (_cache_guard, root) = sandbox_cache();
    let link = root.join("evil-link");
    symlink(&precious, &link);
    let before = fs::metadata(&precious).expect("target metadata before");

    let options = CleanOptions {
        custom_path: Some(root.clone()),
        apply: true,
        yes: true,
        ..CleanOptions::default()
    };
    let plan = create_clean_plan(&options).expect("plan");
    let link_item = plan
        .items
        .iter()
        .find(|item| item.path == link)
        .expect("symlink must be planned as an unlink-only deletion unit");
    assert!(
        !link_item.is_dir,
        "a symlink is never treated as a directory"
    );

    let report = execute_clean_plan(&plan, false).expect("execution");
    assert!(!link.exists(), "the link itself was removed");
    assert!(precious.is_file(), "target of the link survives");

    // The precious file's identity is untouched (same size + mtime).
    let after = fs::metadata(&precious).expect("target metadata after");
    assert_eq!(before.len(), after.len());
    assert_eq!(
        before.modified().ok(),
        after.modified().ok(),
        "mtime of the link target changed"
    );
    assert_eq!(report.errors_count, 0);
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
// Single-file custom paths
// ---------------------------------------------------------------------------

#[test]
fn single_file_custom_path_is_planned_and_deleted() {
    let guard = TempDir::new().expect("tempdir");
    let file = guard.path().join("single.bin");
    fs::write(&file, vec![b'z'; 4_096]).expect("write fixture");

    let options = CleanOptions {
        custom_path: Some(file.clone()),
        apply: true,
        yes: true,
        ..CleanOptions::default()
    };

    let plan = create_clean_plan(&options).expect("plan");
    assert_eq!(plan.total_items, 1, "the file itself is the only unit");
    assert!(!plan.items[0].is_dir);
    assert_eq!(plan.items[0].path, file);

    let report = execute_clean_plan(&plan, false).expect("execute");
    assert_eq!(report.items_freed, 1);
    assert_eq!(report.errors_count, 0);
    assert!(!file.exists(), "single-file target was not deleted");
}

#[test]
fn single_file_custom_path_respects_dry_run_and_filters() {
    let guard = TempDir::new().expect("tempdir");
    let file = guard.path().join("keep-me.bin");
    fs::write(&file, vec![b'z'; 4_096]).expect("write fixture");

    // Dry-run never deletes it.
    let dry = CleanOptions {
        custom_path: Some(file.clone()),
        ..CleanOptions::default()
    };
    let plan = create_clean_plan(&dry).expect("plan");
    assert_eq!(plan.total_items, 1);
    assert!(plan.is_dry_run);
    assert!(file.exists());

    // min_size below the file's size keeps it plannable; above excludes it.
    let small = CleanOptions {
        custom_path: Some(file.clone()),
        min_size: Some(1_024),
        ..CleanOptions::default()
    };
    assert_eq!(create_clean_plan(&small).unwrap().total_items, 1);

    let big = CleanOptions {
        custom_path: Some(file.clone()),
        min_size: Some(1 << 20),
        ..CleanOptions::default()
    };
    assert_eq!(
        create_clean_plan(&big).unwrap().total_items,
        0,
        "file below --min-size must be filtered out"
    );
    assert!(file.exists(), "filtered file was touched");
}

// ---------------------------------------------------------------------------
// Deep filter evaluation: fresh directory mtime hides nothing
// ---------------------------------------------------------------------------

#[test]
fn older_than_descends_into_recently_touched_directories() {
    use filetime::{set_file_mtime, FileTime};

    let (_guard, root) = sandbox_cache();

    // A cache package whose DIRECTORY was touched recently but whose bulk
    // content is ancient. The dir mtime is "now" because we just created it.
    let pkg = root.join("fresh_pkg");
    touch(&pkg.join("stale-1.bin"), 2_048);
    touch(&pkg.join("stale-2.bin"), 2_048);
    touch(&pkg.join("brand-new.bin"), 2_048);

    let hours_ago = |secs: i64| {
        FileTime::from_unix_time(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_secs() as i64
                - secs,
            0,
        )
    };
    set_file_mtime(pkg.join("stale-1.bin"), hours_ago(4 * 3600)).expect("backdate");
    set_file_mtime(pkg.join("stale-2.bin"), hours_ago(3 * 3600)).expect("backdate");

    let options = CleanOptions {
        custom_path: Some(root.clone()),
        older_than: Some(chrono::Duration::hours(1)),
        apply: true,
        yes: true,
        ..CleanOptions::default()
    };

    let plan = create_clean_plan(&options).expect("plan");
    let planned: Vec<_> = plan.items.iter().map(|item| item.path.clone()).collect();

    assert!(
        planned.contains(&pkg.join("stale-1.bin")) && planned.contains(&pkg.join("stale-2.bin")),
        "ancient files inside a freshly-touched dir were missed: {planned:?}"
    );
    assert!(
        !planned.contains(&pkg),
        "the whole fresh dir must not be planned as one unit"
    );
    assert!(
        !planned.contains(&pkg.join("brand-new.bin")),
        "fresh files inside the dir stay protected"
    );

    // Execute and verify exactly the two stale files are gone.
    let report = execute_clean_plan(&plan, false).expect("execute");
    assert_eq!(report.items_freed, 2);
    assert!(!pkg.join("stale-1.bin").exists());
    assert!(pkg.join("brand-new.bin").exists());
    assert!(pkg.is_dir());
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

// ---------------------------------------------------------------------------
// Requested edge-case matrix: single files + combined file-level filters
// ---------------------------------------------------------------------------

#[test]
fn clean_custom_path_single_file() {
    let guard = TempDir::new().expect("tempdir");
    let temp_file = guard.path().join("temp_file.bin");
    // Block-aligned length (4 x 4 KiB) so allocated bytes equal logical
    // length on every filesystem; bytes_freed reports disk allocation.
    let payload = vec![b'A'; 16_384];
    fs::write(&temp_file, &payload).expect("write fixture");
    assert_eq!(fs::metadata(&temp_file).unwrap().len(), 16_384);

    let options = CleanOptions {
        custom_path: Some(temp_file.clone()),
        apply: true,
        yes: true,
        ..CleanOptions::default()
    };

    let plan = create_clean_plan(&options).expect("plan must accept a bare file root");
    assert_eq!(plan.total_items, 1);
    assert!(!plan.items[0].is_dir);

    let report = execute_clean_plan(&plan, false).expect("execute");

    assert_eq!(report.bytes_freed, 16_384, "freed bytes == file length");
    assert_eq!(report.items_freed, 1);
    assert_eq!(report.errors_count, 0);
    assert!(!temp_file.exists(), "temp_file.bin was not deleted");
}

#[test]
fn clean_custom_path_respects_min_size_and_age_for_files() {
    use filetime::{set_file_mtime, FileTime};

    let guard = TempDir::new().expect("tempdir");
    let old_big = guard.path().join("old_big.bin");
    let new_small = guard.path().join("new_small.bin");
    fs::write(&old_big, vec![b'o'; 8_192]).expect("write fixture");
    fs::write(&new_small, vec![b'n'; 1_024]).expect("write fixture");

    // Backdate old_big.bin well past the requested window.
    let three_hours_ago = FileTime::from_unix_time(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64
            - 3 * 3600,
        0,
    );
    set_file_mtime(&old_big, three_hours_ago).expect("backdate mtime");

    let options = CleanOptions {
        custom_path: Some(guard.path().to_path_buf()),
        older_than: Some(chrono::Duration::hours(1)),
        min_size: Some(4_096),
        apply: true,
        yes: true,
        ..CleanOptions::default()
    };

    let plan = create_clean_plan(&options).expect("plan");
    let planned: Vec<_> = plan.items.iter().map(|item| item.path.clone()).collect();

    assert_eq!(plan.total_items, 1, "exactly one survivor: {planned:?}");
    assert!(
        planned.contains(&old_big),
        "old + big file must be selected: {planned:?}"
    );
    assert!(
        !planned.contains(&new_small),
        "fresh small file must stay protected: {planned:?}"
    );

    let report = execute_clean_plan(&plan, false).expect("execute");
    assert_eq!(report.bytes_freed, 8_192);
    assert!(!old_big.exists());
    assert!(new_small.exists(), "new_small.bin was touched");
}

// ---------------------------------------------------------------------------
// Age windows never vouch for directory contents
// ---------------------------------------------------------------------------

#[test]
fn older_than_never_deletes_fresh_files_inside_old_directories() {
    use filetime::{set_file_mtime, FileTime};

    let guard = TempDir::new().expect("tempdir");
    let old_pkg = guard.path().join("old_pkg");
    fs::create_dir_all(&old_pkg).expect("mkdir");
    let old_sub = old_pkg.join("sub");
    fs::create_dir_all(&old_sub).expect("mkdir");

    let old_file = old_pkg.join("ancient.bin");
    fs::write(&old_file, vec![b'o'; 2_048]).expect("write");
    // In-place "rewrite" of an existing file: parent mtime stays untouched.
    let rewritten = old_pkg.join("rewritten.log");
    fs::write(&rewritten, vec![b'r'; 1_024]).expect("write");
    // Fresh file two levels down: only `old_sub`'s own mtime moves.
    let nested_fresh = old_sub.join("just_created.tmp");
    fs::write(&nested_fresh, vec![b'n'; 512]).expect("write");

    let month_ago = FileTime::from_unix_time(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64
            - 30 * 24 * 3600,
        0,
    );
    set_file_mtime(&old_pkg, month_ago).expect("backdate dir");
    set_file_mtime(&old_sub, month_ago).expect("backdate subdir");
    set_file_mtime(&old_file, month_ago).expect("backdate file");
    // `rewritten` keeps its CURRENT mtime: an in-place rewrite refreshes the
    // file itself while leaving every ancestor directory untouched.

    let options = CleanOptions {
        custom_path: Some(old_pkg.clone()),
        older_than: Some(chrono::Duration::hours(1)),
        apply: true,
        yes: true,
        ..CleanOptions::default()
    };

    let plan = create_clean_plan(&options).expect("plan");
    let planned: Vec<_> = plan.items.iter().map(|item| item.path.clone()).collect();

    assert!(
        planned.contains(&old_file),
        "genuinely ancient file must be selected: {planned:?}"
    );
    assert!(
        !planned.iter().any(|p| *p == old_pkg || *p == old_sub),
        "directories must never be whole units under an age window: {planned:?}"
    );

    let report = execute_clean_plan(&plan, false).expect("execute");
    assert_eq!(report.errors_count, 0);
    assert!(!old_file.exists(), "ancient file deleted");
    assert!(rewritten.exists(), "fresh rewrite survived");
    assert!(nested_fresh.exists(), "fresh nested file survived");
    assert!(
        old_sub.is_dir(),
        "parent dirs remain after file-level clean"
    );
}

// ---------------------------------------------------------------------------
// one-file-system execution + trash-mode symlink quarantine
// ---------------------------------------------------------------------------

#[test]
fn plan_carries_one_file_system_flag_and_execution_stays_bottom_up() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().join("ofs-cache");
    // Nested enough that contents_first ordering is exercised: leaves vanish
    // before their parents all the way up.
    let deep = root.join("l1/l2/l3/l4");
    fs::create_dir_all(&deep).expect("mkdirs");
    for i in 0..6 {
        fs::write(deep.join(format!("f{i}.bin")), vec![0u8; 512]).expect("write");
    }

    let options = CleanOptions {
        custom_path: Some(root.clone()),
        apply: true,
        one_file_system: true,
        ..CleanOptions::default()
    };
    let plan = create_clean_plan(&options).expect("plan");
    assert!(plan.one_file_system, "flag must ride on the plan");

    let report = execute_clean_plan(&plan, false).expect("execute");
    assert_eq!(report.errors_count, 0, "{report:?}");
    // Deletion units are the root's direct children; the walkdir bottom-up
    // path must still take the whole l1 subtree.
    assert!(!root.join("l1").exists(), "subtree should be fully removed");
    assert_eq!(report.items_freed, 1);
}
