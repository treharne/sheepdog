# Native TUI

Status: local-rendered prototype tested through real Linux PTYs. macOS acceptance, pi/grok/agy recognition and Attention, File Ingest, history/copy, and further Herdr parity remain required before initial product release.

## Run

On Ubuntu, run `sheepdog-server serve` as the owning Unix user, and install its helper on that user's SSH command path. On the client:

```sh
cargo install --git https://github.com/treharne/sheepdog.git --locked sheepdog-tui
sheepdog agents
```

`--ssh 'ssh -i /path/to/key'` selects SSH options. `--predict=never` disables prediction; Adaptive is the default. `--state-directory` chooses a remote state directory. `--quic-host` overrides SSH's resolved HostName when direct QUIC reachability differs. SSH may prompt normally before raw TTY mode starts. Production client dependencies exclude the Linux server/runtime/PTY crates.

## Startup and layout correction

The first prototype invented a two-row navigation header and mandatory name/directory wizard. That was wrong: the reference is Herdr's ordinary experience, not just its prefix keys.

The correction was traced against original pinned Herdr `4dc23bb15d4a2fd2c093abfb509f903c3015bf56` and freshly fetched `241b99cd397bb642eaabcfee1647bcecffb26be1`, without changing the earlier pin. Key evidence:

- `src/app/mod.rs::ensure_default_workspace`: creates and focuses starter work when empty.
- `src/app/creation.rs`: resolves CWD and creates a workspace with a shell.
- `src/workspace.rs`: automatic directory/repository-derived labels, with optional custom names.
- `src/config/model.rs`: default expanded sidebar width 26, mouse enabled, top tab bar, no new-workspace-name prompt.
- `src/client/shell/{config,sidebar,tabs,mouse,actions}.rs`: sidebar sections, one-row tab strip, hit rectangles, mouse creation/focus and optional naming.

Sheepdog now starts with a default-named Space and shell when no Tabs exist. Reconnecting attaches to existing work. Concurrent first clients use one atomic owner-lock transaction, so they do not each create a starter shell. Saved ended Tabs remain ended: this does NOT turn server restart into implicit command or agent relaunch.

A left sidebar lists Spaces and, below them, terminals across Spaces. Ordinary shells are explicitly listed as shells; detected-agent kinds/states are not fabricated. Automatic pi/grok/agy recognition and Attention will enrich this lower section in the next detection milestone. A thin one-row tab strip spans the terminal area; the rest is the selected terminal. There is no permanent full-width terminal footer.

Mouse controls:

- Click a Space or terminal in the sidebar to select it.
- Click a Tab in the top strip to select it.
- Click sidebar `+ New Space` for a default-named Space with a shell, no wizard.
- Click top-strip `+` for another shell Tab.
- Right-click a Space or Tab to rename it without changing the selected terminal.
- Middle-click a Tab to request confirmed closure.

Normal sidebar width is 26 cells (18 on smaller windows; hidden on very narrow windows). Paint and hit testing share one layout computation. The selected Tab remains visible in overflowing tab strips. Terminal geometry excludes the sidebar and top row; at 80×24, the remote viewport is 54×23. Each remote row is positioned independently so CRLF cannot overwrite the sidebar. Oversized foreign-client screens are safely cropped, including overflow glyphs and wide characters at the right edge.

Default Space labels derive from the remote directory basename, falling back to `Space`. New Spaces inherit the source Space's saved directory or use remote HOME. Live-CWD/git identity tracking and broader Herdr layout configurability remain pending.

## Keyboard alternatives

Ctrl+B prefix, then:

| Key | Action |
| --- | --- |
| c | New shell Tab |
| N | Default-named new Space with shell |
| n / p, 1–9 | Next/previous/jump Tab |
| w / g | Spaces / searchable grouped Goto picker |
| r / , | Rename Space / Tab |
| X / D | Confirm close Tab / Space |
| Enter | Explicit new shell in ended Tab |
| q | Detach, preserve server work |
| ? | Supported-binding help |
| P | Arm one literal paste |
| Ctrl+B | Send literal Ctrl+B |

Pickers support arrows/j/k, Enter, Esc, `/` search and mouse. Naming fields own their editing chords; Esc cancels. Unknown prefix actions are explicitly consumed rather than accidentally sent to the shell.

## Wire and safety

Control v1 adds `EnsureDefault`, `CreateWorkspace` (Space plus shell), `RenameSpace`, and `RenameTab`. Existing low-level `CreateSpace` remains organization-only. Default creation stages Space and Tab together, durably saves once before spawning, and never commits a partial model on save failure. Renames preserve process identities and only persist organization.

Control operations do not block the UI. Read-only polling is canceled for navigation; mutations are never blindly replayed. Canceled partial RPC streams are discarded by the native adapter. Cached views preserve sequencing across navigation. Pending input is bounded to 64 KiB and fenced with an explicit error on overflow/failure.

Local output uses bounded nonblocking partial writes, coalescing repaint demand. Quit and SIGINT/SIGTERM detach without destroying work even when the display is unread. Guards restore termios and descriptor flags and perform best-effort private-mode cleanup. Ordinary raw Ctrl+C still reaches the remote terminal.

Normal paste remains explicitly gated until File Ingest exists. Prefix P arms one literal paste, preserving remote bracketed-paste mode. This is an unfinished prototype behavior, not the accepted final paste policy. No clipboard-image handling or uploads are claimed.

Runtime confirmed close signals job-control groups in the owning PTY session, with TERM/KILL escalation and bounded shell reap. A regression test covers a foreground job in its own group that ignores HUP and TERM. This is not a cgroup sandbox; session-escaping/elevated processes need further hardening/agent-specific acceptance.

## Acceptance evidence

Actual `sheepdog` binary, real helper socket/capabilities, persisted TLS pins, real WebTransport and PTYs are exercised on Linux. Only SSH execution is mocked, using Node to connect to the real helper socket; production does not require Node.

Tests cover keyless starter shell, concurrent first clients, atomic persistence failure, retained screens, sidebar-safe multi-row rendering, mouse Space/Tab creation and switching, durable optional rename, explicit reopen after restart, closure confirmation, independent foreground job groups, literal paste gating, output backpressure, preserved work on quit and local termios restoration.

Validation: **78 workspace tests**, clean formatting and strict Clippy; the six real-TUI PTY tests passed five consecutive parallel runs.

External SSH execution and macOS input/rendering/build behavior remain unverified. Agent recognition/Attention, File Ingest, history/copy, terminal mouse forwarding, configurable bindings, Control Plane subscriptions, current-directory tracking, and the PWA remain implementation work, not scope removals.
