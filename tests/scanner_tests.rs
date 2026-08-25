//! Integration tests for the parallel filesystem scanner.
//!
//! File sizes are multiples of 4096 so allocated-block math stays exact on
//! common platforms (APFS/ext4 allocate whole 4 KiB blocks).

use std::fs;
use std::io::Write;
use std::path::Path;

use tempfile::TempDir;

use diskpulse::errors::{DiskPulseError, ScanError};
use diskpulse::models::DirectoryNode;
use diskpulse::scanner::{scan_path, ScanOptions, SortCriterion};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;

fn base_options() -> ScanOptions {
    ScanOptions {
        max_depth: usize::MAX,
        min_size: None,
        top_n: None,
        apparent_size: false,
        no_ignore: true,
        include_hidden: true,
        one_file_system: false,
        sort_by: SortCriterion::Size,
        exclude_patterns: Vec::new(),
    }
}

fn write_file(root: &Path, relative: &str, len: u64) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(&vec![0_u8; len as usize]).unwrap();
}

fn child<'a>(node: &'a DirectoryNode, name: &str) -> &'a DirectoryNode {
    node.children
        .iter()
        .find(|child| child.name == name)
        .unwrap_or_else(|| panic!("expected child {name:?} under {:?}", node.path))
}

fn names(node: &DirectoryNode) -> Vec<&str> {
    node.children.iter().map(|c| c.name.as_str()).collect()
}

#[test]
fn aggregates_sizes_and_counts_recursively() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_file(root, "a/file1.bin", 100 * KIB);
    write_file(root, "b/file2.bin", 200 * KIB);

    let result = scan_path(root, &base_options()).unwrap();

    assert_eq!(result.summary.total_files, 2);
    assert_eq!(result.summary.total_dirs, 2);
    assert_eq!(result.root.size, 300 * KIB);
    assert_eq!(result.root.apparent_size, 300 * KIB);
    // Largest first by default.
    assert_eq!(names(&result.root), vec!["b", "a"]);
}

#[test]
fn max_depth_limits_display_but_totals_stay_true() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_file(root, "lvl1/lvl2/lvl3/file.bin", 4 * KIB);

    let options = ScanOptions {
        max_depth: 1,
        ..base_options()
    };
    let result = scan_path(root, &options).unwrap();

    let lvl1 = child(&result.root, "lvl1");
    assert!(result.root.is_dir);
    assert_eq!(names(&result.root), vec!["lvl1"]);
    assert!(
        lvl1.children.is_empty(),
        "depth-1 view must hide deeper levels"
    );

    // Aggregates still reflect the full underlying tree.
    assert_eq!(result.summary.total_size, 4 * KIB);
    assert_eq!(result.summary.total_dirs, 3);
    assert_eq!(result.summary.total_files, 1);
    assert_eq!(result.root.size, 4 * KIB);
    assert_eq!(lvl1.dir_count, 2);
    assert_eq!(lvl1.file_count, 1);
}

#[test]
fn ignored_directories_are_pruned_unless_disabled() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_file(root, "node_modules/package/index.js", 4 * KIB);
    write_file(root, "src/main.rs", 4 * KIB);

    let ignoring = base_options();
    let ignoring = ScanOptions {
        no_ignore: false,
        ..ignoring
    };
    let result = scan_path(root, &ignoring).unwrap();
    assert_eq!(names(&result.root), vec!["src"]);
    assert_eq!(result.root.size, 4 * KIB);

    let result = scan_path(root, &base_options()).unwrap();
    // Equal sizes fall back to readdir order, which differs per platform;
    // only membership matters here (size ordering is pinned elsewhere).
    let mut pruned_off = names(&result.root);
    pruned_off.sort_unstable();
    assert_eq!(pruned_off, vec!["node_modules", "src"]);
    assert_eq!(result.root.size, 8 * KIB);
}

