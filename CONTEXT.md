# Sheepdog

Roaming access to an independent Sheepdog Server through Quosh. A TUI client and a subsequent PWA client provide navigation among Spaces, Tabs, Agents, and Attention.

## Language

**Sheepdog Server**:
The process on the remote machine that owns terminal processes and agent state, independently of attached clients.
_Avoid_: Herdr Server

**Remote Attach**:
A client that runs on the operator's device, draws UI locally, and talks to a server. `herdr --remote` is Herdr's implementation of this for the TUI.
_Avoid_: option a, local TUI

**In-shell Attach**:
A local terminal (via Quosh) showing a process that runs on the server — the full Herdr TUI, or one pane. The PTY path cannot File Ingest; an Interceptor beside it can.
_Avoid_: option b, ssh then herdr, nested herdr

**File Ingest**:
Copying local files, directories, or a clipboard image onto the remote machine, then inserting remote paths into the selected terminal.
_Avoid_: SCP, clipboard image paste, drag and drop (those are mechanisms)

**Interceptor**:
The local process that performs File Ingest. It does not require Remote Attach; it can wrap In-shell Attach.
_Avoid_: paste bridge, clipboard image, herdr --remote (those are one implementation)

**Space**:
A named grouping of tabs and agents on one Sheepdog Server.
_Avoid_: workspace, project, machine (Herdr docs sometimes say workspace)

**Tab**:
A labeled session inside a Space. Selecting a Tab opens its Pane Terminal.
_Avoid_: window, pane (a Tab is the navigation object, not the underlying terminal)

**Agent**:
A recognized coding-agent process in a pane. An Agent belongs to a Space and is reached through a Tab.
_Avoid_: session, bot, CLI

**Attention**:
The connected client's navigation signal that an Agent needs the operator, following Herdr's agent-state and completion-viewing behavior.
_Avoid_: waiting, notification, blocked, done (those are Herdr states; Attention is the rollup the operator sees)

**Switcher**:
The navigation interface for Spaces, Tabs, Agents, and Attention, presented by the TUI or PWA client.
_Avoid_: session picker, Herdr chrome

**Pane Terminal**:
A Quosh terminal attachment to one Sheepdog-owned terminal process.
_Avoid_: terminal tab, embedded herdr, mosh into herdr

**Control Plane**:
The channel that lists Spaces, Tabs, and Agents, drives the Switcher, Attention, and File Ingest. It is distinct from the Terminal Plane.
_Avoid_: hop, API, socket

**Terminal Plane**:
Quosh, every time the operator interacts with a terminal — roaming, reconnecting after sleep, and speculative local echo.
_Avoid_: PTY stream, herdr terminal attach (the Herdr command, not the concept)
