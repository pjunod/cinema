# cluster/ — replication, membership, and recovery

The index of `docs/cluster/`: every document in this folder, the question it
answers, and its status. Routed from [the docs index](../README.md), which
defines the status words. Add a row here, in the same commit, when you add
a document to this folder.

Durable cluster work: [implementation](DURABLE-WORK-QUEUE-IMPLEMENTATION.md) ·
[build status](DURABLE-WORK-QUEUE-STATUS.md) — **built**; [Activity and subtitle-throughput follow-up](http://forge.lan:3000/noirr/plurx/pulls/588).

Phase 4 and everything under it: the clustering transition, the performance
and media-pool work built on top, and the diagnoses of specific replicated
failures.

| File | Answers | |
|---|---|---|
| [RUST-ADMISSION-REFINEMENT-STATUS.md](RUST-ADMISSION-REFINEMENT-STATUS.md) | Production admission/start refinement pilot: scope, ownership, verifier feasibility, proof obligations and current delivery state. | built |
| [WORKER-REFINEMENT-STATUS.md](WORKER-REFINEMENT-STATUS.md) | Built source correspondence and two full workers; PR receipt tracks qualification, ownership and landing. | built |
| [OPERATION-PROGRESS-IMPLEMENTATION.md](OPERATION-PROGRESS-IMPLEMENTATION.md) | Source wait-map, repeated-operation model, acceptance and delivery plan. | built |
| [OPERATION-PROGRESS-STATUS.md](OPERATION-PROGRESS-STATUS.md) | Qualified progress scope, evidence, review and live PR delivery link. | built |
| [FORMAL-VERIFICATION-PILOT-PROPOSAL.md](FORMAL-VERIFICATION-PILOT-PROPOSAL.md) | The bounded TLA+/TLC pilot contract, source anchors, verification limits, milestones and acceptance bar. | done |
| [FORMAL-VERIFICATION-PILOT-REVIEW.md](FORMAL-VERIFICATION-PILOT-REVIEW.md) | Fable's design and final implementation findings, scope limits, and coordinator dispositions. | done |
| [FORMAL-VERIFICATION-PILOT-STATUS.md](FORMAL-VERIFICATION-PILOT-STATUS.md) | Qualified finite results, runtime evidence, costs, limits, owner and delivery link. | done |
| [RAFT-FAULT-TESTING-IMPLEMENTATION.md](RAFT-FAULT-TESTING-IMPLEMENTATION.md) | The bounded proposal and build contract for three repeatable Raft fault scenarios, generated WAL regressions, and replay evidence, preserving the current CI/CD process and adding no product gates. | open |
| [RAFT-FAULT-TESTING-REVIEW.md](RAFT-FAULT-TESTING-REVIEW.md) | Independent adversarial findings on the fault-testing proposal and the author's amendments for protocol timing, partition evidence, complete data comparison, and child cleanup. | done |
| [STATUS-HISTORY.md](STATUS-HISTORY.md) | Status history: the `STATUS.md` sections for this folder's efforts, moved verbatim on 2026-09-24 and dated by when each was first recorded; `STATUS.md` keeps a one-line row per section pointing here. | done |
| [CLUSTER-WORK-CAPACITY-IMPLEMENTATION.md](CLUSTER-WORK-CAPACITY-IMPLEMENTATION.md) | Activity tabs, exact worker/job visibility, and reviewed throughput improvements with playback and storage constraints. | open |
| [CLOCK-SKEW-GUARD-DESIGN.md](CLOCK-SKEW-GUARD-DESIGN.md) | Accepted K-06 design and original review/gate receipt; runtime remains open through separately owned measurement and enforcement handoffs. | open |
| [CLOCK-SKEW-MEASUREMENT-IMPLEMENTATION.md](CLOCK-SKEW-MEASUREMENT-IMPLEMENTATION.md) | K-06 measurement release, on `main` since 2026-10-04 (#793): exact signed timing, continuity/filter/roster facts and identified observations; the identified fleet receipt is still owed. | open |
| [CLOCK-MEASUREMENT-OWNED-LAB.md](CLOCK-MEASUREMENT-OWNED-LAB.md) | Prepared owned four-host launcher, bounded idle/network-load collector, source-only artifact boundary and exact-ID cleanup; no build, launch or physical acceptance claimed. | open |
| [CLOCK-SKEW-ENFORCEMENT-IMPLEMENTATION.md](CLOCK-SKEW-ENFORCEMENT-IMPLEMENTATION.md) | K-06 enforcement release, on `main` since 2026-10-04 (#793) behind a Settings → Cluster switch (default off; in Developer until 2026-10-10) the design does not allow: authority boundaries, safe target removal, readiness and controlled acceptance; measurement evidence still owed. | open |
| [STORE-CONTRACT-COVERAGE-AND-PLACEHOLDER-VALIDATION.md](STORE-CONTRACT-COVERAGE-AND-PLACEHOLDER-VALIDATION.md) | Implementation plan from the 2026-09-20 architecture review: why ten store modules never run the three-voter lane, and what a `?N` validator plus a discarded-result audit would catch. | done |
| [HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md](HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md) | Hiqlite fork ownership/dependency plan and current upstream inventory: eleven generic rows, two accepted mechanisms and nine pending dispositions; embedding-pool fleet acceptance remains separate. | open |
| [CLUSTER-BACKUP-AND-RESTORE.md](CLUSTER-BACKUP-AND-RESTORE.md) | Implementation plan from the 2026-09-20 architecture review: a portable, verifiable backup of an activated cluster and a restore procedure that resets identity and fences the old cluster, with RPO/RTO measured and drills for one-node and majority loss. | open |
| [RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT.md](RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT.md) | Accepted default-preserving snapshot path and October 7 real production-data build/publication receipt; future cadence tuning and restore/release qualification stay separate. | done |
| [REPLICATED-WRITE-RATE-HYGIENE.md](REPLICATED-WRITE-RATE-HYGIENE.md) | Implementation plan from the 2026-09-20 architecture review: stopping the watched-outbox's unconditional 1 Hz raft write on every voter and the takeover loop's idle consistent reads while keeping the atomic replicated claim. | open |
| [REPLICATED-WRITE-RATE-HYGIENE-M0.md](REPLICATED-WRITE-RATE-HYGIENE-M0.md) | K-03 M0 readout: the idle fleet's replicated proposals, authority reads and snapshot builds per voter over the one 12-hour-qualifying window, and which writers dominate the replicated log. | done |
| [REPLICATED-WRITE-RATE-HYGIENE-II.md](REPLICATED-WRITE-RATE-HYGIENE-II.md) | K-10 plan: classification/offline loops, local hints and bounded forced checks; October 13 replaces the twelve-hour requirement with deployment-aware bounded qualification, without a fleet freeze. | open |
| [BOUNDED-REPLICA-READS-ROLLOUT.md](BOUNDED-REPLICA-READS-ROLLOUT.md) | Implementation plan from the 2026-09-20 architecture review: flipping `bounded_replica_reads` as a consistency-policy change with per-route coverage, fewer consistent reads per request, and no ordinary-auth cache. | open |
| [SQLITE-READ-PATH-AND-QUERY-PLANS.md](SQLITE-READ-PATH-AND-QUERY-PLANS.md) | Implementation plan from the 2026-09-20 architecture review: moving genuinely read-only store methods off the writer mutex, splitting `user_for_token`, `EXPLAIN QUERY PLAN`-justified indexes, keyset pagination, and the corrected search predicate. | open |
| [CLUSTERING-PLAN.md](CLUSTERING-PLAN.md) | From one plurxd node to Phase 4, milestone by milestone. | open |
| [CLUSTERING-PLAN-REVIEW.md](CLUSTERING-PLAN-REVIEW.md) | The right first slice, on top of eight contracts that did not hold. | done |
| [PHASE3-SPIKE.md](PHASE3-SPIKE.md) | The decision gate: two HA risks spiked, one answer. | done |
| [CLUSTER-PERFORMANCE-PLAN.md](CLUSTER-PERFORMANCE-PLAN.md) | Turning replicated correctness into useful capacity. | open |
| [CLUSTER-MEDIA-POOL-PLAN.md](CLUSTER-MEDIA-POOL-PLAN.md) | Making every node improve playback rather than just hold state. | built |
| [CLUSTER-PANEL-FOLLOWUPS.md](CLUSTER-PANEL-FOLLOWUPS.md) | Live status, a real module seam, one credential surface. | open |
| [CLUSTER_TRANSPORT_RECOVERY_IMPLEMENTATION.md](CLUSTER_TRANSPORT_RECOVERY_IMPLEMENTATION.md) | Fixing buffered writes and bounding recovery end to end. | open |
| [CLUSTER_TRANSPORT_RECOVERY_STATUS.md](CLUSTER_TRANSPORT_RECOVERY_STATUS.md) | Live implementation status of that effort. | open |
| [CLUSTER-TRANSPORT-RECOVERY-POST-MERGE-HANDOFF.md](CLUSTER-TRANSPORT-RECOVERY-POST-MERGE-HANDOFF.md) | Finishing qualification and rollout after the merge. | open |
| [TRANSPORT-RECOVERY-CI-STATUS.md](TRANSPORT-RECOVERY-CI-STATUS.md) | Which independent-role CI milestone is built, reviewed, and proved. | open |
| [TRANSPORT-RECOVERY-CI-IMPLEMENTATION-HANDOFF.md](TRANSPORT-RECOVERY-CI-IMPLEMENTATION-HANDOFF.md) | Build order for independent voter and learner execution, durable diagnostics, phase timings, and separately reported CI roles. | open |
| [TRANSPORT-RECOVERY-RESOURCE-BASELINE.md](TRANSPORT-RECOVERY-RESOURCE-BASELINE.md) | Why that lane never passed under the per-cycle ceiling: the measurement. | done |
| [TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md](TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md) | The campaign-floor contract that replaced it, and the two options not taken. | built |
| [CLUSTER_PAGE_LATENCY_REVIEW.md](CLUSTER_PAGE_LATENCY_REVIEW.md) | Evidence for why Home, Activity and Settings were slow in a cluster. | done |
| [CLUSTER_PAGE_LATENCY_FIX_PLAN.md](CLUSTER_PAGE_LATENCY_FIX_PLAN.md) · [review](CLUSTER_PAGE_LATENCY_FIX_PLAN_REVIEW.md) | Restore quorum truth, then unblock first paint. | built |
| [WAL_GENERATION_REPAIR_PLAN.md](WAL_GENERATION_REPAIR_PLAN.md) · [review](WAL_GENERATION_REPAIR_PLAN_REVIEW.md) | Making snapshot compaction invalidate every stale reader cache. | built |
| [MEMBERSHIP-CREDENTIAL-SPLIT-PLAN.md](MEMBERSHIP-CREDENTIAL-SPLIT-PLAN.md) | Making admission an authorization decision. | open |
| [LAB3_SNAPSHOT_CATCHUP_DIAGNOSIS.md](LAB3_SNAPSHOT_CATCHUP_DIAGNOSIS.md) | Why a live learner waited fifteen idle minutes. | open |
| [ACTIVITY_PEER_READ_FIX_PLAN.md](ACTIVITY_PEER_READ_FIX_PLAN.md) | Coalescing peer-read bursts and reporting failures truthfully. | built |
| [DVR-ACTIVITY-ZERO-GENERATION-STATUS.md](DVR-ACTIVITY-ZERO-GENERATION-STATUS.md) | Why remote Activity rejected a healthy recorder's initial serving epoch, and the repair's review and promotion state. | open |
