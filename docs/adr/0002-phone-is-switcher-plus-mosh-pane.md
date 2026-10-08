---
status: superseded by ADR-0006
---

# Phone is a Switcher plus Mosh Pane Terminals

The phone must not render Herdr's TUI or put an agent switcher in a terminal. Navigation is a real GUI (Control Plane). Opening an agent drops into a Mosh session on that pane's PTY (Terminal Plane) so roam and typing ergonomics are Mosh's. File Ingest and Switcher traffic do not have to be Mosh.
