# Architecture close-out — integrated implementation and remaining acceptance

**Status:** open — integration delivered; routing correction implemented in
this candidate, scoped acceptance remains · **Written:** 2026-10-07 ·
**Audited base:** `9023815cb997394de34c10c3dc574f169b6ff011`

Companion to the [canonical workboard](ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md)
and [architecture review](ARCHITECTURE-REVIEW-2026-09-20.md). The workboard
remains the only 49-row status ledger. This document records the final source
reconciliation and groups the actual remaining actions; it does not replace
the plans or their dated evidence.

## Integration is delivered; qualification is narrower

The effort landed through #793 (`4cfd1bdd1`, 2026-10-04), and #814
(`5dce66e16`, 2026-10-05) landed the close-out repairs. Current source carries
the shared removal transition on all three removal paths, K-06 learner and
lease-owner bounds and admission-budget repairs, D-01 prepare-time matcher
timeout withdrawal, obsolete A-05 policy-runner removal and L-02 single-owner
cleanup. These are implemented work, not pending draft tasks.

[K-07 store contracts](../cluster/STORE-CONTRACT-COVERAGE-AND-PLACEHOLDER-VALIDATION.md)
and [P-04 documentation reconciliation](../ci/ARCHITECTURE-DOC-RECONCILIATION.md)
have their recorded completion receipts. Other integrated milestones retain
their named acceptance limits. A merge title, test count, absent metric or
historical fleet point cannot establish physical playback or fleet acceptance.
This reconciliation does not certify all 49 plans or current client suites,
and does not independently establish a full promoted-tree qualification receipt.

## The confirmed routing repair is in progress

[S-08 interlace routing](../streaming/INTERLACE-IN-THE-MEDIA-CONTRACT.md)
has one confirmed source gap: request construction can route from catalog
scan flags before resolved decode facts, while retry, descriptor, log and
qualification consumers can retain the request pipeline. A progressive heavy
HEVC source mislabeled interlaced can therefore lose an otherwise qualified
GPU path.

The correction in this source candidate retains the existing codec/range/heavy
selection and newer subtitle composition, lets resolved facts decide scan
routing, and makes post-resolution consumers use the resolved plan. Confirmed
interlaced or unavailable verdicts must keep CPU `bwdif`; rejected hardware
deinterlace is not added. Offers retain the ordinary unresolved-plan fallback
without falsely naming a GPU graph or adding an eligibility refusal. Positions,
thread budgets, audio identity and other execution options are preserved.

The new manager regressions in
`crates/plurxd/src/transcode/tests/chunk_01.rs` cover the real candidate-to-plan
verdict distinction and the actual returned offer graph/fallback:
`manager_routes_misflagged_heavy_source_from_resolved_interlace_verdict` and
`media_offer_reports_resolved_pipeline_without_gating_unresolved_candidate`.
Their first execution is reserved for the reviewed PR's fast lane; compilation
is not a passing runtime result. Final candidate execution and landing are
recorded with the PR, not inferred from this base audit. Motion/display
observations remain separate acceptance.

## Conservative decisions retain the measured behavior

The coordinator records the following routine close-out choices for later
review. They are neither new owner rulings nor measurements:

1. Keep CPU deinterlace and current encoder-rate defaults. Hardware
   deinterlace and the proposed QSV rate flip failed their recorded bars;
   [rate-control evidence](../streaming/ENCODER-RATE-CONTROL-DEFAULTS.md)
   is not a mandate to change a default.
2. Do not start a new benchmark week. Use the existing narrow checks and
   retained receipts. [K-02 snapshot acceptance](../cluster/RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT.md)
   asks one production-data snapshot observation under its accepted scope;
   S-11's organic-use week is supplementary, not a prerequisite to repair.
3. Accept [P-01](../ci/RUST-TEST-EXECUTION-POLICY.md)'s failed runner timing
   attempts/source-only fallback and [C-05](../server/DETAIL-READS-AND-STORAGE-AVAILABILITY.md)'s
   unobserved M1-before-M2 backfill order as historical limitations. Do not
   invent the missing table or retroactive intermediate deployment receipt.
   Current policy and marker/availability observations retain their own bars.
4. Keep measured heap containment and decline larger D-01 buffer roles
   without evidence. Keep the existing clock advisory choice, bounded-read
   default and facade behavior during scoped acceptance. Optional hardening,
   switch graduation and release preparation are separate reviewable changes,
   not blockers requiring a new owner ruling before this reconciliation.

## Remaining work is grouped by the observation it needs

