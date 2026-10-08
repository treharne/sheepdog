# Sheepdog product specification

Status: accepted. Shared understanding confirmed; implementation authorized.

## Product

An independent, Herdr-inspired terminal and coding-agent manager for one remote machine. Sheepdog owns its server and clients; it neither depends on Herdr Server nor maintains a Herdr fork. Quosh supplies the Terminal Plane, including reconnection and prediction.

First deliver an Ubuntu server and macOS TUI client. A PWA client follows. License: GPL-3.0-only.

## Navigation and terminals

- One terminal is visible at a time. Tabs select terminals; Spaces group Tabs.
- Each Space has a name and default remote working directory. New Tabs start shells there; existing terminals may change directory independently.
- Follow Herdr for navigation, keybindings, agent-state behavior, and Attention. Reference: `reference/herdr` at `4dc23bb15d4a2fd2c093abfb509f903c3015bf56`.
- Operators launch agents normally inside terminals. Recognize pi, grok, and agy in the initial release.
- Attention appears inside connected clients only.
- Clients navigate independently. Clients viewing the same Tab share terminal contents and input, following Herdr's normal client behavior. Most recent focus, selection, or interaction controls terminal geometry.

## Lifetime

- Closing or losing a client connection leaves server-owned processes, Tabs, terminal contents, and agent state running.
- Reconnecting, including from another device, restores access to existing work.
- Closing a Tab terminates its process and agent, with confirmation for running work. Closing a Space does the same for its Tabs. Client quit is disconnect, not destruction.
- Server restart restores saved organization, not original processes. Restored Tabs explicitly explain the loss and offer a new shell in the saved directory. Do not automatically restart agents or commands.

## Authentication

The TUI uses Quosh's SSH-assisted connection model; the PWA uses Quosh passkey enrolment. Both reach the same Unix user's Sheepdog state. Keep users' work separate; do not introduce Sheepdog accounts.

## File Ingest

- Recognize an entire pasted local path, including quoted paths and Finder file URLs. Do not search arbitrary commands for embedded paths. Text that is not an ingest request passes through unchanged; provide literal paste.
- Support arbitrary file types and clipboard images without existing file paths. Materialize clipboard images locally before uploading.
- Support directory requests and wildcard expansion without executing a shell.
- Require confirmation for directories, ambiguous paths, bare `~` or `/`, wildcard requests, and any individual file over 1 MB. Show resolved files, destination, and total size before upload. No wildcard matches means no upload.
- Never follow symlinks, including directly pasted links. Disclose skipped links.

## Engineering checks before implementation

These are investigation tasks, not settled implementation claims:

- Integrating Sheepdog-owned persistent terminal processes with Quosh, whose current server creates its own shell sessions.
- Supporting Herdr-style shared input and geometry while preserving Quosh input sequencing, causal echo acknowledgments, and prediction correctness. Existing Quosh session attachment rules do not already provide this behavior.
- An authenticated Control Plane and upload path usable by both the native TUI and later PWA.
- Mapping Herdr's recognition and Attention behavior into independent implementations and tests for all three required agents.

If an investigation forces a product trade-off, reopen that decision with the operator rather than silently reducing scope.
