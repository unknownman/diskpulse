//! Shared fixtures for the hermetic test suites.
//!
//! Everything here is built on `tempfile::TempDir`: no test ever touches the
//! real `$HOME`, the real caches or live system directories.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// TestWorkspace
// ---------------------------------------------------------------------------

/// A disposable sandbox directory with helpers for building file trees and
/// proving (non-)mutation via whole-tree fingerprints.
pub struct TestWorkspace {
    root: TempDir,
}

/// Fingerprint of a single filesystem entry: kind, size, mtime, content hash
/// and link-target (for symlinks). `BTreeMap` ordering makes snapshots
/// directly comparable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryFingerprint {
    pub is_dir: bool,
    pub is_symlink: bool,
    pub len: u64,
    pub modified_nanos: Option<(i64, u32)>,
    /// FNV-1a hash of file contents (0 for dirs/symlinks).
    pub content_hash: u64,
    /// Raw target bytes for symlinks.
    pub link_target: Option<PathBuf>,
}

/// Whole-tree snapshot used to prove that an operation did (or did not)
/// mutate anything.
pub struct WorkspaceSnapshot {
    entries: BTreeMap<PathBuf, EntryFingerprint>,
}

impl WorkspaceSnapshot {
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.entries.keys()
    }

    /// Assert-style comparison producing a precise diff on mismatch.
    pub fn diff(&self, other: &WorkspaceSnapshot) -> Vec<String> {
        let mut problems = Vec::new();
        for (path, before) in &self.entries {
            match other.entries.get(path) {
                None => problems.push(format!("MISSING after run: {}", path.display())),
                Some(after) => {
                    if before != after {
                        problems.push(format!("CHANGED after run: {}", path.display()));
                    }
                }
            }
        }
        for path in other.entries.keys() {
            if !self.entries.contains_key(path) {
                problems.push(format!("NEW after run: {}", path.display()));
            }
        }
        problems
    }
}

impl TestWorkspace {
    pub fn new() -> Self {
        Self {
            root: TempDir::new().expect("create temp workspace"),
        }
    }

    /// Absolute sandbox root (unique per workspace).
    pub fn path(&self) -> &Path {
        self.root.path()
    }

    /// Resolve a relative fixture path against the sandbox root.
    pub fn join(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn ensure_parent(&self, abs: &Path) {
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).expect("create parent directories");
        }
    }

    /// Create a dummy file of exact byte size; returns its absolute path.
    pub fn create_file(&self, rel: &str, size_bytes: u64) -> PathBuf {
        let abs = self.join(rel);
        self.ensure_parent(&abs);
        // Pseudo-random-ish payload so hashes differ between files.
        let mut bytes = Vec::with_capacity(size_bytes as usize);
        let mut seed = 0x9E3779B97F4A7C15u64 ^ size_bytes;
        for _ in 0..size_bytes {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            bytes.push(seed as u8);
        }
        fs::write(&abs, bytes).expect("write fixture file");
        abs
    }

    /// Create a directory (and parents); returns its absolute path.
    pub fn create_dir(&self, rel: &str) -> PathBuf {
        let abs = self.join(rel);
        fs::create_dir_all(&abs).expect("create fixture dir");
        abs
    }

    /// Create a symlink. On Windows this picks file/dir variants and may fail
    /// without developer-mode privileges — callers get the raw `io::Result`.
    pub fn create_symlink(&self, source_rel: &str, link_rel: &str) -> io::Result<PathBuf> {
        let source = self.join(source_rel);
        let link = self.join(link_rel);
        self.ensure_parent(&link);

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&source, &link)?;
        }
        #[cfg(windows)]
        {
            if source.is_dir() {
                std::os::windows::fs::symlink_dir(&source, &link)?;
            } else {
                std::os::windows::fs::symlink_file(&source, &link)?;
            }
        }
        Ok(link)
    }

    /// Backdate an existing fixture's mtime by `seconds_ago`.
    pub fn set_age_seconds(&self, rel: &str, seconds_ago: i64) {
        use filetime::{set_file_mtime, FileTime};
        let abs = self.join(rel);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        set_file_mtime(&abs, FileTime::from_unix_time(now - seconds_ago, 0)).expect("set mtime");
    }

    /// Fingerprint every reachable entry (files, dirs, symlink links — never
    /// through symlinks) under the workspace root.
    pub fn snapshot(&self) -> WorkspaceSnapshot {
        let mut entries = BTreeMap::new();
        let mut stack = vec![self.root.path().to_path_buf()];
        while let Some(current) = stack.pop() {
            let meta = match fs::symlink_metadata(&current) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            let ft = meta.file_type();

            let mut fingerprint = EntryFingerprint {
                is_dir: ft.is_dir(),
                is_symlink: ft.is_symlink(),
                len: meta.len(),
                modified_nanos: meta.modified().ok().and_then(|mtime| {
                    let d = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
                    Some((d.as_secs() as i64, d.subsec_nanos()))
                }),
                content_hash: 0,
                link_target: None,
            };

            if ft.is_symlink() {
                fingerprint.link_target = fs::read_link(&current).ok();
            } else if ft.is_dir() {
                if let Ok(read) = fs::read_dir(&current) {
                    for child in read.flatten() {
                        stack.push(child.path());
                    }
                }
            } else if let Ok(bytes) = fs::read(&current) {
                fingerprint.content_hash = fnv1a(&bytes);
            }

            entries.insert(current, fingerprint);
        }
        WorkspaceSnapshot { entries }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// MockCacheBuilder
// ---------------------------------------------------------------------------

/// Rapidly fabricates realistic Cargo / npm / pip / system-temp hierarchies
/// inside a [`TestWorkspace`], with controllable ages.
pub struct MockCacheBuilder<'a> {
    ws: &'a TestWorkspace,
    files_created: Vec<PathBuf>,
}

