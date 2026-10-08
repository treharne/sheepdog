# Quosh replaces Mosh as the Terminal Plane

Keep Sheepdog's product shape: one Switcher on laptop and phone, opening individual Pane Terminals while Herdr Server owns panes and agent state. Quosh replaces Mosh for roaming and speculative local echo; Sheepdog does not replace the SSH byte stream beneath `herdr --remote` or present the full Herdr TUI. This revises ADR-0003's transport choice while retaining the separation of Terminal Plane, Control Plane, and File Ingest. Quosh's browser support reopens ADR-0005's Native Shell decision; client packaging remains undecided.
