---
status: superseded by ADR-0007
---

# One Switcher GUI on laptop and phone

The operator is not tied to Herdr's TUI and does not need tmux-style split panes. What they need is Spaces, Tabs, Agents, and Attention, then a Mosh Pane Terminal. That Switcher is already required on the phone, so using it on the laptop is the same product, not a second multiplexer. One schema: Control Plane for the Switcher, Terminal Plane for the selected pane.
