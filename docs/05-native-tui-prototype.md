# Native TUI prototype

Status: working local-rendered prototype, tested through real Linux PTYs. Not yet macOS-verified and not the complete initial product release: pi/grok/agy recognition, Attention, File Ingest, history/copy mode, and further Herdr parity remain required.

## Run

On the remote Ubuntu host, run `sheepdog-server serve` as the target Unix user. Install `sheepdog-server` on that user's command path for the SSH bootstrap helper.

On the client:

```sh
cargo build --release -p sheepdog-tui
./target/release/sheepdog user@host
./target/release/sheepdog --ssh 'ssh -i /path/to/key' --predict=never user@host
```

`--state-directory` selects an explicit remote state directory. `--quic-host` overrides SSH's resolved HostName when QUIC needs a different directly reachable address. SSH may prompt normally before raw terminal mode starts. No raw credential or certificate hash needs manual entry. No server or user service was deployed as part of this milestone.

The binary is `sheepdog`, from package `sheepdog-tui`. Only Adaptive and Never are CLI prediction choices. Production dependencies exclude the Linux server/PTY crates; the Linux-only end-to-end fixtures use them as development dependencies.

## Implemented controls

Default prefix is Ctrl+B, followed by:

| Key | Action |
| --- | --- |
| c | New shell Tab in selected Space |
| n / p | Next / previous Tab in that Space |
| 1–9 | Jump to Tab |
| w | Spaces picker |
| g | Goto picker, grouped by Space |
| N | New Space: name, then remote absolute directory |
| X | Confirm close Tab |
| D | Confirm close Space and all its work |
| Enter | Explicit new shell in an ended Tab |
| q | Detach; leave server work running |
| ? | Supported-binding help |
| P | Arm one literal paste, bypassing future File Ingest |
| Ctrl+B | Send literal Ctrl+B to the terminal |

These bindings are derived from the pinned Herdr keyboard reference; omitted split actions are not emulated by nesting a remote multiplexer. Unimplemented prefix actions are consumed with an explicit notice, never accidentally sent to the shell.

Pickers support arrows/j/k, Enter, Esc, `/` search, and mouse selection/wheel navigation. Goto Left/Right moves between Space groups without wrapping. Header Spaces and Tabs are clickable. Naming fields support grapheme-safe character movement/deletion, word movement/cuts, local cut/yank, and horizontal scrolling. Naming fields own their editing chords: Esc cancels before returning to ordinary prefix navigation.

Only one selected terminal is painted. Two local navigation rows and a footer are reserved; the server receives the remaining terminal geometry. Foreign clients' larger screens are cropped safely, including overflow glyphs and a wide glyph cut at the right edge. Focus, resize, and selection promote this client's viewport without evicting other clients.

Restored ended Tabs explain server restart and offer an explicit new shell. Merely selecting a restored Tab, pressing ordinary Enter, or reconnecting never launches old commands. Locally retained final screens remain visible when their shell ends; server-side history/ended-screen retrieval for newly connected clients is not implemented yet.

## Responsiveness and safety

Control operations run outside the UI loop. Background snapshot polling is cancellable for user navigation; non-idempotent mutations are never blindly replayed. The operation guard aborts outstanding work on quit or UI errors, and the native client discards canceled partial RPC streams.

Input typed while selecting a known terminal is bounded to 64 KiB. Overflow or failure is explicitly reported and fenced rather than silently continuing. Views retain their Quosh sequencing across navigation; a new core is not fabricated for an old ticket.

TTY output uses nonblocking partial writes and one bounded paint (16 MiB maximum), coalescing subsequent repaint demand. A blocked output consumer cannot block prefix Quit or signal handling. Guards restore termios and descriptor flags, discard unfinished local output on exit, and perform best-effort private-mode cleanup. SIGINT/SIGTERM from outside the raw keyboard path detach rather than destroy work; ordinary Ctrl+C still goes to the selected remote terminal.

Normal paste is explicitly gated until File Ingest is implemented, rather than pretending pasted file paths were uploaded. Prefix P arms one literal paste; if the remote application advertises bracketed-paste mode, the bypass preserves that framing. This is a prototype gap, not the accepted final paste behavior. Clipboard images and OS clipboard integration are not implemented.

## Interactive job-control regression

PTY tests found that the original runtime's process-group-only shutdown missed a foreground job moved to another group by an interactive shell. Runtime shutdown now discovers and signals groups belonging to its owning PTY session, retaining TERM/KILL escalation and bounded shell reap. A real interactive foreground job that ignores HUP and TERM is now killed on confirmed Tab close, without affecting another Tab.

This is session-scoped cleanup, not a cgroup sandbox. Deliberately session-escaping processes or privilege elevation still require further lifecycle hardening/agent-specific acceptance; the prototype does not claim those cases are fully enforced.

## Evidence and gaps

Linux PTY tests run the actual `sheepdog` binary, private bootstrap socket, rotating pinned TLS, real WebTransport, and remote PTYs. Only the SSH command is mocked (using Node to connect to the real helper socket). Therefore Node must be installed for these Linux integration tests; production does not require it.

Validation: all **72 workspace tests** passed; format and strict Clippy checks passed. The six real-TUI PTY tests also passed **five consecutive parallel runs**.

Coverage includes terminal input, independent retained Tab screens, prefix and mouse navigation, Goto search, new Space/Tab, canceled and confirmed close, real foreground job-control groups, explicit reopen after restart, one-shot literal paste, unread-display backpressure, client quit preserving work, and local termios restoration.

Still unverified: external SSH execution by this frontend, actual macOS build/runtime/input behavior, and a full terminal/application compatibility matrix. Still absent: agent recognition/Attention, File Ingest, history/copy/terminal mouse forwarding, rename/configurable bindings, Control Plane subscriptions, current-directory tracking, and the PWA. These remain implementation work, not scope removals.
