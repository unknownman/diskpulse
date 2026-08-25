# Changelog

All notable changes to this project will be documented in this file.
Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.1.0]

### Added

- `viz` command: parallel hierarchical disk-usage tree with size/count/name
  sorts, top-N pruning summaries, `--min-size`, `--exclude`, JSON output.
- `clean` command: dry-run-by-default cache cleaning across 9 default and
  3 opt-in targets, guarded by a hard-coded safety jail, age/size windows,
  trash support, and a TOCTOU re-check before every deletion.
- Shell completions (bash, zsh, fish, PowerShell).
- Documented exit-code contract: 0/1/2/130.

[unreleased]: https://github.com/OWNER/diskpulse/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/OWNER/diskpulse/releases/tag/v0.1.0
