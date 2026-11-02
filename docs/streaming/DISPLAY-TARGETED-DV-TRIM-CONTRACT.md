# Display-targeted DV trims — superseded candidate

**Status:** superseded on 2026-10-25 · **Replaced by:** phase 2 of
[the build plan](DISPLAY-TARGETED-DV-BUILD.md#phase-2-closed-form-dolby-display-management-l1-l3-l2-l8)

This document described a private, never-merged trim candidate: ST 2094-10
Annex B.3 gain/offset/gamma on target-normalised linear RGB, an L2 chroma
shader, the `trim_selection.h` selector and their software-reference
comparisons. None of it was wired into serving.

Phase 2 replaces that approach with closed-form Dolby display management (CM v4
and CM v2.9) measured against the licensed reference renderer, so the candidate
and its controls were removed from the tree. The removed files and the full
text of this contract remain in git history at `ec827da8a`:
`tools/dv_processing/trim_selection.h` and the `controls/trim_*`,
`compare_trim_*`, `build_trim_candidate.sh` and `test_trim_*` files.
