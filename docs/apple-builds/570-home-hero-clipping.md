# Keep the iPhone Home movie title and controls inside the card

Build: 190
Issue: #570

Constrain loaded featured artwork to the compact card width before laying out
its title, metadata, progress and Resume control. The artwork still fills and
crops within the same rounded card; it no longer widens the foreground stack
past the visible edges. iPad and television retain their existing layouts.

The regression checks both the outer card and its 16-point content inset at
320, 375, 402 and 430-point phone widths. Before the repair, the content bounds
escaped the card at every width. The focused iOS tests passed in the initial
working snapshot; final delivery is recorded in issue #570's linked repair PR.
Merge does not publish or install the app.
