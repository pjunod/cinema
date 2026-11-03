# Playback startup, Resume and native callback implementation status

**Status:** Astra source review closed all seven findings on `bd3bfffba2feacc22ee85dfd323f3e15c6539b6d`; focused tests and CI remain pending · **Owner:** Codex/root · **Branch:**
`codex/playback-startup-fixes` · **Base:** `bd213c819` · **Updated:**
2026-11-03

Companion to the [canonical startup proposal](COLD-PLAYBACK-STARTUP-RCA-AND-PROPOSAL.md),
[AC3 retest RCA](COLD-PLAYBACK-AC3-RETEST-RCA.md), [native callback RCA](NATIVE-CALLBACK-DELIVERY-RCA.md)
and [Fable dispositions](NATIVE-CALLBACK-FABLE-REVIEW.md). This is the delivery
ledger for the user-authorized batched repair. Commits follow the smallest-first
work order below; one main PR opens only after the batch is complete and
reviewable. The branch was advanced to current main on 2026-11-02; its runner
maintenance changes do not overlap this implementation. Astra reviewed frozen
candidate `2bc1e54e1` and requested changes: four P1 findings cover custody
recovery, pause revisions, exact continuous-parent telemetry and optional source
admission; three P2 findings cover probe active time, pending family discovery
and post-conclusion Play rejection. Sol owns the Rust corrections; Codex owns
the browser corrections. All seven findings now have source changes and
unrun regressions. Exact activation IDs and durable intents cover lost
activation acknowledgements; receipt-scoped locks preserve unrelated starts.
Probe pause credit excludes Store RPC waits and unknown pressure. A selected
companion retains normal source batching; optional work has revocable bounds.
Production/core compilation passed on frozen pre-findings candidate
`2bc1e54e1`; it does not qualify the changed source. The learner compiler slot
is prepared with pinned Rust 1.97.1, a warm task cache, four CPUs and an 18 GiB
memory cap. Renderer and complete-title C controls pass on the reviewed source hashes.
Focused Rust, Apple/Android and main fast-lane qualification remain pending. Final source review is recorded below;
CI and merge qualification remain pending.

Astra's follow-up review closed all seven findings against exact source commit
`bd3bfffba2feacc22ee85dfd323f3e15c6539b6d`. The private
`ASTRA-REVIEW-COMMIT-BINDING.json` records that reviewed diff SHA-256
`531f1000fc09e8a5f2b2cda0888b602f8dc2134fe3de49cd87e3ab16758c186a` maps to
that commit. A selected pending companion now acquires controlled source
admission from the exact accepted parent selection before init discovery.
`control_required` and `quality_required` remain independent facts; the
persistent companion is Live while either owner requires it. Init verification
still gates family publication and Prepare. This closes the source-review
finding, while focused regression execution, current Linux compilation, CI, and
merge qualification remain pending.

The normal pinned Mac commit hook passed on
`bd3bfffba2feacc22ee85dfd323f3e15c6539b6d`. The earlier Linux production/core
compile remains bound to frozen pre-findings `2bc1e54e1`. Current Linux
all-target Clippy is running on the eligible learner with 18 GiB and four
CPUs; C controls are being prepared. No behavioral regression has run.

The [Astra dispositions](PLAYBACK-STARTUP-ASTRA-DISPOSITIONS.md) distinguish
source changes from findings that are reviewed and qualified.

Qualification update: both C controls, the Apple model test (1) and Android
model tests (10) pass on matching source hashes. The first focused Rust run
retained 35 passes, found 11 confirmed failures, and left 39 cases incomplete
after a test-thread stack abort. Receipt identity encoding and bounded replay
queries are corrected; a decoder fixture now exercises its intended enforced
policy. Retries use the repository's documented 8 MiB test-stack convention.
Only failed, incomplete and newly added cases will execute again.

Further targeted qualification retained 68 Rust passes. Parser diagnostics
identified a 5,915,304-byte collection future and an 11,830,744-byte spawn frame
in the test build. The collection and platform futures are now heap-pinned
before nested owner scopes; ownership and cancellation stay with the existing
collector. A VOD fixture now asserts the protocol's idempotent Replay behavior,
and the browser options type matches its existing optional defaults. Failed,
incomplete and new cases remain the only pending Rust executions.

