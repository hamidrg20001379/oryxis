viewport: 1200x750
mode: Zen
-----
# "Save Output As..." takes the pane's whole buffer, scrollback included,
# and asks where it goes: Clipboard or File. Its home is the terminal's
# own menu, which exists only under the Menu right-click scheme; under
# the default (Paste) the tab chip's menu carries it instead, the same
# single-door rule as Copy Screen. This pins both doors, that Esc saves
# nothing, and that Enter answers the default row (Clipboard). The File
# half opens the OS save dialog, which the emulator cannot drive.
settle 250
click "Skip"
settle 250
click "Continue without password"
settle 250
# A live PTY never lets the emulator quiesce, so cap the per-instruction
# wait instead of burning the full timeout on every line.
timeout 500
type ctrl+shift+l
settle 250
expect "bash (default)"
# Default scheme is Paste: the chip's menu is the door.
click right (150.00, 20.00)
settle 250
click "Save Output As…"
settle 250
expect "Save output"
expect "Clipboard"
expect "File…"
# Esc saves nothing and closes the question.
type escape
settle 250
absent "scrollback included"
# Again, and Enter takes the default row: the clipboard.
click right (150.00, 20.00)
settle 250
click "Save Output As…"
settle 250
expect "Save output"
# The copy toast lives 1.8 s, which the live PTY's settles can outlast:
# assert it first.
type enter
expect "Copied to clipboard"
absent "scrollback included"
# The toast clears itself, so the second copy below proves its own.
wait 2200
absent "Copied to clipboard"
# Flip the scheme to Menu through the palette row that owns it (the
# pick_list is clicked by coordinate, see terminal-copy-screen.ice).
type ctrl+shift+p
settle 400
type "right-click"
settle 400
expect "Terminal Settings: Right-click"
type enter
settle 500
screenshot save-output-scheme
click (1058.00, 79.00)
settle 400
click (1022.00, 121.00)
settle 400
# Back on the shell: the chip no longer offers it, the terminal does.
click (150.00, 20.00)
settle 400
timeout 500
click right (150.00, 20.00)
settle 250
absent "Save Output"
expect "Close Tab"
type escape
settle 250
click right (600.00, 400.00)
settle 250
click "Save Output As…"
settle 250
expect "Save output"
click "Clipboard"
expect "Copied to clipboard"
absent "scrollback included"
