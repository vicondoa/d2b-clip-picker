# d2b-clip-picker

`d2b-clip-picker` is the GPL-3.0-only GTK4/Libadwaita/Layer Shell UI picker for d2b clipboard flows. It is not a standalone clipboard manager and must be launched by `d2b-clipd` with an inherited anonymous Unix socket passed as `--ipc-fd=<fd>`.

This repository was forked from [`Sirulex/cursor-clip`](https://github.com/Sirulex/cursor-clip) at upstream commit `7e12054e55b7b2c34eff8638b88488403686e8dd`. The fork keeps the useful overlay interaction model while removing clipboard-manager authority.

## Trust boundary

The picker owns only presentation:

- destination/source labels supplied by `d2b-clipd`;
- search/filtering;
- rows, safe host thumbnails, and generic image metadata for VM entries;
- keyboard and mouse navigation;
- Escape/window-close cancellation;
- Select/Cancel messages on the inherited picker protocol socket.

It does not monitor clipboard state, persist history, evaluate transfer policy, synthesize input, connect to compositor IPC for labels, receive transfer descriptors, or write selected content to a clipboard. Text and HTML previews are rendered as inert plain text after control-character sanitization. Guest-origin image entries are metadata-only.

## Protocol

The picker speaks newline-delimited JSON over the inherited socketpair.

1. Picker sends `client_hello` containing only `protocol_version_range` and `picker_version`.
2. `d2b-clipd` sends one `open_request` with the request id, destination metadata, placement hints, requested MIME type, and candidate metadata.
3. The picker sends either `select { request_id, entry_id }` or `cancel { request_id }`.
4. Socket EOF, parse failure, or close/error frames terminate the UI.

The picker protocol DTOs are implemented independently in this repository and do not depend on d2b internal Rust crates.

## Build

```bash
cargo build --release
```

With Nix:

```bash
nix build .#d2b-clip-picker
```

The default package is also `d2b-clip-picker`.

## Development

Unit and integration tests use fake `d2b-clipd` socketpairs and policy checks:

```bash
cargo test
```

Run the binary only under a supervising fake or real `d2b-clipd` process that provides `--ipc-fd`.
