//! Safety-first cache cleaning engine.
//!
//! Planning never mutates anything: it enumerates candidate items from the
//! registered target roots, applies age/size filters, and verifies every
//! candidate against the safety jail. Deletion happens exclusively through
//! [`execute_clean_plan`], which re-verifies safety immediately before each
//! removal and quarantines symlinks (only ever unlinking the link itself).

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime};

use chrono::{DateTime, Duration, Utc};
use directories::{BaseDirs, UserDirs};
use indicatif::ProgressBar;
use serde::Serialize;
use walkdir::WalkDir;

use crate::cli::CleanArgs;
use crate::errors::{CleanError, DiskPulseError, SafetyError};

pub use crate::models::{
    CleanItem, CleanItemResult, CleanItemStatus, CleanPlan, CleanReport, TargetSummary,
};
use crate::util;

/// Minimum age applied to volatile system-temp content when the user did not
/// request an explicit `--older-than` window (protects live sessions).
const SYSTEM_TEMP_MIN_AGE: Duration = Duration::days(1);

/// A named location category that diskpulse knows how to clean.
#[derive(Debug, Clone, Serialize)]
pub struct CleanTargetDef {
    /// Stable identifier used on the command line (e.g. `"cargo-cache"`).
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub enabled_by_default: bool,
    /// Roots resolved dynamically for the current platform/OS.
    pub paths: Vec<PathBuf>,
    /// Age floor applied when the user omits `--older-than`.
    pub default_older_than: Option<Duration>,
}

impl CleanTargetDef {
    fn new(
        id: &'static str,
        name: &'static str,
        description: &'static str,
        enabled_by_default: bool,
        paths: Vec<Option<PathBuf>>,
    ) -> Self {
        Self {
            id,
            name,
            description,
            enabled_by_default,
            paths: paths.into_iter().flatten().collect(),
            default_older_than: None,
        }
    }

    fn with_min_age(mut self, age: Duration) -> Self {
        self.default_older_than = Some(age);
        self
    }
}

// ---------------------------------------------------------------------------
// Platform path resolution helpers
// ---------------------------------------------------------------------------

fn home_sub(rel: &[&str]) -> Option<PathBuf> {
    base_dirs().map(|dirs| {
        rel.iter()
            .fold(dirs.home_dir().to_path_buf(), |acc, part| acc.join(part))
    })
}

/// `$XDG_CACHE_HOME`-style helper. Only the non-Windows target definitions
/// reference it; on Windows the INetCache/browser paths are absolute.
#[cfg(not(windows))]
fn cache_sub(rel: &[&str]) -> Option<PathBuf> {
    let cache = base_dirs()?.cache_dir().to_path_buf();
    Some(rel.iter().fold(cache, |acc, part| acc.join(part)))
}

fn base_dirs() -> Option<BaseDirs> {
    BaseDirs::new()
}

fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

/// Pick the current platform's path segments relative to `$HOME`.
fn home_relative(linux: &[&str], macos: &[&str], windows: &[&str]) -> Vec<Option<PathBuf>> {
    let segments: &[&str] = if cfg!(target_os = "linux") {
        linux
    } else if cfg!(target_os = "macos") {
        macos
    } else {
        windows
    };
    if segments.is_empty() {
        return Vec::new();
    }
    vec![home_sub(segments)]
}

