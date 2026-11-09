# Shared-library playback — implementation and merge status

**Status:** built · **Updated:** 2026-11-09 · **Owner:** Codex/root

PR #1472, `claude/shared-library-standalone-fixes`, carries the standalone
repairs and relay budget, retry and abandonment work. Its live gate and
landing state are recorded on the PR. The [root-cause analysis](SHARED-LIBRARY-PLAYBACK-RCA.md)
owns the design and accepted decisions; [playback-start](PLAYBACK-START-PLAN.md)
owns A1.

## Delivered scope

The batch repairs NULL-principal consumers, committed membership bootstrap,
RPC attribution, Source admission, poster retries and verified snapshots,
owned abandonment, and startup clocks and retries across the relay. Main
`30f4343ba` is integrated, retaining the canonical route policy, native startup,
HDR and queued-analysis custody repairs from #1475–#1478.

S5's asynchronous subtitle preparation needs Source custody and remains
deferred. S1–S3, S8 and S9 remain A1 work. Native Apple/Android startup headers
require a client release. RX-ring operations, deployment and physical
playback acceptance remain separate.

## Fixture correction

The native fixture replaced AppState's store after constructing its
JobManager, leaving that manager attached to an empty SQLite store. Its
pre-session WebVTT Source registration therefore found no matching file.
`f23c6d8d1` constructs AppState and store-bound managers from the selected
Hiqlite store together. The replacement TranscodeManager retains actual
Source identity and the same jobs. Production custody predicates remain intact.

## Qualification evidence

| Surface | Evidence |
|---|---|
| Compiler | Pinned Rust 1.97.1 formatting and daemon/core all-target Clippy passed on `b4fc494ce`; production vendor checks are retained. Current-base `78317af96` formatting and daemon/core all-target Clippy also passed. |
| Source and relay | Five real-Source cases passed together, including both abandonment orderings, same-entry retries and discovery/Local clock preservation. The encoded/native fixture passed four subsequent runs, including the original preceding-fixture order. The final `78317af96` daemon batch passed 3/3: encoded/native playback, snapshot decision and typed busy response. |
| Additional regressions | The final restored SQL/dump/bootstrap batch passed 3/3 with `hiqlite-store`. Actor-caption and locate passed 2/2; earlier voter-route/fallback checks are retained. Earlier unaffected artwork/receiver controls and 27 core checks are retained. |
| Historical failures | All 60 old full-run failures were compared by exact name: 17 also failed baseline, 37 passed both comparison runs, five had later focused passes, and the remaining actor-caption case now passes unchanged. This is retained evidence, not a claim that the full suite was rerun. |
| Paperwork | All 16 checks passed on the final integrated source and delivery ledger; the ready-PR lane validates promotion metadata. Regression references remain in the PR body and must accompany the landing message. |
| Final review | Independent Codex review of `28bc0aacc` found one P1 and one P2; both are fixed below. The earlier owner reported an Opus review without retaining its report. |
| Counterfactuals | Retained role/capacity reversals and HEVC strict-child control are executed evidence. Five nonshipping reversals failed at the intended SQL, dump, bootstrap, retirement and discovery assertions. The strengthened snapshot regression executes the production cache/fetch decision and verifies that the old always-fetch operation fails; its final positive run passed. |
| Promotion | Require the current-head fast lane to pass; its receipt and landing commit are recorded on PR #1472. |

The first ready-PR preflight stopped on whole-file Hiqlite authority pins.
All five mapped authority bodies are byte-identical to main; reversing only
the four nullable dump-row declarations reconstructs the entire base file.
Five reviewed source hashes were refreshed. Guards, model pins and counts
remain unchanged; both failed mapping checks then passed.

One combined-run encoded/native Start returned 503 after successful
pre-session WebVTT. It did not recur in six subsequent runs, including standalone, original-order
and current-base qualification. Its cause remains unattributed; the failed receipt
and bounded test diagnostics are retained. No playback workaround was added
for that observation.

The task-specific compiler/test allowance is at most 24 GiB, four CPUs and
no swap on the existing build host. Repository build policy is unchanged.
Receipts and isolated workspaces remain on the external agent drive.

## Final review dispositions

The independent review examined `28bc0aacc` against `8ab61a779` plus the
pinned asset diagnostic. The reviewer ran no tests or builds.

| Finding | Root cause and correction |
|---|---|
| P1 · Ready result abandoned in a retry gap | Readiness ended abandonment before an accepted Start writer claimed the result. `824e19044` preserves the existing rejoin deadline through readiness and serializes response claim against abandonment under the same mutex. Ordinary End settles the Source. |
| P2 · Discovery restarted the peer clock | The clock was constructed after discovery, and Local fall-through parsed the unchanged allowance again. `63a92adf3` constructs it once at ingress and carries it into the Local handler. Only the discovery answer waiter is cancelled; accepted drivers retain filesystem and Start ownership. |
