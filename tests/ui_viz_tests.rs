//! Rendering-correctness tests for the `viz` visualization engine.
//!
//! Trees are built in memory (no filesystem), so byte counts are exact and
//! deterministic regardless of platform block allocation.

use std::path::PathBuf;

use diskpulse::models::{DirectoryNode, ScanResult, ScanSummary};
use diskpulse::scanner::{ScanOptions, SortCriterion};
use diskpulse::ui::{capacity_bar, display_width, format_viz_json, format_viz_tree};

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

#[test]
fn confirmation_and_banner_wording_tracks_trash_mode() {
    use diskpulse::ui::{apply_warning, confirmation_prompt};

    assert_eq!(
        confirmation_prompt(7, false),
        "Permanently delete 7 items of cached data?"
    );
    assert_eq!(
        confirmation_prompt(7, true),
        "Move 7 items of cached data to the trash/recycle bin?"
    );

    assert_eq!(
        apply_warning("21.05 MB", "420", false),
        "Applying will permanently delete 21.05 MB across 420 items."
    );
    assert_eq!(
        apply_warning("21.05 MB", "420", true),
        "Applying will move 21.05 MB across 420 items to the trash."
    );
}

// ---------------------------------------------------------------------------
// Aggregated pruned summary rows (--top / --min-size)
// ---------------------------------------------------------------------------

/// project/            --top 2
/// ├── 📁 big-a/
/// │   └── payload.bin
/// ├── 📁 big-b/
/// │   └── payload.bin
/// └── ⋯ and 3 other items (36.00 KB total)
fn five_dir_fixture() -> ScanResult {
    let mut root = dir("project");
    for (name, size) in [
        ("big-a", 20_480_u64),
        ("big-b", 16_384),
        ("mid-c", 12_288),
        ("mid-d", 8_192),
        ("tiny-e", 4_096),
    ] {
        let mut d = dir(name);
        d.add_child(file("payload.bin", size));
        root.add_child(d);
    }
    ScanResult {
        summary: ScanSummary {
            root_path: PathBuf::from("/tmp/project"),
            total_size: root.size,
            total_apparent_size: root.apparent_size,
            total_files: root.file_count,
            total_dirs: root.dir_count,
            duration_ms: 3,
            errors_count: 0,
        },
        root,
    }
}

#[test]
fn truncated_levels_render_aggregated_pruned_row() {
    let mut result = five_dir_fixture();
    result.root.sort_by_size_descending();
    result.root.retain_top_n(2);

    let tree = format_viz_tree(&result, &opts(), true);

    assert!(
        tree.contains("⋯ and 3 other items ("),
        "missing summary row:\n{tree}"
    );
    assert!(tree.contains("(24.00 KB total)"), "\n{tree}");

    // Glyph balance: visible siblings keep ├; only the summary closes with └.
    for name in ["big-a", "big-b"] {
        let line = tree.lines().find(|l| l.contains(name)).unwrap();
        assert!(
            line.contains("├── "),
            "{name} should not be visually last:\n{line:?}"
        );
    }
    let summary = tree
        .lines()
        .find(|l| l.contains("other items"))
        .expect("summary line");
    assert!(
        summary.starts_with("└── ⋯"),
        "summary row must close the level:\n{summary:?}"
    );

    // The last VISIBLE child is a directory: its rail must continue down to
    // the summary row instead of terminating early.
    let payload_line = tree.lines().find(|l| l.contains("payload.bin")).unwrap();
    assert!(
        payload_line.starts_with("│   "),
        "subtree of last visible dir must stay connected to the summary row:\n{payload_line:?}"
    );
}

#[test]
fn min_size_filtered_levels_render_pruned_row() {
    let mut result = fixture();
    result.root.filter_min_size(3_000);

    let tree = format_viz_tree(&result, &opts(), true);

    // README.md (2048) was dropped from the root level.
    assert!(
        tree.contains("⋯ and 1 other item (2.00 KB total)"),
        "singular wording expected:\n{tree}"
    );
    assert!(!tree.contains("README.md"), "\n{tree}");
    // src survives; totals remain truthful.
    assert!(tree.contains("📁 src"));
    assert!(tree.contains("Total: 8.00 KB"));
}

#[test]
fn fully_filtered_level_still_renders_summary_row() {
    let mut result = fixture();
    result.root.children[1].children.clear(); // assets emptied first
    result.root.filter_min_size(10_000);

    // Every entry below root is gone; the row still explains where bytes went.
    let tree = format_viz_tree(&result, &opts(), true);
    assert!(
        tree.contains("└── ⋯ and"),
        "expected a lone summary row under root:\n{tree}"
    );
}

#[test]
fn wide_unicode_names_keep_bar_column_aligned() {
    let mut root = dir("project");
    for (name, size) in [
        ("ascii.txt", 5_000),
        ("📁_test_文件_🚀.txt", 4_000),
        ("日本語のファイル.txt", 3_000),
        ("микро-файл.dat", 2_000),
    ] {
        root.add_child(file(name, size));
    }

    let tree = format_viz_tree(
        &ScanResult {
            summary: ScanSummary {
                root_path: PathBuf::from("/tmp/project"),
                total_size: root.size,
                total_apparent_size: root.apparent_size,
                total_files: root.file_count,
                total_dirs: root.dir_count,
                duration_ms: 1,
                errors_count: 0,
            },
            root,
        },
        &opts(),
        true,
    );

    // Every entry's proportional bar must start at the same VISUAL COLUMN:
    // cells occupied on the terminal, where CJK/emoji names are two cells
    // per character. Char counts would misalign these lines.
    let offsets: Vec<usize> = tree
        .lines()
        .filter(|l| l.contains('[') && l.contains("%)"))
        .map(|l| {
            let byte_idx = l.find('[').expect("bar bracket");
            display_width(&l[..byte_idx])
        })
        .collect();
    assert_eq!(offsets.len(), 4, "all four entries rendered:\n{tree}");
    let all_same = offsets.iter().all(|o| *o == offsets[0]);
    assert!(all_same, "misaligned bar columns at {offsets:?}:\n{tree}");

    // The width estimator itself: wide chars count double, combining marks
    // and zero-width joiners count for nothing.
    assert_eq!(display_width("ascii"), 5);
    assert_eq!(display_width("日本"), 4);
    assert_eq!(display_width("🚀"), 2);
    assert_eq!(display_width("e\u{301}"), 1); // e + combining acute
}

#[test]
fn display_width_matches_char_count_only_for_ascii() {
    use diskpulse::ui::display_width;

    // Guards the discriminator: these names have MORE cells than chars,
    // so the old chars().count() padding could never have aligned them.
    let name = "📁_test_文件_🚀.txt";
    assert!(
        display_width(name) > name.chars().count(),
        "test fixture lost its wide characters"
    );
}
