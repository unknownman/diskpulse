//! diskpulse domain library: CLI surface, typed errors, pure domain models,
//! and the scanner/cleaner engine contracts.

pub mod cleaner;
pub mod cli;
pub mod errors;
pub mod models;
pub mod scanner;
pub mod ui;
pub mod util;

pub use errors::DiskPulseError;
pub use models::{
    CleanItem, CleanItemResult, CleanItemStatus, CleanPlan, CleanReport, DirectoryNode, EntryKind,
    FileEntry, ScanResult, ScanSummary, TargetSummary,
};
