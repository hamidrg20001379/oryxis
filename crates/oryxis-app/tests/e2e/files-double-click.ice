viewport: 1200x750
mode: Zen
-----
# A row opens on a double click and only selects on a single one, and the
# `..` row is a row like the others (FileZilla's model): a single click
# selects it, a double click goes up. Both browsers: the dual-pane SFTP view
# and the terminal sidebar's Files tab. `click double` sends both clicks on
# one drawn frame; two `click` lines would be paired or not depending on how
# long the app took to settle in between.
#
# The subject folder and a marker file are created here, so the test never
# depends on what the sandbox home holds. The marker is what tells the two
# folders apart: the folder's own name also shows in the breadcrumb once
# inside it, so it cannot be the assertion.
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
click "New folder"
settle
click (599, 365)
type "dc-outer"
click "Create"
settle
expect "dc-outer"
click right (300, 400)
settle
click "New file"
settle
click (599, 365)
type "dc-marker.txt"
click "Create"
settle
expect "dc-marker.txt"
# Filter to the two, so the rows sit at fixed heights: `..` (y 165), then
# the folder (y 194), then the file. Clicks land in the Modified column,
# off the name, so no click here can arm the slow-click rename.
click (453, 72)
type "dc-"
settle
# Single click on the folder: selected, not entered.
click (300, 194)
settle
expect "dc-marker.txt"
wait 1200
click double (300, 194)
settle
absent "dc-marker.txt"
# Inside the (empty) folder. A single click on `..` selects it and stays.
click (300, 165)
settle
absent "dc-marker.txt"
wait 1200
click double (300, 165)
settle
expect "dc-marker.txt"
# The sidebar Files tab of a local shell lists the same home. The new-tab
# button sits after the SFTP chip; a coordinate, as it carries no text.
click (333, 20)
settle
click "Local Shell"
settle 800
click (1036, 20)
settle
click (979, 64)
settle 500
expect "dc-marker.txt"
click "dc-outer"
settle 300
expect "dc-marker.txt"
wait 1200
click double "dc-outer"
settle 500
absent "dc-marker.txt"
click ".."
settle 300
absent "dc-marker.txt"
wait 1200
click double ".."
settle 500
expect "dc-marker.txt"
