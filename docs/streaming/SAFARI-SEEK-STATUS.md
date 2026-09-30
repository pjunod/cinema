# Safari seek build — implementation and promotion status

**Status:** review fixes committed; post-review validation running · **Updated:** 2026-09-29 ·
**Starting base:** `1869871ce` · **Current main sync:** `38c917225` ·
**Branch:** `effort/safari-seek`

Companion to [SAFARI-SEEK-IMPLEMENTATION.md](SAFARI-SEEK-IMPLEMENTATION.md)
(the contract and acceptance cases) and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the merge rules). This
page records what is actually built and proved. A checked box means evidence
exists for the named milestone; it does not imply a fleet rollout.

## 1. Build state

| Milestone | State | Evidence and next step |
|---|---|---|
| M0 — browser fixture and stage diagnostics | In progress | Added bounded client route and settlement traces. The `scripts/playback-lab safari-seek-fixture` native Safari run and [four served-manifest reports](../evidence/safari-seek-native-2026-09-29.json) reproduce the clamp. The real-server fixture now forces native rolling HLS and refuses a capture without a served media playlist. Its physical run is pending. Server playback events include produced/published edges, accepted demand sequence and age. Exact request diagnostics now separate shared I/O refusal, local capacity refusal, eligibility, attestation, artifact construction, hydration, and local availability. |
| M1 — viewer preparation | Implemented; fixture proved | Both Store claims filter incompatible engines. The playback path joins an exact, 120-second waiter through analysis, build and targeted hydration; active session ownership refreshes it at most every 30 seconds, and terminal sessions retire their own interest. Analysis priority follows live waiters without changing claim fences or attempts. The source reader has a separate local pass guard so it cannot hold the artifact and delivery pass. Exact Store regressions passed on SQLite and three-voter Hiqlite, including remote hydration ownership. Physical first-frame latency remains unmeasured. |
| M2 — source I/O admission | Implemented; fixture proved | The fenced analysis claim reserves pre-artifact source reads by storage domain. Common job claims count those reservations and admit at most one reader without live interest across the two-slot domain; existing holders drain without revocation. Cancellation preserves the physical reservation through lease expiry instead of prematurely freeing a slot. Activity exposes up to 16 active shared reader leases with kind, priority, start and analysis phase timestamps, without source paths or ownership tokens; Store metrics count both kinds. Multi-node Store regressions passed on SQLite and three-voter Hiqlite; measured maintenance throughput remains unavailable. |
| M3 — reproducible media runtime | In progress | The runtime uses a digest-pinned Debian base, a dated Debian package snapshot, a versioned and SHA-256-verified Jellyfin 8.1.3 artifact, and the existing verified dovi_tool archive. The release workflow publishes the runtime by per-architecture digest and builds the app from that digest; the app record includes that digest, packages, source SHA and its own cache-key engine digest. Two clean amd64 runtime builds matched; two clean amd64 application packages from the same runtime reported identical engine and packaged byte hashes. One arm64 runtime image passed its capability assertions under temporary QEMU. Final digest-binding wiring and exact-candidate release checks remain. |
| M4 — bounded rolling coverage | Negative result; candidate withdrawn | The proposed 108-second steady runway nominally fit the 124-second reserve ceiling after one segment of rounding, but the integrated flow and publication clocks did not maintain the same bound during cutover. Three of 17 `rolling_publication_budget` regressions failed, including an observed 39.6-second produced overshoot beyond the allowed edge in one simulation. The candidate is removed; startup, steady publication, and scratch limits retain their prior behavior. A future design needs one shared incremental allowance plus native full-cycle and ten-minute continuity evidence before claiming +10-second local coverage. +30 seconds and 2× do not fit the proposed allowance. |
| M5 — integrated evidence | Partial | Same-source Store and browser-route contracts are exercised after review. Native real-server playback, first-frame latency, fleet source-I/O throughput, and same-source before/after timings remain unavailable until a physical run and later operational measurement. |

## 2. Validation and promotion

