# Native client adapter

`sheepdog-client` is a platform-independent native connection library for the forthcoming macOS TUI. It depends on the pure model/protocol, Quosh client/protocol, and WebTransport, not on Linux PTY/runtime/server crates. Tested on Linux; no macOS acceptance test or TUI exists yet.

## Bootstrap

Call `bootstrap_ssh` before entering raw terminal mode, so OpenSSH can perform ordinary host-key, key-passphrase, and login interaction. The configured SSH command is split into argv without executing a local shell. Targets cannot begin with an option or contain control characters. A custom remote state directory is quoted as one remote-shell argument.

`ssh -G` supplies the resolved HostName instead of treating a local SSH alias as a DNS name. An optional explicit QUIC host handles cases where SSH uses a different reachable name or jump host; QUIC itself still requires direct UDP connectivity. Subprocess replies are limited to 64 KiB while reading, and stalled helpers are deadlined and killed on drop.

The remote helper must be installed as `sheepdog-server` on the target's command path. This library does not install or start a remote daemon implicitly. Bootstrap secrets are never logged by the adapter.

## Control RPCs

`Client::connect` validates the Control Plane version and nonempty certificate window, pins TLS, and authenticates the client capability. Authenticated reconnects refresh the in-memory forward certificate hashes.

Requests are serialized. The adapter takes ownership of the stream before cancellable I/O: if a request is canceled, times out, or encounters a wire error, that stream is discarded. The next request authenticates a fresh stream rather than resuming a partially read reply. Non-idempotent requests are never retried automatically after a lost reply.

## Terminal views

`Client::attach` gets a ticket and returns a bounded `View` handle. The client caches the Quosh state machine for each Tab/ticket; repeated selection must not instantiate sequence zero for an already-used ticket. The cache is bounded to 256 Tabs and reconciled against organization snapshots. An explicitly reopened terminal receives a different ticket and a fresh sequence domain.

- `input`: nonblocking, individually bounded to 64 KiB. Queued commands are capped at 32; Quosh retains at most 256 KiB unacknowledged input. Budget exhaustion fences the view with an explicit error/status rather than silently dropping more keystrokes.
- `resize`: updates the view's independently remembered geometry and promotes it when active.
- `pause`: closes only the physical terminal connection, retaining sequencing and replay state. This is the operation for navigation away from a Tab.
- `resume`: binds a replacement transport using the retained state.
- `stop`: irreversible, out-of-band termination of this local view task. It does not close the server's Tab. A stopped/rejected core cannot be recreated from the same ticket; use a new SSH-authenticated client rather than silently reinterpreting uncertain input.

A view returns immutable watched snapshots with frame, connection status, unacknowledged byte count, and terminal mode. Unchanged frames do not trigger repaint notifications. Prediction overlays are reset during transport loss/pause; queued input remains available for replay. The configured prediction preference is fixed per view, and a later conflicting Never request is explicitly rejected rather than ignored.

A bounded dedicated reader keeps partial-frame progress across sender/timer events. The writer uses cancellation-safe partial writes and advances Quosh's outbound cursor only after a successful write. Security/protocol/framing rejection is fatal. Temporary transport failure retries; consecutive pre-HelloOk handshake failures stop after five attempts. Established roaming connections can continue retrying while retaining unacknowledged input.

`Client::disconnect` stops cached views and explicitly revokes the capability through the Control Plane, preserving server-owned work. Cached views do not retain an Arc back to their parent client, so dropping the client cannot leak an ownership cycle.

## Test evidence

Actual pinned WebTransport and PTYs cover pause/resume without duplicate or lost typed commands, per-view geometry, Never preference enforcement, queue overflow and sticky Stop, client quit without killing work, final output/exit code, explicit reopen with a fresh sequence domain, and parent lifetime.

Deliberately faulty authenticated peers verify fatal malformed framing and recovery after cancellation during a partial RPC reply. Separate subprocess tests verify SSH quoting/target validation, bounded stdout, and helper deadlines.

Outstanding: user-facing TUI/CLI, Herdr-style navigation/pickers/keybindings and rendering, macOS verification, Control Plane event subscriptions, terminal history/copy mode, clipboard/File Ingest, and agent recognition/Attention. The SSH invocation has not yet been exercised against an external SSH host by this adapter.
