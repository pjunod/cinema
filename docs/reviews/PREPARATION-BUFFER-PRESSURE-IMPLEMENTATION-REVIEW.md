# Preparation buffer pressure — adversarial implementation review

**Status:** approved at `4b8ae6f02` · **Reviewed:** 2026-10-08 ·
**Base:** `079960dae` · **Reviewer:** independent `storage_review` agent.

Companion to [the canonical repair ledger](../streaming/PREPARATION-BUFFER-PRESSURE-RCA-AND-IMPLEMENTATION.md)
and [the approved plan review](PREPARATION-BUFFER-PRESSURE-PLAN-REVIEW.md).
This report records review of the actual implementation, separate from approval
of the design. It does not claim deployment or physical Apple TV acceptance.

## Findings and corrections

| Finding | Concrete defect | Correction and regression |
|---|---|---|
| F1 · P1 | Cleanup could observe an empty producer slot before a dispatched child registered. | `PrivateOperation` closes dispatch acquisition on storage release and follows detached registration. The registration barrier and real wait-failure regressions retain capacity until child and writers settle. |
| F2 · P1 | Cancelling an inventory waiter could discard a directory removed from the resumable scan state. | A detached owner runs each bounded inventory batch; waiter cancellation loses only its receiver. Legacy-entry and marker-read barriers exercise cancellation. |
| F3 · P1 | Fallible cold adoption after map installation could leave a healthy rendition without a driver. | Driver installation completes before conservative adoption; unmatched or unreadable identity remains reserved. |
| F4 · P2 | Removed cold files could remain charged indefinitely; adoption racing initial inventory could duplicate claims. | Bounded revalidation releases only proved missing path claims; exact-key, inode/length-matched transfer recognizes media already owned by a live ordinary rendition. |
| F5 · P2 | Recovered owner-count saturation permanently blocked a valid startup inventory. | A deferred directory retains scan ownership and admission stays closed until cleanup frees a slot. The scan then resumes. |
| F6 · P1 | Startup cleanup with no in-memory rendition skipped durable quality dependencies. | Every exact-key cleanup checks durable dependencies before plan deletion or unlink, including reconstructed owners. |
| F7 · P1 | A stale owner could delete a replacement's Store plan before detecting its different marker. | Validate the exact marker/directory incarnation before Store deletion under the key gate. The regression preserves the replacement marker, file and real plan with no in-memory rendition. |

Secondary corrections recognize temporary/torn private markers and unreadable
marker metadata before generic adoption or encoded deletion. Storage release
uses an available runtime handle; its persisted marker survives shutdown.
Unavailable inode identity cannot authorize deduction or missing-marker cleanup.
The exact private plan row is retired after settlement and dependency clearance;
retained reacquisition validates its full manifest against the newly resolved
canonical rendition and does not require that old nonce-key row.

## Final verdict and evidence

The same independent reviewer approved the corrected tree at `4b8ae6f02`
with no remaining implementation findings. The pinned Rust 1.97.1 focused
suite passed 21 tests, including copy and encoded foreground GET progress,
real child-wait retries plus unlink failure and independent cleanup, forced
expired retained collection, cancellation and restart inventory, unreadable
real durable dependencies, and replacement marker/file/Store-plan survival.
The tracked hook passed formatting, workspace all-target Clippy with denied
warnings, catalog lint and all 77 served JavaScript syntax checks.

[PR #947](http://forge.lan:3000/noirr/plurx/pulls/947) carries the corrective
repair to main and preserves the named regression anchors. The required
main gate remains separate from this review approval. The user requested
watching and repairing the remaining broad units after merge. Native host
font contracts and a stalled executable-fixture run are rerun on the full
workflow's pinned Linux/Rust 1.97.1/Jellyfin FFmpeg 8 surface; no failed test
is hidden or waived. Deployment and
physical Apple TV acceptance remain unclaimed.

## Ownership census reconciliation

The same reviewer confirmed nine exact census totals against a source-only
archive of base `079960dae`. Six catalog counts were already stale on that
base: finite regression owners added by `142d9f4e07` and `b38387a133` in
`vod/tests/chunk_03.rs`. Their sleep children are kill-on-drop, job-attached
and registered in `ProducerSlot` with the background permit; their writers
settle and the tests await confirmed reap. The extra gate and five timers
are bounded fixture observations, with no inherited production owner.

The repair itself adds two existing-key gate calls, one cancellation-independent
inventory batch, one exact-owner cleanup task plus four fixture tasks, one
release-triggered runtime maintenance spawn, three production deadline bounds
plus nine fixture observations, two collected foreground FFmpeg fixtures, and
three reap/wait observations. These are the F1–F7 owners already reviewed.
The census patterns, scopes and exact equality assertions remain unchanged.
The failed preflight receipts are retained as runs 4523 and 4525; their
bookkeeping corrections do not waive either the gate or a runtime test.

After main advanced to `1088d7529`, the same reviewer reconciled the combined
tree against actual current-main source. Base counts were 30 gates, 5
cancellation-independent owners, 902 namespaced spawns, 63 method spawns,
1,622 time constructors, 245 command constructors, 733 launch methods,
320 lifecycle methods and 101 kill-on-drop calls. The combined counts are
32, 6, 907, 64, 1,634, 247, 734, 323 and 101 respectively. These are the
same reviewed repair deltas. Current-main reasons, patterns, scopes and
strict equality checks are preserved.
