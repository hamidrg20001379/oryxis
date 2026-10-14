viewport: 1200x750
mode: Zen
-----
# A snippet's shortcut is not the Shortcuts editor's to take: the binding
# table dispatches first, so handing the chord to an action would silence
# the snippet with no sign of why. The capture refuses it, names the
# snippet in a toast and stays armed for another chord; a free chord
# lands as usual. (The emulator does not redraw after a modified key, so
# the toast is not asserted here: the armed capture is the proof.)
expect "Welcome to Oryxis"
click "Skip"
click "Continue without password"
settle 250
click "Snippets"
settle
click "SNIPPET"
settle
# Name and command fields by position: a text_input's value is not
# text-selectable.
click (990.00, 145.00)
type "deploy"
click (990.00, 550.00)
type "echo deploy"
click "Record"
settle 250
type ctrl+alt+k
settle 250
expect "Ctrl + Alt + K"
click "Save"
settle 300
# Toolbar gear (icon only, no text selector).
click (1175, 64)
settle
click "Shortcuts"
settle
# The chip of the "Duplicate Tab" row.
click (251, 714)
settle 250
expect "Press a key or mouse button…"
type ctrl+alt+k
settle 250
expect "Press a key or mouse button…"
type escape
settle 250
absent "Press a key or mouse button"
# A chord no snippet holds lands at once.
click (251, 714)
settle 250
type ctrl+alt+j
settle 250
absent "Press a key or mouse button"
