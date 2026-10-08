# Sheepdog

An independent Herdr-inspired terminal and coding-agent manager using Quosh.
Ubuntu server, macOS TUI first, then a PWA. GPL-3.0-only.

## Status

The product specification is accepted. The lifecycle model and real Linux
terminal-owner library are implemented; there is no network server or TUI yet.

`sheepdog-runtime` owns one PTY and emulator per terminal, with independent
attachment epochs, input sequences, replay deduplication, causal echo barriers,
and last-interaction geometry. Closing terminates the process group; detaching
preserves it. There are 27 passing tests, including 11 real-PTY integration tests.

Quosh emulator and Linux PTY libraries are pinned Git dependencies, not copies
or dependencies on a sibling checkout. Network authentication and prediction
invalidation wiring remain the next milestone.

- [Domain language](CONTEXT.md)
- [Product specification](docs/01-product-spec.md)
- [Implementation plan and integration findings](docs/02-implementation-plan.md)
- [Recorded interview decisions](docs/design-notes.md)
- [Architectural decisions](docs/adr/)

## Checks

```sh
CARGO_INCREMENTAL=0 cargo test --workspace
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

`reference/herdr/` is an ignored reference checkout, not a build dependency.
