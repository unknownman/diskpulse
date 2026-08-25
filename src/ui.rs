//! Terminal presentation layer: headers, trees, tables, plans and reports.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, ContentArrangement, Table};
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::{OwoColorize, Stream};

use crate::cleaner::{CleanPlan, CleanReport, CleanTargetDef};
use crate::cli::DEFAULT_TOP;
use crate::errors::{CleanError, DiskPulseError, ParseError, SafetyError};
use crate::models::{CleanItemStatus, DirectoryNode, ScanResult};
use crate::scanner::{ScanOptions, SortCriterion};

pub fn heading(text: &str) -> String {
    text.if_supports_color(Stream::Stdout, |t| t.cyan().bold().to_string())
        .to_string()
}

pub fn dim(text: &str) -> String {
    text.if_supports_color(Stream::Stdout, |t| t.dimmed().to_string())
        .to_string()
}

pub fn success(text: &str) -> String {
    text.if_supports_color(Stream::Stdout, |t| t.green().to_string())
        .to_string()
}

pub fn warning(text: &str) -> String {
    text.if_supports_color(Stream::Stdout, |t| t.yellow().to_string())
        .to_string()
}

pub fn print_error(error: &anyhow::Error) {
    print_error_chain(error);
}

/// Render an error with its full causal chain and, where possible, an
/// actionable remediation hint. Never panics; never prints debug output.
///
/// ```text
/// error: Failed to scan directory '/root/secret'
///   └── Caused by: Permission denied (os error 13)
///   hint: Check directory permissions or run with elevated access.
/// ```
pub fn print_error_chain(error: &anyhow::Error) {
    let label = "error:".if_supports_color(Stream::Stderr, |t| t.red().bold().to_string());
    let mut causes = error.chain();
    let head = causes.next().map(|c| c.to_string()).unwrap_or_default();
    eprintln!("{label} {head}");

    for (depth, cause) in causes.enumerate() {
        let indent = " ".repeat(2 + depth * 2);
        eprintln!("{indent}└── Caused by: {cause}");
    }

    if let Some(hint) = remediation_hint(error) {
        let tag = "hint:".if_supports_color(Stream::Stderr, |t| t.cyan().bold().to_string());
        eprintln!("{tag} {hint}");
    }
}

/// Best-effort actionable advice keyed off the root [`DiskPulseError`].
fn remediation_hint(error: &anyhow::Error) -> Option<String> {
    let domain = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<DiskPulseError>())?;

    Some(match domain {
        DiskPulseError::Safety(SafetyError::InvalidCliCombination(msg)) => {
            if msg.contains("--yes") {
                "Add --apply to actually execute, e.g. `diskpulse clean --apply --yes`.".into()
            } else {
                "Review the conflicting flags shown above.".into()
            }
        }
        DiskPulseError::Safety(SafetyError::ProtectedUserData(_)) => {
            "Pick a cache location instead — run `diskpulse targets` to see candidates.".into()
        }
        DiskPulseError::Safety(SafetyError::ProtectedHomeRoot(_)) => {
            "Target a subfolder such as ~/.cache rather than the home root.".into()
        }
        DiskPulseError::Safety(SafetyError::ProtectedSystemPath(_)) => {
            "System locations cannot be bulk-cleaned; choose a specific cache target instead."
                .into()
        }
        DiskPulseError::Safety(_) => {
            "Inspect the path above; symlinked entries are skipped by design.".into()
        }
        DiskPulseError::Parse(ParseError::InvalidSortField(_)) => {
            "Valid sort fields: size, count, name.".into()
        }
        DiskPulseError::Parse(ParseError::InvalidByteSize { .. }) => {
            "Acceptable size suffixes: B, K/KiB, M/MiB, G/GiB (e.g. 50M, 2G).".into()
        }
        DiskPulseError::Parse(ParseError::InvalidDuration { .. }) => {
            "Acceptable duration suffixes: s, m, h, d (e.g. 12h, 7d).".into()
        }
        DiskPulseError::Parse(_) => "Re-check the flagged value.".into(),
        DiskPulseError::Scan(crate::errors::ScanError::PathNotFound(_)) => {
            "Verify spelling and that the path exists (`ls <path>`).".into()
        }
        DiskPulseError::Scan(crate::errors::ScanError::PermissionDenied(_)) => {
            "Check permissions on the path, or re-run with broader read access.".into()
        }
        DiskPulseError::Clean(CleanError::TargetNotFound(_)) => {
            "Run `diskpulse targets --all` to list valid target IDs.".into()
        }
        DiskPulseError::Scan(crate::errors::ScanError::FilesystemLoopDetected(_)) => {
            "The path contains a filesystem loop; narrow the target directory.".into()
        }
        DiskPulseError::Clean(_) => {
            "Some items could not be removed; they were left in place. Re-run to retry.".into()
        }
        DiskPulseError::Io(_) => {
            "This is usually transient — check disk state and permissions.".into()
        }
    })
}

