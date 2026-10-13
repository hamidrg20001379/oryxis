viewport: 1200x750
mode: Zen
-----
# "Copy Last Command + Output" copies what the shell marked (OSC 133): the
# command line from its B mark, the output from C to D, the next prompt
# left out. It ships unbound, so the palette runs it here. A shell that
# marks nothing gets a toast saying why, not a silent no-op.
#
# The harness bash marks nothing, so the test gives it marks: D and A in
# front of the prompt and B after it (PS1), C right before each command
# runs (a DEBUG trap): the four marks a shell integration emits.
settle 250
click "Skip"
settle 250
click "Continue without password"
settle 250
timeout 500
type ctrl+shift+l
settle 250
expect "bash (default)"
# Nothing marked yet.
type ctrl+shift+p
settle 400
type "last command"
settle 400
expect "Copy Last Command + Output"
type enter
expect "No command to copy yet: the shell on this pane has to mark its commands (OSC 133 shell integration)."
settle 400
type "PS1='\\[\\e]133;D;0\\a\\e]133;A\\a\\]$ \\[\\e]133;B\\a\\]'"
type enter
settle 400
type "trap 'printf \"\\e]133;C\\a\"' DEBUG"
type enter
settle 400
type "echo oryxis-out"
type enter
settle 600
type ctrl+shift+p
settle 400
type "last command"
settle 400
type enter
expect "Copied to clipboard"
clipboard is "echo oryxis-out\noryxis-out"
