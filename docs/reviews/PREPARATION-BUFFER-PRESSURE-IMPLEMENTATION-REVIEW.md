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
font contracts and a stalled executable-fixture run are rerun on the pinned
Linux/FFmpeg 6 surface; no failed test is hidden or waived. Deployment and
physical Apple TV acceptance remain unclaimed.