// ---------------------------------------------------------------------------
// Visualizer rendering
// ---------------------------------------------------------------------------

const BAR_WIDTH: usize = 10;
const MIB: u64 = 1024 * 1024;
/// Width of the subtle rule separating tree body and summary footer.
const SEPARATOR_WIDTH: usize = 80;

/// Launch the animated scan progress indicator (drawn on stderr, so stdout
/// stays clean for redirection). The caller owns the lifecycle and must call
/// `finish_and_clear()` once scanning completes.
pub fn scan_spinner(target: &Path) -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner:.green} Scanning {msg} [{elapsed_precise}]")
            .expect("spinner template is statically valid"),
    );
    spinner.enable_steady_tick(Duration::from_millis(100));
    spinner.set_message(target.display().to_string());
    spinner
}

// ---------------------------------------------------------------------------
// Interrupt (Ctrl+C) support
// ---------------------------------------------------------------------------

/// The spinner currently animating, if any. The SIGINT handler consults this
/// to wipe the progress line before printing the interruption notice.
static ACTIVE_SPINNER: Mutex<Option<ProgressBar>> = Mutex::new(None);

/// Track `spinner` so a Ctrl+C arriving mid-scan can clear it.
pub fn track_spinner(spinner: &ProgressBar) {
    if let Ok(mut slot) = ACTIVE_SPINNER.lock() {
        *slot = Some(spinner.clone());
    }
}

/// Stop tracking and erase `spinner` after normal completion.
pub fn release_spinner(spinner: &ProgressBar) {
    spinner.finish_and_clear();
    if let Ok(mut slot) = ACTIVE_SPINNER.lock() {
        *slot = None;
    }
}

/// Restore terminal state after an abrupt interrupt: clear any active
/// spinner, un-hide the cursor, and print the cancellation notice on stderr.
///
/// Called from the ctrlc handler thread — safe to lock/allocate there.
pub fn recover_terminal_on_interrupt() {
    if let Ok(slot) = ACTIVE_SPINNER.lock() {
        if let Some(spinner) = slot.as_ref() {
            spinner.finish_and_clear();
        }
    }
    // Best-effort cursor restore in case an interactive prompt hid it.
    eprint!("\n\x1b[?25hOperation interrupted by user.\n");
}

