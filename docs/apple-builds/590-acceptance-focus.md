# Keep physical TV focus on the requested control

**Status:** open — sole review finding addressed; signed compilation and source-scoped regressions pass; fast qualification and final installed acceptance pending.

Build: 196
Issue: #590

The physical TV navigation helper crosses tab/content boundaries vertically and moves between disjoint rows before steering horizontally. This prevents a rightward move on Category from changing grouping to Library and removing the requested movie shelf. The original build194 failed focus observation remains retained; no production view or playback contract changes.

This batch also repairs web prepared-HLS readiness observation and records exact PR582 deployment and device evidence. Apple source build196 requires fresh signed artifacts and installed-build readback after qualification. Android133 inputs remain unchanged.

The sole PR590 review identified the retained Library grouping prerequisite. The paging case now observes the original selected grouping, chooses the requested category/share through ordinary UI, and restores the original preference in deferred cleanup even on failure. No second review is requested.

Focused regressions ran after that review. Web readiness passed and unchanged old source failed its intended assertion; physical paging passed against canonical194 with grouping restored. Three retained Settings failures led to one bounded diagnostic: Quality focus is an anonymous leaf Other with exactly the uniquely identified Button's frame. The helper accepts only that observed proxy, with nonzero matching geometry and no focused descendants. A larger Cell is not evidence of picker focus. The diagnostic also restored the first failed case's Autoplay change from Off to its original On through observed ordinary UI.

Main moved through #589 to f400c0ea with native Live TV changes and claimed195. This candidate reserves196; fresh signed production apps and focused runner compilation precede qualification. Previous195 artifacts remain historical. No second adversarial review is requested.

Fresh signed196 iOS/tvOS and physical runner compilation passed on72d69a32/mainf400. The corrected runner passed paging and Settings once each against canonical194, zero failures/skips; original grouping restored and Auto/Original menu observed. Receipt545c1f86dda7c1ee9628d494aba892c12cf4eb0975f58c288dc82f3fd2940c20. Final196 installed/current-main acceptance follows qualified deployment.