/// Paths rooted at `%LOCALAPPDATA%` (empty on non-Windows platforms).
fn local_appdata_relative(windows: &[&str]) -> Vec<Option<PathBuf>> {
    if !cfg!(windows) {
        return Vec::new();
    }
    local_app_data()
        .map(|base| vec![Some(windows.iter().fold(base, |acc, part| acc.join(part)))])
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Target registry
// ---------------------------------------------------------------------------

/// Platform-aware catalog of every clean target supported by this build.
///
/// Paths that do not exist on this machine are retained (so the `targets`
/// listing can advertise them) and skipped during planning.
pub fn get_registered_targets() -> Vec<CleanTargetDef> {
    vec![
        CleanTargetDef::new(
            "system-temp",
            "System Temp",
            "Stale files inside OS temporary directories.",
            true,
            {
                #[cfg(unix)]
                {
                    vec![Some(PathBuf::from("/tmp")), Some(PathBuf::from("/var/tmp"))]
                }
                #[cfg(not(unix))]
                {
                    let mut paths = vec![Some(std::env::temp_dir())];
                    paths.extend(local_appdata_relative(&["Temp"]));
                    paths
                }
            },
        )
        .with_min_age(SYSTEM_TEMP_MIN_AGE),
        CleanTargetDef::new(
            "user-cache",
            "OS User Cache",
            "Operating-system level user cache directory.",
            true,
            {
                #[cfg(windows)]
                {
                    local_appdata_relative(&["Microsoft", "Windows", "INetCache"])
                }
                #[cfg(not(windows))]
                {
                    vec![cache_sub(&[])]
                }
            },
        ),
        CleanTargetDef::new(
            "cargo-cache",
            "Cargo Cache",
            "Rust crate archives and git checkouts cached by Cargo.",
            true,
            vec![
                home_sub(&[".cargo", "registry", "cache"]),
                home_sub(&[".cargo", "git", "db"]),
            ],
        ),
        CleanTargetDef::new(
            "npm-cache",
            "npm Cache",
            "Package tarball cache maintained by npm (_cacache).",
            true,
            {
                let mut paths = home_relative(&[".npm", "_cacache"], &[".npm", "_cacache"], &[]);
                paths.extend(local_appdata_relative(&["npm-cache"]));
                paths
            },
        ),
        CleanTargetDef::new(
            "yarn-cache",
            "Yarn Cache",
            "Offline mirror and package cache maintained by Yarn.",
            true,
            home_relative(
                &[".cache", "yarn"],
                &["Library", "Caches", "Yarn"],
                &["Yarn", "Cache"],
            ),
        ),
        CleanTargetDef::new(
            "pnpm-cache",
            "pnpm Store",
            "Content-addressable package store maintained by pnpm.",
            true,
            {
                let mut paths = home_relative(
                    &[".local", "share", "pnpm", "store"],
                    &["Library", "pnpm", "store"],
                    &[],
                );
                paths.extend(local_appdata_relative(&["pnpm", "store"]));
                paths
            },
        ),
        CleanTargetDef::new(
            "pip-cache",
            "pip Cache",
            "Downloaded wheel cache maintained by pip.",
            true,
            home_relative(
                &[".cache", "pip"],
                &["Library", "Caches", "pip"],
                &["pip", "Cache"],
            ),
        ),
        CleanTargetDef::new(
            "gradle-cache",
            "Gradle Cache",
            "Build and dependency caches under ~/.gradle/caches.",
            true,
            vec![home_sub(&[".gradle", "caches"])],
        ),
        CleanTargetDef::new(
            "go-build",
            "Go Build Cache",
            "Compiled package cache of the Go toolchain.",
            true,
            {
                let mut paths =
                    home_relative(&[".cache", "go-build"], &[".cache", "go-build"], &[]);
                paths.extend(local_appdata_relative(&["go-build"]));
                paths
            },
        ),
        // -- Opt-in ----------------------------------------------------------
        CleanTargetDef::new(
            "xcode",
            "Xcode Derived Data",
            "Per-project Xcode build artifacts and indexes (macOS only).",
            false,
            home_relative(&[], &["Library", "Developer", "Xcode", "DerivedData"], &[]),
        ),
        CleanTargetDef::new(
            "docker-build",
            "Docker Buildx Cache",
            "Local buildx layer cache and references.",
            false,
            vec![
                home_sub(&[".docker", "buildx", "refs"]),
                home_sub(&[".docker", "buildx", "cache"]),
            ],
        ),
        CleanTargetDef::new(
            "browser-cache",
            "Browser Caches",
            "Chromium, Chrome, Firefox and Brave HTTP caches.",
            false,
            {
                #[cfg(target_os = "macos")]
                {
                    vec![
                        cache_sub(&["Google", "Chrome"]),
                        cache_sub(&["Firefox"]),
                        cache_sub(&["BraveSoftware", "Brave-Browser"]),
                        cache_sub(&["Chromium"]),
                    ]
                }
                #[cfg(target_os = "linux")]
                {
                    vec![
                        home_sub(&[".cache", "google-chrome"]),
                        home_sub(&[".cache", "mozilla"]),
                        home_sub(&[".cache", "BraveSoftware"]),
                        home_sub(&[".cache", "chromium"]),
                    ]
                }
                #[cfg(all(not(target_os = "macos"), not(target_os = "linux")))]
                {
                    let lad = local_app_data();
                    vec![
                        lad.clone().map(|base| {
                            base.join("Google")
                                .join("Chrome")
                                .join("User Data")
                                .join("Default")
                                .join("Cache")
                        }),
                        lad.map(|base| base.join("Mozilla").join("Firefox")),
                    ]
                }
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// Safety jail
// ---------------------------------------------------------------------------

/// Filesystem roots whose deletion (or wholesale clearing) is never allowed.
#[cfg(unix)]
const SYSTEM_ROOTS: &[&str] = &[
    "/", "/bin", "/sbin", "/usr", "/usr/bin", "/etc", "/lib", "/boot", "/dev", "/sys", "/proc",
    "/var",
];
#[cfg(windows)]
const SYSTEM_ROOTS: &[&str] = &[
    "C:\\",
    "C:\\Windows",
    "C:\\Program Files",
    "C:\\Program Files (x86)",
];
#[cfg(not(any(unix, windows)))]
const SYSTEM_ROOTS: &[&str] = &[];

/// System roots resolved through the filesystem once, lazily. Both the
/// literal spelling and the canonical location are kept: on macOS entries
/// such as `/var` are symlinks into `/private`.
fn system_roots() -> &'static [PathBuf] {
    static ROOTS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut resolved: Vec<PathBuf> = Vec::new();

        // Windows honors the live environment first: boot drive, OS
        // directory and install trees may sit on any volume.
        #[cfg(windows)]
        resolved.extend(windows_dynamic_system_roots());

        for root in SYSTEM_ROOTS {
            let path = PathBuf::from(root);
            if let Ok(canon) = fs::canonicalize(&path) {
                if canon != path {
                    resolved.push(canon);
                }
            }
            resolved.push(path);
        }
        resolved
    })
}

/// Windows locations discovered from the environment at runtime: the active
/// boot drive (`SystemDrive`, normalized `C:` → `C:\`), the OS directory
/// (`SystemRoot`/`windir`) and program-install trees for both architectures.
#[cfg(windows)]
fn windows_dynamic_system_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(drive) = std::env::var("SystemDrive") {
        let mut drive_root = drive.trim().to_owned();
        if drive_root.is_empty() {
            drive_root = String::from("C:");
        }
        if !drive_root.ends_with('\\') {
            drive_root.push('\\');
        }
        roots.push(PathBuf::from(&drive_root));
        roots.push(PathBuf::from(&drive_root).join("Windows"));
    }
    for key in [
        "SystemRoot",
        "windir",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
    ] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                roots.push(PathBuf::from(value));
            }
        }
    }
    roots
}

