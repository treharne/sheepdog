# Implementation plan

## Ownership boundary

Sheepdog Server owns Spaces, Tabs, terminal process lifetime, authoritative terminal state, and agent detection. Quosh supplies screen synchronization, client prediction, authenticated attachment, and reconnect behavior. Never run Herdr's binary bridge inside a Quosh terminal or nest the Sheepdog navigation TUI inside a remote shell.

The TUI draws navigation locally and uses `quosh-client` for the selected terminal. The subsequent PWA consumes the same terminal and Control Plane contracts. Reference source is not a runtime dependency.

## Current foundation

`sheepdog-core` is an I/O-free Rust model for Spaces, Tabs, client selection, terminal launch/exit generations, geometry promotion, close actions, and restart restoration. Runtime adapters execute returned launch and stop requests. The model does not pretend a requested shell has already started, and ignores stale runtime events after replacement.

Organization snapshots exclude clients and process handles. Restoring creates ended Tabs, not runnable processes. The daemon now persists organization using private temporary files, fsync, atomic rename, and directory fsync.

`sheepdog-runtime` now implements real Linux PTY ownership using pinned reusable `quosh-terminal`, `quosh-pty`, and `quosh-proto` libraries. A bounded actor serializes input, attachment changes, and geometry. Per-attachment sequence acceptance is separate from echo acknowledgment; partial writes retain their original attachment and sequence until completion. Settled checkpoints are published with immutable screen snapshots. Disconnect preserves the terminal; close bypasses input backpressure, terminates groups in the owning PTY session, and escalates to SIGKILL if needed. An interactive job-control regression exposed and fixed the narrower process-group-only shutdown.

The server must retain a `Terminal` handle independently of client views. Runtime commands are trusted in-process calls: this crate does not authenticate callers or expose a network listener. Disconnected attachments can be explicitly retired after all accepted input settles; retained attachment state is capped at 128 entries per terminal, and queued bytes plus barrier records at 1 MiB (with a separate bounded 32-command actor channel).

## Quosh integration findings

Current Quosh `Session` combines a terminal process with a single attachment identity, input sequence, latest-screen stream, and transport epoch. `claim_transport()` replaces the existing transport; `Owner` orders input using one `last_seq` and publishes one `echo_ack`. Using this object unchanged would contradict Sheepdog's shared-client behavior.

The intended seam is a shared terminal owner plus independent attachment state:

- One PTY reader and terminal emulator per terminal, regardless of viewers.
- Each attachment has its own reconnect epoch, input sequence, replay deduplication, input-acceptance acknowledgment, and echo acknowledgment.
- One owner serializes all accepted input into PTY writes. Completion barriers retain both attachment identity and its sequence; partial writes must not acknowledge an entire input early.
- Screen snapshots carry the receiving attachment's echo acknowledgment, not another client's sequence. Every acknowledgment is bound to the screen it describes.
- Last interaction promotes geometry without replacing another client's attachment.
- Client disconnect destroys neither the terminal owner nor the Tab. Confirmed close terminates the terminal process group.

Foreign-client input can invalidate local prediction even with correct per-client acknowledgments. The runtime publishes a per-attachment prediction epoch and eligibility: foreign input and geometry changes invalidate predictions, and eligibility stays false while multiple writers are attached or foreign accepted input remains unsettled. The native network adapter now carries this metadata atomically with each screen using Quosh's additive shared-screen message. The reusable client resets overlays on context changes, retains unacknowledged bytes, ignores older datagrams, and rejects reliable generation regression or missing metadata. Ordinary QS2 stays unchanged. Multiple connected writers currently suppress prediction; a sole writer retains adaptive prediction.

The existing Quosh browser auth path also creates shell sessions. It needs a reusable authenticated-session boundary so Sheepdog can select its own terminals without creating unwanted shells.

## Milestones

1. **Lifecycle model**: implemented with ten unit tests, including stale owner geometry updates.
2. **Terminal owner and attachment model**: implemented. Seven ledger tests and eleven real-PTY integration tests cover shared sequences, partial-write barriers, replay, stale epochs, output under input backpressure, geometry promotion, process-group shutdown, and final output. The independent model and runtime remain reusable beneath the network daemon.
3. **Native Quosh integration and Control Plane**: implemented with private SSH-bootstrap helper, pinned WebTransport, per-client capabilities/tickets, durable organization, terminal attach/select/close, and prediction-context wiring. Browser/passkey authentication remains deferred to the PWA milestone. See [native server and wire](03-native-server-and-wire.md).
4. **Remote macOS TUI**: native connection groundwork implemented in `sheepdog-client`: bounded SSH-assisted bootstrap, pinned Control Plane RPCs, retained per-view sequencing, pause/resume/reconnect, watched frames, and out-of-band Stop. Local-rendered prototype now implemented in `sheepdog-tui`: Spaces/Tabs, pickers/search, creation/confirmed closure, explicit reopen, retained sequencing, safe cropped rendering, prefix/mouse navigation, and nonblocking output/quit. See [native client adapter](04-native-client-adapter.md) and [TUI evidence and gaps](05-native-tui-prototype.md). Startup/layout correction now adds an atomic starter shell, automatic new Space names/shells, left sidebar, one-row top tab strip, shared mouse hit rectangles, mouse creation and optional durable rename. macOS acceptance, history/copy, terminal mouse, configurable bindings, live-CWD tracking, full agent metadata, and full paste handling remain.
5. **Agent recognition and Attention**: pi, grok, agy, behavior fixtures derived from pinned Herdr reference and actual agent output.
6. **File Ingest**: clipboard/path recognition, non-shell wildcard expansion, symlink-safe discovery, confirmation, authenticated upload and literal-paste bypass.
7. **PWA**: same model and protocol, passkeys, mobile navigation/input and browser upload facilities.

Each milestone must expose its test evidence and outstanding gaps. Product-changing trade-offs go back to the operator.
