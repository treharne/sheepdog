---
status: superseded by ADR-0003
---

# Keep Herdr's TUI as the laptop client

Sheepdog exists because Remote Attach over SSH dies on sleep and roaming, not because Herdr's TUI is wrong. The laptop UI stays `herdr`; Sheepdog replaces the hop under that attach. A new desktop TUI would reimplement chrome and File Ingest for no gain on the stated pain.

Superseded: the laptop must use Mosh for terminals, which cannot sit under `herdr --remote`. File Ingest does not require Remote Attach. Laptop chrome is reopened in ADR-0003.