/// Volatile temp areas that stay cleanable even though they live beneath
/// otherwise-protected parents (`/var/tmp` sits under `/var`). Empty where
/// no such overlap exists (the Windows `%TEMP%` tree is outside the jail).
#[cfg(unix)]
const CLEANABLE_TEMP_PREFIXES: &[&str] = &["/tmp", "/var/tmp"];
#[cfg(not(unix))]
const CLEANABLE_TEMP_PREFIXES: &[&str] = &[];

/// Canonicalized [`CLEANABLE_TEMP_PREFIXES`] plus the OS-designated temp dir
/// (lazily). The runtime temp dir matters because e.g. macOS `$TMPDIR`
/// resolves into `/var/folders/...`, outside the static `/var/tmp` prefix.
fn cleanable_temp_prefixes() -> &'static [PathBuf] {
    static PREFIXES: OnceLock<Vec<PathBuf>> = OnceLock::new();
    PREFIXES.get_or_init(|| {
        let mut prefixes: Vec<PathBuf> = CLEANABLE_TEMP_PREFIXES
            .iter()
            .map(|prefix| {
                let path = PathBuf::from(prefix);
                fs::canonicalize(&path).unwrap_or(path)
            })
            .collect();

        let os_temp = std::env::temp_dir();
        let canon_temp = fs::canonicalize(&os_temp).unwrap_or_else(|_| os_temp.clone());

        // Sanitize OS temp dir against TMPDIR poisoning (e.g., TMPDIR=/).
        // A poisoned value would mark the entire system "under temp" and
        // bypass every other jail rule via the carve-out.
        let is_safe = |p: &Path| -> bool {
            if p.parent().is_none() {
                return false;
            }
            if system_roots().iter().any(|r| p == r) {
                return false;
            }
            if let Some(home) = base_dirs().map(|d| d.home_dir().to_path_buf()) {
                if p == home {
                    return false;
                }
                if let Some(parent) = home.parent() {
                    if p == parent {
                        return false;
                    }
                }
            }
            true
        };

        if is_safe(&os_temp) {
            prefixes.push(os_temp);
        }
        let last = prefixes
            .last()
            .map(|p| p.as_path())
            .unwrap_or(Path::new(""));
        if canon_temp != last && is_safe(&canon_temp) {
            prefixes.push(canon_temp);
        }
        prefixes
    })
}

fn is_cleanable_temp(normalized: &Path) -> bool {
    cleanable_temp_prefixes()
        .iter()
        .any(|prefix| normalized == prefix || normalized.starts_with(prefix))
}

/// Evaluate `path` against the safety jail.
///
/// The path is canonicalized first so symlinks are judged by where they point,
/// falling back to lexical normalization when the entry does not exist.
pub fn validate_path_safety(path: &Path) -> Result<(), SafetyError> {
    let normalized = canonicalize_for_jail(path);
    ensure_safe_location(&normalized)
}

/// Canonicalize for jail comparison. Windows `fs::canonicalize` returns
/// verbatim paths (`\\?\C:\Users\me`); compared raw against plain paths
/// from `directories`/env they never match, which would silently bypass
/// every protection rule below. UNC form maps back to `\\srv\share`.
fn canonicalize_for_jail(path: &Path) -> PathBuf {
    let canon = fs::canonicalize(path).unwrap_or_else(|_| lexical_normalize(path));
    strip_verbatim_prefix(canon)
}

fn strip_verbatim_prefix(p: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.as_os_str().to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    p
}

