# Sheepdog owns its server and clients

Sheepdog is an independent implementation inspired by Herdr, not a maintained Herdr fork or a client dependent on Herdr Server. Its server owns terminal processes and agent state on one remote machine; a TUI client comes first and a PWA client follows. This replaces the earlier Herdr-owned server boundary and ADR-0004's GUI-only client direction. Reusing Herdr would reduce initial implementation work but couple Sheepdog to its model and integration surface.
