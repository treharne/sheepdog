# Sheepdog

An independent Herdr-inspired terminal and coding-agent manager using Quosh.
Ubuntu server, macOS TUI first, then a PWA. GPL-3.0-only.

## Status

The lifecycle model, real Linux terminal owner, and authenticated native
network server are implemented. The TUI and PWA are not implemented yet.

`sheepdog-runtime` owns one PTY and emulator per terminal, with independent
attachment epochs, input sequences, replay deduplication, causal echo barriers,
and last-interaction geometry. Closing terminates the process group; detaching
preserves it. The server adds pinned WebTransport, per-client capabilities,
resumable view tickets, and durable organization.

Quosh emulator and Linux PTY libraries are pinned Git dependencies, not copies
or dependencies on a sibling checkout. Shared-screen metadata now drives
prediction invalidation in the reusable Quosh client.

- [Domain language](CONTEXT.md)
- [Product specification](docs/01-product-spec.md)
- [Implementation plan and integration findings](docs/02-implementation-plan.md)
- [Native server, bootstrap helper, and wire](docs/03-native-server-and-wire.md)
- [Recorded interview decisions](docs/design-notes.md)
- [Architectural decisions](docs/adr/)

## Checks

```sh
CARGO_INCREMENTAL=0 cargo test --workspace
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

`reference/herdr/` is an ignored reference checkout, not a build dependency.