## 1. Work lanes

| Order | Lane | State | Owner |
|---:|---|---|---|
| 1 | Per-element callback multiplexer and native discontinuity renewal; preserve Resume/seek proof and scope renewal evidence to native-src. | Astra source review closed; focused regressions and fast-lane qualification remain pending. | Codex/root |
| 2 | Typed pending-family retry under the original deadline. | Astra source review closed; focused regressions and fast-lane qualification remain pending. | Sol |
| 3 | One shared heavy-source predicate for decode and filter planning, including cropped UHD. | Astra source review closed; focused regressions and fast-lane qualification remain pending. | Codex/root |
| 4 | Resume through the retained-incumbent owner. | Astra source review closed; focused regressions and fast-lane qualification remain pending. | Codex/root |
| 5 | Native clock and buffer ownership across attachment/source execution. | Astra source review closed; focused regressions and fast-lane qualification remain pending. | Codex/root |
| 6 | Continuous companion deferral plus client binding and versioned-family-update consumption. | Astra source review closed, including accepted parent selection before discovery and independent `control_required` / `quality_required` ownership; focused regressions remain pending. | Sol |
| 7 | M3 short-window first publication while retaining steady continuity reserve. | Astra source review closed; focused regressions and fast-lane qualification remain pending. | Codex/root |
| 8 | M1 shared source ledger and sparse subtitle-read cancellation with bounded fairness. | Astra source review closed across custody, finite service, sparse cancellation, durable hashes and verified quota holds, including isolated probes; focused regressions remain pending. | Sol / Codex/root |

The user's one-batched-main-PR instruction supersedes Fable's separate-PR
sequence. Keep the order above in reviewable commits within the batch. The
[WebKit report draft](NATIVE-CALLBACK-WEBKIT-REPORT-DRAFT.md) is prepared and unfiled; external submission was not
authorized. It does not block the application repair.

## 2. Review and validation sequence

Keep the user-visible root cause and acceptance limits in the linked RCA. Do
not report implementation from a design change. Complete the batch, inspect the
exact main merge candidate, then perform the requested adversarial review.
Only after that review, run the final fast lane and each requested test once on
that exact candidate. Retry failed tests only. The fast-lane result and each
actual test result must name the candidate revision; a result against an older
base does not qualify the moved candidate. No tests have run for this status
entry.

Open the owned PR only when the batched main candidate is complete and
reviewable. Record its URL/number and exact head here when created. Keep PR
status explicit; a local branch is not an open PR, and a passing local check is
not a merge qualification.

## 3. Integration findings addressed before review

- Source sharing needs a reachable producer bank above its unchanged steady
  reserve. Producer-only banking now targets that high watermark; advertised
  windows retain their existing bounds. Physical group-stop proof precedes a
  quiescent projection.
- Direct and progressive service requires original input byte accounting.
  Compressed output bytes cannot stand in for source reads. A private held-source
  range bridge is being integrated for FFmpeg paths; finite domain service
  allocation must cover background readers too.
- Foreground and background source custody cannot disappear on lease expiry
  before physical read/reap settlement. Cleanup remains charged and visible.
- Linux process control requires Linux compilation. A pinned 1.97.1 container
  route is verified through the repository's capped build wrapper. Mac checks
  alone are not Linux evidence.

## 4. M3 protocol decision

The production bootstrap draft covers one immutable advertised HLS target
(16 media seconds at 1x), rather than the old 48 media seconds. The separate
writer duration gate is removed and completed cuts replenish during reserve
banking. The steady reserve, retention promises and capacity checks stay intact.
The one-complete-segment arm remains an explicit test-lab control: a 2-second
snapshot under a 16-second reload contract cannot establish native continuity.
A smaller production target requires validated bounded random-access cuts or
an HLS parts transport; changing the startup constant cannot supply either.