/// Jail check for an already-normalized path.
fn ensure_safe_location(normalized: &Path) -> Result<(), SafetyError> {
    let under_temp = is_cleanable_temp(normalized);

    // 1. Bare filesystem roots ("/" on POSIX, any "D:\"-style volume root
    //    on Windows) terminate in a root and therefore have no parent. They
    //    are never deletable themselves — even when not enumerated in the
    //    static jail lists.
    if normalized.parent().is_none() && !under_temp {
        return Err(SafetyError::ProtectedSystemPath(normalized.to_path_buf()));
    }

    // 2. Resolve $HOME and its parent (/home, /Users, C:\Users). Wiping the
    //    base directory holding all user profiles would destroy every
    //    account at once.
    if let Some(home) = base_dirs().map(|dirs| dirs.home_dir().to_path_buf()) {
        if normalized == home {
            return Err(SafetyError::ProtectedHomeRoot(home));
        }
        if let Some(parent) = home.parent() {
            // Never allow wiping the base directory holding all user profiles
            if normalized == parent {
                return Err(SafetyError::ProtectedSystemPath(parent.to_path_buf()));
            }
        }
    }

    // 3. System roots
    if !under_temp {
        for root in system_roots() {
            // Bare filesystem/drive roots match exactly only — otherwise a
            // drive root would swallow every user path on that volume. All
            // other protected roots also refuse their contents.
            let hit = if root.parent().is_none() {
                normalized == root
            } else {
                normalized.starts_with(root)
            };
            if hit {
                return Err(SafetyError::ProtectedSystemPath(normalized.to_path_buf()));
            }
        }
    }

    // 4. Protected user directories
    for dir in protected_user_dirs() {
        if normalized == dir || normalized.starts_with(&dir) {
            return Err(SafetyError::ProtectedUserData(dir));
        }
    }

    Ok(())
}

/// Personal-data directories that must never be touched (including their
/// contents). Missing/unavailable dirs are skipped.
fn protected_user_dirs() -> Vec<PathBuf> {
    UserDirs::new()
        .map(|dirs| {
            [
                dirs.document_dir(),
                dirs.desktop_dir(),
                dirs.download_dir(),
                dirs.audio_dir(),
                dirs.picture_dir(),
                dirs.video_dir(),
            ]
            .into_iter()
            .flatten()
            .map(Path::to_path_buf)
            .collect()
        })
        .unwrap_or_default()
}

/// Resolve `.`/`..` components without touching the filesystem. Unlike
/// canonicalization this never follows symlinks, which is exactly what the
/// execution-time re-check wants: removing a link must remain possible even
/// when the link points somewhere protected.
fn lexical_normalize(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };

    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Tunables for a cleanup run, derived from [`CleanArgs`].
#[derive(Debug, Clone, Default)]
pub struct CleanOptions {
    /// Empty or `["all"]` expands to every registered target; `"all"` also
    /// pulls in opt-in targets.
    pub targets: Vec<String>,
    pub apply: bool,
    pub yes: bool,
    pub use_trash: bool,
    pub older_than: Option<Duration>,
    pub min_size: Option<u64>,
    pub custom_path: Option<PathBuf>,
    pub one_file_system: bool,
}

impl From<&CleanArgs> for CleanOptions {
    fn from(args: &CleanArgs) -> Self {
        Self {
            targets: args.targets.clone(),
            apply: args.apply,
            yes: args.yes,
            use_trash: args.trash,
            older_than: args
                .older_than
                .as_deref()
                .and_then(|raw| util::parse_duration(raw).ok()),
            min_size: args
                .min_size
                .as_deref()
                .and_then(|raw| util::parse_size(raw).ok()),
            custom_path: args.path.clone(),
            one_file_system: args.one_file_system,
        }
    }
}

// ---------------------------------------------------------------------------
// Planning (read-only)
// ---------------------------------------------------------------------------

/// Resolve requested targets and enumerate deletion candidates.
///
/// This function is strictly read-only: the filesystem is never mutated.
pub fn create_clean_plan(options: &CleanOptions) -> Result<CleanPlan, DiskPulseError> {
    let mut items = Vec::new();

    if let Some(custom) = &options.custom_path {
        // A user-supplied path gets hard-fail validation: pointing at a
        // protected location aborts instead of silently cleaning nothing.
        validate_path_safety(custom)?;
        items.extend(collect_root_items(
            custom,
            "custom-path",
            "Custom Path",
            options,
            None,
        ));
    } else {
        for target in resolve_targets(&options.targets)? {
            for root in &target.paths {
                if !root.exists() || validate_path_safety(root).is_err() {
                    continue;
                }
                items.extend(collect_root_items(
                    root,
                    target.id,
                    target.name,
                    options,
                    target.default_older_than,
                ));
            }
        }
    }

    let mut plan = CleanPlan::from_items(items, !options.apply);
    // The mount boundary is an execution-time concern; carry it on the plan
    // so `execute_clean_plan` can enforce it without signature churn.
    plan.one_file_system = options.one_file_system;
    Ok(plan)
}

