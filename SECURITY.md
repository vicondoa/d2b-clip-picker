# Security

## Reporting

Please report suspected vulnerabilities through GitHub's private security
advisory flow for `vicondoa/d2b-clip-picker`. Do not include real clipboard
contents, credentials, personal data, or unredacted screenshots in a report.

## Security boundary

`d2b-clip-picker` is an untrusted presentation client for one clipboard paste
request. It receives bounded metadata over an inherited anonymous Unix socket
and can return only `Select` or `Cancel`. `d2b-clipd` remains responsible for
policy, clipboard ownership, transfer file descriptors, and fulfillment after a
selection.

The picker deliberately has no:

- d2bd or toolkit client;
- command-execution path;
- compositor-specific IPC or data-control protocol;
- clipboard payload file-descriptor transport;
- persistence, history, or credential store;
- policy engine or app-id identity inference.

Protocol provider, posture, realm, application, color, and preview fields are
presentation-only. They cannot authorize a transfer or change the candidates
that `d2b-clipd` supplied.

## Unsafe-local presentation

An `unsafe-local` endpoint has no isolation boundary. The picker preserves that
provider name and displays `unsafe-local · no isolation`; it does not reinterpret
the label as a sandbox or use it to decide whether selection is allowed.

## Protocol failures

The picker accepts negotiated protocol versions 1 and 2 in release 0.2.0.
Unsupported selected versions, oversized metadata, unknown field names, and
unknown closed-enum values terminate the request visibly. Missing v2
presentation fields default to the bounded legacy `unknown` state. Parse errors
do not echo untrusted frame values.

See [docs/protocol.md](docs/protocol.md) for the complete wire contract.
