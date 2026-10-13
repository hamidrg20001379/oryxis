viewport: 1200x750
mode: Zen
-----
# Keys typed into a focused field belong to the field. The SFTP list router
# runs on every key from the global subscription, so Left in the Local
# pane's filter used to move the caret AND send the pane up a folder. A key
# the field captured now skips the list router; with the list in focus,
# Left and Backspace go up.
#
# The marker file is what tells the folders apart: it exists only in the
# sandbox home, so it vanishes from the listing once the pane goes up.
click "Skip"
click "Continue without password"
settle
type ctrl+shift+e
settle
# Dismiss the host picker: this flow only needs the Local pane.
click (785, 166)
settle
click right (300, 400)
settle
click "New file"
settle
click (599, 365)
type "fk-marker.txt"
click "Create"
settle
expect "fk-marker.txt"
# Type into the filter, move the caret both ways and delete a character:
# the field edits, the pane stays where it is.
click (453, 72)
type "fk-"
settle
type left
settle
type right
settle
type backspace
settle
expect "fk-marker.txt"
# Click the row (in the Modified column, off the name) so the list has the
# focus, then Backspace goes up a folder, where the marker is not.
click (300, 194)
settle
expect "fk-marker.txt"
type backspace
settle
absent "fk-marker.txt"