/// Expand the requested target list against the registry:
/// empty -> defaults only; contains "all" -> everything; otherwise exact ids.
fn resolve_targets(requested: &[String]) -> Result<Vec<CleanTargetDef>, DiskPulseError> {
    let registry = get_registered_targets();
    if requested.is_empty() {
        return Ok(registry
            .into_iter()
            .filter(|target| target.enabled_by_default)
            .collect());
    }

    if requested.iter().any(|raw| raw == "all") {
        return Ok(registry);
    }

    let mut selected = Vec::new();
    for raw in requested {
        match registry.iter().find(|target| target.id == raw.as_str()) {
            Some(target) => {
                if !selected
                    .iter()
                    .any(|existing: &CleanTargetDef| existing.id == target.id)
                {
                    selected.push(target.clone());
                }
            }
            None => return Err(CleanError::TargetNotFound(raw.clone()).into()),
        }
    }
    Ok(selected)
}

/// Enumerate the direct children of one target root as candidate items.
///
/// Granularity notes:
/// - Each child becomes ONE removal unit — directories are measured
///   recursively here but deleted as whole subtrees at execution time
///   (never through symlinks), keeping plans compact even for huge caches.
/// - A root that is itself a file or symlink is treated as a single unit,
///   so `clean --path <file>` works.
/// - When an age window is active, directories are never deletion units:
///   their mtimes cannot vouch for nested content, so planning always
///   descends and judges files and links individually.
fn collect_root_items(
    root: &Path,
    target_id: &str,
    target_name: &str,
    options: &CleanOptions,
    default_older_than: Option<Duration>,
) -> Vec<CleanItem> {
    let cutoff = effective_cutoff(options.older_than.or(default_older_than));
    let root_meta = match fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(_) => return Vec::new(),
    };

    // Single-file / symlink roots are deletion units themselves.
    if !root_meta.is_dir() {
        let mut out = Vec::new();
        collect_entry(
            root.to_path_buf(),
            &root_meta,
            target_id,
            target_name,
            options,
            cutoff,
            None,
            &mut out,
        );
        return out;
    }

    let mut items = Vec::new();
    collect_from_dir(
        root,
        &root_meta,
        target_id,
        target_name,
        options,
        cutoff,
        &mut items,
    );
    items
}

/// Enumerate one directory level into `out`.
fn collect_from_dir(
    dir: &Path,
    dir_meta: &std::fs::Metadata,
    target_id: &str,
    target_name: &str,
    options: &CleanOptions,
    cutoff: Option<SystemTime>,
    out: &mut Vec<CleanItem>,
) {
    let parent_dev = util::device_id(dir_meta);
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Explicit symlink_metadata: classification must NEVER depend on
        // following links, so an entry pointing outside the cache is always
        // treated as a link unit, never as a local directory.
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        collect_entry(
            path,
            &meta,
            target_id,
            target_name,
            options,
            cutoff,
            parent_dev,
            out,
        );
    }
}

/// Apply every filter to one candidate and either emit it, descend into it,
/// or discard it.
#[allow(clippy::too_many_arguments)]
fn collect_entry(
    path: PathBuf,
    meta: &std::fs::Metadata,
    target_id: &str,
    target_name: &str,
    options: &CleanOptions,
    cutoff: Option<SystemTime>,
    parent_dev: Option<u64>,
    out: &mut Vec<CleanItem>,
) {
    let file_type = meta.file_type();
    // Symlinks ARE valid deletion units — the link itself gets unlinked,
    // never followed. `is_dir` stays false for them so execution removes
    // just the link, leaving the target's contents untouched.
    let is_symlink = file_type.is_symlink();
    let is_dir = file_type.is_dir() && !is_symlink;

    if is_dir && options.one_file_system {
        // Unknown device ids on either side mean "cannot prove a boundary"
        // — stay conservative and keep traversing.
        if let (Some(item_dev), Some(root_id)) = (util::device_id(meta), parent_dev) {
            if item_dev != root_id {
                return;
            }
        }
    }

    let modified = meta.modified();

    // Age windows NEVER trust directory mtimes for wholesale removal. A dir
    // mtime only tracks its immediate entries: in-place rewrites of files
    // inside it do not bump it at all, and fresh grandchildren two levels
    // down move only the immediate parent. An ancient-looking directory can
    // therefore hide brand-new content, and `remove_dir_all` on it would
    // destroy that content. Under a cutoff every directory expands into
    // individually-judged children instead of becoming one unit.
    if cutoff.is_some() && is_dir {
        collect_from_dir(&path, meta, target_id, target_name, options, cutoff, out);
        return;
    }

    if let Some(cutoff) = cutoff {
        // Files and links failing the window are dropped outright.
        match modified {
            Ok(mtime) if mtime <= cutoff => {}
            _ => return,
        }
    }

    let size = if is_dir {
        tree_size(&path, options.one_file_system, parent_dev)
    } else {
        // Links report their own metadata, never the target's.
        util::physical_disk_size(meta)
    };

    if let Some(min_size) = options.min_size {
        if size < min_size {
            return;
        }
    }
    if validate_path_safety(&path).is_err() {
        return;
    }

    out.push(CleanItem {
        path,
        size,
        target_id: target_id.to_owned(),
        target_name: target_name.to_owned(),
        is_dir,
        modified: modified.ok().map(DateTime::<Utc>::from),
    });
}

fn effective_cutoff(age: Option<Duration>) -> Option<SystemTime> {
    let std_age = age?.to_std().unwrap_or_default();
    SystemTime::now().checked_sub(std_age)
}

