# Implementation plan

## Ownership boundary

Sheepdog Server owns Spaces, Tabs, terminal process lifetime, authoritative terminal state, and agent detection. Quosh supplies screen synchronization, client prediction, authenticated attachment, and reconnect behavior. Never run Herdr's binary bridge inside a Quosh terminal or nest the Sheepdog navigation TUI inside a remote shell.

The TUI draws navigation locally and uses `quosh-client` for the selected terminal. The subsequent PWA consumes the same terminal and Control Plane contracts. Reference source is not a runtime dependency.

## Current foundation

`sheepdog-core` is an I/O-free Rust model for Spaces, Tabs, client selection, terminal launch/exit generations, geometry promotion, close actions, and restart restoration. Runtime adapters execute returned launch and stop requests. The model does not pretend a requested shell has already started, and ignores stale runtime events after replacement.

Organization snapshots exclude clients and process handles. Restoring creates ended Tabs, not runnable processes. Persistence encoding and atomic on-disk storage are not implemented yet.

## Quosh integration findings

Current Quosh `Session` combines a terminal process with a single attachment identity, input sequence, latest-screen stream, and transport epoch. `claim_transport()` replaces the existing transport; `Owner` orders input using one `last_seq` and publishes one `echo_ack`. Using this object unchanged would contradict Sheepdog's shared-client behavior.

The intended seam is a shared terminal owner plus independent attachment state:

- One PTY reader and terminal emulator per terminal, regardless of viewers.
- Each attachment has its own reconnect epoch, input sequence, replay deduplication, input-acceptance acknowledgment, and echo acknowledgment.
- One owner serializes all accepted input into PTY writes. Completion barriers retain both attachment identity and its sequence; partial writes must not acknowledge an entire input early.
- Screen snapshots carry the receiving attachment's echo acknowledgment, not another client's sequence. Every acknowledgment is bound to the screen it describes.
- Last interaction promotes geometry without replacing another client's attachment.
- Client disconnect destroys neither the terminal owner nor the Tab. Confirmed close terminates the terminal process group.

Foreign-client input can invalidate local prediction even with correct per-client acknowledgments. Before enabling shared prediction, investigate and test invalidation across streamed and datagram screen updates, reconnect replay, and geometry changes. Do not assume a shared screen plus independent sequence counters alone solves it. Any needed extension belongs in reusable Quosh libraries, not a private Sheepdog fork.

The existing Quosh browser auth path also creates shell sessions. It needs a reusable authenticated-session boundary so Sheepdog can select its own terminals without creating unwanted shells.

## Milestones

1. **Lifecycle model**: implemented with nine unit tests. Not a running server or client.
2. **Terminal owner and attachment model**: real PTYs, bounded serialized input, per-attachment replay and causal echo barriers; shared-client tests before network wiring.
3. **Quosh library integration and local Control Plane**: reusable terminal/auth seams, authenticated namespace, durable organization, terminal attach/select/close.
4. **Remote macOS TUI**: SSH-assisted bootstrap, local navigation, Quosh terminal rendering and prediction, reconnection, scrollback/input behavior validated against Herdr.
5. **Agent recognition and Attention**: pi, grok, agy, behavior fixtures derived from pinned Herdr reference and actual agent output.
6. **File Ingest**: clipboard/path recognition, non-shell wildcard expansion, symlink-safe discovery, confirmation, authenticated upload and literal-paste bypass.
7. **PWA**: same model and protocol, passkeys, mobile navigation/input and browser upload facilities.

Each milestone must expose its test evidence and outstanding gaps. Product-changing trade-offs go back to the operator.