The <=5 s cold outcome is still a runtime acceptance requirement, not a measured
result for this candidate. Source arbitration and actual read mediation must be
qualified with this publication policy before claiming that outcome. Protocol
references: [RFC8216 reload rules](https://www.rfc-editor.org/rfc/rfc8216.html#section-6.3.4)
and [Apple HLS authoring](https://developer.apple.com/streaming/hls-authoring-specification-for-apple-devices.html).

The existing `scripts/playback-startup-lab` now names the one-complete 2/2,
4/4, 4/8 and 16/16 reload-window controls on its generated 2 s closed-GOP fixture.
Other below-two-target pairs remain rejected. The old a/b acceptance decision
is unchanged; admission of an experiment is not native/daemon qualification.
`tests/operations/test_playback_startup_short_arms.py` covers that boundary;
it has not run yet.

M1 now also distinguishes explicit film telemetry from transport, and versions
prepared-artifact source classification as managed-resource v4 (SQLite v123).
Models, text inference and prepared artifact hydration/verification/derivatives
have no original-media source charge. Legacy extraction-capable workers retain
that charge. Existing physical reservations survive the classification upgrade.

## 5. Final integration closure

All original-source paths found in the reader inventory now use the common
custody and byte-service boundary, including library probes, offline production
and processed-DV renderer/audio inputs. The renderer uses exact finite ranges
without speculative read-ahead and retains its cumulative 128 MiB bound.
Finite quota pacing uses the existing verified group-pause owner; actual reads
and Store failures continue spending active time. The native observer's original deadline now concludes `observation_unavailable`
when fresh decode/clock progress conflicts with silent callbacks. It withdraws
only exact owner/generation/intent observational claims and offers a nonblocking
Try again / Close notice. It neither pauses playback nor certifies presentation
or seek landing; genuine freezes and authoritative failures retain their owners.
Web syntax and Swift model type-checks passed; regression execution remains
pending. The final source review closed all seven findings on the bound commit
above. Compiler checks execute no unit tests.

The strict parser keeps its existing isolation and execution identity. Linux
source reads pass through its existing notification supervisor, WASI reads
through the held host capability, and Windows through the private source
bridge. Quota holds exclude active time only after aggregate physical stop
proof; source-service refusal is not cached as decoder evidence. These new
boundaries require Linux and packaged-parser qualification after review.

## 6. Compiler receipts and remaining merge work

| Check | Current result |
|---|---|
| Pinned Mac workspace Clippy, all targets, formatting, catalog and served JS syntax | Passed through the normal commit hook on `bd3bfffba2feacc22ee85dfd323f3e15c6539b6d`. |
| Web TypeScript baseline | Passed without raising the baseline. |
| Pinned Linux production daemon and core all-target compilation | Passed on frozen pre-findings candidate `2bc1e54e1`; it does not qualify the changed source. |
| Full Linux renderer SDK plus CPU harness compilation | Passed; renderer/header hashes match the current code. No renderer regression has executed. |
| Current Linux all-target Clippy | Running on the eligible learner with 18 GiB and four CPUs. |
| Full Linux daemon test-target compilation and C controls | Pending; the eligible builder is ready and C controls are being prepared. |
| Astra source review | Closed all seven findings on `bd3bfffba2feacc22ee85dfd323f3e15c6539b6d`; the private binding receipt ties the reviewed diff to this commit. |
| Focused behavioral regressions, main fast lane and merge qualification | Pending; no behavioral tests have run. |

Private compiler logs, archive checksums and source ownership handoffs live in
the workflow's durable evidence directory, outside its disposable build cache.
The full daemon test target is an environment capacity gap, not a test pass.
A source-only production check does not replace that target or runtime startup
acceptance. The operator deployment remains separate from this repair PR.

Final reader inventory corrections require owned input at every production
processed-runtime entry point, including the first prepared window. Standalone
and paired title scans now use the same private AVIO despite the SDK's disabled
network protocols; complete-title scanning retains its existing owner limits
rather than inheriting the renderer's window cap or socket deadline. Their
new compiler and regression receipts remain pending.

Private C input sockets follow the existing child owner's cancellation and
work deadlines for both renderer and scanner. A separate socket wall deadline
would keep counting while verified source-service holds suspend active work;
it is removed. The scanner IO control checks the actual kernel socket policy.
