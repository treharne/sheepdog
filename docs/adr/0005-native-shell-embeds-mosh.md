---
status: superseded by ADR-0007
---

# Native Shell embeds Mosh; Switcher is shared web UI

Reimplementing Mosh ergonomics (SSP, roam, speculative echo) in a browser is a large project. Embedding Mosh is the small path. A Pure PWA cannot speak Mosh UDP, and a local UDP↔WebSocket sidecar is not a Pure PWA. The client is a Native Shell (webview + native plugins) with one Switcher codebase. A server-side PTY relay for browsers is rejected for the Terminal Plane; that would be a new Mosh, not Mosh.
