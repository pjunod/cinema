# Weekly review remediation — what is fixed, what remains, and why

**Status:** done — main promotion completed in PR #225 · **Baseline:**
`4cef0da740c797364023284520adad8a93b172cd` · **Promotion base:**
`200574162ad78786de49d06aa4b1127df1a206bf` · **Branch:**
`codex/weekly-review-remediation` · **Updated:** 2026-09-10

Companion to [SECURITY.md](../SECURITY.md) (the current trust boundaries),
[PLAYBACK.md](../PLAYBACK.md) (the current delivery contract), and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the one-review main
promotion process) — this page answers *which September 3–9 review findings
were selected for the bounded correction pass, what evidence exists, and what
remains a separately scoped capability*.

The baseline is the exact frozen review revision and was clean in a fresh clone.
Before promotion, current `main` was merged at the promotion base recorded
above and the resulting tree was verified again.
The repository-pinned Rust 1.97.1 compiler completed
`cargo check -p plurxd --all-targets` before the first source edit and again on
the completed source. Full runtime tests are deliberately deferred until after
the fast lane; the separately scheduled sweep owns broader unit and hardware
evidence.

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
| Baseline and traceability | **Complete** | Exact reviewed SHA recorded in `5fd8c2eb`; pinned compile-only baseline green. |
| Client intent and sign-out | **Complete** | Web, Apple, and Android corrections plus focused regressions committed in `b1335e32`; Android Kotlin and both Apple targets compile. |
| Server work bounds | **Complete** | Hash, guide, Live TV environment, and startup process bounds committed in `319b2ad7`; Rust 1.97.1 format, all-target check, and Clippy are green. |
| Peer, worker, and audit hardening | **Complete** | Authenticated acknowledgements, immutable action inputs, checkout credential removal, and deterministic audit commands committed in `2b4d32fd`; workflow YAML parses. |
| Documentation and bookkeeping | **Complete** | Security, operations, both client guides, this ledger, the docs index, and functionality ownership describe the changed contract. |
| Main promotion | **Pending** | The one adversarial review found nine issues; all are addressed below. Rust 1.97.1 format/check/Clippy, Android and Apple compilation, workflow parsing, client syntax, mobile-version validation, and diff hygiene are green; the fast lane is next. |

The compile-only verification also includes the shipped page's inline
JavaScript syntax check. No unit, integration, browser, simulator, emulator,
recovery, playback, package, or smoke suite ran before the fast lane.

## Apple device trace — the evidence needed before changing the handoff

Source inspection found one initial exact seek during preparation. At commit,
the Apple path chooses the later of that requested position and the incumbent's
current film time, then waits for an `AVPlayerItemVideoOutput` pixel buffer at
that boundary within the existing 250 ms tolerance. It does not issue a second
seek at commit. That is not evidence of a visible defect by itself: AVPlayer's
asynchronous seek and decoded-frame timing need a physical reproduction.

A qualifying trace must record, on one monotonic wall clock:

1. the initial requested film position and the successor item's time when that
   seek completes;
2. the incumbent film position, play/pause state, and rate immediately before
   replacement;
3. the successor's first displayed item time and derived film position; and
4. the first displayed frame's wall-clock delta from replacement.

If that trace shows a repeat or skip outside 250 ms, the corrective change is a
commit-boundary reseek with an explicit completion/timeout contract. Until
then, the retained incumbent and bounded rollback remain the safer behavior.

## Adversarial review — one pass, every finding addressed

The required single review found nine concrete defects. The author corrected
and verifies them without requesting a second review:

1. revocation response proof now admits every committed member, including a
   learner, while other voter-only response proofs keep their narrower role;
2. the wire change advertises `cache_admin_revocation_v4`, and mixed v3/v4
   rosters fail closed through the existing heartbeat capability mechanism;
3. private ref refreshes use a run-scoped token header for one command after
   checkout removes its credential;
4. probe deadlines cover leader wait and both pipe drains, and normal leader
   exit also kills any descendant left in the owned process group;
5. Android's final seek must publish its discontinuity and then re-prove ready
   state, drift, generation, and runway before the surface moves;
6. guide settings and shutdown cancel active work, while an already-running
   blocking parser retains refresh admission until it finishes and its stale
   answer is discarded;
7. only success or the logout endpoint's `401`, never a generic `403`, counts
   as confirmed revocation;
8. Android clears the captured local bearer in a non-cancellable section and
   launches optional package deletion independently afterward; and
9. decoder identity opens before metadata inspection and counts bytes while
   hashing, closing replacement and growth races around the 512 MiB ceiling.

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
