# Playback start — review history

**Status:** live historical record through 2026-11-08. Current lane ownership and acceptance belong to the [implementation plan](PLAYBACK-START-PLAN.md); execution history and landing state are in the [continuation ledger](PLAYBACK-START-STATUS.md).

Moved verbatim from the plan on 2026-11-09 to keep its active contract within
the documentation size budget. Historical open gaps remain open until the
plan records their disposition; this move grants no qualification.

## 7. Reviews applied

**Fable (adversarial, 2026-11-07):** B1 D1 unbuildable on continuous families → rebuilt on M6 · B2 C1 targeted rows that cannot exist → cross-node claim · B3/B4 C3 uniqueness and `reopen` → corrected (Astra later showed the uniqueness index is superseded) · B5 D2b already done → deleted · B6 A1 growing plan/partial artifact/attestation → design items · S1–S12 pacing, twin SQL, `waiting` input, digest advertisement, capacity split, placement arms, anchors, web-only recorder, §2 gaps, census/wire/schema hazards → applied.

**Astra (independent, 2026-11-07):** 1 merged plan at `ae3e06eaf` was the pre-review text → revision 3 lands the reviewed text and §6 names the revision · 2 C1 starvation moved to execution → one decision through claim/hash/submit/produce, shared-claim eligibility/takeover, cross-node proof tests · 3 C3 superseded uniqueness, one-for-one migration holds depth flat, unreachable worker ≠ retired digest → runnable vs backlog accounting, fresh-worker digest via internal activity, mixed/missing/two-reconciler tests · 4 A1 cold resume, row extraction, checkpointing, cross-ingress, EVENT→VOD, long GOP, HEVC eligibility → items (i)–(viii) · 5 B1 verify at first servability incl. seeks, versioned contract, dual-readable provenance, quarantine per title → rewritten; lands with A1 · 6 D1 first-frame trigger, cancellation, mechanism changes Paul's ruling → trigger/cancel added; **mechanism pending Paul (§0.3)**; Developer row until measured per platform · 7 D2 remote-start ownership → durable identity, one owner, cross-ingress lookup, reclamation, both deadline orders · 8 D3 fence and cacheable unit → revisions carried, descriptor verified, ≤ 2 mutations, capability-fact cache · 9 H2 ahead of writers, migrations on both stores → Phase 1 first · should-fix A1/A2, A3/H1, C2, D2, E1, F1, G1, R1 → applied in each lane · dependency graph and §2 ownership → §4 · ownership/wire checks per lane → §3 preamble · evidence table → acceptance lines and the lane-ledger rule below.

**Evidence rule (Astra):** every lane ledger entry records process-counter deltas over a stated interval and build (resets preserved), replicated gauges once, attempt counts with cold/warm, route, device and failed/abandoned attempts alongside TTFF; for the ≤ 5 s target, the share of eligible attempts reaching first frame inside the 5 s bucket plus successful-start p90. The 2026-11-07 fleet samples are baselines; no after-deploy reading exists yet.
