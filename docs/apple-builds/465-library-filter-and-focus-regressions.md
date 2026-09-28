# Verify native library filtering and page-arrival focus

**Status:** open — source branches integrated; this batch's single review,
focused execution and qualification remain pending.

Build: 200
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

Apple200 and Android137 are reserved above the preceding native batch's
199/136 inputs. The separately authored app and regression sources compile;
this integrated counter claim has not been compiled or behaviorally executed.
No signed product, installation or performance improvement is claimed.
Named Apple TV and Lenovo 6000-title request, order, duplicate, filter, focus
and frame traces remain open. Synthetic fixtures cannot close those rows.

[Execution status](../reviews/ARCHITECTURE-REVIEW-GPT-BUILD-STATUS.md) and the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) retain
qualification and physical evidence separately.
