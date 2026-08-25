//! Core domain models shared between the visualizer, cleaner, UI formatting
//! and JSON output pipelines.
//!
//! These types are pure data plus aggregation helpers: no I/O happens here.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// File system & visualizer models
// ---------------------------------------------------------------------------

/// The kind of a file system entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// A single file system entry observed during a scan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    /// Allocated physical bytes (or apparent bytes, per scan mode).
    pub size: u64,
    /// Logical byte length reported by the file system.
    pub apparent_size: u64,
    pub kind: EntryKind,
    pub modified: Option<DateTime<Utc>>,
}

/// A node of the hierarchical disk-usage tree.
///
/// Aggregates are maintained incrementally: leaf file nodes carry
/// `file_count == 1` and directories grow through [`DirectoryNode::add_child`].
/// `dir_count` counts directories strictly below a node (excluding itself).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectoryNode {
    pub name: String,
    pub path: PathBuf,
    /// Accumulated allocated size of this subtree.
    pub size: u64,
    /// Accumulated logical size of this subtree.
    pub apparent_size: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub is_dir: bool,
    pub children: Vec<DirectoryNode>,
    /// Siblings hidden at this level by display filters (`--top`, `--min-size`),
    /// counted as direct-child units. Purely presentational: excluded from
    /// every size/count aggregate above.
    #[serde(default)]
    pub pruned_entries: u64,
    /// Accumulated size of those hidden siblings.
    #[serde(default)]
    pub pruned_size: u64,
}

impl DirectoryNode {
    /// Create an empty node. File nodes start with `file_count == 1`;
    /// directory nodes start with zeroed aggregates and no children.
    pub fn new(name: String, path: PathBuf, is_dir: bool) -> Self {
        Self {
            name,
            path,
            size: 0,
            apparent_size: 0,
            file_count: u64::from(!is_dir),
            dir_count: 0,
            is_dir,
            children: Vec::new(),
            pruned_entries: 0,
            pruned_size: 0,
        }
    }

    /// Append `child`, folding its aggregates into this node.
    pub fn add_child(&mut self, child: DirectoryNode) {
        self.size += child.size;
        self.apparent_size += child.apparent_size;
        self.file_count += child.file_count;
        self.dir_count += child.dir_count + u64::from(child.is_dir);
        self.children.push(child);
    }

    /// Total number of entries beneath this node (files + directories).
    pub fn item_total(&self) -> u64 {
        self.file_count + self.dir_count
    }

    /// Recursively sort children by accumulated size, largest first.
    /// The sort is stable, so equal sizes keep their insertion order.
    pub fn sort_by_size_descending(&mut self) {
        self.children.sort_by(|a, b| b.size.cmp(&a.size));
        for child in &mut self.children {
            child.sort_by_size_descending();
        }
    }

    /// Recursively sort children alphabetically by name.
    pub fn sort_by_name(&mut self) {
        self.children.sort_by(|a, b| a.name.cmp(&b.name));
        for child in &mut self.children {
            child.sort_by_name();
        }
    }

    /// Recursively sort children by total entry count, highest first.
    pub fn sort_by_count_descending(&mut self) {
        self.children
            .sort_by_key(|child| std::cmp::Reverse(child.item_total()));
        for child in &mut self.children {
            child.sort_by_count_descending();
        }
    }

    /// Recursively drop children whose accumulated `size` is below `min_size`.
    ///
    /// Ancestor aggregates intentionally remain untouched: they continue to
    /// report the true totals of the underlying tree, mirroring `du -t`
    /// semantics where hidden entries still count toward parents. Dropped
    /// children are tallied in `pruned_entries`/`pruned_size` so renderers
    /// can surface an aggregated "hidden items" row.
    pub fn filter_min_size(&mut self, min_size: u64) {
        for child in &mut self.children {
            child.filter_min_size(min_size);
        }
        let mut kept = Vec::with_capacity(self.children.len());
        for child in self.children.drain(..) {
            if child.size >= min_size {
                kept.push(child);
            } else {
                self.pruned_entries += 1;
                self.pruned_size += child.size;
            }
        }
        self.children = kept;
    }

