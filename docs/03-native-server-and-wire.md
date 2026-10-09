# Native server and wire protocol

Status: implemented and tested on Linux. The macOS TUI and browser/passkey path are not implemented yet.

## Running

```sh
cargo build -p sheepdog-server
./target/debug/sheepdog-server serve --bind 0.0.0.0:4443
# On the same remote host, normally invoked through an SSH channel:
./target/debug/sheepdog-server bootstrap
```

`--state-dir PATH` is a global option. Default: `$HOME/.local/state/sheepdog`. The server runs as the Unix user whose work it owns, not as a root daemon impersonating multiple users. Separate users require separate private state directories and nonconflicting bind addresses/ports. This is not a system-wide user registry.

The state directory is mode 0700, the bootstrap socket 0600, and the socket checks Linux peer credentials against the server's effective uid. An advisory lock prevents competing daemons from using the same identity and organization files. Bootstrap output contains a secret token: transfer it through SSH, never a public URL or logs.

No daemon has been installed or exposed as part of these implementation tests. Tests use loopback addresses and temporary directories.

## Native authentication

Bootstrap returns Control Plane version 1, the actual UDP port, the rotating certificate-hash window, and a random 256-bit capability. Native capabilities are in-memory, expire after seven idle days, and are refreshed by authenticated activity. Explicit client Disconnect revokes its authority and views while preserving terminals. This is the SSH-native policy, not the subsequent browser/passkey session-token policy.

TLS identity and rotation come from the reusable `quosh-trust` crate. The client must pin bootstrap hashes; ordinary Web PKI or an unchecked self-signed certificate is not substituted. Authenticated Control Plane connects return a refreshed forward hash chain.

## Control Plane

Path: `/sheepdog/control`. The first bidirectional stream uses Quosh's length framing: little-endian u32 byte count, u8 type, payload. The first frame is AUTH (32), carrying exactly 32 credential bytes; AUTH_OK (33) returns JSON version and hashes. REQUEST (34) and REPLY (35) carry JSON defined by `sheepdog-proto`.

Supported actions: snapshot, create Space, create Tab, explicitly reopen an ended Tab, obtain a terminal attachment, confirmed close Tab/Space, and Disconnect the client. Navigation is per client; terminal organization belongs to the server. Unknown credentials cannot mutate it.

Request records are bounded to 16 KiB and replies to 2 MiB. Handshakes and writes have deadlines; concurrency is bounded. There are limits of 128 client capabilities, 128 Spaces, and 256 Tabs. Orphaned terminal views are retired only after accepted input settles, so revocation cannot silently throw away queued accepted bytes.

Control requests are sequential request/response, not an automatically replayable transaction log. A future client must not blindly retry non-idempotent create/close operations after a lost reply; refresh server state first.

## Terminal Plane

Path: `/sheepdog/terminal`. The first frame is ordinary Quosh Hello using the view ticket's session id and secret. Claims increment that view's transport epoch; replacing one connection does not replace another client's view of the same terminal.

After HelloOk, full authoritative screens use `MSG_SHARED_SCREEN` (19), followed by receiving-client input-acceptance acknowledgments. Input, resize, state acknowledgment, ping/pong, and exit use Quosh framing. A terminal-plane Hangup detaches its view; destructive process termination is a separate confirmed Control Plane operation. Clients should close the transport out of band when quitting, including under input backpressure.

Initial transport sends shared screens reliably, not through datagrams. The reusable Quosh client also handles reordered shared-screen datagrams, so a future datagram path need not invent separate prediction rules.

Each screen carries its own version, receiving view's echo acknowledgment, prediction generation, and prediction eligibility. Generation changes reset overlays without dropping unacknowledged input or replaying stale predictions. Current policy suppresses prediction with multiple connected writers or unsettled foreign input; a sole writer uses ordinary adaptive prediction. User Never remains authoritative.

A separate bounded reader retains partial message progress while output, resize, or screen events wake the sender loop. PTY input backpressure never blocks the terminal owner from draining output. Read, write, decompression, and record budgets are enforced before accepting arbitrarily large payloads.

## Persistence and shutdown

Organization is staged, serialized to a private temporary file, fsynced, atomically renamed, and followed by a directory fsync. Process handles, client credentials, terminal contents, and agent commands are not persisted. Restore marks Tabs ended on server restart; Reopen explicitly starts a fresh shell.

The daemon retains terminal handles independently of network views. SIGINT/SIGTERM closes the endpoint, terminates terminal process groups with bounded escalation, stops connection tasks, and removes the bootstrap socket. A process-launch error produces an ended Tab rather than falsely advertising a running shell.

## Evidence and remaining work

Tests exercise actual WebTransport, real PTYs, the reusable Quosh client, certificate-pin rejection, credential rejection/expiry, fragmented input during foreign resize, reconnect deduplication, explicit disconnect without process destruction, persistence restoration, and the actual server/helper binaries.

Still absent: a macOS TUI, live agent recognition/Attention, File Ingest, Control Plane event subscriptions, terminal history browsing, and PWA/passkey authentication. These are not implied by the existence of the native network server.