/// Recursively sum allocated bytes of all regular files under `dir`.
///
/// Symlinks are strictly excluded: their type is taken from the directory
/// entry itself (`entry.file_type()`, no stat at all), they are never pushed
/// onto the traversal stack and contribute zero bytes. Without link descent,
/// cycles through directory symlinks are impossible, so termination is
/// guaranteed even in adversarial caches.
fn tree_size(dir: &Path, one_file_system: bool, root_dev: Option<u64>) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            // Cheap classification straight from readdir; never follows.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if one_file_system {
                // Skip subtrees AND bytes living on another filesystem:
                // reclaimable-space accounting must mirror what deletion
                // would actually touch.
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if let (Some(dev), Some(root)) = (util::device_id(&meta), root_dev) {
                    if dev != root {
                        continue;
                    }
                }
            }
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total += util::physical_disk_size(&meta);
            }
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Execution (mutating)
// ---------------------------------------------------------------------------

/// Execute a plan produced by [`create_clean_plan`].
///
/// Dry-run plans are never executed: every item is reported as
/// [`CleanItemStatus::SkippedDryRun`] without touching the filesystem.
/// Failures are captured per item; the batch never aborts mid-way.
pub fn execute_clean_plan(
    plan: &CleanPlan,
    use_trash: bool,
) -> Result<CleanReport, DiskPulseError> {
    execute_clean_plan_with_progress(plan, use_trash, None)
}

/// [`execute_clean_plan`] with optional per-item progress reporting.
///
/// The progress callback fires after every attempted item (success or
/// failure), so the bar always reaches `len` when the run completes.
#[allow(clippy::needless_pass_by_value)]
pub fn execute_clean_plan_with_progress(
    plan: &CleanPlan,
    use_trash: bool,
    progress: Option<&ProgressBar>,
) -> Result<CleanReport, DiskPulseError> {
    let started = Instant::now();

    let mut results = Vec::with_capacity(plan.items.len());
    let mut bytes_freed = 0;
    let mut items_freed = 0;
    let mut errors_count = 0;

    for item in &plan.items {
        let status = if plan.is_dry_run {
            CleanItemStatus::SkippedDryRun
        } else {
            let status = match remove_item(&item.path, item.is_dir, use_trash, plan.one_file_system)
            {
                Ok(status) => {
                    bytes_freed += item.size;
                    items_freed += 1;
                    status
                }
                Err(reason) => {
                    errors_count += 1;
                    CleanItemStatus::Failed(reason)
                }
            };
            if let Some(p) = progress {
                p.inc(1);
            }
            status
        };
        results.push(CleanItemResult {
            path: item.path.clone(),
            size: item.size,
            target_id: item.target_id.clone(),
            status,
        });
    }

    Ok(CleanReport {
        plan: plan.clone(),
        results,
        bytes_freed,
        items_freed,
        errors_count,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        dry_run: plan.is_dry_run,
    })
}

/// Remove a single planned item.
///
/// Jail verification matches the location the mutation will REALLY touch:
/// - Symlink items are unlinked in place, so the lexical location of the
///   link itself is what matters.
/// - Real files and directories mutate through any symlinked parent chain,
///   so the canonicalized path decides. `/tmp/sandbox/link_to_usr/file`
///   therefore fails the jail even though its lexical spelling looks tame.
///
/// The `ensure_safe_location` call below is a deliberate TOCTOU
/// (time-of-check-to-time-of-use) mitigation, NOT redundant paranoia: the
/// plan was built earlier against a read-only scan, so anything on disk —
/// including a symlink's target or an entire parent directory swapped for
/// a link — may have changed between planning and this function actually
/// mutating the filesystem. Re-deriving the jail target here, at the last
/// possible moment, is what catches that race.
fn remove_item(
    path: &Path,
    is_dir: bool,
    use_trash: bool,
    one_file_system: bool,
) -> std::result::Result<CleanItemStatus, String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) => return Err(deletion_failed(path, err)),
    };

    let jail_target: PathBuf = if meta.file_type().is_symlink() {
        lexical_normalize(path)
    } else {
        // The entry exists, so canonicalization normally succeeds; the
        // lexical fallback covers races where it vanishes mid-check.
        canonicalize_for_jail(path)
    };
    if let Err(safety) = ensure_safe_location(&jail_target) {
        return Err(format!("blocked by safety jail: {safety}"));
    }

    // Symlink quarantine outranks trash mode: links are ALWAYS hard-unlinked
    // in place and never handed to the OS trash API, whose move semantics
    // must never be trusted with a link's target.
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        {
            fs::remove_file(path)
                .map(|_| CleanItemStatus::Deleted)
                .map_err(|err| deletion_failed(path, err))
        }
        // Windows: directory symlinks and junctions carry the DIRECTORY
        // attribute on the link itself (`meta` is symlink_metadata) and
        // fail remove_file with AccessDenied — they need rmdir semantics.
        #[cfg(windows)]
        {
            if meta.is_dir() {
                fs::remove_dir(path)
                    .map(|_| CleanItemStatus::Deleted)
                    .map_err(|err| deletion_failed(path, err))
            } else {
                fs::remove_file(path)
                    .map(|_| CleanItemStatus::Deleted)
                    .map_err(|err| deletion_failed(path, err))
            }
        }
    } else if use_trash {
        trash::delete(path)
            .map(|_| CleanItemStatus::MovedToTrash)
            .map_err(|err| format!("failed to trash {}: {err}", path.display()))
    } else if !meta.is_dir() {
        fs::remove_file(path)
            .map(|_| CleanItemStatus::Deleted)
            .map_err(|err| deletion_failed(path, err))
    } else if is_dir && one_file_system {
        safe_remove_dir_all_one_fs(path, None)
    } else if is_dir {
        fs::remove_dir_all(path)
            .map(|_| CleanItemStatus::Deleted)
            .map_err(|err| deletion_failed(path, err))
    } else {
        fs::remove_dir(path)
            .map(|_| CleanItemStatus::Deleted)
            .map_err(|err| deletion_failed(path, err))
    }
}

