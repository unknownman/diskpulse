//! Filesystem traversal engine.
//!
//! Backed by a parallel [`jwalk`] walker: heavyweight dev directories,
//! dotfiles and foreign mount points are pruned while directories are read
//! (never descending into skipped subtrees), per-entry byte counts are
//! captured through platform allocation-aware metadata on the rayon worker
//! threads, and the resulting entry stream is assembled into a
//! [`DirectoryNode`] tree that downstream rendering post-processes cheaply.
//!
//! Traversal is always exhaustive: [`ScanOptions::max_depth`] limits how much
//! of the finished tree is *displayed*, not how much is measured, so summary
//! totals stay correct regardless of depth.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use jwalk::{Parallelism, WalkDirGeneric};
use serde::Serialize;

use crate::cli::VizArgs;
use crate::errors::{DiskPulseError, ParseError, ScanError};
use crate::models::{DirectoryNode, ScanResult, ScanSummary};
use crate::util;

/// Directory names skipped by default during traversal.
///
/// These are development artifacts that dwarf regular content and are almost
/// never what a user is hunting for; pass `--no-ignore` to include them.
const IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "env",
    ".tox",
    ".next",
    ".nuxt",
    ".output",
    "dist",
    "build",
    "Pods",
    ".gradle",
    "__pycache__",
];

/// Key used to order entries in visualizations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SortCriterion {
    Size,
    Count,
    Name,
}

impl SortCriterion {
    pub fn parse(raw: &str) -> Result<Self, ParseError> {
        match raw.to_ascii_lowercase().as_str() {
            "size" => Ok(Self::Size),
            "count" => Ok(Self::Count),
            "name" => Ok(Self::Name),
            other => Err(ParseError::InvalidSortField(other.to_owned())),
        }
    }
}

/// Tunables for a traversal, derived from [`VizArgs`].
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// How deep the *rendered* tree may go; measurement always covers everything.
    pub max_depth: usize,
    /// Drop entries whose accumulated size is below this threshold.
    pub min_size: Option<u64>,
    /// Keep only the largest N children at every level.
    pub top_n: Option<usize>,
    /// Report logical file lengths instead of allocated blocks.
    pub apparent_size: bool,
    /// Traverse heavyweight dev directories ([`IGNORED_DIRECTORIES`]) too.
    pub no_ignore: bool,
    /// Include hidden files and directories.
    pub include_hidden: bool,
    /// Prune subtrees living on a different device than the scan root.
    pub one_file_system: bool,
    pub sort_by: SortCriterion,
}

impl From<&VizArgs> for ScanOptions {
    fn from(args: &VizArgs) -> Self {
        Self {
            max_depth: args.depth,
            min_size: args
                .min_size
                .as_deref()
                .and_then(|raw| util::parse_size(raw).ok()),
            top_n: Some(args.top),
            apparent_size: args.apparent_size,
            no_ignore: args.no_ignore,
            include_hidden: args.hidden,
            one_file_system: args.one_file_system,
            sort_by: SortCriterion::parse(&args.sort).unwrap_or(SortCriterion::Size),
        }
    }
}

/// Per-entry byte counts captured into jwalk's client-state slot:
/// `(bytes allocated on disk, logical bytes)`.
type EntrySizes = (u64, u64);

/// The configured walker: no shared read-dir state, `(allocated, logical)`
/// state attached to every entry.
type Walker = WalkDirGeneric<((), EntrySizes)>;

/// A flat filesystem entry awaiting placement in the result tree.
struct FlatEntry {
    path: PathBuf,
    name: String,
    sizes: EntrySizes,
    is_dir: bool,
}

/// Walk `path` and package the tree as a [`ScanResult`].
pub fn scan_path(path: &Path, options: &ScanOptions) -> crate::errors::Result<ScanResult> {
    let started = Instant::now();

    let canonical = std::fs::canonicalize(path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => {
            DiskPulseError::Scan(ScanError::PathNotFound(path.to_path_buf()))
        }
        std::io::ErrorKind::PermissionDenied => {
            DiskPulseError::Scan(ScanError::PermissionDenied(path.to_path_buf()))
        }
        _ => err.into(),
    })?;

    let root_meta = std::fs::metadata(&canonical)?;
    let root_dev = util::device_id(&root_meta);
    let errors_count = Arc::new(AtomicUsize::new(0));

    let walker = build_walker(&canonical, options, root_dev, Arc::clone(&errors_count));
    let mut groups = collect_groups(walker, &errors_count)?;

    let mut root = DirectoryNode::new(
        canonical
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| canonical.display().to_string()),
        canonical.clone(),
        root_meta.is_dir(),
    );
    if !root_meta.is_dir() {
        let logical = root_meta.len();
        root.size = if options.apparent_size {
            logical
        } else {
            util::physical_disk_size(&root_meta)
        };
        root.apparent_size = logical;
    }
    assemble_children(&mut root, &mut groups, options.apparent_size);

    if let Some(min_size) = options.min_size {
        root.filter_min_size(min_size);
    }
    match options.sort_by {
        SortCriterion::Size => root.sort_by_size_descending(),
        SortCriterion::Count => root.sort_by_count_descending(),
        SortCriterion::Name => root.sort_by_name(),
    }
    if let Some(top_n) = options.top_n {
        root.retain_top_n(top_n);
    }
    root.truncate_depth(0, options.max_depth);

    let summary = ScanSummary {
        root_path: canonical,
        total_size: root.size,
        total_apparent_size: root.apparent_size,
        total_files: root.file_count,
        total_dirs: root.dir_count,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        errors_count: errors_count.load(Ordering::Relaxed),
    };

    Ok(ScanResult { summary, root })
}

