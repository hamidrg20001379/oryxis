viewport: 1200x750
mode: Zen
-----
# Ctrl+C copies the text selected in the UI, and only that: with nothing
# selected it leaves the clipboard alone, on the hosts screen and in a
# terminal, where the keystroke is the shell's interrupt.
expect "Welcome to Oryxis"
click "Skip"
click "Continue without password"
expect "Create host"
clipboard "kept on the hosts screen"
type ctrl+c
settle
clipboard is "kept on the hosts screen"

# A local shell: copy a selection with Ctrl+Shift+C, then interrupt the
# shell. The copied text survives the Ctrl+C.
click (92, 20)
expect "Local Shell"
click "Local Shell"
timeout 500
wait 4000
type "clear; yes HELLO_CLIP | head -20"
type enter
wait 4000
press (8, 123)
move (60, 123)
move (95, 123)
release (95, 123)
settle
clipboard "seeded"
type ctrl+shift+c
wait 500
clipboard is "HELLO_CLIP"
type ctrl+c
wait 500
clipboard is "HELLO_CLIP"