/// Bottom-up directory removal that refuses to cross filesystems.
///
/// `std::fs::remove_dir_all` unconditionally descends through mount points;
/// a cache directory containing a mounted volume would be wiped across the
/// boundary. This variant walks `contents_first` so children vanish before
/// their parents, and any entry whose device id differs from the tree root
/// aborts the whole item — leaving everything above the boundary intact.
///
/// `expected_dev` optionally pins the tree to a caller-known volume; `None`
/// derives the reference from the item itself (self-consistent single-fs).
fn safe_remove_dir_all_one_fs(
    path: &Path,
    expected_dev: Option<u64>,
) -> std::result::Result<CleanItemStatus, String> {
    let root_meta = fs::symlink_metadata(path).map_err(|err| deletion_failed(path, err))?;
    let root_dev = util::device_id(&root_meta);
    if let (Some(dev), Some(expected)) = (root_dev, expected_dev) {
        if dev != expected {
            return Err(format!(
                "refusing to cross mount point at {}",
                path.display()
            ));
        }
    }

    for entry in WalkDir::new(path)
        .contents_first(true)
        .follow_links(false)
        .into_iter()
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                return Err(format!("traversal failed under {}: {err}", path.display()));
            }
        };
        let child = entry.path();
        if child == path {
            continue; // The root goes last.
        }
        let child_meta = match fs::symlink_metadata(child) {
            Ok(meta) => meta,
            Err(err) => return Err(deletion_failed(child, err)),
        };
        // Symlinks report their own device id (the tree's volume), so links
        // into other filesystems are still unlinked as links — quarantine
        // semantics hold inside the walk too.
        if let Some(dev) = util::device_id(&child_meta) {
            if Some(dev) != root_dev {
                return Err(format!(
                    "refusing to cross mount point at {}",
                    child.display()
                ));
            }
        }
        let result = if child_meta.is_dir() {
            fs::remove_dir(child)
        } else {
            fs::remove_file(child)
        };
        result.map_err(|err| deletion_failed(child, err))?;
    }

    fs::remove_dir(path)
        .map(|_| CleanItemStatus::Deleted)
        .map_err(|err| deletion_failed(path, err))
}

