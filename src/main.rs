//! diskpulse — a fast, safe, and beautiful disk visualizer and cache cleaner.
//!
//! Binary entry point: parses the CLI, installs signal handling, dispatches
//! into the library and maps failures onto the documented exit-code matrix:
//!
//! * `0`   — success (including clean user aborts and dry runs)
//! * `1`   — runtime error (I/O, failed deletion, missing path)
//! * `2`   — CLI usage/validation error (bad flags, unknown target)
//! * `130` — interrupted by SIGINT (Ctrl+C)

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};

use diskpulse::cleaner::{create_clean_plan, execute_clean_plan, CleanOptions, CleanTargetDef};
use diskpulse::cli::{CleanArgs, Cli, Commands, TargetsArgs, VizArgs};
use diskpulse::errors::DiskPulseError;
use diskpulse::scanner::{self, ScanOptions};
use diskpulse::ui;

fn main() -> ExitCode {
    #[cfg(unix)]
    restore_default_sigpipe();

    let cli = Cli::parse();
    install_interrupt_handler();

    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            ui::print_error_chain(&error);
            ExitCode::from(exit_code_for(&error) as u8)
        }
    }
}

/// Walk the causal chain looking for a typed [`DiskPulseError`] and derive
/// its exit code; unclassified failures are runtime errors (exit 1).
fn exit_code_for(error: &anyhow::Error) -> i32 {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<DiskPulseError>())
        .map_or(1, DiskPulseError::exit_code)
}

/// Route SIGINT/SIGTERM through the `ctrlc` thread so terminal cleanup can
/// lock, allocate and print safely before exiting with code 130.
fn install_interrupt_handler() {
    let _ = ctrlc::set_handler(|| {
        ui::recover_terminal_on_interrupt();
        std::process::exit(130);
    });
}

/// Rust ignores SIGPIPE by default, turning early-closed pipes (`diskpulse
/// viz | head`) into stdout write panics. Restoring SIG_DFL makes the process
/// terminate silently, like any other well-behaved CLI tool.
#[cfg(unix)]
fn restore_default_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

fn run(cli: &Cli) -> Result<()> {
    // `--no-color` wins over the environment; NO_COLOR is honored unless the
    // user explicitly forces color back on via CLICOLOR_FORCE.
    let no_color_env = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let force_color = std::env::var_os("CLICOLOR_FORCE").is_some_and(|v| !v.is_empty());
    if cli.no_color || (no_color_env && !force_color) {
        owo_colors::set_override(false);
    }

    match &cli.command {
        Commands::Viz(args) => cmd_viz(args, cli),
        Commands::Clean(args) => cmd_clean(args, cli),
        Commands::Targets(args) => cmd_targets(args, cli),
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            clap_complete::generate(*shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
    }
}

fn cmd_viz(args: &VizArgs, cli: &Cli) -> Result<()> {
    args.validate()?;

    let target_path = args.path.clone().unwrap_or_else(|| PathBuf::from("."));
    let options = ScanOptions::from(args);

    // Progress lives on stderr and is fully disabled for quiet/JSON runs,
    // keeping stdout clean for redirection.
    let spinner = if cli.quiet || cli.json {
        None
    } else {
        Some(ui::scan_spinner(&target_path))
    };
    if let Some(spinner) = &spinner {
        ui::track_spinner(spinner);
    }

    let result = scanner::scan_path(&target_path, &options)
        .with_context(|| format!("failed to inspect {}", target_path.display()))?;

    if let Some(spinner) = &spinner {
        ui::release_spinner(spinner);
    }

    if !cli.quiet && !cli.json {
        println!(
            "Scanning: {} ... [{} files, {} dirs scanned]",
            target_path.display(),
            ui::thousands(result.summary.total_files),
            ui::thousands(result.summary.total_dirs)
        );
    }

    if cli.json {
        ui::render_viz_json(&result)
    } else {
        ui::render_viz_tree(&result, &options, cli.no_color)
    }
}

fn cmd_clean(args: &CleanArgs, cli: &Cli) -> Result<()> {
    // Typed gate: `--yes` without `--apply` aborts before any IO happens.
    args.validate()?;

    let options = CleanOptions::from(args);
    let plan = create_clean_plan(&options).context("failed to build a cleanup plan")?;

    if cli.json && !options.apply {
        return ui::render_clean_plan_json(&plan);
    }

    if !cli.quiet && !cli.json {
        ui::render_clean_plan_table(&plan, options.apply)?;
    }

    if !options.apply {
        return Ok(());
    }

    if !options.yes && plan.total_items > 0 {
        if !stdin_is_interactive() {
            // Piped/EOF stdin cannot answer a prompt: treat as a clean abort.
            println!("Operation cancelled by user.");
            return Ok(());
        }
        let confirmed = dialoguer::Confirm::new()
            .with_prompt(format!(
                "Permanently delete {} items of cached data?",
                plan.total_items
            ))
            .default(false)
            .interact()?;
        if !confirmed {
            println!("Operation cancelled by user.");
            return Ok(());
        }
    }

    let report = execute_clean_plan(&plan, options.use_trash)?;
    if cli.json {
        ui::render_clean_report_json(&report)
    } else if !cli.quiet {
        ui::render_clean_report_table(&report)
    } else {
        Ok(())
    }
}

/// True when stdin can actually deliver keystrokes to an interactive prompt.
#[cfg(unix)]
fn stdin_is_interactive() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

#[cfg(not(unix))]
fn stdin_is_interactive() -> bool {
    true
}

fn cmd_targets(args: &TargetsArgs, cli: &Cli) -> Result<()> {
    let catalog: Vec<CleanTargetDef> = diskpulse::cleaner::get_registered_targets();
    let visible: Vec<CleanTargetDef> = if args.all {
        catalog
    } else {
        catalog
            .into_iter()
            .filter(|target| target.enabled_by_default)
            .collect()
    };

    if !cli.quiet {
        ui::render_targets(&visible);
    }
    Ok(())
}