/// Group an integer with thousands separators (e.g. `4821` -> `"4,821"`).
#[must_use]
pub fn thousands(value: u64) -> String {
    let raw = value.to_string();
    let len = raw.len();
    let mut grouped = String::with_capacity(raw.len() + raw.len() / 3);
    for (idx, digit) in raw.chars().enumerate() {
        if idx > 0 && (len - idx).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// Render a proportional capacity bar of [`BAR_WIDTH`] blocks.
///
/// `0%` -> `░░░░░░░░░░`, `50%` -> `█████░░░░░`, `100%` -> `██████████`.
#[must_use]
pub fn capacity_bar(percent: f64) -> String {
    let filled = fill_blocks(percent);
    let mut bar = String::with_capacity(BAR_WIDTH * '█'.len_utf8());
    bar.push_str(&"█".repeat(filled));
    bar.push_str(&"░".repeat(BAR_WIDTH - filled));
    bar
}

fn fill_blocks(percent: f64) -> usize {
    (((percent.clamp(0.0, 100.0) / 100.0) * BAR_WIDTH as f64).round() as usize).min(BAR_WIDTH)
}

/// Compact two-decimal size label (e.g. `"140.20 KB"`, `"3.80 GB"`).
fn size_label(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.2} {}", UNITS[unit])
}

/// Color policy for viz rendering. When disabled, every method returns the
/// plain text; when enabled, output is additionally gated on stdout's color
/// support (TTY detection, `NO_COLOR`, overrides).
struct Palette {
    enabled: bool,
}

impl Palette {
    fn paint(&self, text: &str, style: impl Fn(&&str) -> String) -> String {
        if !self.enabled {
            return text.to_owned();
        }
        text.if_supports_color(Stream::Stdout, style).to_string()
    }

    fn dir_name(&self, text: &str) -> String {
        self.paint(text, |t| t.bold().cyan().to_string())
    }

    fn file_name(&self, text: &str) -> String {
        self.paint(text, |t| t.white().dimmed().to_string())
    }

    /// Size labels tier by absolute magnitude:
    /// green below 10 MB, yellow below 500 MB, red above.
    fn size(&self, bytes: u64) -> String {
        let label = size_label(bytes);
        if bytes < 10 * MIB {
            self.paint(&label, |t| t.green().to_string())
        } else if bytes < 500 * MIB {
            self.paint(&label, |t| t.yellow().to_string())
        } else {
            self.paint(&label, |t| t.red().to_string())
        }
    }

    /// Capacity bar whose filled blocks tier by share of the parent:
    /// cyan below 25%, yellow up to 75%, red above. Empty blocks stay gray.
    fn bar(&self, percent: f64) -> String {
        let filled_text = "█".repeat(fill_blocks(percent));
        let empty_text = "░".repeat(BAR_WIDTH - fill_blocks(percent));
        if !self.enabled {
            return format!("{filled_text}{empty_text}");
        }
        let painted_fill = if percent >= 75.0 {
            self.paint(&filled_text, |t| t.red().to_string())
        } else if percent >= 25.0 {
            self.paint(&filled_text, |t| t.yellow().to_string())
        } else {
            self.paint(&filled_text, |t| t.cyan().to_string())
        };
        let painted_empty = self.paint(&empty_text, |t| t.truecolor(110, 110, 110).to_string());
        format!("{painted_fill}{painted_empty}")
    }

    fn metric(&self, text: &str) -> String {
        self.paint(text, |t| t.bold().to_string())
    }

    fn subtle(&self, text: &str) -> String {
        self.paint(text, |t| t.dimmed().to_string())
    }
}

/// Build the complete visualizer view (root card, ANSI tree, summary footer)
/// as a string. Pure formatting: no direct terminal writes, fully testable.
#[must_use]
pub fn format_viz_tree(result: &ScanResult, options: &ScanOptions, no_color: bool) -> String {
    let palette = Palette { enabled: !no_color };
    let mut out = String::new();

    out.push_str(&format!(
        "📁 {} (Total: {}, Apparent: {})\n",
        result.summary.root_path.display(),
        palette.size(result.summary.total_size),
        palette.size(result.summary.total_apparent_size),
    ));

    append_children(
        &mut out,
        &result.root.children,
        result.root.size,
        "",
        &palette,
    );

    out.push_str(&palette.subtle(&"─".repeat(SEPARATOR_WIDTH)));
    out.push('\n');
    out.push_str("Summary: ");
    out.push_str(&palette.metric(&size_label(result.summary.total_size)));
    out.push_str(" allocated across ");
    out.push_str(&palette.metric(&thousands(result.summary.total_files)));
    out.push_str(" files and ");
    out.push_str(&palette.metric(&thousands(result.summary.total_dirs)));
    out.push_str(" directories (Scanned in ");
    out.push_str(&palette.metric(&result.summary.duration_ms.to_string()));
    out.push_str("ms)");
    append_hints(&mut out, options, &palette);
    out.push('\n');
    out
}

fn append_hints(out: &mut String, options: &ScanOptions, palette: &Palette) {
    let mut hints = Vec::new();
    if options.sort_by != SortCriterion::Size {
        hints.push(format!("sorted by {}", sort_label(options.sort_by)));
    }
    if let Some(min_size) = options.min_size {
        hints.push(format!("min {}", size_label(min_size)));
    }
    if let Some(top_n) = options.top_n {
        if top_n != DEFAULT_TOP {
            hints.push(format!("top {top_n}"));
        }
    }
    if !hints.is_empty() {
        out.push_str(&palette.subtle(&format!(" · {}", hints.join(" · "))));
    }
}

fn sort_label(criterion: SortCriterion) -> &'static str {
    match criterion {
        SortCriterion::Size => "size",
        SortCriterion::Count => "count",
        SortCriterion::Name => "name",
    }
}

