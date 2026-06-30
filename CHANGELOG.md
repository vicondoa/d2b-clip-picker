# Changelog

## [Unreleased]

### Added

- The picker now uses Layer Shell top-layer overlay presentation with output
  placement hints from `d2b-clipd`, activates rows on single click, and keeps
  Select/Cancel as its only runtime protocol actions.
- Added repository controls matching the d2b desktop tooling model: AGENTS.md,
  a pinned Rust toolchain, pull-request and main-branch CI workflows, and flake
  outputs for the binary package and deterministic source tarball.

### Changed

- Host clipboard rows now show the host realm label without the noisy
  `(best effort)` suffix while rendering focused-window attribution as a
  `Focused-window guess` detail.
- Normal placement and test-select diagnostics now log at info level instead of
  warning level.
- The README now describes the d2b-specific trust boundary, install path, flake
  outputs, protocol shape, and Cursor Clip acknowledgement.

### Fixed

- Pointer-capture polling now processes readable Wayland events before hangup
  handling so final pointer/output events are not dropped during disconnect.
