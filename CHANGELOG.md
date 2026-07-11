# Changelog

## [Unreleased]

## [0.2.0] - 2026-07-11

### Added

- Added clipd-to-picker protocol v2 with optional, closed provider and isolation
  posture metadata for destinations and candidates. The picker renders
  `local-vm`, `qemu-media`, `provider-managed`, and `unsafe-local` identity and
  shows `unsafe-local · no isolation` without using these fields for policy.
- Added a protocol-v1 compatibility window: the picker advertises versions 1
  through 2, and omitted v2 presentation fields default to legacy `unknown`.
- Added explicit endpoint realm provenance, provider/posture labels, and
  unsafe-local warnings to the destination and source-row UI.
- Added source-policy gates covering d2bd/toolkit coupling, command execution,
  compositor-specific IPC, data-control protocols, clipboard payload file
  descriptors, persistence, policy decisions, and app-id identity inference.
- Added a configurable GTK palette, destination-realm framing, source-realm
  color rails, deterministic fallback realm colors, and safe per-realm color
  hints supplied by `d2b-clipd`.
- Added repository controls, a pinned Rust toolchain, CI workflows, a binary
  package, an app output, a development shell, and a deterministic source
  tarball.

### Changed

- Bumped the crate and flake-derived package version to 0.2.0.
- The picker now uses a Layer Shell top-layer overlay with output placement
  hints from `d2b-clipd`, activates rows on single click, and retains
  `Select`/`Cancel` as its only terminal actions.
- Clipboard rows carry source-realm identity directly rather than adding
  non-selectable realm header rows, so keyboard navigation remains on
  selectable entries.
- Host rows show focused-window attribution as a detail without a noisy
  best-effort suffix.
- Documentation now defines the independent protocol, strict trust boundary,
  security reporting process, and Cursor Clip fork acknowledgement.
- The deterministic source tarball now includes the repository documentation,
  security policy, changelog, and license in addition to Cargo build inputs.

### Fixed

- Pointer-capture polling processes readable Wayland events before hangup so
  final pointer/output events are not dropped during disconnect.
- Pointer-capture shared memory uses safe `rustix` memfd APIs, and protocol
  frames use buffered bounded line reads.
- The inherited picker IPC descriptor is rejected when it overlaps standard
  streams and is marked close-on-exec immediately after adoption.
- Initial clipd/picker protocol reads and writes now fail closed on a bounded
  handshake timeout instead of waiting forever on a partial frame; both socket
  timeouts are removed before the interactive picker lifetime begins.

### Security

- Protocol boundaries now reject incompatible selected versions, oversized
  metadata, unknown fields, and unknown closed-enum values visibly.
- Protocol `Debug` output redacts dynamic request, endpoint, application,
  preview, thumbnail, and selection identifiers; parse errors do not echo
  untrusted frame values.
- Provider, posture, realm, color, and application metadata remain
  presentation-only and cannot grant selection, alter operations, infer
  identity, or bypass `Select`/`Cancel`.
