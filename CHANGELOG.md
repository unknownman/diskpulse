# Changelog

All notable changes to this project will be documented in this file.
Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.1.1]

### Fixed

- **Windows safety jail bypass**: `fs::canonicalize` returns verbatim paths
  (`\\?\C:\...`) which never compared equal to plain protected paths,
  silently disabling every protection rule for existing paths on Windows.
  Verbatim/UNC prefixes are now stripped before comparison.
- CI: cross-platform test assumptions (XDG-unregistered personal dirs on
  bare Linux, drive-prefix absolutization, readdir tie ordering).

### Changed

- crates.io metadata: real repository/homepage URLs.

## [0.1.0]

### Added

- `viz` command: parallel hierarchical disk-usage tree with size/count/name
  sorts, top-N pruning summaries, `--min-size`, `--exclude`, JSON output.
- `clean` command: dry-run-by-default cache cleaning across 9 default and
  3 opt-in targets, guarded by a hard-coded safety jail, age/size windows,
  trash support, and a TOCTOU re-check before every deletion.
- Shell completions (bash, zsh, fish, PowerShell).
- Documented exit-code contract: 0/1/2/130.

[unreleased]: https://github.com/unknownman/diskpulse/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/unknownman/diskpulse/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/unknownman/diskpulse/releases/tag/v0.1.0
