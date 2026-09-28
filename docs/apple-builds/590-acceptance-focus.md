# Keep physical TV focus on the requested control

**Status:** open — source prepared; sole review, qualification and corrected physical acceptance pending.

Build: 195
Issue: #590

The physical TV navigation helper crosses tab/content boundaries vertically and moves between disjoint rows before steering horizontally. This prevents a rightward move on Category from changing grouping to Library and removing the requested movie shelf. The original build194 failed focus observation remains retained; no production view or playback contract changes.

This batch also repairs web prepared-HLS readiness observation and records exact PR582 deployment and device evidence. Apple source build195 requires fresh signed artifacts and installed-build readback after qualification. Android133 inputs remain unchanged.