fn deletion_failed(path: &Path, source: std::io::Error) -> String {
    CleanError::DeletionFailed {
        path: path.to_path_buf(),
        source,
    }
    .to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn trash_mode_never_hands_a_symlink_to_the_trash_api() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let target = tmp.path().join("payload.txt");
        fs::write(&target, b"precious").expect("write");
        let link = tmp.path().join("link_to_payload");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        // use_trash=true: the link must be hard-unlinked in place anyway.
        let status = remove_item(&link, false, true, false).expect("remove link");
        assert_eq!(status, CleanItemStatus::Deleted, "not MovedToTrash");
        assert!(!link.exists(), "link should be gone");
        assert!(
            target.exists(),
            "trash API followed the symlink and took the target"
        );
    }

    #[cfg(unix)]
    #[test]
    fn remove_item_refuses_real_paths_behind_symlinked_parents() {
        let tmp = tempfile::TempDir::new().expect("tempdir");

        // Positive control: a plain file inside the cleanable temp tree is
        // still deletable after the canonicalization change.
        let plain = tmp.path().join("plain.bin");
        fs::write(&plain, b"x").expect("write");
        assert!(matches!(
            remove_item(&plain, false, false, false),
            Ok(CleanItemStatus::Deleted)
        ));

        // Escape attempt: a symlinked parent pointing at "/" drags the real
        // mutation target into /etc. The lexical spelling looks tame; only
        // the canonical view reveals the protected location.
        let link = tmp.path().join("to_root");
        std::os::unix::fs::symlink("/", &link).expect("symlink");
        let via_link = link.join("etc/hosts");

        let result = remove_item(&via_link, false, false, false);
        assert!(
            result.is_err(),
            "deletion through symlinked parent must be refused"
        );
        assert!(
            Path::new("/etc/hosts").exists(),
            "the system file behind the link must survive"
        );
    }

    #[test]
    fn jail_blocks_parentless_drive_and_filesystem_roots() {
        // "/" on POSIX — and by the same rule any Windows drive root such as
        // "D:\" whose Path::parent() is None — must never pass the jail,
        // even when absent from the static lists.
        let err =
            ensure_safe_location(Path::new("/")).expect_err("parentless roots must be blocked");
        assert!(matches!(err, SafetyError::ProtectedSystemPath(_)), "{err}");
    }

    #[test]
    fn lexical_normalize_resolves_dots_without_fs() {
        // Compare via component collections, not hardcoded separators:
        // on Windows the same input normalizes to `\tmp\a\c`.
        let normalized = lexical_normalize(Path::new("/tmp/a/./b/../c"));
        assert!(normalized.is_absolute());
        // Windows absolutizes root-relative inputs with the CWD drive
        // ("D:\\tmp\\a\\c"), so compare trailing components only.
        assert!(
            normalized.ends_with(Path::new("tmp").join("a").join("c")),
            "{normalized:?}"
        );

        let above_root = lexical_normalize(Path::new("/../.."));
        assert!(above_root.is_absolute());
        assert!(above_root.ends_with(Path::new("/")), "{above_root:?}");

        let absolute = lexical_normalize(Path::new("relative/x"));
        assert!(absolute.is_absolute());
        assert!(absolute.ends_with("relative/x"));
    }

    #[test]
    fn jail_blocks_system_roots() {
        for root in SYSTEM_ROOTS {
            let err =
                ensure_safe_location(Path::new(root)).expect_err("system roots must be blocked");
            assert!(matches!(err, SafetyError::ProtectedSystemPath(_)), "{err}");
        }
    }

    #[test]
    fn jail_blocks_home_and_user_data_but_allows_cache() {
        let Some(dirs) = BaseDirs::new() else { return };
        let home = dirs.home_dir();
        assert!(matches!(
            ensure_safe_location(home),
            Err(SafetyError::ProtectedHomeRoot(_))
        ));

        if let Some(documents) =
            UserDirs::new().and_then(|d| d.document_dir().map(|p| p.to_path_buf()))
        {
            assert!(matches!(
                ensure_safe_location(&documents),
                Err(SafetyError::ProtectedUserData(_))
            ));
            // Contents of personal folders are protected too.
            let nested = documents.join("report.pdf");
            assert!(matches!(
                ensure_safe_location(&nested),
                Err(SafetyError::ProtectedUserData(_))
            ));
        }

        let cache = home.join(".cache").join("diskpulse_test");
        assert!(ensure_safe_location(&cache).is_ok());
    }

    #[test]
    fn verbatim_prefixes_are_stripped_before_comparison() {
        #[cfg(windows)]
        {
            assert_eq!(
                strip_verbatim_prefix(PathBuf::from(r"\\?\C:\Users\me")),
                PathBuf::from(r"C:\Users\me")
            );
            assert_eq!(
                strip_verbatim_prefix(PathBuf::from(r"\\?\UNC\srv\share")),
                PathBuf::from(r"\\srv\share")
            );
        }
        #[cfg(unix)]
        {
            let plain = PathBuf::from("/tmp/unchanged");
            assert_eq!(strip_verbatim_prefix(plain.clone()), plain);
        }
    }

    #[test]
    fn validate_path_safety_canonicalizes_before_checking() {
        let Some(dirs) = BaseDirs::new() else { return };
        // A path that does not exist still normalizes lexically.
        assert!(validate_path_safety(&dirs.home_dir().join(".cache/diskpulse_test")).is_ok());
    }

    #[test]
    fn effective_cutoff_maps_durations_to_instants() {
        assert!(effective_cutoff(None).is_none());

        let cutoff = effective_cutoff(Some(Duration::hours(1))).unwrap();
        let hour_ago = SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(3600))
            .unwrap();
        assert!(
            cutoff < hour_ago + std::time::Duration::from_secs(5),
            "cutoff should sit roughly one hour in the past"
        );

        // A negative/zero duration degrades safely to "delete nothing fresh".
        assert!(effective_cutoff(Some(Duration::zero())).is_some());
    }

    #[test]
    fn resolve_targets_expands_defaults_all_and_unknown() {
        let defaults = resolve_targets(&[]).unwrap();
        assert!(!defaults.is_empty());
        assert!(defaults.iter().all(|t| t.enabled_by_default));
        assert!(defaults.iter().any(|t| t.id == "system-temp"));

        let all = resolve_targets(&["all".to_string()]).unwrap();
        assert!(all.len() >= defaults.len());
        assert!(all.iter().any(|t| !t.enabled_by_default));

        let specific =
            resolve_targets(&["cargo-cache".to_string(), "cargo-cache".to_string()]).unwrap();
        assert_eq!(specific.len(), 1);
        assert_eq!(specific[0].id, "cargo-cache");

        let err = resolve_targets(&["bogus".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("bogus"),
            "unknown target must be reported: {err}"
        );
    }
}
