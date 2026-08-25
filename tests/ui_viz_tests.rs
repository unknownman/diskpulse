//! Rendering-correctness tests for the `viz` visualization engine.
//!
//! Trees are built in memory (no filesystem), so byte counts are exact and
//! deterministic regardless of platform block allocation.

use std::path::PathBuf;

use diskpulse::models::{DirectoryNode, ScanResult, ScanSummary};
use diskpulse::scanner::{ScanOptions, SortCriterion};
use diskpulse::ui::{capacity_bar, format_viz_json, format_viz_tree};

fn opts() -> ScanOptions {
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

fn dir(name: &str) -> DirectoryNode {
    DirectoryNode::new(name.to_string(), PathBuf::from(name), true)
}

fn file(name: &str, size: u64) -> DirectoryNode {
    let mut node = DirectoryNode::new(name.to_string(), PathBuf::from(name), false);
    node.size = size;
    node.apparent_size = size;
    node
}

/// project/
/// ├── src/            (6144)
/// │   └── assets/     (2048)
/// │       └── logo.png
/// └── README.md
fn fixture() -> ScanResult {
    let mut src = dir("src");
    src.add_child(file("main.rs", 4096));

    let mut assets = dir("assets");
    assets.add_child(file("logo.png", 2048));
    src.add_child(assets);

    let mut root = dir("project");
    root.add_child(src);
    root.add_child(file("README.md", 2048));

    ScanResult {
        summary: ScanSummary {
            root_path: PathBuf::from("/tmp/project"),
            total_size: root.size,
            total_apparent_size: root.apparent_size,
            total_files: root.file_count,
            total_dirs: root.dir_count,
            duration_ms: 7,
            errors_count: 0,
        },
        root,
    }
}

#[test]
fn tree_uses_unicode_branch_glyphs_and_hierarchy() {
    let tree = format_viz_tree(&fixture(), &opts(), true);

    assert!(tree.contains("├── 📁 src"), "first child uses ├:\n{tree}");
    assert!(
        tree.contains("└── 📄 README.md"),
        "last child uses └:\n{tree}"
    );
    // Descendants of a non-last child continue with the vertical rail.
    assert!(tree.contains("│   └── 📁 assets"), "\n{tree}");
    // Grandchildren nest one rail segment deeper.
    let logo_line = tree.lines().find(|l| l.contains("logo.png")).unwrap();
    assert!(
        logo_line.starts_with("│       └── 📄 logo.png"),
        "unexpected nesting: {logo_line:?}"
    );

    // Root card and per-directory counts render.
    assert!(tree.contains("📁 /tmp/project (Total: 8.00 KB, Apparent: 8.00 KB)"));
    let src_line = tree.lines().find(|l| l.contains("📁 src")).unwrap();
    assert!(src_line.contains("[1 dirs, 2 files]"), "{src_line:?}");

    // Footer rule and summary line.
    assert!(tree.contains("Summary: 8.00 KB allocated across 3 files and 2 directories"));
    assert!(tree.contains('─'));
}

#[test]
fn capacity_bar_is_proportional_and_fixed_width() {
    assert_eq!(capacity_bar(0.0), "░░░░░░░░░░");
    assert_eq!(capacity_bar(50.0), "█████░░░░░");
    assert_eq!(capacity_bar(100.0), "██████████");

    // Rounding to nearest block, clamped into range.
    assert_eq!(capacity_bar(78.3), "████████░░");
    assert_eq!(capacity_bar(-5.0), "░░░░░░░░░░");
    assert_eq!(capacity_bar(250.0), "██████████");

    // Every bar is exactly BAR_WIDTH blocks wide.
    for pct in [7.5_f64, 33.3, 66.6, 99.9] {
        let blocks = capacity_bar(pct).chars().count();
        assert_eq!(blocks, 10, "bar for {pct}% has wrong width");
    }
}

#[test]
fn no_color_renders_without_ansi_escapes() {
    let plain = format_viz_tree(&fixture(), &opts(), true);
    assert!(!plain.contains("\x1b["), "escape found in plain output");

    // Structural content is identical either way apart from styling.
    assert!(plain.contains("├── 📁 src"));
    assert!(plain.contains("Scanned in 7ms"));
}

#[test]
fn viz_json_is_well_formed_and_complete() {
    let json = format_viz_json(&fixture()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(value["summary"]["total_size"].as_u64(), Some(8192));
    assert_eq!(value["summary"]["total_files"].as_u64(), Some(3));
    assert_eq!(value["summary"]["total_dirs"].as_u64(), Some(2));
    assert_eq!(value["summary"]["root_path"], "/tmp/project");

    let children = value["root"]["children"].as_array().unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0]["name"], "src");
    assert_eq!(children[0]["is_dir"], serde_json::json!(true));
    assert_eq!(children[0]["size"].as_u64(), Some(6144));
    assert_eq!(children[1]["name"], "README.md");
    assert_eq!(children[1]["is_dir"], serde_json::json!(false));
}