#[test]
fn hidden_entries_are_filtered_per_option() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_file(root, ".secret_dir/data.txt", 4 * KIB);
    write_file(root, "public_dir/data.txt", 4 * KIB);

    let options = ScanOptions {
        include_hidden: false,
        ..base_options()
    };
    let result = scan_path(root, &options).unwrap();
    assert_eq!(names(&result.root), vec!["public_dir"]);
    assert_eq!(result.root.size, 4 * KIB);

    let result = scan_path(root, &base_options()).unwrap();
    let mut visible = names(&result.root);
    visible.sort_unstable();
    assert_eq!(visible, vec![".secret_dir", "public_dir"]);
    assert_eq!(result.root.size, 8 * KIB);
}

#[test]
fn min_size_filters_entries_but_keeps_true_totals() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_file(root, "small.bin", 40 * KIB);
    write_file(root, "medium.bin", 500 * KIB);
    write_file(root, "big.bin", 5 * MIB);

    let options = ScanOptions {
        min_size: Some(1_000_000),
        ..base_options()
    };
    let result = scan_path(root, &options).unwrap();

    assert_eq!(names(&result.root), vec!["big.bin"]);
    // Ancestors keep reporting true totals despite hidden children.
    let true_total = 40 * KIB + 500 * KIB + 5 * MIB;
    assert_eq!(result.root.size, true_total);
    assert_eq!(result.summary.total_size, true_total);
    assert_eq!(result.summary.total_files, 3);
}

#[test]
fn top_n_keeps_largest_children_after_sorting() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    for i in 0..10_u64 {
        write_file(root, &format!("f{i}.bin"), (i + 1) * 4 * KIB);
    }

    let options = ScanOptions {
        top_n: Some(3),
        ..base_options()
    };
    let result = scan_path(root, &options).unwrap();

    assert_eq!(names(&result.root), vec!["f9.bin", "f8.bin", "f7.bin"]);
    // Totals are unaffected by display truncation.
    assert_eq!(result.root.file_count, 10);
    assert_eq!(result.root.size, 220 * KIB);
}

#[test]
#[cfg(unix)]
fn symlink_loop_terminates_without_following() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root, root.join("loop")).unwrap();

    // Must return rather than hang or overflow the stack.
    let result = scan_path(root, &base_options()).unwrap();

    let loop_node = child(&result.root, "loop");
    assert!(
        !loop_node.is_dir,
        "symlinks must not be treated as directories"
    );
    assert_eq!(result.root.dir_count, 0);
}

#[test]
fn missing_root_reports_path_not_found() {
    let err = scan_path(Path::new("/definitely/not/a/real/path"), &base_options())
        .expect_err("missing path must fail");

    assert!(matches!(
        err,
        DiskPulseError::Scan(ScanError::PathNotFound(_))
    ));
}

#[test]
fn scan_target_with_ignored_or_hidden_name_is_not_pruned_as_root() {
    // The scan root itself travels through process_read_dir in its own
    // batch. Filtering by name without a depth-0 exemption would drop it
    // and collapse the entire result.
    for target_name in ["node_modules", ".cache-store"] {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(target_name);
        std::fs::create_dir_all(&root).unwrap();
        write_file(&root, "payload.bin", 8 * KIB);

        let hidden = ScanOptions {
            include_hidden: false,
            ..base_options()
        };
        let result = scan_path(&root, &hidden).unwrap();

        assert_eq!(
            names(&result.root),
            vec!["payload.bin"],
            "root {target_name:?} was pruned by its own name"
        );
        assert_eq!(result.root.size, 8 * KIB);
    }
}

// ---------------------------------------------------------------------------
// --one-file-system coverage (single-device no-op + device_id plumbing)
// ---------------------------------------------------------------------------

