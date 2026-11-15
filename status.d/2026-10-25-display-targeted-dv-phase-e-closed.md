# Display-targeted DV Phase E (closed-form Dolby display management)

[build plan, phase 2](../docs/streaming/DISPLAY-TARGETED-DV-BUILD.md#phase-2-closed-form-dolby-display-management-l1-l3-l2-l8). PR #1275 replaces the Annex B.2 curve with our own CM v4 / CM v2.9 equations, checked privately against the licensed reference (CM v4 real P8 title mean ≤ 0.007 at 100 nits, ≤ 0.001 at 300–1600). Merged as 489e86e4 with served trims off; the trims PR #1276 turns them on. Paul deploys.
