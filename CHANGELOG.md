# Changelog

## [Unreleased]

### Added

- Clipboard sources and destinations are now grouped by realm in the picker
  list. When candidates span more than one realm, a non-selectable realm group
  header row appears before each group. Group headers use per-realm color hints
  supplied by `d2b-clipd` in the new optional `realm_display` field of
  `OpenRequest`; the field defaults to an empty map for backward compatibility
  with older `d2b-clipd` versions.
- Added `realm_display: HashMap<String, RealmDisplayMetadata>` to `OpenRequest`
  (optional, `#[serde(default)]`). `RealmDisplayMetadata` carries an optional
  `color` hint (`#rrggbb` or `alpha(...)`) for the group header background.
  Colors are validated against the same safe-CSS allowlist as the theme palette
  and are used only for presentation; they carry no authorization weight.
- Added `realm_header_background` to `ThemePalette` (default
  `alpha(#89b4fa, 0.10)`) as the fallback group header background when no
  per-realm color is provided by `d2b-clipd`.
- Realm headers now derive a deterministic palette color when `d2b-clipd` does
  not provide display metadata, keeping screenshot/test fixtures visually
  realm-specific.
- Realm headers now render pure realm names without a `VM` suffix and use more
  top spacing so adjacent realm groups are visually distinct.
- The picker shell border now follows the destination realm color when
  `realm_display` includes one, and realm headers use a compact pill shape.
- Clipboard source rows now carry their source realm as a rounded item border
  plus left color rail instead of rendering separate realm header rows.
- Keyboard navigation remains on selectable clipboard rows because realm
  identity is now part of each row rather than a separate header row.

  GTK palette for picker shell colors without giving the picker clipboard or
  compositor authority.
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
- Clipboard realm group headers now carry source-realm colored borders while
  clipboard rows keep neutral inner styling, matching the other d2b desktop
  companions.
- Normal placement and test-select diagnostics now log at info level instead of
  warning level.
- The README now describes the d2b-specific trust boundary, install path, flake
  outputs, protocol shape, and Cursor Clip acknowledgement.
- The source flake output now produces a tarball with a standard top-level
  `d2b-clip-picker-<version>/` directory.
- Main-branch CI no longer cancels in-progress runs, preserving release and
  artifact signals for every merge while PR validation remains in `pr.yml`.

### Fixed

- Pointer-capture polling now processes readable Wayland events before hangup
  handling so final pointer/output events are not dropped during disconnect.
- Pointer-capture shared memory now uses safe `rustix` memfd APIs, and picker
  protocol frame reads use buffered bounded line reads instead of byte-at-a-time
  polling.
- The inherited picker IPC fd is now rejected when it overlaps standard streams
  and is marked close-on-exec immediately after adoption, preventing leaks into
  GTK/GLib child processes.
