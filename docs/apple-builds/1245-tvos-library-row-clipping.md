# Keep complete Apple TV library cards visible

Build: 271
Issue: #1245

**Status:** built; physical Apple TV acceptance pending.

Movie and show libraries scroll their title, summary, search and letter index
with the results. Only the compact tools remain fixed. The previous fixed
preamble left a viewport shorter than a complete focused card, so native focus
revealed the title and metadata by clipping the top of its poster.

The results scroll view retains its viewport focus section: scrolled titles
cannot compete with the toolbar for Left/Right. Back still reaches View and
Down restores the exact content bookmark. Poster sizes remain unchanged.

Native regressions cover medium and extra-large movie cards through a deep
alphabetical row, horizontal moves and toolbar return, plus an extra-large
show card. Existing grid tools, Jump and View all journeys remain required.
The native runner includes all three visibility regressions. Physical Apple
TV acceptance remains pending; this note does not claim a device deployment.
