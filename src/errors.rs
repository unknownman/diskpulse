//! Typed domain error taxonomy shared across scanning, cleaning, parsing,
//! safety enforcement and the CLI layer.

use std::path::PathBuf;

use thiserror::Error;

/// Convenience alias for results failing with the top-level domain error.
pub type Result<T, E = DiskPulseError> = std::result::Result<T, E>;

/// Top-level error type aggregating every subsystem failure mode.
///
/// Variants are transparent: the child error's own (already descriptive)
/// message is displayed directly, so chained diagnostics never repeat.
#[derive(Debug, Error)]
pub enum DiskPulseError {
    /// A safety invariant was about to be violated.
    #[error(transparent)]
    Safety(#[from] SafetyError),

    /// A user-supplied string could not be parsed.
    #[error(transparent)]
    Parse(#[from] ParseError),

    /// A filesystem scan failed.
    #[error(transparent)]
    Scan(#[from] ScanError),

    /// A cleanup operation failed.
    #[error(transparent)]
    Clean(#[from] CleanError),

    /// An underlying I/O operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Violations of diskpulse's safety guarantees.
#[derive(Debug, Error)]
pub enum SafetyError {
    /// Attempted operation on a filesystem/system root (e.g. `/`, `C:\Windows`).
    #[error("refusing to operate on protected system path {0:?}")]
    ProtectedSystemPath(PathBuf),

    /// Attempted operation on the home directory itself rather than a subfolder.
    #[error(
        "{0:?} is the home directory root itself; target a safe subfolder (e.g. ~/.cache) instead"
    )]
    ProtectedHomeRoot(PathBuf),

    /// Attempted operation on standard personal data directories.
    #[error("{0:?} holds personal data and is protected from bulk deletion")]
    ProtectedUserData(PathBuf),

    /// A symlink points outside of the permitted scan/clean boundary.
    #[error("symlink {link:?} points outside the permitted boundary at {target:?}")]
    SymlinkEscape { link: PathBuf, target: PathBuf },

    /// Mutually exclusive or dependent flags were combined incorrectly.
    #[error("{0}")]
    InvalidCliCombination(String),
}

/// Malformed user-supplied values (sizes, durations, sort fields).
#[derive(Debug, Error, PartialEq)]
pub enum ParseError {
    #[error("invalid byte size {input:?}: {reason}")]
    InvalidByteSize { input: String, reason: String },

    #[error("invalid duration {input:?}: {reason}")]
    InvalidDuration { input: String, reason: String },

    #[error("invalid sort field {0:?} (expected \"size\", \"count\" or \"name\")")]
    InvalidSortField(String),

    /// A flag received a syntactically valid but out-of-range value.
    #[error("--{flag} {reason}")]
    InvalidFlagValue { flag: String, reason: String },
}

impl DiskPulseError {
    /// Process exit code per the CLI contract:
    /// `2` = user/usage mistake (bad flags, unknown target),
    /// `1` = runtime failure (I/O, deletion, missing path).
    pub fn exit_code(&self) -> i32 {
        match self {
            DiskPulseError::Parse(_) => 2,
            DiskPulseError::Safety(SafetyError::InvalidCliCombination(_)) => 2,
            DiskPulseError::Clean(CleanError::TargetNotFound(_)) => 2,
            _ => 1,
        }
    }
}

/// Failures raised while traversing the filesystem.
#[derive(Debug, Error, PartialEq)]
pub enum ScanError {
    #[error("path {0:?} does not exist")]
    PathNotFound(PathBuf),

    #[error("permission denied while reading {0:?}")]
    PermissionDenied(PathBuf),

    #[error("filesystem loop detected at {0:?}")]
    FilesystemLoopDetected(PathBuf),
}

/// Failures raised while deleting or trashing items.
#[derive(Debug, Error)]
pub enum CleanError {
    #[error("trash is unsupported or unavailable on this system: {0}")]
    TrashUnsupported(String),

    #[error("failed to delete {}: {source}", path.display())]
    DeletionFailed {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("unknown clean target {0:?}; run `diskpulse targets --all` to list candidates")]
    TargetNotFound(String),
}
