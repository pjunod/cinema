# HEVC sample-entry admission — software qualification receipt

**Status:** built — focused software evidence green; Forgejo promotion gate pending ·
**Recorded:** 2026-09-16 · **PR:** `#337` · **Base:**
`df3721320a8efe5967d95ce331cc30ccf6f0e3ea` · **Qualified code:**
`b4a6d59c`

Companion to the
[implementation ledger](../streaming/HEVC-SAMPLE-ENTRY-STATUS.md). This receipt
records the exact reviewed source and the one focused runtime-test window. The
final documentation and status commits change no runtime or test behavior;
Forgejo's `Main promotion gate` is the authoritative exact-head result for the
promotion candidate.

## Outcome

The candidate stores and recovers the first playable video's normalized sample
entry, validates an explicit progressive HEVC packaging constraint, selects
copy-video Remux for packaging-only mismatches, and prevents updated clients
from executing an incompatible progressive output. A constrained HEVC copy
session requires an explicit HLS transport claim before durable request
admission. The Developer settings surface reports rollout and evidence status
as advice only; no feature flag gates the behavior.

## Adversarial review

One read-only adversarial review inspected `origin/main...4f1efd6a` after all
server, web, and Apple implementation commits were assembled. It reported two
actionable findings:

1. Progressive packaging probes skipped failed rungs below the ceiling emitted
   on the wire, including Main10-only probe shapes that also emit Main.
2. An admitted progressive tag let a caller create copy-HLS without explicitly
   claiming the HLS transport.

Commit `1092acda` addressed both findings. Progressive evidence now covers
every profile and rung implied by the wire document, including PQ and holes
below the ceiling. Every constrained HEVC copy-HLS create and preparation path
now requires the exact `hls` transport before request admission. New negative
regressions cover the two review scenarios.

## Focused results

The repository-pinned compiler was `rustc 1.97.1 (8bab26f4f 2026-07-14)`.
Final green executions totalled **280 tests/checks**:

| Surface | Command | Result |
|---|---|---:|
| Shared decision | `cargo test -p plurx-core --locked --lib playback::tests::` | 59 passed |
| Probe parser | `cargo test -p plurx-core --locked --lib scan::probe::tests::` | 12 passed |
| SQLite + Hiqlite Store contract | `cargo test -p plurx-core --locked --features cluster-read-cost-validation,hiqlite-contract-tests --test store_contract video_codec_tag_round_trips_and_backfill_updates_are_exactly_fenced -- --exact --test-threads=1` | 1 passed across both backends |
| Daemon HEVC seams | `cargo test -p plurxd --locked --bin plurxd hevc` plus the three named output/serialization filters | 12 passed |
| Web playback policy | `node tests/playback/web-policy.test.js` | 157 passed |
| Developer settings | `node tests/web/settings-sections.test.js` | 26 passed |
| Routing inventory | `python3 -m unittest discover -s tests/validation -p test_playback_routing_inventory.py` | 4 passed |
| Documentation index | `python3 -m unittest discover -s tests/operations -p test_docs_index.py` | 4 passed |
| Apple capability/request contract | focused `xcodebuild ... test` with five `-only-testing` selectors | 5 passed |

The focused Apple selectors were:

- `testAppleCapsKeepGenericHDRSeparateFromDolbyVision`
- `testAppleCapsDocumentCoversTheProbeMatrixWithoutInventingAHeight`
- `testDeliveryRequiresHLSDecodesAndDefaultsForOldResponses`
- `testDecisionPostFallsBackOnlyForTheMixedFleetStatuses`
- `testSessionCreateCarriesTheCapabilitiesDocument`

Build 165 compiled through `make apple-build` for both generic iOS Simulator
and generic tvOS Simulator destinations before the focused iOS execution. A
simulator result is not physical playback evidence.

## Failures corrected inside the window

The first Store-contract execution found that the shared additive column
statement lacked the semicolon required before SQLite's migration-wrapper
`COMMIT`. Commit `b4a6d59c` corrected the statement; the same test then passed
against SQLite and the loopback three-voter Hiqlite fixture. Its first Hiqlite
retry was blocked by the filesystem/network sandbox before a test could run;
the approved loopback execution passed.

The first web run exposed three incomplete harnesses: the PQ negative used an
8-bit rung that cannot make a PQ claim, the Developer composition omitted its
new card dependency, and the isolated decision function omitted the shipped
selection-query dependency. The harnesses were corrected without weakening
the production assertions; both complete web files then passed.

The first Forgejo promotion run stopped in the historical-evidence preflight.
The five corrective implementation commits now have explicit durable evidence:
four `regressions.d` mappings and one Apple source-to-test anchor. Reproducing
the rest of preflight also found the module-wide task inventory still recorded
511 namespaced spawns before the finite HEVC backfill task was added. Its
ownership review now records 512. The complete 198-test validation contract,
357-test operations contract, player-input contract, and player DOM contract
then passed locally before the rerun head was pushed.

That rerun passed preflight, web syntax, Apple compile, and mobile-version
checks. Both native Rust and Windows cross-compile found the same omitted
`video_codec_tag` initializer in the Plex compatibility crate's `MediaFile`
test fixture. The fixture now states `None` explicitly; `make
effort-rust-check` and `make hiqlite-vendor-clippy` passed locally before the
next exact-head promotion attempt.

## Review findings F1–F10

| Finding | Disposition |
|---|---|
| F1 | Exact additive caps validation and null/empty/list semantics implemented. |
| F2 | Apple emits `hvc1` only with HEVC, preserves one caps snapshot, and decodes old plans. Android omission remains unchanged. |
| F3 | The actual progressive builder output drives `requires_hls`; incompatible progressive rescue is forbidden. |
| F4 | One first-playable-video parser owns the normalized stored fact. |
| F5 | SQLite/Hiqlite storage, import/publication, 256-row recovery, and stale-snapshot fencing are covered. |
| F6 | File-only, tier-complete, PQ-aware, per-label web evidence is separate from MSE. |
| F7 | Auto and Original use the shared packaging admission rule without redefining decoder support. |
| F8 | Invalid/non-null claims cannot enter the permissive legacy fallback; `[]` remains restrictive. |
| F9 | Decision, create, and preparation retain the transport constraint; refusal occurs before admission. |
| F10 | Routing catalogue, API/playback docs, status, focused tests, and this receipt are registered. |

The two follow-up safeguards are retained: a non-null field never downgrades
to a legacy request after refusal, and every serving node must enforce the
field before refreshed web or Apple clients are published.

## Honest limits and rollout

No real Safari, Chrome, iOS-device, or tvOS-device playback was executed.
Actual progressive probe answers, presented frames, HDR/Dolby Vision grade,
seek continuity, and two-minute playback remain pending physical acceptance.
The candidate base does not include the separate seek repair from PR `#336`.

Roll out enforcing servers first. Only after every serving node is current may
the web client refresh and Apple build 165 publish. Old clients omit the field
and retain the legacy limitation; old servers may silently ignore the additive
v2 field. No library mutation, media rewrite, deployment, TestFlight upload,
or App Store publication is part of this receipt.