/// Recursively append `children` of one parent, drawing Unicode branch
/// glyphs relative to `prefix` and sizing percentages against `parent_total`.
fn append_children(
    out: &mut String,
    children: &[DirectoryNode],
    parent_total: u64,
    prefix: &str,
    palette: &Palette,
) {
    let Some(name_width) = children.iter().map(|c| c.name.chars().count()).max() else {
        return;
    };
    let last_idx = children.len() - 1;

    for (idx, child) in children.iter().enumerate() {
        let is_last = idx == last_idx;
        out.push_str(prefix);
        out.push_str(if is_last { "└── " } else { "├── " });
        append_entry(out, child, parent_total, name_width, palette);

        if child.is_dir {
            let mut nested_prefix = String::from(prefix);
            nested_prefix.push_str(if is_last { "    " } else { "│   " });
            append_children(out, &child.children, child.size, &nested_prefix, palette);
        }
    }
}

fn append_entry(
    out: &mut String,
    node: &DirectoryNode,
    parent_total: u64,
    name_width: usize,
    palette: &Palette,
) {
    let percent = if parent_total > 0 {
        (node.size as f64 / parent_total as f64) * 100.0
    } else {
        0.0
    };

    let padded = pad_right(&node.name, name_width);
    let name = if node.is_dir {
        palette.dir_name(&padded)
    } else {
        palette.file_name(&padded)
    };
    let icon = if node.is_dir { "📁" } else { "📄" };

    out.push_str(icon);
    out.push(' ');
    out.push_str(&name);
    out.push_str(" [");
    out.push_str(&palette.bar(percent));
    out.push_str("] ");
    out.push_str(&palette.size(node.size));
    out.push_str(&format!(" ({percent:.1}%)"));

    if node.is_dir {
        let counts = format!(
            "[{} dirs, {} files]",
            thousands(node.dir_count),
            thousands(node.file_count)
        );
        out.push(' ');
        out.push_str(&palette.subtle(&counts));
    }
    out.push('\n');
}

/// Left-pad `text` with trailing spaces to `width` characters, aligning the
/// bar column across siblings.
fn pad_right(text: &str, width: usize) -> String {
    let visible = text.chars().count();
    if visible >= width {
        text.to_owned()
    } else {
        let mut padded = String::with_capacity(width);
        padded.push_str(text);
        padded.push_str(&" ".repeat(width - visible));
        padded
    }
}

/// Render the visualizer tree to stdout.
pub fn render_viz_tree(result: &ScanResult, options: &ScanOptions, no_color: bool) -> Result<()> {
    print!("{}", format_viz_tree(result, options, no_color));
    Ok(())
}

/// Serialize a scan result as pretty-printed JSON (pure string form).
pub fn format_viz_json(result: &ScanResult) -> Result<String> {
    Ok(serde_json::to_string_pretty(result)?)
}

/// Render the visualizer result as JSON to stdout.
pub fn render_viz_json(result: &ScanResult) -> Result<()> {
    println!("{}", format_viz_json(result)?);
    Ok(())
}

// ---------------------------------------------------------------------------
// Cleaner rendering
// ---------------------------------------------------------------------------

fn item_count_label(count: usize) -> String {
    let noun = if count == 1 { "item" } else { "items" };
    format!(
        "{} {}",
        thousands(u64::try_from(count).unwrap_or(u64::MAX)),
        noun
    )
}

fn status_cell(status: &CleanItemStatus) -> Cell {
    match status {
        CleanItemStatus::Deleted => Cell::new(status.to_string()).fg(Color::Green),
        CleanItemStatus::MovedToTrash => Cell::new(status.to_string()).fg(Color::Cyan),
        CleanItemStatus::SkippedDryRun => Cell::new(status.to_string()).fg(Color::DarkYellow),
        CleanItemStatus::Failed(_) => Cell::new(status.to_string()).fg(Color::Red),
    }
}