fn build_walker(
    root: &Path,
    options: &ScanOptions,
    root_dev: Option<u64>,
    errors_count: Arc<AtomicUsize>,
) -> Walker {
    // Copied eagerly so the closure owns plain values (`Fn`, called from many
    // threads concurrently).
    let include_hidden = options.include_hidden;
    let no_ignore = options.no_ignore;
    let one_file_system = options.one_file_system;

    WalkDirGeneric::<((), EntrySizes)>::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .parallelism(Parallelism::RayonDefaultPool {
            busy_timeout: std::time::Duration::from_secs(60),
        })
        .process_read_dir(move |depth, _dir_path, _state, children| {
            let _ = depth;
            // Unreadable entries and unreadable directories are counted, not fatal.
            children.retain(|result| match result {
                Err(_) => {
                    errors_count.fetch_add(1, Ordering::Relaxed);
                    false
                }
                Ok(entry) => {
                    if entry.read_children_error.is_some() {
                        errors_count.fetch_add(1, Ordering::Relaxed);
                    }
                    true
                }
            });

            // Name-based filters run for EVERY batch. jwalk delivers the
            // scan-root entry itself in a separate batch; those depth-0
            // entries are exempt from filtering, otherwise walking a target
            // literally named `.cache` or `node_modules` would prune the
            // root and collapse the whole result.
            children.retain(|result| match result {
                Err(_) => false,
                Ok(entry) => {
                    if entry.depth == 0 {
                        return true;
                    }
                    let name = entry.file_name().to_string_lossy();
                    if !include_hidden && name.starts_with('.') {
                        return false;
                    }
                    if !no_ignore && IGNORED_DIRECTORIES.contains(&name.as_ref()) {
                        return false;
                    }
                    true
                }
            });

            if one_file_system {
                children.retain(|result| match result {
                    Err(_) => false,
                    Ok(entry) => {
                        if !entry.file_type().is_dir() {
                            return true;
                        }
                        match entry.metadata() {
                            Ok(meta) => util::device_id(&meta) == root_dev,
                            Err(_) => {
                                errors_count.fetch_add(1, Ordering::Relaxed);
                                false
                            }
                        }
                    }
                });
            }

            for entry in children.iter_mut().flatten() {
                match entry.metadata() {
                    Ok(meta) => {
                        entry.client_state = (util::physical_disk_size(&meta), meta.len());
                    }
                    Err(_) => {
                        errors_count.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
}

fn collect_groups(
    walker: Walker,
    errors_count: &AtomicUsize,
) -> crate::errors::Result<HashMap<PathBuf, Vec<FlatEntry>>> {
    let mut groups: HashMap<PathBuf, Vec<FlatEntry>> = HashMap::new();

    for entry in walker
        .try_into_iter()
        .map_err(|err| std::io::Error::other(err.to_string()))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                errors_count.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        if entry.depth == 0 {
            continue;
        }

        let path = entry.path();
        let parent = entry.parent_path().to_path_buf();
        let flat = FlatEntry {
            path: path.clone(),
            name: entry.file_name().to_string_lossy().into_owned(),
            sizes: entry.client_state,
            is_dir: entry.file_type().is_dir(),
        };
        groups.entry(parent).or_default().push(flat);
    }

    Ok(groups)
}

/// Materialize `node`'s subtree from the grouped flat entries.
///
/// Entries are keyed by parent path, so assembly is immune to the walker's
/// completion-order across directories. Directory nodes contribute no bytes
/// of their own; their aggregates grow purely from file descendants.
fn assemble_children(
    node: &mut DirectoryNode,
    groups: &mut HashMap<PathBuf, Vec<FlatEntry>>,
    apparent_size: bool,
) {
    let Some(children) = groups.remove(&node.path) else {
        return;
    };
    for flat in children {
        let mut child = DirectoryNode::new(flat.name, flat.path.clone(), flat.is_dir);
        if !flat.is_dir {
            child.size = if apparent_size {
                flat.sizes.1
            } else {
                flat.sizes.0
            };
            child.apparent_size = flat.sizes.1;
        } else {
            assemble_children(&mut child, groups, apparent_size);
        }
        node.add_child(child);
    }
}