    /// Clear all children at depth `max_depth` and beyond.
    /// The root sits at `current_depth == 0`.
    pub fn truncate_depth(&mut self, current_depth: usize, max_depth: usize) {
        if current_depth >= max_depth {
            self.children.clear();
            return;
        }
        for child in &mut self.children {
            child.truncate_depth(current_depth + 1, max_depth);
        }
    }

    /// Keep only the first `n` children at every level, following each
    /// level's current order (pair with a `sort_by_*` call for "top N").
    /// Dropped children are tallied in `pruned_entries`/`pruned_size`.
    pub fn retain_top_n(&mut self, n: usize) {
        if self.children.len() > n {
            let dropped = self.children.split_off(n);
            self.pruned_entries += dropped.len() as u64;
            self.pruned_size += dropped.iter().map(|child| child.size).sum::<u64>();
        }
        for child in &mut self.children {
            child.retain_top_n(n);
        }
    }
}

/// Aggregate statistics describing a completed scan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanSummary {
    pub root_path: PathBuf,
    pub total_size: u64,
    pub total_apparent_size: u64,
    pub total_files: u64,
    pub total_dirs: u64,
    pub duration_ms: u64,
    pub errors_count: usize,
}

/// Top-level payload produced by `diskpulse viz` (also its JSON shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanResult {
    pub summary: ScanSummary,
    pub root: DirectoryNode,
}

// ---------------------------------------------------------------------------
// Cleaner models
// ---------------------------------------------------------------------------

/// One file/folder identified for removal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CleanItem {
    pub path: PathBuf,
    pub size: u64,
    /// Registry identifier of the owning target (e.g. `"cargo-cache"`).
    pub target_id: String,
    /// Human-readable target name (e.g. `"Cargo Cache"`).
    pub target_name: String,
    pub is_dir: bool,
    pub modified: Option<DateTime<Utc>>,
}

/// Aggregated breakdown of planned work per target category.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetSummary {
    pub target_id: String,
    pub target_name: String,
    /// Representative root path of the matched items (first item's location).
    pub path: PathBuf,
    pub item_count: usize,
    pub total_bytes: u64,
}

/// Produced before any deletion: the exact set of actions a run would take.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CleanPlan {
    pub items: Vec<CleanItem>,
    pub targets: Vec<TargetSummary>,
    pub total_bytes: u64,
    pub total_items: usize,
    pub is_dry_run: bool,
}

impl CleanPlan {
    /// Build a plan from discovered items, computing totals and grouping the
    /// per-target breakdown automatically (summaries ordered by `target_id`).
    pub fn from_items(items: Vec<CleanItem>, is_dry_run: bool) -> Self {
        let total_items = items.len();
        let total_bytes = items.iter().map(|item| item.size).sum();

        let mut grouped: BTreeMap<&str, (&str, PathBuf, usize, u64)> = BTreeMap::new();
        for item in &items {
            let entry = grouped.entry(item.target_id.as_str()).or_insert((
                item.target_name.as_str(),
                item.path.clone(),
                0,
                0,
            ));
            entry.2 += 1;
            entry.3 += item.size;
        }

        let targets = grouped
            .into_iter()
            .map(|(id, (name, path, count, bytes))| TargetSummary {
                target_id: id.to_owned(),
                target_name: name.to_owned(),
                path,
                item_count: count,
                total_bytes: bytes,
            })
            .collect();

        Self {
            items,
            targets,
            total_bytes,
            total_items,
            is_dry_run,
        }
    }
}

/// Outcome recorded for one planned item after execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanItemStatus {
    Deleted,
    MovedToTrash,
    SkippedDryRun,
    Failed(String),
}

impl std::fmt::Display for CleanItemStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deleted => write!(f, "deleted"),
            Self::MovedToTrash => write!(f, "moved to trash"),
            Self::SkippedDryRun => write!(f, "skipped (dry run)"),
            Self::Failed(reason) => write!(f, "failed: {reason}"),
        }
    }
}

/// Per-item execution result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CleanItemResult {
    pub path: PathBuf,
    pub size: u64,
    pub target_id: String,
    pub status: CleanItemStatus,
}

