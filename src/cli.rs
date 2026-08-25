//! Command-line interface definition (clap derive) and argument validation.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

// ---------------------------------------------------------------------------
// Help copy
// ---------------------------------------------------------------------------

const ABOUT: &str = "A fast, safe, and beautiful disk visualizer and cache cleaner";

const LONG_ABOUT: &str = "\
A fast, safe, and beautiful disk visualizer and cache cleaner.

diskpulse answers two questions:

  * `viz`    — what is using all this space? (hierarchical tree view)
  * `clean`  — which caches can be safely reclaimed? (dry-run first)

Safety model: `clean` never deletes anything unless `--apply` is passed, and
interactive confirmation is required on top of that unless `--yes` is also
given. Personal folders (Documents, Desktop, Downloads, ...) are always
protected.";

const VIZ_LONG_ABOUT: &str = "\
Visualize directory disk usage in a hierarchical tree view.

Sizes reflect physical allocated blocks by default (pass --apparent-size for
logical byte sizes). Directories are sorted largest-first; each level shows
the top N entries plus an aggregate row for everything pruned from view.
Heavyweight dev directories (.git, node_modules, target, dist, build,
__pycache__, .venv) are skipped unless --no-ignore is given.";

const VIZ_EXAMPLES: &str = "\
EXAMPLES:
    # Visualize current directory up to depth 2 (largest first)
    diskpulse viz

    # Scan custom path up to depth 3, showing top 5 largest items
    diskpulse viz ~/Projects --depth 3 --top 5

    # Filter out anything smaller than 50 MB and show hidden files
    diskpulse viz /var/log --min-size 50M --hidden

    # Export disk usage breakdown as JSON
    diskpulse viz /data --json > usage.json";

const CLEAN_LONG_ABOUT: &str = "\
Safely inspect and clean temporary files and caches.

By default this command is a DRY RUN: it prints what would be reclaimed and
deletes nothing. Add --apply to execute. Even then, an interactive
confirmation prompt is shown unless --yes is also passed. Use --trash to move
items to the OS Recycle Bin instead of unlinking them permanently.

Run `diskpulse targets` to see every supported cache location on this OS.";

const CLEAN_EXAMPLES: &str = "\
EXAMPLES:
    # 1. Safe Dry-Run: Inspect all reclaimable cache space (deletes NOTHING)
    diskpulse clean

    # 2. Interactive Clean: Clean default caches with interactive confirmation prompt
    diskpulse clean --apply

    # 3. Clean specific targets only:
    diskpulse clean cargo-cache npm-cache --apply

    # 4. Safe deletion via OS Recycle Bin / Trash:
    diskpulse clean --apply --trash

    # 5. Non-interactive cleanup for CI / Automation (requires both --apply and --yes):
    diskpulse clean system-temp --apply --yes --older-than 7d";

const TARGETS_LONG_ABOUT: &str = "\
List all supported cache targets and locations on this OS.

Each target has a stable ID usable with `diskpulse clean <ID>...`. Targets
marked `default` are included when `clean` runs without arguments; opt-in
targets require an explicit ID or `clean ... all`.";

const TARGETS_EXAMPLES: &str = "\
EXAMPLES:
    # Show default targets and their resolved paths
    diskpulse targets

    # Include opt-in targets (xcode, docker-build, browser-cache)
    diskpulse targets --all";

const COMPLETIONS_LONG_ABOUT: &str = "\
Generate shell completion scripts for diskpulse.

Suggested installation:

    # bash (~/.bashrc)
    source <(diskpulse completions bash)

    # zsh (~/.zshrc; requires compinit)
    source <(diskpulse completions zsh)

    # fish (~/.config/fish/completions/diskpulse.fish)
    diskpulse completions fish > ~/.config/fish/completions/diskpulse.fish

    # PowerShell ($PROFILE)
    diskpulse completions powershell | Out-String | Invoke-Expression";

/// fast, safe, and beautiful disk space visualization and cache cleanup
#[derive(Parser, Debug)]
#[command(
    name = "diskpulse",
    version,
    propagate_version = true,
    about = ABOUT,
    long_about = LONG_ABOUT
)]
pub struct Cli {
    /// Increase verbosity (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress non-error output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Disable colored terminal output (also honored via NO_COLOR env var)
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Emit machine-readable JSON output (stdout only; progress stays silent)
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Commands,
}

/// Severity threshold derived from the number of `-v` occurrences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LevelFilter {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LevelFilter {
    const DEFAULT: LevelFilter = LevelFilter::Warn;
}

impl Cli {
    /// Map the counted `-v` flags onto a log level filter.
    pub fn log_level(&self) -> LevelFilter {
        match self.verbose {
            0 => LevelFilter::DEFAULT,
            1 => LevelFilter::Info,
            2 => LevelFilter::Debug,
            _ => LevelFilter::Trace,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Visualize directory disk usage in a hierarchical tree view
    #[command(
        aliases = ["v", "scan", "analyze"],
        long_about = VIZ_LONG_ABOUT,
        after_help = VIZ_EXAMPLES,
        after_long_help = VIZ_EXAMPLES
    )]
    Viz(VizArgs),

