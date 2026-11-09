# Playback start — review and execution history

**Status:** live historical record through 2026-11-08. Current lane ownership,
merge state and acceptance belong to the [implementation plan](PLAYBACK-START-PLAN.md).

Moved verbatim from the plan on 2026-11-09 to keep its active contract within
the documentation size budget. Historical open gaps remain open until the
plan records their disposition; this move grants no qualification.

## 7. Reviews applied

**Fable (adversarial, 2026-11-07):** B1 D1 unbuildable on continuous families → rebuilt on M6 · B2 C1 targeted rows that cannot exist → cross-node claim · B3/B4 C3 uniqueness and `reopen` → corrected (Astra later showed the uniqueness index is superseded) · B5 D2b already done → deleted · B6 A1 growing plan/partial artifact/attestation → design items · S1–S12 pacing, twin SQL, `waiting` input, digest advertisement, capacity split, placement arms, anchors, web-only recorder, §2 gaps, census/wire/schema hazards → applied.

**Astra (independent, 2026-11-07):** 1 merged plan at `ae3e06eaf` was the pre-review text → revision 3 lands the reviewed text and §6 names the revision · 2 C1 starvation moved to execution → one decision through claim/hash/submit/produce, shared-claim eligibility/takeover, cross-node proof tests · 3 C3 superseded uniqueness, one-for-one migration holds depth flat, unreachable worker ≠ retired digest → runnable vs backlog accounting, fresh-worker digest via internal activity, mixed/missing/two-reconciler tests · 4 A1 cold resume, row extraction, checkpointing, cross-ingress, EVENT→VOD, long GOP, HEVC eligibility → items (i)–(viii) · 5 B1 verify at first servability incl. seeks, versioned contract, dual-readable provenance, quarantine per title → rewritten; lands with A1 · 6 D1 first-frame trigger, cancellation, mechanism changes Paul's ruling → trigger/cancel added; **mechanism pending Paul (§0.3)**; Developer row until measured per platform · 7 D2 remote-start ownership → durable identity, one owner, cross-ingress lookup, reclamation, both deadline orders · 8 D3 fence and cacheable unit → revisions carried, descriptor verified, ≤ 2 mutations, capability-fact cache · 9 H2 ahead of writers, migrations on both stores → Phase 1 first · should-fix A1/A2, A3/H1, C2, D2, E1, F1, G1, R1 → applied in each lane · dependency graph and §2 ownership → §4 · ownership/wire checks per lane → §3 preamble · evidence table → acceptance lines and the lane-ledger rule below.

**Evidence rule (Astra):** every lane ledger entry records process-counter deltas over a stated interval and build (resets preserved), replicated gauges once, attempt counts with cold/warm, route, device and failed/abandoned attempts alongside TTFF; for the ≤ 5 s target, the share of eligible attempts reaching first frame inside the 5 s bucket plus successful-start p90. The 2026-11-07 fleet samples are baselines; no after-deploy reading exists yet.

## 8. Execution log

| Date | Lane | Model / session | Action |
|---|---|---|---|
| 2026-11-07 | plan | claude-fable-5-1 / 01BCWLA2SJPLAmibeaz9L5xb | Rev 1 written from the review; rulings 1–4 recorded. |
| 2026-11-07 | plan | claude-fable-5-1 / 01BCWLA2SJPLAmibeaz9L5xb | Fleet state read (queue wedged at cap; release-as-fence); Fable review applied (rev 2, not what #1401 merged — bridge propagation). |
| 2026-11-07 | plan | claude-fable-5-1 / 01BCWLA2SJPLAmibeaz9L5xb | Astra review applied; rev 3 is the first revision build agents may claim. |
| 2026-11-07 | plan | claude-fable-5-1 / 01BCWLA2SJPLAmibeaz9L5xb | Ruling 3 mechanism settled (M6); Phase 1 build contracts written (QUEUE, CREATE). |
| 2026-11-07 | H2, C2, C3, C1, D2, R1, H1, G1, D1 | claude-opus-5-5 / 01BCWL9xwmpSjitHqZMTRAht | Phase 1 claimed. Lanes integrate on `effort/playback-start`; per Paul (2026-11-07) one adversarial review and one fast-lane run on effort → main rather than per lane. H2/C2/C3 built; H1 built by a Sonnet sub-agent; C1, D2, R1 by Opus sub-agents. |
| 2026-11-08 | D2 (review 1 S4) | claude-opus-5-5 / 01BCWL9xwmpSjitHqZMTRAht | Open gap, not fixed: a remote start whose reply is lost on both the ask and the one re-ask (`start_remote_recovering_lost_reply`) moves to the next candidate under the same incarnation while the first worker may still hold a session for it. That first producer runs until its confirmation watcher sees another owner's activation of the incarnation (≤ 500 ms after it lands) or, when no activation comes, to `REMOTE_ACTIVATION_CONFIRMATION_WINDOW`. Closing it needs the transport to tell "never delivered" from "delivered, reply lost" (`PeerTransportError` does not) so only the first moves on. Pinned by `media_sessions::tests::a_dropped_remote_start_reply_takes_the_session_the_worker_already_holds`. |
| 2026-11-08 | H2, C2, C3, H1, R1, D2, C1, G1 | claude-opus-5-5 / 01BCWL9xwmpSjitHqZMTRAht | Landed on the effort: #1424, #1427, #1428, #1431, #1433, #1434. Fable review 1 (landed lanes) → fixes #1436, #1440; Fable review 2 (C1, G1) → fixes #1438. Combined effort head clippy-clean; paperwork vs main 16/16. D1 building; then D1 review, one fast-lane run, effort → main. |
| 2026-11-08 | D1 (review 3 S7) | claude-opus-5-5 / 01BCWL9xwmpSjitHqZMTRAht | Open gap: remote-owned ordinary sessions never upgrade (the create registers the intent only where the ingress owns the route). Follow-up: carry the intent on the remote start dispatch and trigger from the owner actor's presented phase. |
