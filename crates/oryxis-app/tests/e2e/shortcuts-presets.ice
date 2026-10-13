viewport: 1200x750
mode: Zen
-----
# Settings > Shortcuts: a preset is picked in the selector, confirmed
# (the dialog names the actions it leaves with no shortcut), applied, and
# the selector then names it; picking Oryxis (default) the same way brings
# the factory table back. The selector's options are clicked by position
# (a dropdown's rows are not text-selectable): Oryxis first, then the
# platform's presets, Tabby alone on Linux. The rows a preset changes and
# what it keeps (mouse and wheel chords) are pinned by the unit tests in
# hotkeys/presets.rs; this pins the door.
expect "Welcome to Oryxis"
click "Skip"
click "Continue without password"
settle 250
# Toolbar gear (icon only, no text selector).
click (1175, 64)
settle
click "Shortcuts"
settle
expect "Oryxis (default)"
expect "Export…"
expect "Import…"
click (1075.00, 111.00)
settle 300
click (1075.00, 193.00)
settle 300
expect "Apply"
click "Apply"
expect "Shortcuts set: Tabby"
settle 300
expect "Tabby"
absent "Oryxis (default)"
# Back to Oryxis's own through the same selector.
click (1075.00, 111.00)
settle 300
click (1075.00, 152.00)
settle 300
expect "Apply"
click "Apply"
expect "Shortcuts set: Oryxis"
settle 300
expect "Oryxis (default)"
