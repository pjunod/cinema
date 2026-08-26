# Playback control protocol M2 web adversarial review

This records the pre-test adversarial review of implementation PR #605. The
review evaluated the browser shadow reporter, client-log join, lifecycle
fencing, documentation, and proposed tests against
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md) and
[PLAYBACK-CONTROL-PROTOCOL-M2-WEB.md](PLAYBACK-CONTROL-PROTOCOL-M2-WEB.md).

## Verdict

Four adversarial passes requested changes. They found no P0 issue, eight P1
correctness or lifecycle issues, and six P2 test, observability, or inventory
issues. Every finding was accepted and fixed before any unit test ran. The
fifth pass approved exact commit `964dc2c3` with no actionable finding.

M2 remains a passive shadow slice. It sends explicit browser state and joins
that evidence to legacy recovery logs, but accepts only `action: none` and
removes no legacy recovery owner or watchdog.

## Findings and remediation

| Severity | Finding | Resolution |
| --- | --- | --- |
| P1 | Typed `425`, `429`, and `503` responses were not retried under their bounded server hints, while a fenced `409 owner_changed` could not safely establish a fresh generation/epoch sequence space. | Added exact-body retries for temporary typed failures and a resnapshotting owner-change reset that starts sequence one with capabilities under the returned generation and epoch. |
| P1 | A late completion from a stopped reporter could mutate state belonging to its successor. | Stopped reporters discard late success and error completions. The UI callback also verifies both the player and reporter identities. |
| P1 | An accepted `demand: end` armed another cadence exchange, allowing a completed controller to renew forever. | Accepted end is terminal. It stops the reporter and never schedules another cadence exchange. |
| P1 | Persistent stall and fatal recovery could replace the player before the cadence reporter sent the evidence that triggered the legacy mutation. | Recovery sites capture an immediate trigger context before mutation. The real reporter now carries its bounded observation through `contextFor`, and the server validates and persists it separately as client-asserted trigger evidence. |
| P2 | The first test set did not exercise typed retry, owner change, terminal end, stopped-controller completion, or shipped adapter behavior. | Added focused reporter and shipped-adapter coverage, then expanded it through every later race found by review. |
| P2 | The exact watchdog inventory omitted the early-ended/truncated-stream retry path. | Added `handleEnded` and `endedTries`, their present mutation, and the reason they remain until active control actions exist. |
| P2 | A raw, unverified client generation could enter persisted recovery telemetry. | The server accepts only a UUID, hashes it to bounded correlation, validates every enum/value, and labels accepted and trigger objects as client assertions rather than authoritative joins. |
| P1 | Persistent waits labeled healthy-runway decoder stalls as starvation, and fatal hls.js paths did not preserve network, manifest, media, or decoder cause. | Recovery callbacks now provide bounded event-specific observations: supply is starved; decode is failed; network/manifest retain honest decoder readiness; media/decoder failures report failed state. Specific HLS evidence takes precedence over a generic media-element error. |
| P2 | Replaying ended media could call `HTMLMediaElement.play()` behind a reporter already stopped by accepted end. | Replay uses the ordinary playback-open path at position zero, producing a fresh delivery session, control generation, and sequence space. |
| P1 | Typed trigger evidence was reduced out of `Reporter.contextFor` and out of the server's client-log schema. | Added a bounded observation to immediate and accepted reporter contexts and to the separately validated server persistence object. Tests use a real reporter for both paths. |
| P1 | A replay guard stored on `PLAYER` disappeared when `play()` replaced that object during an asynchronous open. | Full opens now use a generation-aware gate independent of `PLAYER`; decision and session continuations require the same current attempt, and stale returned sessions are released. |
| P2 | The global one-shot attempt reason was consumed after a decision await and could be stolen by another quality, subtitle, or replay open. | The reason is captured and cleared synchronously at `play()` entry. Interleaved reversed-completion coverage proves isolation. |
| P1 | Replacing the player with a global replay promise created a different latch: a never-settling superseded fetch could prevent replay on an unrelated later title. | Replay claims are keyed to the open-gate generation, not promise lifetime. Close or any newer full open immediately makes the claim inactive; identity-based completion cannot clear a successor claim. |
| P2 | Source-pattern assertions did not behaviorally prove reversed decision/session completion, close-during-await, or stale-resource release. | Tests execute the shipped open gate with deferred decisions and sessions, reversed completion, close invalidation, replay supersession, stale-session release, and interleaved attempt reasons. Both transcode and copy-HLS production paths call the tested resource fence. |

## Preserved properties

- The reporter is default-off and starts only from a server-advertised M1
  bootstrap.
- Only one exchange is in flight; a bounded newest snapshot replaces queued
  work, and an unacknowledged request retries with the same sequence and body.
- Client cancellation and replacement are control flow, not playback failure
  evidence.
- The client transport deadline cannot seek, pause, reopen, downgrade, or
  otherwise mutate playback.
- Control generations are UUID-validated and hashed before log persistence;
  free-form values do not become metric labels.
- Replay and overlapping full opens cannot attach or retain a stale returned
  session.

## Review chronology

1. The first pass reviewed the initial reporter and requested seven changes.
   Commit `3726c650` remediated them.
2. The second pass found typed recovery evidence and terminal replay gaps.
   Commit `69a10673` remediated them.
3. The third pass found trigger-context loss, observation precedence, replay
   replacement, and attempt-reason races. Commit `55f8ea03` remediated them.
4. The fourth pass found the never-settling replay latch and insufficient
   behavioral fencing coverage. Commit `964dc2c3` remediated them.
5. The final pass approved exact commit `964dc2c3`. It confirmed the shipped
   open gate, stale-session release, typed immediate and accepted observation,
   replay supersession, reversed-completion coverage, and absence of new
   correctness, security/privacy, performance, documentation, or test-contract
   findings.

The reviewer ran no tests and modified no files. No unit test was run before
the final approval.

## Validation order

The remaining gate order for PR #605 is:

1. run the focused web reporter and server client-log tests;
2. run the repository unit and merge gates;
3. repair and repeat until green;
4. mark the PR ready only after local validation and required CI pass; and
5. merge only while GitHub reports the exact reviewed/tested head mergeable.

The required order was preserved. After final adversarial approval, the
focused shipped web reporter suite and server client-log persistence test both
passed. The first complete web gate found two stale source-harness assumptions:
one expected the persistent stall call to end before its new trigger argument,
and one evaluated `closePlayer`/`attachSession` without their new passive
lifecycle dependencies. The harnesses were updated to require the new contract.
The complete web gate then passed playback policy/control, reader and page
contracts, inline-JavaScript syntax, and contrast validation.

The fast workspace Rust lane also passed from end to end, including 754 core
tests and 999 server tests with three intentionally ignored, plus the remaining
workspace package, SQLite contract, and documentation tests.
