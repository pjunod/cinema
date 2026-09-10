# Weekly review remediation — what is fixed, what remains, and why

**Status:** implementation in progress · **Baseline:**
`4cef0da740c797364023284520adad8a93b172cd` · **Branch:**
`codex/weekly-review-remediation` · **Updated:** 2026-09-09

Companion to [SECURITY.md](../SECURITY.md) (the current trust boundaries),
[PLAYBACK.md](../PLAYBACK.md) (the current delivery contract), and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the one-review main
promotion process) — this page answers *which September 3–9 review findings
were selected for the bounded correction pass, what evidence exists, and what
remains a separately scoped capability*.

The baseline is the exact frozen review revision and was clean in a fresh clone.
The repository-pinned Rust 1.97.1 compiler completed
`cargo check -p plurxd --all-targets` before the first source edit. Full runtime
tests are deliberately deferred to the main-bound fast lane; the separately
scheduled sweep owns broader unit and hardware evidence.

## Outcome — small corrections ship without hiding the residual risk

This pass fixes the concrete client-state, bounded-work, peer-acknowledgement,
worker-launch, and audit-reporting defects that fit the existing architecture.
It does not claim to deliver fleet identity, portable disaster recovery, local
administrator recovery, or a new playback architecture. Those are named below
because an honest backlog is part of the security boundary.

Developer settings remain the authority for optional runtime behavior. Their
requirements and observed status are advice: missing qualification evidence
does not turn an explicitly enabled feature back off. Authentication,
authorization, valid input, protocol correctness, and actual resource
availability still decide whether an individual operation succeeds.

## Finding ledger — every review item has one disposition

| ID | Baseline disposition | This pass | Evidence or next action |
|---|---|---|---|
| F01 | Present | Selected | Web, Apple, and Android clear locally on every outcome after attempting bounded server revocation; late completion cannot clear a newer login. |
| F02 | Present by design | Deferred | Quorum-authoritative ordinary authorization needs a versioned migration away from the all-member cache-exclusion protocol. |
| F03 | Present | Selected | Revocation Begin and End acknowledgements must use exact request-and-response authentication. |
| F04 | Present, documented trust boundary | Deferred | Per-node identity, enrollment, rotation, removal, and mixed-version activation are one fleet capability. |
| F05 | Present, documented gap | Deferred | Portable export plus catastrophic restore must be designed and drilled together. |
| F06 | Present, documented gap | Deferred | Daemon-owned, OS-authenticated local recovery requires a platform control-channel design. |
| F07 | Present | Selected narrowly | Live TV FFmpeg receives an explicit minimal environment and no unnecessary inherited standard descriptors. Full OS/GPU sandboxing remains open. |
| F08 | Present | Selected | Argon2 verification and creation move behind bounded admission and blocking workers; accepted password size is consistent. |
| F09 | Present | Selected | Web handoff copies current mute, volume, rate, and play/pause intent at the commit boundary and restores them on rollback. |
| F10 | Needs physical-device reproduction | Investigate only | Preserve the Apple incumbent and existing bounded rollback; record the missing final-alignment device trace rather than guessing at AVPlayer timing. |
| F11 | Present | Selected | Android copies current play/pause, volume, speed, and pitch and performs final commit-boundary alignment. |
| F12 | Present | Selected | Guide refresh receives one total deadline, shared admission, stale-publication fencing, and cancellation-aware publication. |
| F13 | Present | Selected | Startup measurement receives bounded output, wall time, artifact size/time, and kill/reap ownership. |
| F14 | Present in fuzz/scheduled reporting | Selected | Scanner verdicts become deterministic commands; Forgejo reporting becomes best-effort and cannot change the verdict. |
| F15 | Mixed | Selected narrowly | Pin mutable action inputs and stop persisting checkout credentials; registry TLS, signing, and attestation remain a release-infrastructure project. |
| A01 | Broad structural improvement | Deferred | Extract only seams required by the selected fixes. |
| A02 | Present across selected paths | Selected narrowly | Reuse small admission and process-lifetime helpers without a framework rewrite. |
| A03 | Unmeasured performance hypothesis | Deferred | Measure warm decode-fact identity cost before changing the cache. |
| A04 | Partially built | Selected | Commit-boundary intent is distinct from the earlier prepared snapshot. |
| A05 | Operational gap | Deferred | Preserve physical fencing; durable ambiguous-start reconciliation is separate. |
| A06 | Partial | Selected where deterministic | Add contract regressions now; retain native physical-device cases as open evidence. |
| A07 | Stale claims exist | Selected | Update current security, operations, client, and validation claims with each behavior change. |
| A08 | Broad lifecycle improvement | Partial | Bound and redact selected paths; lifecycle inventory, rotation, offline limits, and release identity remain open. |

## Delivery ledger — coherent commits, one main-bound review

| Change | State | Evidence |
|---|---|---|
| Baseline and traceability | **In progress** | Exact reviewed SHA recorded; pinned compile-only baseline green. |
| Client intent and sign-out | **Queued** | Source corrections and focused regressions not yet committed. |
| Server work bounds | **Queued** | Hash, guide, and startup process contracts not yet committed. |
| Peer, worker, and audit hardening | **Queued** | Narrow compatible changes not yet committed. |
| Documentation and bookkeeping | **In progress** | This page and the docs index are maintained with the branch. |
| Main promotion | **Not started** | Draft PR, exactly one adversarial review, findings addressed, then the fast lane. |

## Decisions to review later — implementation may continue without blocking

1. **Password input is capped by encoded byte length.** Argon2 cost does not
   depend materially on password length, but accepting an unbounded request
   still wastes memory and makes every client contract ambiguous. The selected
   cap will preserve ordinary password-manager output while refusing abuse.
2. **Guide refresh uses one short operation budget.** Partial HDHomeRun bulk
   data is more useful than multiplying 64 per-page timeouts into minutes of
   stale work. Extension pages stop when the shared budget expires.
3. **Apple timing remains evidence-led.** The incumbent and rollback path are
   already retained. Without a physical reproduction, changing asynchronous
   AVPlayer seek semantics would be a speculative regression, so this pass
   records the exact missing trace instead.
4. **Release identity is not papered over.** Digest-pinned inputs and detached
   audit verdicts fit this pass. Registry PKI, signing identity, verification at
   rollout, and emergency rollback are one later end-to-end capability.

## Non-goals — what this branch does not claim

- It does not weaken authentication, cluster quorum, or Live TV physical
  fencing to make a test pass.
- It does not add a readiness or certification gate to feature enablement.
- It does not call replication a backup or claim an untested restore path.
- It does not claim simulator or source inspection as physical playback
  qualification.
- It does not run the broad unit suite before the fast lane; compilation is the
  iteration loop, and the separate sweep owns post-merge breadth.