impl<'a> MockCacheBuilder<'a> {
    pub fn new(ws: &'a TestWorkspace) -> Self {
        Self {
            ws,
            files_created: Vec::new(),
        }
    }

    /// Every regular file this builder created (absolute paths).
    pub fn files(&self) -> &[PathBuf] {
        &self.files_created
    }

    fn blob(&mut self, rel: &str, size: u64) {
        let path = self.ws.create_file(rel, size);
        self.files_created.push(path);
    }

    /// Fake `~/.cargo`: registry cache crates + git db packs.
    pub fn cargo_cache(&mut self, crates: usize) -> &mut Self {
        for i in 0..crates {
            self.blob(
                &format!("cargo/registry/cache/index.crates.io/fake-crate-{i}-1.0.{i}.crate"),
                2_048 + (i as u64) * 137,
            );
        }
        self.blob("cargo/git/db/some-repo-abc123.pack", 8_192);
        self
    }

    /// Fake npm `_cacache` content-addressed store + index entries.
    pub fn npm_cache(&mut self, packages: usize) -> &mut Self {
        for i in 0..packages {
            self.blob(
                &format!("npm/_cacache/content-v2/sha512/{i:02}/{i:02}/shard-{i}"),
                4_096 + i as u64,
            );
            self.blob(&format!("npm/_cacache/index-v5/{i:02}/entry-{i}"), 256);
        }
        self
    }

    /// Fake pip HTTP + wheels cache.
    pub fn pip_cache(&mut self, wheels: usize) -> &mut Self {
        for i in 0..wheels {
            self.blob(
                &format!("pip/wheels/{i:02}/pkg_{i}-py3-none-any.whl"),
                6_144 + (i as u64) * 11,
            );
        }
        self.blob("pip/http-v2/body-cache-0", 1_024);
        self
    }

    /// Fake system-temp leftovers; each gets `age_seconds`.
    pub fn system_temp(&mut self, count: usize, age_seconds: i64) -> &mut Self {
        for i in 0..count {
            let rel = format!("tmp/diskpulse-leftover-{i}.tmp");
            self.blob(&rel, 512 + (i as u64) * 7);
            self.ws.set_age_seconds(&rel, age_seconds);
        }
        self
    }
}

// ---------------------------------------------------------------------------
// Binary runner
// ---------------------------------------------------------------------------

/// Invoke the compiled `diskpulse` binary hermetically.
pub struct BinRun {
    pub output: std::process::Output,
}

impl BinRun {
    pub fn args(args: &[&str]) -> Self {
        Self {
            output: Command::new(env!("CARGO_BIN_EXE_diskpulse"))
                .args(args)
                .stdin(Stdio::null())
                .output()
                .expect("spawn diskpulse binary"),
        }
    }

    /// Variant with a custom working directory (paths still absolute).
    pub fn args_in(args: &[&str], cwd: &Path) -> Self {
        Self {
            output: Command::new(env!("CARGO_BIN_EXE_diskpulse"))
                .args(args)
                .current_dir(cwd)
                .stdin(Stdio::null())
                .output()
                .expect("spawn diskpulse binary"),
        }
    }

    pub fn code(&self) -> i32 {
        self.output.status.code().unwrap_or(-1)
    }

    pub fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).into_owned()
    }

    pub fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).into_owned()
    }

    pub fn assert_success(&self) {
        assert_eq!(
            self.code(),
            0,
            "expected exit 0\nstdout: {}\nstderr: {}",
            self.stdout(),
            self.stderr()
        );
    }

    /// No ANSI escape sequences anywhere in stdout.
    pub fn assert_stdout_clean(&self) {
        let out = self.stdout();
        assert!(!out.contains("\x1b["), "ANSI codes leaked into stdout");
    }
}