| Surface | Concrete remaining action and canonical contract |
|---|---|
| Fidelity and recovery | Original [S-11 codec/GPU](../streaming/CODEC-AND-GPU-QUALIFICATION.md) content, graph, throughput and device fidelity remains open. Scoped PQ/NAL proofs do not erase the failed sharp-edge cell or qualify skin/gamut, Dolby, 4K, HDR burn or physical displays. [A-04](../clients/NATIVE-ADAPTIVE-QUALITY-DESIGN.md)'s shaped-network matrix remains incomplete; qualify relevant current behavior under [display-aware](../streaming/DISPLAY-AWARE-AUTO-QUALITY-PLAN.md) and [continuous-quality](../playback-control/CONTINUOUS-QUALITY-BUILD.md) contracts, not removed native reducers. Preserve S-02's failed 2160p warm-up until causal evidence closes that exact cell. |
| Streaming and audio | Named deployed process/output, retirement, font, producer-progress, tone-map and source-cache observations remain in their workboard-linked S-01–S-07/S-13 plans. [S-09 audio](../streaming/AUDIO-RESOLVED-INDEPENDENTLY.md) needs applicable sink/listening evidence; exclusions remain scope. [S-10 master facts](../streaming/HONEST-MASTER-PLAYLIST.md) needs applicable AVPlayer/client acceptance with SDR CODECS still off. [S-12 reordered streams](../streaming/VOD-BFRAMES-TIMELINE-DESIGN.md) needs scoped physical/compression observations; normal default remains zero B-frames. |
| Cluster correctness and cost | [K-01 restore](../cluster/CLUSTER-BACKUP-AND-RESTORE.md) retains arm64 image smoke and authorized loss/recovery RPO/RTO drills. [K-03](../cluster/REPLICATED-WRITE-RATE-HYGIENE.md)/[K-10](../cluster/REPLICATED-WRITE-RATE-HYGIENE-II.md) share a source-bound attributed after-readout where inputs support both. [K-04](../cluster/BOUNDED-REPLICA-READS-ROLLOUT.md) retains cross-node watch/fallback, per-route read-cost and rolling-version observations. [K-06 measurement](../cluster/CLOCK-SKEW-MEASUREMENT-IMPLEMENTATION.md) and [enforcement](../cluster/CLOCK-SKEW-ENFORCEMENT-IMPLEMENTATION.md) retain identified uncertainty/cost and controlled disposable-lab response evidence; no step drill runs from this document. |
| Server delivery and telemetry | Existing C-01–C-04 plans retain bounded asset/EPUB, scan/provider, derivative/browser and expired-token recovery checks. [C-05](../server/DETAIL-READS-AND-STORAGE-AVAILABILITY.md) needs current marker convergence and detail/unavailable-storage observations. [C-06](../server/TELEMETRY-BACKPRESSURE.md) retains slow-sidecar playback/queue/RSS/recovery evidence; local capacities are not fleet RSS. [C-08](../server/OBSERVABILITY-BASELINE.md) retains actual-use scrape/hygiene and attributed play/seek/attempt/admission observations. |
| Physical clients | A-01–A-03 and D-01–D-03 retain HDMI/audio/session/lifecycle, large-library ordering/frame cost, reader/offline/real-backup and ART consumption observations. [Web seek](../streaming/WEB-SEEK-LATENCY-RCA-AND-FIX.md) evidence must follow current held-seek/VOD fixes; withdrawn Safari runway work stays withdrawn. W-02 retains browser/TV remote input acceptance. Missing hardware is evidence debt, not missing decomposition. |
| Live TV and subtitles | L-01 retains extended-guide scheduling/reminder and mixed-storage recording interruption observations; L-02 uses current multi-node ownership for leader restart/warm-start checks; L-03 retains capacity/backlog/recording-only/caption rendering observations. [K-09](../clients/SUBTITLE-CLUSTER-EXTRACTION-PLAN.md) requires its existing narrow cold/warm/off/slow checks, then the backfill sample; cold `startup_exhausted` plus Retry is the documented outcome. |
| Upstream coordination | [K-08](../cluster/HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md) retains two-thread semantic indexing/search observations and nine incomplete combined generic upstream rows. Partial upstream fixes are not complete patch equivalence. Reconcile existing submissions before public specimens or filings; do not drop local patches from related issue titles. |
| Discretionary deployment and release | [P-02](../ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md) retains busy child-priority/limit readback. Its deployment hardening proposal is a separate isolated-lab change, and overflow changes remain conditional. Packed profile C fulfills former split-debug work; five bounded fuzz campaigns have scoped acceptance. [P-03](../ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md) retains concrete release/tag/index/deployed-version work; the publication trigger is integrated. [C-07](../server/PLEX-FACADE-PAGING.md) retains observed route-cache decision work; declined paging is not missing implementation. |

## Declined and superseded scope does not become a new backlog

K-05 keyset/`next_up` changes, unsupported tokenizer substitution, unjustified
semantic feature/lane alternatives, C-07 paging and S-13 conditional
optimizations retain their recorded decisions. A-05's unused reducers are
removed; L-02's old single-owner routes are retired. P-03's text-contract
prune failed its own revert protocol and stays declined. Do not rebuild these
merely to satisfy an old unchecked milestone.

This document authorizes no runtime load, deployments, disruptive faults,
public filings or release tags. Remaining checks retain exact source/build,
date and observation scope; old failures and missing evidence remain visible.
