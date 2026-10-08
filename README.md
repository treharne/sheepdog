# Sheepdog

An independent Herdr-inspired terminal and coding-agent manager using Quosh.
Ubuntu server, macOS TUI first, then a PWA. GPL-3.0-only.

## Status

The product specification is accepted. The first implementation milestone is
an I/O-free server lifecycle model; there is no runnable server or TUI yet.

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
