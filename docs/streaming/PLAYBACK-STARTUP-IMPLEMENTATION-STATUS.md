# Playback startup, Resume and native callback implementation status

**Status:** Implemented; focused Rust, C and mobile qualification passed. Main
CI and landing are recorded in [PR #1366](http://forge.lan:3000/noirr/plurx/pulls/1366).
Runtime cold-start acceptance remains open. **Owner:** Codex/root · **Branch:**
`codex/playback-startup-fixes` · **Base:** `bd213c819` · **Updated:** 2026-11-03

Companion to the [canonical startup proposal](COLD-PLAYBACK-STARTUP-RCA-AND-PROPOSAL.md),
[AC3 retest RCA](COLD-PLAYBACK-AC3-RETEST-RCA.md), [native callback RCA](NATIVE-CALLBACK-DELIVERY-RCA.md)
and [Fable dispositions](NATIVE-CALLBACK-FABLE-REVIEW.md). This is the delivery
ledger for the user-authorized batched repair. One main PR contains the work;
the maintainer owns deployment. Startup within five seconds remains a runtime
acceptance target, not a measured result for this candidate.

Astra's follow-up review closed all seven findings against source commit
`bd3bfffba2feacc22ee85dfd323f3e15c6539b6d`. The private
`ASTRA-REVIEW-COMMIT-BINDING.json` binds reviewed diff SHA-256
`531f1000fc09e8a5f2b2cda0888b602f8dc2134fe3de49cd87e3ab16758c186a` to
that commit. Exact activation IDs, durable intents and receipt-scoped locks
cover acknowledgement loss without serializing unrelated starts. Verified
pause credit excludes Store RPC waits and unknown pressure. Accepted pending
companion selection acquires source admission before init discovery;
`control_required` and `quality_required` remain independent ownership facts.
The [Astra dispositions](PLAYBACK-STARTUP-ASTRA-DISPOSITIONS.md) record the
source changes and qualification boundaries.

Qualification record through implementation candidate `9205cb735`:

- All 86 focused Rust cases have passing results: 34 core, 2 decoder selection
  and 50 daemon cases. This includes the explicitly executed packaged-parser
  qualification. Passing cases were retained; only failed, incomplete or new
  cases ran again. Receipts preserve each test's source and binary identity.
- All four parser regressions pass after the future-storage correction on
  `ad2180342`. The final SDR fixture and same-directory journal replay cases
  pass on `9205cb735`; earlier passing cases were preserved.
- Renderer and complete-title C controls pass on matching source hashes.
  The focused Apple model test passes (1/1) and Android model class passes
  (10/10), with matching input hashes and no skipped cases.
- Pinned Linux all-target Clippy passes on `ad2180342`; the affected daemon
  check passes again on `9205cb735`. Focused binaries compile. Normal Mac
  commit hooks and the unchanged web TypeScript baseline pass.
- The main fast lane owns browser regression execution and the affected CI
  surfaces. The PR records its live result, exact candidate and landing;
  focused test passes alone do not authorize merge.

Narrow test-phase corrections follow the reviewed source: `91745417d` fixes
Linux fixture imports; `580ae8f3f` canonicalizes typed receipt identities,
parameterizes bounded replay queries and corrects fixture policy/schema;
`ad2180342` heap-pins the collector and platform futures before nested owner
scopes and asserts the VOD protocol's idempotent Replay behavior. `9205cb735` supplies
explicit SDR transfer metadata and clears replay receipts from the same
journal directory that was enumerated. Parser
tracing identified a 5,915,304-byte collection future and an 11,830,744-byte
spawn frame. Heap placement removes that stack expansion while preserving
existing ownership, cancellation and reap. The test stack stays at the
repository's documented 8 MiB convention.

## 1. Work lanes

| Order | Lane | State | Owner |
|---:|---|---|---|
| 1 | Per-element callback multiplexer and native discontinuity renewal; preserve Resume/seek proof and scope renewal evidence to native-src. | Source review closed; browser regressions and main gate are recorded in PR #1366. | Codex/root |
| 2 | Typed pending-family retry under the original deadline. | Source review closed; browser regressions and main gate are recorded in PR #1366. | Sol |
| 3 | One shared heavy-source predicate for decode and filter planning, including cropped UHD. | Source review closed; focused Rust passes. Main-gate result is recorded in PR #1366. | Codex/root |
| 4 | Resume through the retained-incumbent owner. | Source review closed; browser regressions and main gate are recorded in PR #1366. | Codex/root |
| 5 | Native clock and buffer ownership across attachment/source execution. | Source review closed; browser regressions and main gate are recorded in PR #1366. | Codex/root |
| 6 | Continuous companion deferral plus client binding and versioned-family-update consumption. | Source review closed; focused Rust passes. Accepted parent selection and independent ownership are covered; browser and main-gate results are recorded in PR #1366. | Sol |
| 7 | M3 short-window first publication while retaining steady continuity reserve. | Source review closed; focused Rust passes. Cold-start timing requires deployment and measurement. | Codex/root |
| 8 | M1 shared source ledger and sparse subtitle-read cancellation with bounded fairness. | Source review closed; focused Rust and C controls pass across custody, finite service, sparse cancellation, durable hashes and verified quota holds. Runtime cold acceptance remains open. | Sol / Codex/root |

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
base does not qualify the moved candidate. The snapshot above records completed focused evidence and the remaining
qualification work.

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
| Pinned Mac workspace Clippy, all targets, formatting, catalog and served JS syntax | Normal commit hooks pass through implementation candidate `9205cb735`. |
| Web TypeScript baseline | Pass; no increase to the existing baseline. |
| Linux all-target Clippy and focused test binaries | Pass on `ad2180342`; affected daemon lint and decoder/daemon binaries pass on `9205cb735`. |
| Renderer and complete-title C controls | Pass; renderer, scanner and input-header hashes match current source. |
| Focused Apple / Android model tests | Pass: 1 Apple and 10 Android; input hashes match current source. |
| Focused Rust regressions | 86/86 pass, retaining successful cases and retrying failures only; per-test source and binary receipts retained. |
| Astra source review | Seven findings source-closed on `bd3bfffba`; narrow test-phase corrections are identified above. |
| Browser regressions, main fast lane and landing | Live candidate, checks, owner and merge state: [PR #1366](http://forge.lan:3000/noirr/plurx/pulls/1366). |

Private compiler logs, archive checksums, test receipts and ownership handoffs
live in the workflow's durable evidence directory, outside its disposable build
cache. Source-only checks and focused synthetic controls do not establish
runtime cold-start acceptance. The maintainer owns deployment after merge.

Final reader inventory corrections require owned input at every production
processed-runtime entry point, including the first prepared window. Standalone
and paired title scans now use the same private AVIO despite the SDK's disabled
network protocols; complete-title scanning retains its existing owner limits
rather than inheriting the renderer's window cap or socket deadline. Their
C compiler and regression controls pass on matching input hashes.

Private C input sockets follow the existing child owner's cancellation and
work deadlines for both renderer and scanner. A separate socket wall deadline
would keep counting while verified source-service holds suspend active work;
it is removed. The scanner IO control checks the actual kernel socket policy.
