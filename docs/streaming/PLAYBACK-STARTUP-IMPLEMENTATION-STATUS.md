# Playback startup, Resume and native callback implementation status

**Status:** implementation committed; platform lint and final review pending · **Owner:** Codex/root · **Branch:**
`codex/playback-startup-fixes` · **Base:** `bd213c819` · **Updated:**
2026-11-02

Companion to the [canonical startup proposal](COLD-PLAYBACK-STARTUP-RCA-AND-PROPOSAL.md),
[AC3 retest RCA](COLD-PLAYBACK-AC3-RETEST-RCA.md), [native callback RCA](NATIVE-CALLBACK-DELIVERY-RCA.md)
and [Fable dispositions](NATIVE-CALLBACK-FABLE-REVIEW.md). This is the delivery
ledger for the user-authorized batched repair. Commits follow the smallest-first
work order below; one main PR opens only after the batch is complete and
reviewable. The branch was advanced to current main on 2026-11-02; its runner
maintenance changes do not overlap this implementation. Implementation is committed through `7ec51905a` and frozen for integration
checks. Mac workspace hooks and web typing pass. Linux production/core
compilation and the renderer SDK build pass; Linux lint cleanup is finishing.
No unit suite, PR or merge qualification is recorded yet.

## 1. Work lanes

| Order | Lane | State | Owner |
|---:|---|---|---|
| 1 | Per-element callback multiplexer and native discontinuity renewal; preserve Resume/seek proof and scope renewal evidence to native-src. | Drafted; mux, four consumer migrations and scoped native observation-unavailable surface added. Review and validation pending. | Codex/root |
| 2 | Typed pending-family retry under the original deadline. | Drafted; final review and validation pending. | Sol |
| 3 | One shared heavy-source predicate for decode and filter planning, including cropped UHD. | Drafted; shared predicate and regressions added. Tests not run; final review pending. | Codex/root |
| 4 | Resume through the retained-incumbent owner. | Drafted; awaiting final review and validation. | Codex/root |
| 5 | Native clock and buffer ownership across attachment/source execution. | Drafted; delayed native source fences clock, buffers and events. Validation pending. | Codex/root |
| 6 | Continuous companion deferral plus client binding and versioned-family-update consumption. | Drafted; final review and validation pending. | Sol |
| 7 | M3 short-window first publication while retaining steady continuity reserve. | Drafted; awaiting final review and validation. | Codex/root |
| 8 | M1 shared source ledger and sparse subtitle-read cancellation with bounded fairness. | Drafted: shared physical custody, finite service, sparse cancellation, durable hashes and verified quota holds cover the reader inventory, including isolated probes. Compiler and review pending. | Sol / Codex/root |

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
Web syntax and Swift model type-checks passed; regression execution and final
review remain pending. Compiler checks execute no unit tests.

The strict parser keeps its existing isolation and execution identity. Linux
source reads pass through its existing notification supervisor, WASI reads
through the held host capability, and Windows through the private source
bridge. Quota holds exclude active time only after aggregate physical stop
proof; source-service refusal is not cached as decoder evidence. These new
boundaries require Linux and packaged-parser qualification after review.

## 6. Compiler receipts and remaining merge work

| Check | Current result |
|---|---|
| Pinned Mac workspace Clippy, all targets, formatting, catalog and served JS syntax | Passed through the normal commit hooks. |
| Web TypeScript baseline | Passed without raising the baseline. |
| Pinned Linux production daemon and core all-target compilation | Passed on `7ec51905a`; three lint-only corrections are being committed. |
| Full Linux renderer SDK plus CPU harness compilation | Passed; renderer/header hashes match the current code. No renderer regression has executed. |
| Full Linux daemon test-target compilation | Exceeds the voter compiler's 8 GiB limit, including with incremental compilation disabled. A larger eligible lab/learner slot remains needed. The voter cap stays intact. |
| Adversarial review, focused regressions and main fast lane | Pending, in that order. |

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
