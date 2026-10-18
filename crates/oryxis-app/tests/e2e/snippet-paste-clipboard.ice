viewport: 1200x750
mode: Zen
-----
# A snippet's paste step reads the clipboard when it runs. With "Confirm
# before pasting" (on by default) the careful-paste dialog shows the text,
# names the snippet and the host, and nothing is pasted until the user
# agrees; Cancel and Esc stop the run. The text goes to the RUN's pane:
# here the focus moves to the other half of a split while the run waits,
# and the paste still lands where the run started. A paste the user makes
# by hand meanwhile, confirmed in the same dialog, does not move a waiting
# run on.
#
# "paste it": paste, wait for the command's end, Copy Last Command + Output
# (what proves the paste ran, and where). "paste late": a 3 s wait, then
# "paste it". "waits": an output wait that never matches.
#
# The harness bash marks nothing, so the test gives it marks (PS1 and a
# DEBUG trap), as terminal-copy-last-command.ice does.
settle 250
click "Skip"
settle 250
click "Continue without password"
settle 250
click "Snippets"
settle
click "SNIPPET"
settle
# Name field and the first action's kind picker by position: a
# text_input's value is not text-selectable.
click (990.00, 145.00)
type "paste it"
click (957.00, 504.00)
settle 250
click "Paste clipboard and run"
settle 250
expect "Confirm before pasting"
click "Add action"
settle 250
# The second row's kind picker.
click (957.00, 651.00)
settle 250
click "Wait for the command to finish"
settle 250
scroll (0, -400)
settle 250
click "Add action"
settle 250
scroll (0, -400)
settle 250
# The third row's kind picker.
click (957.00, 536.00)
settle 250
click "App action"
settle 250
scroll (0, -400)
settle 250
click "Pick an action"
settle 250
click "Copy Last Command + Output"
settle 250
click "Save"
settle 300
expect "3 actions"
click "SNIPPET"
settle
click (990.00, 145.00)
type "waits"
click (957.00, 504.00)
settle 250
click "Wait for output"
settle 250
# The pattern field.
click (990.00, 550.00)
type "never-appears-in-output"
click "Save"
settle 300
expect "waits"
click "SNIPPET"
settle
click (990.00, 145.00)
type "paste late"
click (957.00, 504.00)
settle 250
click "Wait"
settle 250
# The seconds field.
click (990.00, 570.00)
type ctrl+a
type "3"
click "Add action"
settle 250
click (957.00, 636.00)
settle 250
click "Run another snippet"
settle 250
scroll (0, -400)
settle 250
click "Pick a snippet"
settle 250
# The picker's first option, "paste it".
click (990.00, 506.00)
settle 250
click "Save"
settle 300
expect "paste late"
# A live shell keeps tasks pending: pace by time from here on.
timeout 500
type ctrl+shift+l
settle 400
expect "bash (default)"
type ctrl+shift+p
settle 400
type "split side by side"
settle 400
type enter
settle 800
click "Local Shell"
settle 1200
# The new (right) pane has the focus: give its shell the marks.
type "PS1='\\[\\e]133;D;0\\a\\e]133;A\\a\\]$ \\[\\e]133;B\\a\\]'"
type enter
settle 400
type "trap 'printf \"\\e]133;C\\a\"' DEBUG"
type enter
settle 400
# Sidebar, Snippets tab; rows: paste it, paste late, (installs), waits.
click (1036, 20)
settle 300
click (911, 64)
settle 300
# Started on the right pane, then the left one takes the focus.
clipboard "echo in-pane-b"
move (1000, 240)
settle 250
click (1161, 240)
wait 300
expect "Running paste late (1/4)"
click (200, 400)
wait 4500
expect "Run the clipboard?"
expect "The snippet \"paste late\" pastes this into bash (default) and presses Enter."
expect "echo in-pane-b"
click "Paste and run"
wait 1500
# Copied from the right pane: the paste ran there.
clipboard is "echo in-pane-b\nin-pane-b"
absent "Running paste late"
# Refused: Cancel, then Esc.
clipboard "echo never-runs"
move (1000, 170)
settle 250
click (1161, 170)
wait 800
expect "Run the clipboard?"
click "Cancel"
wait 300
expect "\"paste it\" stopped at action 1: the paste was not confirmed"
absent "Running paste it"
move (1000, 170)
settle 250
click (1161, 170)
wait 800
expect "Run the clipboard?"
type escape
wait 300
absent "Run the clipboard?"
absent "Running paste it"
# A waiting run (on the left pane, which has the focus), and a two-line
# paste made by hand into that same pane and confirmed.
move (1000, 442)
settle 250
click (1161, 442)
wait 800
expect "Running waits (1/1)"
clipboard "echo one\necho two"
click (200, 400)
type ctrl+shift+v
wait 500
expect "Paste multiple lines?"
click "Paste"
wait 800
absent "Paste multiple lines?"
expect "Running waits (1/1)"
