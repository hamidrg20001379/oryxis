viewport: 1200x750
mode: Zen
-----
# The Menu key opens the pane's context menu at the text cursor, whatever
# the right-click scheme: under the default (Paste) a right-click never
# shows that menu, so the key is the keyboard's only door to it. The
# harness has no name for the Menu key, so the palette row runs the same
# action. A selection made with the mouse is read off the emulator, so
# the menu offers Copy exactly when a right-click would.
#
# Also pinned: the palette's Enter runs the TOP row on screen, with the
# mouse resting where the rows appear as the list shrinks (each row arms
# its hover; the query change drops that pick).
settle 250
click "Skip"
settle 250
click "Continue without password"
settle 250
timeout 500
type ctrl+shift+l
settle 250
expect "bash (default)"
# A mouse resting over where the palette's rows will be.
move (600.00, 375.00)
type ctrl+shift+p
settle 400
type "paste"
settle 400
expect "Terminal Settings: Right-click"
type enter
settle 400
absent "Terminal Settings"
# Known text on a known row: after `clear`, row 0 is the prompt and row 1
# is the echo, from column 0.
type "clear"
type enter
settle 400
type "echo oryxis"
type enter
settle 400
screenshot context-menu-key-text
type ctrl+shift+p
settle 400
type "context menu"
settle 400
expect "Open context menu"
type enter
settle 400
expect "Save Output As…"
expect "Copy Screen"
expect "Clear Scrollback"
type escape
settle 250
absent "Save Output As…"
# Select the echoed word, then ask again: the menu leads with Copy.
click double (30.00, 75.00)
settle 300
type ctrl+shift+p
settle 400
type "context menu"
settle 400
type enter
settle 400
expect "Copy"
expect "Save Output As…"
