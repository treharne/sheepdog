# Confirmed design constraints

- Sheepdog provides a server-side component, a TUI client, and subsequently a PWA client.
- Do not maintain a Herdr fork. Reimplement ideas in Sheepdog rather than build on a copied Herdr codebase.
- First-release scope is one Sheepdog server on one remote machine. No unified multi-machine Switcher.
- The client shows one selected terminal. Tabs switch terminals; Spaces group Tabs.
- A Space has a name and default remote working directory. New Tabs start shells there; existing terminals may change directory independently. No automatic Git worktree or repository management.
- Closing a Tab terminates its terminal process and running agent, with confirmation for running work. Closing a Space does the same for its Tabs. Disconnecting or quitting the client preserves running work.
- Use Herdr as the reference for navigation and keybindings.
- Closing or losing a client connection preserves Tabs, running processes, agent state, and terminal contents on the server. Reconnecting clients regain access to those terminals.
- Server restart restores saved organization, not running processes, and does not automatically restart commands. Restored Tabs explicitly show that their terminal ended on server restart and offer an action to open a new shell in the saved directory; never silently relaunch an agent.
- Agents are launched by the operator inside terminals and recognized by Sheepdog. Recognition, rather than a built-in agent launcher, is the requested interaction.
- Quosh is the Terminal Plane (ADR-0006).
- Initial agent support must include pi, grok, and agy; none is deferred.
- Agent state and Attention behavior follow Herdr rather than a newly invented policy. Reference checkout: `reference/herdr`, commit `4dc23bb15d4a2fd2c093abfb509f903c3015bf56`.
- TUI connections use Quosh's SSH-assisted authentication; the subsequent PWA uses Quosh passkey enrolment. Both reach the same Unix user's Sheepdog state.
- Initial platforms: Ubuntu server and macOS TUI client.
- Project license: GPL-3.0-only, matching Quosh.
- Attention is displayed only inside connected clients. No external push or other closed-client notification system.
- Multi-client interaction follows Herdr's normal client behavior: independent navigation, shared terminal state when viewing the same Tab, and geometry controlled by the most recent focus, selection, or interaction. Do not substitute exclusive terminal takeover merely because Herdr also exposes a separate direct-terminal attachment mode.
- File Ingest is required in the first TUI release, for arbitrary pasted local file paths rather than only selected image types.
- Automatically ingest only when the entire clipboard identifies an existing local file, accepting quoted paths and Finder file URLs. Otherwise paste unchanged; provide a literal-paste bypass. Do not scan commands for embedded paths.
- Upload requires confirmation for large or ambiguous paths, including directories, bare `~`, bare `/`, paths containing `*`, and any individual file over 1 MB.
- Expand wildcard paths locally without executing a shell. Before uploading, show resolved files, destination, and total size for confirmation. No matches means no upload; `~` and `/` are directory requests requiring explicit confirmation.
- Never follow symlinks for File Ingest, including a directly pasted symlink. Disclose skipped links in the upload preview.
- Clipboard images without file paths are supported: save the image as a temporary local file, upload it, and insert its remote path. Apply the same size-confirmation rules.

## Decisions reopened

- Server ownership is resolved by ADR-0007: Sheepdog owns terminal processes and agent state independently of Herdr.
- The client direction is resolved: TUI first, PWA subsequently, with one selected terminal and navigation through Tabs and Spaces.
- Client packaging in ADR-0005 is no longer settled; the PWA follows the TUI rather than being the sole initial client.
