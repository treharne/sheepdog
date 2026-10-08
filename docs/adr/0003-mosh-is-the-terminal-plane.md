---
status: superseded by ADR-0006
---

# Mosh is the Terminal Plane; File Ingest is an Interceptor

Every interactive terminal is Mosh (roam, sleep, speculative echo). That cannot be the transport under `herdr --remote`, which multiplexes many panes over one SSH byte stream. Mosh can *show* the Herdr TUI (In-shell) or one pane (Pane Terminal). File Ingest is a local Interceptor (drop/pick → copy to server → insert server path), not a reason to use Remote Attach. Control Plane traffic (Switcher, agent wait state, ingest) is not Mosh.
