# d2b-clip-picker

`d2b-clip-picker` is the GPL-3.0-only GTK4, Libadwaita, and Layer Shell picker
UI for [d2b](https://github.com/vicondoa/d2b) clipboard flows. It is launched by
`d2b-clipd` with an inherited anonymous Unix socket (`--ipc-fd=<fd>`) and acts
only as a presentation client for one paste request.

It is **not** a standalone clipboard manager. It does not own any clipboard,
read compositor clipboard state, evaluate policy, persist history, or receive
Wayland transfer file descriptors.

## Origin and acknowledgement

This repository is forked from
[`Sirulex/cursor-clip`](https://github.com/Sirulex/cursor-clip), originally by
Sirulex, at upstream commit `7e12054e55b7b2c34eff8638b88488403686e8dd`.

The fork keeps the useful compact overlay interaction model and adapts it for
d2b's split architecture: all trusted clipboard state stays in
[`d2b`](https://github.com/vicondoa/d2b), while this repository remains a UI-only
client. See the full
[fork acknowledgement](docs/explanation/fork-acknowledgement.md).

## Trust boundary

The picker owns only presentation:

- destination and source metadata supplied by `d2b-clipd`;
- search/filtering;
- safe text preview rendering;
- host thumbnails supplied by `d2b-clipd`;
- keyboard and mouse navigation;
- compositor-agnostic observation of normal-toplevel `Activated` state solely
  for transient focus-loss cancellation;
- Escape, close, and cancel behavior;
- `Select` / `Cancel` messages over the inherited socketpair.

The picker must never:

- use `ext-data-control-v1`, `zwlr-data-control-manager-v1`, primary selection,
  `wl-copy`, `wl-paste`, virtual-keyboard injection, or `ydotool`;
- receive or write clipboard transfer file descriptors;
- connect to `$NIRI_SOCKET` or other compositor IPC for labels or authority;
- persist clipboard history or payloads;
- evaluate d2b policy or make transfer decisions;
- execute commands or connect to `d2bd`;
- depend on d2b internal Rust crates.

The picker never infers realm, provider, or isolation identity from an app id.
Those labels are optional presentation metadata supplied by `d2b-clipd`.
Selection fulfillment remains exclusively in `d2b-clipd` after the picker sends
`Select`. The focus observer discards foreign-toplevel titles, app ids, and
outputs and exposes no compositor action or control request.

## Picker interaction

Each request opens an unpinned, fixed-size panel. After the panel has received
focus, moving focus to another application cancels the request and closes the
picker. The accessible **Pin** toggle in the top-right header keeps that request
open across focus changes; it does not grant any clipboard operation or change
the existing Select, Escape, close, or socket-shutdown behavior.

Type to search, use Up/Down or `j`/`k` to navigate, Enter to select, and Escape
to cancel. The picker bootstraps with exclusive keyboard interactivity so Niri
focuses it on map, then immediately switches to on-demand interactivity after
activation so focus can transfer normally.

Drag the title area between the header controls to move the panel. Placement is
clamped to the compositor-provided usable output area, including Layer Shell
exclusive zones such as Waybar. Dragged coordinates are never persisted: every
request starts from its pointer/output placement hints. The picker does not use
Niri IPC or assume a bar height.

## Protocol

The picker speaks an independent, versioned newline-delimited JSON protocol over
the inherited socketpair. Package 0.2.0 advertises protocol versions 1 through 2.

1. Picker sends `client_hello` with only `protocol_version_range` and
   `picker_version`.
2. `d2b-clipd` sends one `open_request` containing request id, destination
   metadata, placement hints, requested MIME type, expiry, and candidate
   metadata.
3. The picker returns either `select { request_id, entry_id }` or
   `cancel { request_id }`.
4. EOF, malformed input, or `close` / `error` frames terminate the UI.

Selecting an item never writes to the clipboard. It only tells `d2b-clipd` which
entry the user chose.

Protocol v2 adds optional closed provider and isolation-posture labels to source
and destination metadata. The UI renders `local-vm`, `qemu-media`,
`provider-managed`, and `unsafe-local` provider identity, and displays
`unsafe-local · no isolation` whenever either supplied label has unsafe-local
posture. Omitted v2 fields and all v1 frames use the legacy `unknown` default.
Unknown field names or future closed-enum values are rejected visibly rather
than interpreted as policy.

See [the protocol reference](docs/reference/protocol.md) for frame shapes,
compatibility,
bounds, and the complete presentation-only contract.

## Install

`d2b-clip-picker` is a Nix flake:

```bash
nix build github:vicondoa/d2b-clip-picker#d2b-clip-picker
```

In a d2b host configuration, add this flake as an input and point
`d2b.site.clipboard.picker.package` at:

```nix
inputs.d2b-clip-picker.packages.${pkgs.system}.default
```

See the d2b how-to:
[`docs/how-to/configure-clipboard-picker.md`](https://github.com/vicondoa/d2b/blob/main/docs/how-to/configure-clipboard-picker.md).

## Theme palette

The picker is theming-system agnostic. Operators can pass a generic JSON palette
with `--theme-json <path>`; d2b hosts that use Stylix can render Stylix colors
into this file from their NixOS config, while non-Stylix hosts can write the same
shape by hand or with another theme system.

```json
{
  "background": "#1e1e2e",
  "foreground": "#f8f8f2",
  "border": "#89b4fa",
  "accent": "#3584e4",
  "selected_background": "alpha(#3584e4, 0.14)",
  "realm_background": "alpha(#3584e4, 0.14)",
  "realm_header_background": "alpha(#89b4fa, 0.10)",
  "search_background": "alpha(currentColor, 0.07)",
  "warning_background": "alpha(#f5c211, 0.22)"
}
```

Fields accept normalized lowercase `#rrggbb` colors or GTK
`alpha(#rrggbb, opacity)` / `alpha(currentColor, opacity)` expressions. The
palette controls only picker shell presentation; it never grants clipboard
authority or access to payload bytes.

`realm_header_background` remains the fallback realm presentation color when
`d2b-clipd` does not supply a per-realm color in `realm_display`.

## Realm grouping

Each clipboard row keeps its source realm visible with a label and colored rail.
Colors come from the optional `realm_display` map in the `OpenRequest` frame
supplied by `d2b-clipd`. These colors are purely presentational and do not
influence which transfers are permitted. A deterministic safe palette is used
when `realm_display` is absent.

## Flake outputs

- `packages.${system}.default` / `packages.${system}.d2b-clip-picker` — source-built binary package.
- `packages.${system}.binary` — alias for the binary package.
- `packages.${system}.source` — deterministic source tarball.
- `apps.${system}.default` — `d2b-clip-picker` app.
- `devShells.${system}.default` — Rust + GTK/Layer Shell development shell.

## Development

Use the Nix dev shell so GTK and Layer Shell dependencies match CI:

```bash
nix develop --command cargo fmt --all -- --check
nix develop --command cargo clippy --all-targets -- -D warnings
nix develop --command cargo test
nix flake check --no-build --all-systems
nix build .#d2b-clip-picker
nix build .#source
```

The tests use fake `d2b-clipd` socketpairs and policy scans to verify that the
picker remains UI-only.

Security assumptions and reporting guidance are in [SECURITY.md](SECURITY.md).

Run the binary only under a supervising fake or real `d2b-clipd` process that
provides `--ipc-fd`.

### Deterministic UI render

From an active Wayland compositor with GTK Layer Shell support (including
Niri), render synthetic review data through the production widget builder:

```bash
nix develop --command cargo run -- --render-sample ./clip-picker-sample.png
```

The command uses no live IPC or clipboard data, requires an explicit `.png`
path, and exits within ten seconds. It checks the 420×520 PNG signature,
dimensions, non-uniform content, and 5 MB size cap before succeeding. The sample
includes multiple realms, provider/isolation postures, text and image
placeholders, and the unpinned pin icon. Generated PNGs are review artifacts
and must remain untracked.

For live checks, recover the compositor environment and opt in explicitly:

```bash
export XDG_RUNTIME_DIR=/run/user/$(id -u)
export WAYLAND_DISPLAY=wayland-1
D2B_RUN_WAYLAND_UI_TESTS=1 nix develop --command \
  cargo test --test binary wayland_ -- --nocapture
NIRI_SOCKET=$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -type s \
  -name 'niri.wayland-1.*.sock' -print -quit) \
D2B_RUN_NIRI_FOCUS_TESTS=1 nix develop --command \
  cargo test --test binary niri_focus_bootstrap_transitions_then_cancels_once -- --nocapture
```

The Niri test requires an unlocked active Niri session plus `foot`; Niri IPC is
used only by this test harness, never by the picker.

## CI and releases

Pull requests run formatting, clippy, tests, flake evaluation, and source/binary
flake builds. Main-branch CI repeats those gates and uploads the deterministic
source tarball artifact.

The version in `Cargo.toml` is the flake package version. Changes are recorded
under `CHANGELOG.md` using Keep a Changelog; release notes should be
user-facing and omit internal process markers.

## License

[GPL-3.0-only](./LICENSE), inherited from Cursor Clip.