#[test]
fn one_file_system_is_a_noop_on_a_single_device_tempdir() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::create_dir(root.join("nested")).expect("mkdir");
    fs::write(root.join("a.bin"), vec![0u8; 4096]).expect("write");
    fs::write(root.join("nested/b.bin"), vec![0u8; 2048]).expect("write");
    fs::write(root.join("nested/c.txt"), b"hello").expect("write");

    let plain = scan_path(root, &base_options()).unwrap();
    let ofs = scan_path(
        root,
        &ScanOptions {
            one_file_system: true,
            ..base_options()
        },
    )
    .unwrap();

    assert_eq!(
        plain.summary.total_size, ofs.summary.total_size,
        "over-pruning on a single filesystem"
    );
    assert_eq!(plain.summary.total_files, ofs.summary.total_files);
    assert_eq!(plain.summary.total_dirs, ofs.summary.total_dirs);
}

#[cfg(unix)]
#[test]
fn one_file_system_option_reads_device_ids_consistently() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::write(root.join("x.bin"), vec![0u8; 1024]).expect("write");
    fs::write(root.join("y.bin"), vec![0u8; 512]).expect("write");

    // device_id takes std Metadata; st_dev is the unix field it wraps.
    let root_dev = diskpulse::util::device_id(&fs::metadata(root).expect("root stat"))
        .expect("root has a device id");
    for name in ["x.bin", "y.bin"] {
        let dev = diskpulse::util::device_id(&fs::metadata(root.join(name)).expect(name))
            .unwrap_or_else(|| panic!("{name} missing device id"));
        assert_eq!(dev, root_dev, "{name} strayed off the root device");
    }
}

#[test]
fn exclude_glob_filters_matching_paths() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    // One full allocation block so the physical-size assertion stays exact.
    fs::write(root.join("keep.txt"), vec![0u8; 4096]).expect("write");
    fs::write(root.join("debug.log"), vec![0u8; 8192]).expect("write");

    let options = ScanOptions {
        exclude_patterns: vec!["*.log".to_string()],
        ..base_options()
    };
    let result = scan_path(root, &options).unwrap();

    let kept = names(&result.root);
    assert!(
        !kept.contains(&"debug.log"),
        "excluded entry survived: {kept:?}"
    );
    assert!(kept.contains(&"keep.txt"));

    // Exclusion happens before aggregation: excluded bytes/files must not
    // leak into totals (unlike --min-size, which only prunes rendering).
    assert_eq!(result.summary.total_files, 1);
    assert_eq!(result.summary.total_size, 4096);
}

// ---------------------------------------------------------------------------
// --top × non-size sorts
// ---------------------------------------------------------------------------

#[test]
fn top_n_with_sort_by_name_keeps_alphabetically_first() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    for name in ["a.bin", "b.bin", "c.bin", "d.bin", "e.bin"] {
        write_file(root, name, 1024);
    }

    let options = ScanOptions {
        sort_by: SortCriterion::Name,
        top_n: Some(3),
        ..base_options()
    };
    let result = scan_path(root, &options).unwrap();

    assert_eq!(names(&result.root), vec!["a.bin", "b.bin", "c.bin"]);
}

#[test]
fn top_n_with_sort_by_count_keeps_dirs_with_most_entries() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    // Size order deliberately differs from count order: dir_a holds the
    // single LARGEST file, dir_b three tiny ones. A size-driven selection
    // would keep dir_a and fail this test.
    write_file(root, "dir_a/huge.bin", 40 * KIB);
    for i in 0..3 {
        write_file(root, &format!("dir_b/tiny{i}.bin"), 4 * KIB);
    }
    for i in 0..2 {
        write_file(root, &format!("dir_c/mid{i}.bin"), 8 * KIB);
    }

    let options = ScanOptions {
        sort_by: SortCriterion::Count,
        top_n: Some(2),
        ..base_options()
    };
    let result = scan_path(root, &options).unwrap();

    assert_eq!(
        names(&result.root),
        vec!["dir_b", "dir_c"],
        "top_n must follow entry count, not accumulated size"
    );
}
