viewport: 1200x750
mode: Zen
-----
# A snippet holds a list of actions and runs them in order on one pane:
# a command, a wait for the shell to mark its end (OSC 133), an app
# action (Copy Last Command + Output), a timed wait, a second command.
# The card says how many actions; the sidebar row offers Run and no Paste;
# the status bar shows the run while it waits.
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
# Name field and the first action's editor by position: a text_input's
# value is not text-selectable.
click (990.00, 145.00)
type "two steps"
click (990.00, 550.00)
type "echo first-step"
click "Add action"
settle 250
# The second row's kind picker.
click (957.00, 616.00)
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
click "Add action"
settle 250
scroll (0, -600)
settle 250
# The fourth row's kind picker, then its seconds field.
click (957.00, 535.00)
settle 250
click "Wait"
settle 250
scroll (0, -600)
settle 250
click (990.00, 582.00)
type ctrl+a
type "3"
click "Add action"
settle 250
scroll (0, -600)
settle 250
# The fifth row's command editor.
click (990.00, 582.00)
type "echo second-step"
click "Save"
settle 300
expect "5 actions"
# A live shell keeps tasks pending: pace by time from here on.
timeout 500
type ctrl+shift+l
settle 400
expect "bash (default)"
type "PS1='\\[\\e]133;D;0\\a\\e]133;A\\a\\]$ \\[\\e]133;B\\a\\]'"
type enter
settle 400
type "trap 'printf \"\\e]133;C\\a\"' DEBUG"
type enter
settle 400
# Sidebar, Snippets tab, hover the row for its buttons.
click (1036, 20)
settle 300
click (911, 64)
settle 300
move (1000, 310)
settle 250
expect "5 actions"
click (1161, 310)
wait 1000
expect "Running two steps (4/5)"
clipboard is "echo first-step\nfirst-step"
wait 3500
settle 500
absent "Running two steps"