/// Render the cleanup plan as a Unicode table.
///
/// `is_apply` switches the footer between dry-run guidance and the
/// pre-execution warning shown before interactive confirmation.
pub fn render_clean_plan_table(plan: &CleanPlan, is_apply: bool) -> Result<()> {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec![
        Cell::new("Target").add_attribute(Attribute::Bold),
        Cell::new("Path").add_attribute(Attribute::Bold),
        Cell::new("Items").add_attribute(Attribute::Bold),
        Cell::new("Reclaimable").add_attribute(Attribute::Bold),
    ]);

    if plan.targets.is_empty() {
        println!(
            "{}",
            warning("No cleanable items found — everything is already tidy.")
        );
        return Ok(());
    }

    for target in &plan.targets {
        table.add_row(vec![
            Cell::new(target.target_name.clone()),
            Cell::new(target.path.display().to_string()),
            Cell::new(item_count_label(target.item_count)),
            Cell::new(size_label(target.total_bytes)),
        ]);
    }
    table.add_row(vec![
        Cell::new("TOTAL")
            .fg(Color::Cyan)
            .add_attribute(Attribute::Bold),
        Cell::new(""),
        Cell::new(item_count_label(plan.total_items))
            .fg(Color::Cyan)
            .add_attribute(Attribute::Bold),
        Cell::new(size_label(plan.total_bytes))
            .fg(Color::Cyan)
            .add_attribute(Attribute::Bold),
    ]);
    println!("{table}");

    if is_apply {
        println!(
            "{}",
            warning(&format!(
                "⚠  Applying will {} {} across {} items. Confirmation required unless --yes.",
                "permanently delete",
                size_label(plan.total_bytes),
                thousands(u64::try_from(plan.total_items).unwrap_or(u64::MAX)),
            ))
        );
    } else {
        println!("{}", warning("💡 [DRY-RUN MODE]: No files were deleted."));
        println!("   To apply these changes, run: diskpulse clean --apply");
    }
    Ok(())
}

/// Render the cleanup plan as pretty JSON (dry-run machine output).
pub fn render_clean_plan_json(plan: &CleanPlan) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(plan)?);
    Ok(())
}

/// Render post-execution outcomes: freed totals, per-item statuses and
/// failure reasons.
pub fn render_clean_report_table(report: &CleanReport) -> Result<()> {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec![
        Cell::new("Target").add_attribute(Attribute::Bold),
        Cell::new("Path").add_attribute(Attribute::Bold),
        Cell::new("Size").add_attribute(Attribute::Bold),
        Cell::new("Status").add_attribute(Attribute::Bold),
    ]);
    for item in &report.results {
        table.add_row(vec![
            Cell::new(item.target_id.clone()),
            Cell::new(item.path.display().to_string()),
            Cell::new(size_label(item.size)),
            status_cell(&item.status),
        ]);
    }
    println!("{table}");

    println!(
        "Freed {} across {} items in {} ms.",
        heading(&size_label(report.bytes_freed)),
        heading(&thousands(
            u64::try_from(report.items_freed).unwrap_or(u64::MAX)
        )),
        report.duration_ms
    );
    if report.dry_run {
        println!("{}", warning("Dry run — nothing was deleted."));
    }
    if report.errors_count > 0 {
        println!(
            "{} {} item(s) failed; see the table above for reasons.",
            warning("warning:"),
            report.errors_count
        );
    }
    Ok(())
}

/// Render the execution report as pretty JSON.
pub fn render_clean_report_json(report: &CleanReport) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(report)?);
    Ok(())
}

pub fn render_targets(targets: &[CleanTargetDef]) {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec![
        Cell::new("ID").add_attribute(Attribute::Bold),
        Cell::new("Name").add_attribute(Attribute::Bold),
        Cell::new("Active").add_attribute(Attribute::Bold),
        Cell::new("Paths").add_attribute(Attribute::Bold),
        Cell::new("Description").add_attribute(Attribute::Bold),
    ]);

    for target in targets {
        let active_cell = if target.enabled_by_default {
            Cell::new("default").fg(Color::Green)
        } else {
            Cell::new("opt-in").fg(Color::DarkYellow)
        };
        let paths = if target.paths.is_empty() {
            "—".to_string()
        } else {
            target
                .paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        table.add_row(vec![
            Cell::new(target.id),
            Cell::new(target.name),
            active_cell,
            Cell::new(paths),
            Cell::new(target.description),
        ]);
    }

    println!("{table}");
    println!(
        "{}",
        dim(&format!(
            "{} target(s) shown; pass --all to include opt-in targets.",
            targets.len()
        ))
    );
}
