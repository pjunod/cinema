# Verify native library filtering and page-arrival focus

**Status:** open — on `main` since 2026-10-04 (#793); focused execution on
`main` and physical qualification remain pending.

Build: 206
Issue: #465

The Apple library view delegates its existing page, query and filter task
ownership to an internal coordinator. Three controlled-loader regressions
exercise query-driven completion and count summaries, rejection of a late
filter result, and cancellation of five edits during the existing 150 ms wait.
The visible grid, item identities, merged order and focus policy remain.

Android adds two actual-pager watch-filter regressions and a Compose TV
page-arrival focus case using the production grid and card actions. A source
contract binds its local-loader route to the view-model route. Android category
query is excluded by plan section 5.5 and is not introduced.

This change was first reserved as Apple 200 / Android 137. It reached `main`
with the architecture effort (#793, 2026-10-04), and `main` has held Apple
build 206 and Android versionCode 144 since, so build 206 is the first build
number that carries it. Build 206 is the effort's counter, not this change's
alone: it contains every Apple change on `main` at that point. The last
recorded Apple suite run is the 2026-10-02 simulator run of the effort branch
at `6f6466ebc` (both platforms built; seven failing tests, none of them this
change's — listed in
[APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION.md §6.1](../clients/APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION.md));
no run on `main` is recorded. Build 206 has not been signed, uploaded or
installed, and no performance improvement is claimed.
Named Apple TV and Lenovo 6000-title request, order, duplicate, filter, focus
and frame traces remain open. Synthetic fixtures cannot close those rows.

[Execution status](../reviews/ARCHITECTURE-REVIEW-GPT-BUILD-STATUS.md) and the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) retain
qualification and physical evidence separately.
