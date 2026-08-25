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
    assert_eq!(names(&result.root), vec!["node_modules", "src"]);
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
    assert_eq!(names(&result.root), vec![".secret_dir", "public_dir"]);
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
