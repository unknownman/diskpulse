# diskpulse

[![crates.io](https://img.shields.io/crates/v/diskpulse.svg)](https://crates.io/crates/diskpulse)
[![license](https://img.shields.io/crates/l/diskpulse.svg)](#license)
[![CI](https://github.com/unknownman/diskpulse/actions/workflows/ci.yml/badge.svg)](https://github.com/unknownman/diskpulse/actions/workflows/ci.yml)

![diskpulse demo](demo/diskpulse-demo.gif)

See diskpulse visualize a project's disk usage and preview a cache cleanup —
nothing shown here was ever deleted; try it yourself with `diskpulse viz` and
`diskpulse clean` (dry-run by default).

A fast, safe, and beautiful disk visualizer and cache cleaner for the terminal.

`diskpulse` answers two questions: **`viz`** shows what is using all your space
as a hierarchical tree, and **`clean`** shows which caches can be safely
reclaimed. Scanning is parallel (via [`jwalk`](https://crates.io/crates/jwalk)),
cleaning is safety-first with a hard-coded jail around your personal data and
the operating system, and `clean` is a dry run by default.

---

## ⚠️ Safety model

These guarantees are enforced in code (`src/cleaner.rs`), not by convention:

- **Dry-run by default.** `diskpulse clean` prints a plan of what *would* be
  reclaimed and deletes nothing without `--apply`.
- **Confirmation on top of `--apply`.** Even with `--apply`, an interactive
  confirmation prompt is shown unless `--yes` is also passed.
- **A hard-coded safety jail** unconditionally refuses to clean:
  - Filesystem/drive roots: `/` on Unix; `C:\` and other drive roots on Windows.
  - `$HOME` itself **and its parent** (the directory holding all user
    profiles, e.g. `/home`, `/Users`).
  - System directories (Unix): `/bin`, `/sbin`, `/usr`, `/usr/bin`, `/etc`,
    `/lib`, `/boot`, `/dev`, `/sys`, `/proc`, `/var` — with `/tmp` and
    `/var/tmp` deliberately carved back out as cleanable temp locations.
    Windows additionally protects `C:\Windows`, `C:\Program Files` and
    `C:\Program Files (x86)`.
  - Personal-data directories: Documents, Desktop, Downloads, Music,
    Pictures, Videos.
- **Symlinks are quarantined.** diskpulse only ever unlinks a symlink itself;
  it never follows a link into deletion and never sizes a directory symlink by
  its target's contents.
- **`--trash` moves items to the OS recycle bin** instead of unlinking them —
  except symlinks, which are always hard-unlinked even under `--trash`,
  because a trash API that follows links would be unsafe.
- **Re-validation immediately before every deletion** (a TOCTOU mitigation):
  the plan is produced by a read-only scan, so anything may have changed on
  disk by execution time. Each item's real location is re-checked against the
  jail at the last possible moment.

This is not a substitute for backups. It is designed so that ordinary use
cannot accidentally destroy personal data or the OS itself.

## Installation

### From crates.io

Once published:

```bash
cargo install diskpulse
```

### From source

```bash
git clone https://github.com/unknownman/diskpulse
cd diskpulse
cargo build --release
# binary at target/release/diskpulse
```

### Pre-built binaries

Grab an archive from the
[GitHub Releases](https://github.com/unknownman/diskpulse/releases) page, which is
populated automatically by the release workflow for these platforms:
Linux x86_64, Linux aarch64, macOS x86_64, macOS arm64, Windows x86_64.

```bash
# Linux x86_64
curl -LO https://github.com/unknownman/diskpulse/releases/download/v0.1.0/diskpulse-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
tar xzf diskpulse-v0.1.0-x86_64-unknown-linux-gnu.tar.gz

# macOS arm64 (Apple Silicon)
curl -LO https://github.com/unknownman/diskpulse/releases/download/v0.1.0/diskpulse-v0.1.0-aarch64-apple-darwin.tar.gz
tar xzf diskpulse-v0.1.0-aarch64-apple-darwin.tar.gz

# macOS x86_64
curl -LO https://github.com/unknownman/diskpulse/releases/download/v0.1.0/diskpulse-v0.1.0-x86_64-apple-darwin.tar.gz

# Windows x86_64 (PowerShell): diskpulse-v0.1.0-x86_64-pc-windows-msvc.zip
```

### Shell completions

```bash
# bash (~/.bashrc)
source <(diskpulse completions bash)

# zsh (~/.zshrc; requires compinit)
source <(diskpulse completions zsh)

# fish (~/.config/fish/completions/diskpulse.fish)
diskpulse completions fish > ~/.config/fish/completions/diskpulse.fish

# PowerShell ($PROFILE)
diskpulse completions powershell | Out-String | Invoke-Expression
```

## Quick start

```bash
# Visualize current directory up to depth 2 (largest first)
diskpulse viz

# Scan custom path up to depth 3, showing top 5 largest items
diskpulse viz ~/Projects --depth 3 --top 5

# Filter out anything smaller than 50 MB and show hidden files
diskpulse viz /var/log --min-size 50M --hidden

# Export disk usage breakdown as JSON
diskpulse viz /data --json > usage.json

# 1. Safe Dry-Run: Inspect all reclaimable cache space (deletes NOTHING)
diskpulse clean

# 2. Interactive Clean: Clean default caches with confirmation prompt
diskpulse clean --apply

# 3. Clean specific targets only:
diskpulse clean cargo-cache npm-cache --apply

# 4. Safe deletion via OS Recycle Bin / Trash:
diskpulse clean --apply --trash

# 5. Non-interactive cleanup for CI / Automation (requires --apply AND --yes):
diskpulse clean system-temp --apply --yes --older-than 7d
```

A couple more worth knowing:

```bash
# Skip heavyweight dev directories from the visualization selectively
diskpulse viz . --exclude "*.log" --exclude "coverage"

# Visualize without descending into mounted volumes
diskpulse viz / --one-file-system --depth 1
```

## Command reference

Global flags (all subcommands): `-v/--verbose` (repeat for more detail),
`-q/--quiet`, `--no-color` (also honors `NO_COLOR`), `--json`.

### `viz` *(aliases: v, scan, analyze)*

| Flag | Purpose |
| --- | --- |
| `[PATH]` | Directory to inspect (defaults to the current directory) |
| `-d, --depth <INT>` | Maximum traversal depth to visualize (default: 2) |
| `-n, --top <INT>` | Number of largest entries to show per directory (default: 10) |
| `-m, --min-size <SIZE>` | Hide entries smaller than this threshold (e.g. `"100M"`, `"1G"`) |
| `-s, --sort <KIND>` | Sort entries by `size`, `count`, or `name` (default: `size`) |
| `--exclude <GLOB>` | Exclude paths matching this glob pattern (repeatable) |
| `--apparent-size` | Report logical file sizes instead of allocated disk blocks |
| `--no-ignore` | Do not ignore heavyweight dev directories |
| `--hidden` | Include hidden and dot-files |
| `-x, --one-file-system` | Stay on a single filesystem (do not cross mount points) |

### `clean` *(aliases: c, prune, sweep)*

| Flag | Purpose |
| --- | --- |
| `[TARGETS]...` | Target IDs to clean; empty selects every default target, `all` expands to every registered target including opt-ins |
| `--apply` | Perform the deletion; without this flag only a dry-run plan is printed |
| `-y, --yes` | Skip the confirmation prompt (requires `--apply`) |
| `--trash` | Move items to the OS trash instead of unlinking them permanently |
| `--older-than <DURATION>` | Only consider items older than this age (e.g. `"30d"`, `"12h"`) |
| `--min-size <SIZE>` | Only consider items at least this large (e.g. `"10M"`) |
| `-p, --path <PATH>` | Clean a specific path using cleaner heuristics instead of registered targets |
| `-x, --one-file-system` | Stay on a single filesystem (do not cross mount points) |

### `targets` *(alias: rules)*

| Flag | Purpose |
| --- | --- |
| `--all` | Include opt-in (non-default) targets as well |

### `completions`

| Argument | Purpose |
| --- | --- |
| `<shell>` | Shell to generate completions for: `bash`, `zsh`, `fish`, `powershell` |

## Supported cache targets

Run `diskpulse targets` to see every location resolved on your OS.

### Default (included in plain `diskpulse clean`)

| ID | Name | Description |
| --- | --- | --- |
| `system-temp` | System Temp | Stale files inside OS temporary directories (items must be at least 1 day old) |
| `user-cache` | OS User Cache | Operating-system level user cache directory |
| `cargo-cache` | Cargo Cache | Rust crate archives and git checkouts cached by Cargo |
| `npm-cache` | npm Cache | Package tarball cache maintained by npm (`_cacache`) |
| `yarn-cache` | Yarn Cache | Offline mirror and package cache maintained by Yarn |
| `pnpm-cache` | pnpm Store | Content-addressable package store maintained by pnpm |
| `pip-cache` | pip Cache | Downloaded wheel cache maintained by pip |
| `gradle-cache` | Gradle Cache | Build and dependency caches under `~/.gradle/caches` |
| `go-build` | Go Build Cache | Compiled package cache of the Go toolchain |

### Opt-in (require an explicit ID or `clean ... all`)

| ID | Name | Description |
| --- | --- | --- |
| `xcode` | Xcode Derived Data | Per-project Xcode build artifacts and indexes (macOS only) |
| `docker-build` | Docker Buildx Cache | Local buildx layer cache and references |
| `browser-cache` | Browser Caches | Chromium, Chrome, Firefox and Brave HTTP caches |

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success (including clean user aborts and dry runs) |
| `1` | Runtime error (I/O, failed deletion, missing path) |
| `2` | CLI usage/validation error (bad flags, unknown target) |
| `130` | Interrupted by SIGINT (Ctrl+C) |

## How it compares

| | diskpulse | ncdu | dust | dua | du |
| --- | --- | --- | --- | --- | --- |
| Written in Rust | ✅ | ❌ (C/C++) | ✅ | ✅ | ❌ (C) |
| Parallel scanning | ✅ | ➖ | ✅ | ✅ | ❌ |
| Tree/TUI visualization | ✅ | ✅ | ✅ | ✅ | ❌ |
| Built-in safe cache cleaning with curated targets | ✅ | ❌ | ❌ | ❌ | ❌ |
| Dry-run-by-default deletion | ✅ | ➖ (interactive deletes) | ❌ | ➖ (interactive deletes) | ❌ |
| Trash/recycle-bin support | ✅ | ❌ | ❌ | ➖ | ❌ |
| JSON output | ✅ | ➖ (export file) | ❌ | ❌ | ❌ |
| Shell completions shipped via CLI | ✅ | ❌ | ❌ | ❌ | ❌ |
| Cross-platform (Linux/macOS/Windows) | ✅ | ✅ | ✅ | ✅ | ✅ |

ncdu, dust and dua are excellent visualizers; none of them ship a curated,
jail-guarded cache-cleaning mode — that is diskpulse's reason to exist.
(➖ marks partial or conditional support.)

## Development

### Regenerating the demo GIF

The demo GIF is built with [VHS](https://github.com/charmbracelet/vhs)
(install it first), from a fully synthetic fixture — nothing real is ever
scanned or deleted:

```bash
bash demo/build_fixture.sh && vhs demo/diskpulse-demo.tape
```

```bash
cargo test          # unit + integration suites
cargo clippy -- -D warnings
cargo fmt --check
```

Integration test suites live in `tests/`:

- `scanner_tests.rs` — traversal, aggregation, filtering/sorting/top-N,
  one-file-system behavior, glob excludes
- `cleaner_tests.rs` — planning semantics, age/size windows, dry-run
- `safety_tests.rs` — the safety jail: protected roots, personal dirs,
  traversal-escape resistance
- `symlink_tests.rs` — symlink quarantine, loops, TOCTOU retargeting attacks
- `e2e_cli_tests.rs` — end-to-end runs of the built binary (flags, exit codes,
  poisoned-environment resistance)
- `dry_run_tests.rs` — dry-run guarantees: nothing deleted, stdout contracts,
  JSON shapes

Contributions welcome — open an issue or pull request. Please keep the test
suites green (`cargo test && cargo clippy -- -D warnings && cargo fmt --check`).

## License

Dual-licensed under either of:

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.