/// Output of a cleaning execution, referencing the plan it fulfilled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CleanReport {
    pub plan: CleanPlan,
    pub results: Vec<CleanItemResult>,
    pub bytes_freed: u64,
    pub items_freed: usize,
    pub errors_count: usize,
    pub duration_ms: u64,
    pub dry_run: bool,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::{CleanError, DiskPulseError, ParseError, SafetyError, ScanError};
    use chrono::TimeZone;

    fn dir_node(name: &str) -> DirectoryNode {
        DirectoryNode::new(name.to_string(), PathBuf::from(name), true)
    }

    fn file_node(name: &str, size: u64, apparent: u64) -> DirectoryNode {
        let mut node = DirectoryNode::new(name.to_string(), PathBuf::from(name), false);
        node.size = size;
        node.apparent_size = apparent;
        node
    }

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).single().unwrap()
    }

    // -- Tree aggregation ---------------------------------------------------

    #[test]
    fn add_child_aggregates_recursively() {
        let mut docs = dir_node("docs");
        docs.add_child(file_node("a.txt", 100, 100));
        docs.add_child(file_node("b.bin", 200, 320));
        assert_eq!(docs.size, 300);
        assert_eq!(docs.apparent_size, 420);
        assert_eq!(docs.file_count, 2);
        assert_eq!(docs.dir_count, 0);

        let mut pics = dir_node("pics");
        pics.add_child(file_node("c.raw", 40, 40));

        let mut root = dir_node("root");
        root.add_child(docs);
        root.add_child(pics);
        root.add_child(file_node("top.txt", 7, 7));

        assert_eq!(root.size, 300 + 40 + 7);
        assert_eq!(root.apparent_size, 420 + 40 + 7);
        assert_eq!(root.file_count, 4);
        assert_eq!(root.dir_count, 2);
    }

    #[test]
    fn file_leaf_counts_itself() {
        let leaf = file_node("x", 1, 1);
        assert_eq!(leaf.file_count, 1);
        assert_eq!(leaf.dir_count, 0);

        let empty_dir = dir_node("empty");
        assert_eq!(empty_dir.file_count, 0);
        assert_eq!(empty_dir.dir_count, 0);
    }

    // -- Sorting ------------------------------------------------------------

    #[test]
    fn sort_by_size_descending_orders_every_level() {
        let mut inner = dir_node("inner");
        inner.add_child(file_node("small", 5, 5));
        inner.add_child(file_node("large", 45, 45));

        let mut root = dir_node("root");
        root.add_child(inner); // size 50
        root.add_child(file_node("tiny", 10, 10));
        root.add_child(file_node("big", 70, 70));

        root.sort_by_size_descending();

        let names: Vec<&str> = root.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["big", "inner", "tiny"]);

        let inner_names: Vec<&str> = root.children[1]
            .children
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(inner_names, vec!["large", "small"]);
    }

    #[test]
    fn sort_by_name_orders_alphabetically() {
        let mut nested = dir_node("nested");
        nested.add_child(file_node("zeta", 1, 1));
        nested.add_child(file_node("beta", 1, 1));

        let mut root = dir_node("root");
        root.add_child(file_node("mike", 1, 1));
        root.add_child(file_node("alpha", 1, 1));
        root.add_child(nested);

        root.sort_by_name();

        let names: Vec<&str> = root.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mike", "nested"]);
        let nested_names: Vec<&str> = root.children[2]
            .children
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(nested_names, vec!["beta", "zeta"]);
    }

    #[test]
    fn sort_by_count_descending_keeps_ties_stable() {
        let make = |name: &str, files: usize| {
            let mut d = dir_node(name);
            for i in 0..files {
                d.add_child(file_node(&format!("f{i}"), 1, 1));
            }
            d
        };

        let mut root = dir_node("root");
        root.add_child(make("one", 1));
        root.add_child(make("three", 3));
        root.add_child(make("two", 2));
        root.add_child(make("one-b", 1));
        root.add_child(make("one-c", 1));

        root.sort_by_count_descending();

        let names: Vec<&str> = root.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["three", "two", "one", "one-b", "one-c"]);
    }

    // -- Filtering & truncation ----------------------------------------------

    #[test]
    fn filter_min_size_prunes_subtrees_recursively() {
        let mut medium = dir_node("medium");
        medium.add_child(file_node("dust", 5, 5));
        medium.add_child(file_node("keeper", 150, 150));

        let mut root = dir_node("root");
        root.add_child(file_node("big", 500, 500));
        root.add_child(file_node("small", 10, 10));
        root.add_child(medium);

        let original_root_size = root.size;
        root.filter_min_size(100);

        let names: Vec<&str> = root.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["big", "medium"]);

        let medium = &root.children[1];
        let inner: Vec<&str> = medium.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(inner, vec!["keeper"]);

        assert_eq!(root.size, original_root_size);
    }

    #[test]
    fn retain_top_n_limits_each_level_without_corruption() {
        let mut kept = dir_node("kept");
        kept.add_child(file_node("k1", 1, 1));
        kept.add_child(file_node("k2", 2, 2));
        kept.add_child(file_node("k3", 3, 3));

        let mut root = dir_node("root");
        root.add_child(kept);
        root.add_child(file_node("second", 9, 9));
        root.add_child(file_node("third", 8, 8));

        let kept_size_before = root.children[0].size;
        root.retain_top_n(2);

        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].name, "kept");
        assert_eq!(root.children[1].name, "second");

        let kept = &root.children[0];
        assert_eq!(kept.children.len(), 2);
        assert_eq!(kept.size, kept_size_before);
        assert_eq!(kept.children[0].name, "k1");
    }

    #[test]
    fn retain_top_n_tallies_hidden_siblings_per_level() {
        let mut inner = dir_node("inner");
        for i in 0..4 {
            inner.add_child(file_node(&format!("f{i}"), 10, 10));
        }

        let mut root = dir_node("root");
        root.add_child(inner);
        root.add_child(file_node("a", 100, 100));
        root.add_child(file_node("b", 90, 90));
        root.add_child(file_node("c", 80, 80));

        root.retain_top_n(2);

        // Root level: 4 children -> 2 kept, 2 dropped.
        assert_eq!(root.pruned_entries, 2);
        assert_eq!(root.pruned_size, 80 + 90);
        // Inner level: 4 files -> top 2 kept, independent tally.
        let inner = &root.children[0];
        assert_eq!(inner.pruned_entries, 2);
        assert_eq!(inner.pruned_size, 20);
        // Aggregates stay untouched by display filtering.
        assert_eq!(root.size, 310);
    }

    #[test]
    fn filter_min_size_tallies_dropped_entries() {
        let mut root = dir_node("root");
        root.add_child(file_node("big", 500, 500));
        root.add_child(file_node("dust1", 5, 5));
        root.add_child(file_node("dust2", 7, 7));

        root.filter_min_size(100);

        assert_eq!(root.pruned_entries, 2);
        assert_eq!(root.pruned_size, 12);
        assert_eq!(root.size, 512, "aggregates keep reporting truth");
    }

    #[test]
    fn truncate_depth_strips_grandchildren_but_keeps_children() {
        let mut child_a = dir_node("a");
        child_a.add_child(file_node("grandchild", 4, 4));
        let mut child_b = dir_node("b");
        child_b.add_child(file_node("grandchild", 6, 6));

        let mut root = dir_node("root");
        root.add_child(child_a);
        root.add_child(child_b);

        root.truncate_depth(0, 1);

        assert_eq!(root.children.len(), 2);
        assert!(root.children.iter().all(|c| c.children.is_empty()));
    }

    #[test]
    fn truncate_depth_at_max_clears_immediately() {
        let mut root = dir_node("root");
        root.add_child(file_node("only", 1, 1));
        root.truncate_depth(0, 0);
        assert!(root.children.is_empty());
    }

    // -- CleanPlan calculation ------------------------------------------------

    fn clean_item(path: &str, size: u64, id: &str, name: &str) -> CleanItem {
        CleanItem {
            path: PathBuf::from(path),
            size,
            target_id: id.to_string(),
            target_name: name.to_string(),
            is_dir: false,
            modified: None,
        }
    }

    #[test]
    fn from_items_totals_and_groups_by_target() {
        let items = vec![
            clean_item("/cache/a", 100, "cargo-cache", "Cargo Cache"),
            clean_item("/npm/x", 400, "npm-cache", "npm Cache"),
            clean_item("/cache/b", 250, "cargo-cache", "Cargo Cache"),
        ];

        let plan = CleanPlan::from_items(items, true);
        assert_eq!(plan.total_bytes, 750);
        assert_eq!(plan.total_items, 3);
        assert!(plan.is_dry_run);
        assert_eq!(plan.items.len(), 3);

        assert_eq!(plan.targets.len(), 2);
        assert_eq!(plan.targets[0].target_id, "cargo-cache");
        assert_eq!(plan.targets[0].target_name, "Cargo Cache");
        assert_eq!(plan.targets[0].item_count, 2);
        assert_eq!(plan.targets[0].total_bytes, 350);
        assert_eq!(plan.targets[1].target_id, "npm-cache");
        assert_eq!(plan.targets[1].item_count, 1);
        assert_eq!(plan.targets[1].total_bytes, 400);
    }

    #[test]
    fn from_items_empty_plan_is_zeroed() {
        let plan = CleanPlan::from_items(Vec::new(), false);
        assert_eq!(plan.total_bytes, 0);
        assert_eq!(plan.total_items, 0);
        assert!(plan.targets.is_empty());
        assert!(!plan.is_dry_run);
    }

    // -- Serialization roundtrips ---------------------------------------------

    #[test]
    fn scan_result_json_roundtrip() {
        let mut inner = dir_node("src");
        inner.add_child(file_node("main.rs", 1200, 1300));
        let mut root = dir_node("project");
        root.add_child(inner);
        root.add_child(file_node("README.md", 300, 310));

        let result = ScanResult {
            summary: ScanSummary {
                root_path: PathBuf::from("/data/project"),
                total_size: root.size,
                total_apparent_size: root.apparent_size,
                total_files: root.file_count,
                total_dirs: root.dir_count,
                duration_ms: 42,
                errors_count: 0,
            },
            root,
        };

        let json = serde_json::to_string_pretty(&result).unwrap();
        let parsed: ScanResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, result);
    }

    #[test]
    fn file_entry_and_entry_kind_roundtrip() {
        let entry = FileEntry {
            path: PathBuf::from("/tmp/link"),
            name: "link".to_string(),
            size: 12,
            apparent_size: 34,
            kind: EntryKind::Symlink,
            modified: Some(ts(1_700_000_000)),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: FileEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, entry);
        assert!(json.contains("\"symlink\""));
    }

    #[test]
    fn clean_report_json_roundtrip_preserves_statuses() {
        let plan = CleanPlan::from_items(
            vec![
                clean_item("/cache/a", 100, "cargo-cache", "Cargo Cache"),
                clean_item("/npm/x", 400, "npm-cache", "npm Cache"),
            ],
            false,
        );

        let report = CleanReport {
            plan,
            results: vec![
                CleanItemResult {
                    path: PathBuf::from("/cache/a"),
                    size: 100,
                    target_id: "cargo-cache".to_string(),
                    status: CleanItemStatus::Deleted,
                },
                CleanItemResult {
                    path: PathBuf::from("/npm/x"),
                    size: 400,
                    target_id: "npm-cache".to_string(),
                    status: CleanItemStatus::MovedToTrash,
                },
                CleanItemResult {
                    path: PathBuf::from("/skipped"),
                    size: 7,
                    target_id: "system-temp".to_string(),
                    status: CleanItemStatus::SkippedDryRun,
                },
                CleanItemResult {
                    path: PathBuf::from("/locked"),
                    size: 9,
                    target_id: "system-temp".to_string(),
                    status: CleanItemStatus::Failed("file locked".to_string()),
                },
            ],
            bytes_freed: 100,
            items_freed: 1,
            errors_count: 1,
            duration_ms: 250,
            dry_run: false,
        };

        let json = serde_json::to_string_pretty(&report).unwrap();
        let parsed: CleanReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, report);
        assert!(json.contains("\"moved_to_trash\""));
        assert!(json.contains("\"skipped_dry_run\""));
    }

    #[test]
    fn status_display_strings() {
        assert_eq!(CleanItemStatus::Deleted.to_string(), "deleted");
        assert_eq!(CleanItemStatus::MovedToTrash.to_string(), "moved to trash");
        assert_eq!(
            CleanItemStatus::SkippedDryRun.to_string(),
            "skipped (dry run)"
        );
        assert_eq!(
            CleanItemStatus::Failed("busy".to_string()).to_string(),
            "failed: busy"
        );
    }

    // -- Error display messages -----------------------------------------------

    #[test]
    fn safety_error_messages_are_actionable() {
        assert!(SafetyError::ProtectedSystemPath(PathBuf::from("/"))
            .to_string()
            .contains("protected system path \"/\""));

        let home_msg = SafetyError::ProtectedHomeRoot(PathBuf::from("/home/alice")).to_string();
        assert!(home_msg.contains("home directory root"));
        assert!(home_msg.contains("~/.cache"));

        assert!(
            SafetyError::ProtectedUserData(PathBuf::from("/home/alice/Documents"))
                .to_string()
                .contains("personal data")
        );

        let escape = SafetyError::SymlinkEscape {
            link: PathBuf::from("/scan/evil"),
            target: PathBuf::from("/etc/shadow"),
        };
        let msg = escape.to_string();
        assert!(msg.contains("/scan/evil") && msg.contains("/etc/shadow"));

        assert_eq!(
            SafetyError::InvalidCliCombination("--yes requires --apply".to_string()).to_string(),
            "--yes requires --apply"
        );
    }

    #[test]
    fn parse_error_messages_include_input_and_reason() {
        let err = ParseError::InvalidByteSize {
            input: "100ZB".to_string(),
            reason: "unknown unit".to_string(),
        };
        assert_eq!(err.to_string(), "invalid byte size \"100ZB\": unknown unit");

        let err = ParseError::InvalidDuration {
            input: "-5d".to_string(),
            reason: "must be non-negative".to_string(),
        };
        assert!(err.to_string().contains("-5d"));
        assert!(err.to_string().contains("non-negative"));

        assert!(ParseError::InvalidSortField("weight".to_string())
            .to_string()
            .contains("invalid sort field \"weight\""));
    }

    #[test]
    fn scan_error_messages_mention_the_path() {
        assert!(ScanError::PathNotFound(PathBuf::from("/nope"))
            .to_string()
            .contains("/nope"));
        assert!(ScanError::PermissionDenied(PathBuf::from("/root"))
            .to_string()
            .contains("permission denied"));
        assert!(ScanError::FilesystemLoopDetected(PathBuf::from("/loop"))
            .to_string()
            .contains("loop"));
    }

    #[test]
    fn clean_error_messages_carry_context() {
        assert!(
            CleanError::TrashUnsupported("no portal available".to_string())
                .to_string()
                .contains("no portal available")
        );

        let err = CleanError::DeletionFailed {
            path: PathBuf::from("/tmp/stuck"),
            source: std::io::Error::other("device busy"),
        };
        let msg = err.to_string();
        assert!(msg.contains("failed to delete /tmp/stuck"));
        assert!(msg.contains("device busy"));

        assert!(CleanError::TargetNotFound("bogus".to_string())
            .to_string()
            .contains("bogus"));
    }

    #[test]
    fn child_errors_convert_into_top_level_error() {
        let safety = SafetyError::ProtectedSystemPath(PathBuf::from("/"));
        let expected = safety.to_string();
        let top = DiskPulseError::from(safety);
        assert!(matches!(top, DiskPulseError::Safety(_)));
        assert_eq!(top.to_string(), expected);

        let parse = ParseError::InvalidSortField("x".to_string());
        let expected = parse.to_string();
        let top = DiskPulseError::from(parse);
        assert!(matches!(top, DiskPulseError::Parse(_)));
        assert_eq!(top.to_string(), expected);

        let scan = ScanError::PathNotFound(PathBuf::from("/gone"));
        let expected = scan.to_string();
        let top = DiskPulseError::from(scan);
        assert!(matches!(top, DiskPulseError::Scan(_)));
        assert_eq!(top.to_string(), expected);

        let clean = CleanError::TargetNotFound("zz".to_string());
        let expected = clean.to_string();
        let top = DiskPulseError::from(clean);
        assert!(matches!(top, DiskPulseError::Clean(_)));
        assert_eq!(top.to_string(), expected);

        let io_error = std::io::Error::other("boom");
        let expected = io_error.to_string();
        let top = DiskPulseError::from(io_error);
        assert!(matches!(top, DiskPulseError::Io(_)));
        assert_eq!(top.to_string(), expected);
    }
}