| Check | Result |
|---|---|
| Pinned compiler | Rust 1.97.1 confirmed on the separate clone host; baseline `rustup run 1.97.1 cargo check -p plurxd --all-targets --locked` passed 2026-09-29. |
| M0 compile and syntax | Pinned `plurxd` check, `cargo fmt --check`, and JavaScript syntax checks passed after the first trace edit. The added Rust log regression has not been executed. `node --check scripts/playback-lab` passed for the real-server capture command. |
| M1 compatibility compile | Pinned `cargo check --workspace --all-targets --locked` passed on 2026-09-29; the backend-neutral regression is written and deferred for the final review sequence. |
| M1/M2 compile | Pinned `cargo check -p plurxd --all-targets --locked` passed after the viewer-interest wiring; a core check with `hiqlite-store` also passed. The new SQL migrations and claim behavior await the final test phase. |
| Runtime build evidence | Two clean amd64 `runtime-assets` builds on the separate build host produced FFmpeg SHA-256 `90004301255382e1beb441a294fa8b75ac7fbb54678837f224d3458032e38787`, FFprobe `2f270d6f6f97bfb4167ee8ab00d4e17ecd18bd22069f4d270f7af41c06e11ac2`, and package-manifest `01d84f56a3ba50494cfe62f2fd95867d3cd3f48fa526a3a192c057b914a08be3` in both images. The image IDs differ because build metadata is not the engine identity. |
| Arm64 runtime evidence | One clean `runtime-assets` image built under temporary QEMU on the separate build host. Its arm64 FFmpeg SHA-256 is `10a017e55452a171a8e24287caabfbc24ce3a8d6182acdedf86a000ac8f3e929`, FFprobe `26f9a2e0b353af160ef75fcbd12d5d117cbac014a323626af5b9fe06ae79dc93`, and package manifest `f9054eac1b0e55099e53408275051d887b3288795ff7954c29d6d8f7e6aa096e`. The binary reported `ffmpeg version 8.1.3-Jellyfin`; the Dockerfile capability assertions passed. |
| Amd64 application packages | Two clean packages from the same local runtime input reported actual daemon `engine_digest` `85e6fa5e7614ad2165fdf2e83d96542ada3f18f28fadb04d2a960f9cbf4938ab`. Both carried application binary SHA-256 `77f65bb1c86f04fba608c00a3faf622f93bd502c8343049900ad0e27d1f08b40` and the FFmpeg, FFprobe and manifest hashes above. A third local package with the amended final stage reported the same engine digest and the supplied runtime image ID through `media-runtime-identity`. The release workflow supplies and checks the published registry digest on its final image. |
| Focused regressions | Post-review phase: 39 JavaScript seek tests passed; 17 release publication contracts passed; five exact Store regressions passed on SQLite and three-voter Hiqlite after correcting SQL scope and placeholder-order failures; the analysis wake test passed. The M4 candidate failed three of 17 wider rolling budget tests, was withdrawn, then all 17 rolling budget and four MKV/HLS scheduling tests passed. Physical Safari runs remain. |
| Astra adversarial review | Completed as a read-only review of `6ba5c59de` against `38c917225`: seven findings on viewer artifact admission, learner execution, source reservation lifetime, attested artifact demand, hydration demand, the Safari server fixture, and waiter capacity. Corrective commits `6f583c519`, `aacc9697c`, and `9b403ba2e` address them. Focused testing found SQL scope and Hiqlite parameter-order issues, corrected in `a4236f00c`. |
| Ready fast lane | Pending exact candidate; M4 disposition is the bounded negative result above. |
| Task pull request | [#622](http://192.168.4.7:3000/noirr/plurx/pulls/622) merged into `effort/safari-seek` as `1fa576a05`, preserving its regression fields. The user directed local compile evidence here and one final test lane after review. |
| Main pull request | Draft [#623](http://192.168.4.7:3000/noirr/plurx/pulls/623) into `main`. It remains draft while the final evidence and candidate are resolved. |
| Main merge | Pending green lane and exact-candidate qualification. |
| Deployment | Outside this build; no production action authorized. |

## 3. Decisions to reconcile

The user requested tests only after the final review; that sequence governs
this effort. Focused regressions started after Astra completed its review.
The user clarified on 2026-09-29 that a Developer switch is needed only if a
separate optional behavior needs one. This correctness and queue-admission
repair needs no switch. No new product feature gate is planned.

The reviewed runtime candidate uses Jellyfin FFmpeg `8.1.3-1-bookworm`. A
change in the daemon's engine digest creates a distinct fragment keyspace:
existing artifacts and queued requests retain their original identity, and
only workers with the matching engine may claim them. Before an operational
rollout, prepare the incident reference title (file 9) and a small set of
copy-compatible reference titles under the new exact recipe on each affected
architecture, then let viewer demand order the remaining work. No whole
library rebuild is part of this build. The new digest must be read from the
actual released image; package equality with an existing node is not proof
that its loaded libraries match.

## 4. How to read this page

The branch and base name identify the source snapshot, not a qualified
candidate. Only a green ready fast lane tied to the final review fixes and a
recorded merge establish that the code reached `main`. Physical Safari and
fleet results remain explicitly unavailable until captured.
