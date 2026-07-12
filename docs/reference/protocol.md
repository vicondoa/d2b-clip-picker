# clipd-to-picker protocol

`d2b-clip-picker` uses an independent newline-delimited JSON protocol over the
anonymous Unix socket inherited with `--ipc-fd`. The protocol intentionally
does not import d2b internal crates or d2b-toolkit. Its small presentation enums
are duplicated at this boundary so the picker cannot acquire control-plane
authority through a client dependency.

## Version negotiation

The picker starts every connection with:

```json
{"type":"client_hello","protocol_version_range":{"min":1,"max":2},"picker_version":"0.2.0"}
```

`d2b-clipd` selects one version in the advertised inclusive range and includes
it as `selected_protocol_version` in `open_request`. `Select` and `Cancel` echo
that exact version. A selected version outside `1..=2` is rejected with a clear
incompatibility error before the UI opens.

| Protocol | Package support | Change |
| --- | --- | --- |
| v1 | Compatibility window in the 0.2.x line | Original request, candidate, `Select`, and `Cancel` frames. Provider/posture fields are omitted. |
| v2 | Current | Adds optional presentation-only provider and isolation-posture fields. |

The v2 decoder accepts v1 omission: absent provider/posture fields become the
explicit bounded `unknown` state. Version 1 remains supported for this release
window and may be removed only in a later compatibility-breaking release.

Existing d2b v1 frames may also carry optional canonical-target and clipboard
capability-preflight metadata. The picker validates and accepts those fields for
wire compatibility, redacts their dynamic values from `Debug`, and never uses
them to authorize or fulfill a transfer.

## Request presentation metadata

Protocol v2 adds these optional fields:

| Object | Provider field | Isolation field |
| --- | --- | --- |
| `destination` | `provider_kind` | `isolation_posture` |
| candidate | `source_provider_kind` | `source_isolation_posture` |

Provider values are closed:

- `local-vm`
- `qemu-media`
- `provider-managed`
- `unsafe-local`
- `unknown`

Isolation values are closed:

- `virtual-machine`
- `provider-managed`
- `unsafe-local`
- `unknown`

`realm_kind` remains snake-case and accepts `host`, `vm`, `unsafe_local`, or
`unknown`.

These values exist only to render provenance. The picker shows the supplied
realm plus provider/isolation identity. If either closed value is
`unsafe-local`, it also shows:

```text
unsafe-local · no isolation
```

The picker never derives provider, posture, or realm from `application`,
`app_id`, title, workspace, color, or preview text.

## Compatibility and unknown values

The protocol follows the picker's existing strict decoder pattern:

- omitted optional v2 fields decode as `unknown`;
- the explicit `unknown` value renders no invented identity;
- unknown field names fail deserialization;
- unknown future enum strings fail deserialization;
- additive enum values therefore require a negotiated protocol-version update.

This is a visible protocol failure, not a policy decision. It prevents a future
security posture from being silently relabeled while leaving all transfer
authority in `d2b-clipd`.

## Bounds and redaction

The picker enforces these boundary caps before opening the UI:

| Item | Limit |
| --- | ---: |
| Picker-to-clipd frame | 4 KiB |
| Clipd `open_request` frame | 23,553,408 bytes |
| Candidates | 64 |
| Metadata string | 1,024 bytes |
| Preview | 2,048 bytes |
| Base64 thumbnail field | 349,528 bytes |
| Realm display entries | 64 |
| Capability tokens per list | 64 |
| Request or entry id | 256 bytes |
| MIME type | 256 bytes |

Text fields also reject NUL. Placement coordinates and dimensions are finite,
positive where applicable, and capped at 1,000,000. Protocol `Debug` output
redacts request ids, entry ids, realm/application labels, previews, thumbnails,
and other dynamic text. Boundary parse errors report a fixed malformed or
unsupported-frame message rather than echoing attacker-supplied values.

## Authority invariant

The only picker-to-clipd actions are:

```text
ClientHello
Select
Cancel
```

`Select` carries only the negotiated version, request id, and entry id. It does
not receive or publish clipboard bytes. `d2b-clipd` alone revalidates the
selection, applies policy, owns transfer descriptors, and fulfills the paste.