    /// Safely inspect and clean temporary files and caches
    #[command(
        aliases = ["c", "prune", "sweep"],
        long_about = CLEAN_LONG_ABOUT,
        after_help = CLEAN_EXAMPLES,
        after_long_help = CLEAN_EXAMPLES
    )]
    Clean(CleanArgs),

    /// List all supported cache targets and locations on this OS
    #[command(
        alias = "rules",
        long_about = TARGETS_LONG_ABOUT,
        after_help = TARGETS_EXAMPLES,
        after_long_help = TARGETS_EXAMPLES
    )]
    Targets(TargetsArgs),

    /// Generate shell completion scripts (bash, zsh, fish, powershell)
    #[command(long_about = COMPLETIONS_LONG_ABOUT)]
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

pub const DEFAULT_DEPTH: usize = 2;
pub const DEFAULT_TOP: usize = 10;

/// Arguments for `diskpulse viz`.
#[derive(Args, Debug)]
pub struct VizArgs {
    /// Directory to inspect (defaults to the current directory)
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Maximum traversal depth to visualize
    #[arg(short, long, default_value_t = DEFAULT_DEPTH, value_name = "INT")]
    pub depth: usize,

    /// Number of largest entries to show per directory
    #[arg(short = 'n', long, default_value_t = DEFAULT_TOP, value_name = "INT")]
    pub top: usize,

    /// Hide entries smaller than this threshold (e.g. "100M", "1G")
    #[arg(short = 'm', long, value_name = "SIZE")]
    pub min_size: Option<String>,

    /// Sort entries by "size", "count", or "name"
    #[arg(short, long, default_value = "size")]
    pub sort: String,

    /// Report logical file sizes instead of allocated disk blocks
    #[arg(long)]
    pub apparent_size: bool,

    /// Do not ignore heavyweight dev directories (.git, node_modules, target, ...)
    #[arg(long)]
    pub no_ignore: bool,

    /// Include hidden and dot-files
    #[arg(long)]
    pub hidden: bool,

    /// Stay on a single filesystem (do not cross mount points)
    #[arg(short = 'x', long)]
    pub one_file_system: bool,
}

impl VizArgs {
    /// Typed validation so malformed flags exit with code 2.
    pub fn validate(&self) -> crate::errors::Result<()> {
        use crate::errors::ParseError;
        if self.depth == 0 {
            return Err(ParseError::InvalidFlagValue {
                flag: "depth".into(),
                reason: "must be at least 1".into(),
            }
            .into());
        }
        if self.top == 0 {
            return Err(ParseError::InvalidFlagValue {
                flag: "top".into(),
                reason: "must be at least 1".into(),
            }
            .into());
        }
        crate::scanner::SortCriterion::parse(&self.sort)?;
        if let Some(raw) = &self.min_size {
            crate::util::parse_size(raw)?;
        }
        Ok(())
    }
}

/// Arguments for `diskpulse clean`.
#[derive(Args, Debug)]
pub struct CleanArgs {
    /// Target IDs to clean (e.g. "cargo-cache", "system-temp"); empty selects every default target, "all" expands to every registered target including opt-ins
    #[arg(value_name = "TARGETS")]
    pub targets: Vec<String>,

    /// Perform the deletion; without this flag diskpulse only prints a dry-run plan
    #[arg(long)]
    pub apply: bool,

    /// Skip the confirmation prompt (requires --apply)
    #[arg(short, long)]
    pub yes: bool,

    /// Move items to the OS trash instead of unlinking them permanently
    #[arg(long)]
    pub trash: bool,

    /// Only consider items older than this age (e.g. "30d", "12h")
    #[arg(long, value_name = "DURATION")]
    pub older_than: Option<String>,

    /// Only consider items at least this large (e.g. "10M")
    #[arg(long, value_name = "SIZE")]
    pub min_size: Option<String>,

    /// Clean a specific path using cleaner heuristics instead of registered targets
    #[arg(short, long, value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Stay on a single filesystem (do not cross mount points)
    #[arg(short = 'x', long)]
    pub one_file_system: bool,
}

impl CleanArgs {
    /// Typed validation so safety violations surface as
    /// [`crate::errors::SafetyError::InvalidCliCombination`] (exit code 2).
    pub fn validate(&self) -> crate::errors::Result<()> {
        if self.yes && !self.apply {
            return Err(crate::errors::SafetyError::InvalidCliCombination(
                "--yes flag was provided without --apply. To prevent accidental operations, --yes requires --apply."
                    .to_string(),
            )
            .into());
        }
        if let Some(raw) = &self.min_size {
            crate::util::parse_size(raw)?;
        }
        if let Some(raw) = &self.older_than {
            crate::util::parse_duration(raw)?;
        }
        Ok(())
    }
}

/// Arguments for `diskpulse targets`.
#[derive(Args, Debug)]
pub struct TargetsArgs {
    /// Include opt-in (non-default) targets as well
    #[arg(long)]
    pub all: bool,
}
