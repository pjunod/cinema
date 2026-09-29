# Safari seek build — implementation and promotion status

**Status:** building · **Updated:** 2026-09-29 · **Base:** `1869871ce` ·
**Branch:** `codex/safari-seek-build` from `effort/safari-seek`

Companion to [SAFARI-SEEK-IMPLEMENTATION.md](SAFARI-SEEK-IMPLEMENTATION.md)
(the contract and acceptance cases) and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the merge rules). This
page records what is actually built and proved. A checked box means evidence
exists for the named milestone; it does not imply a fleet rollout.

## 1. Build state

| Milestone | State | Evidence and next step |
|---|---|---|
| M0 — browser fixture and stage diagnostics | In progress | Added bounded client route and settlement traces. The `scripts/playback-lab safari-seek-fixture` native Safari run and [four served-manifest reports](../evidence/safari-seek-native-2026-09-29.json) reproduce the clamp. The new `scripts/playback-lab safari-seek-server` command captures actual rolling served playlists and Safari ranges from an isolated daemon; its physical run is pending final validation. Server playback events now include produced/published edges, accepted demand sequence and age. Preparation stage diagnostics remain. |
| M1 — viewer preparation | In progress | Both Store claims filter incompatible engines. The playback path joins an exact, 120-second waiter through analysis, build and targeted hydration; active session ownership refreshes it at most every 30 seconds, and terminal sessions retire their own interest. Analysis priority follows live waiters without changing claim fences or attempts. The source reader now has a separate local pass guard so it cannot hold the artifact and delivery pass. An integrated remote hydration case remains. |
| M2 — source I/O admission | In progress | The fenced analysis claim now reserves pre-artifact source reads by storage domain. Common job claims count those reservations and admit at most one reader without live interest across the two-slot domain; existing holders drain without revocation. Multi-node behavior, transition diagnostics and measured maintenance throughput remain. |
| M3 — reproducible media runtime | In progress | The runtime uses a digest-pinned Debian base, a dated Debian package snapshot, a versioned and SHA-256-verified Jellyfin 8.1.3 artifact, and the existing verified dovi_tool archive. The release workflow publishes the runtime by per-architecture digest and builds the app from that digest; the app record includes that digest, packages, source SHA and its own cache-key engine digest. Two clean amd64 runtime builds had identical FFmpeg, FFprobe and package-manifest hashes. One arm64 image built with its capability assertions under temporary QEMU; two app-image digest comparisons remain. |
| M4 — bounded rolling coverage | Pending measurement | Widen only if native range and resource measurements satisfy §8 of the implementation contract. |
| M5 — integrated evidence | Pending | Compare the same source, recipe, engine and cache state. |

## 2. Validation and promotion

| Check | Result |
|---|---|
| Pinned compiler | Rust 1.97.1 confirmed on the separate clone host; baseline `rustup run 1.97.1 cargo check -p plurxd --all-targets --locked` passed 2026-09-29. |
| M0 compile and syntax | Pinned `plurxd` check, `cargo fmt --check`, and JavaScript syntax checks passed after the first trace edit. The added Rust log regression has not been executed. `node --check scripts/playback-lab` passed for the real-server capture command. |
| M1 compatibility compile | Pinned `cargo check --workspace --all-targets --locked` passed on 2026-09-29; the backend-neutral regression is written and deferred for the final review sequence. |
| M1/M2 compile | Pinned `cargo check -p plurxd --all-targets --locked` passed after the viewer-interest wiring; a core check with `hiqlite-store` also passed. The new SQL migrations and claim behavior await the final test phase. |
| Runtime build evidence | Two clean amd64 `runtime-assets` builds on the separate build host produced FFmpeg SHA-256 `90004301255382e1beb441a294fa8b75ac7fbb54678837f224d3458032e38787`, FFprobe `2f270d6f6f97bfb4167ee8ab00d4e17ecd18bd22069f4d270f7af41c06e11ac2`, and package-manifest `01d84f56a3ba50494cfe62f2fd95867d3cd3f48fa526a3a192c057b914a08be3` in both images. The image IDs differ because build metadata is not the engine identity. |
| Arm64 runtime evidence | One clean `runtime-assets` image built under temporary QEMU on the separate build host. Its arm64 FFmpeg SHA-256 is `10a017e55452a171a8e24287caabfbc24ce3a8d6182acdedf86a000ac8f3e929`, FFprobe `26f9a2e0b353af160ef75fcbd12d5d117cbac014a323626af5b9fe06ae79dc93`, and package manifest `f9054eac1b0e55099e53408275051d887b3288795ff7954c29d6d8f7e6aa096e`. The binary reported `ffmpeg version 8.1.3-Jellyfin`; the Dockerfile capability assertions passed. |
| Focused regressions | Deferred until the final adversarial review, per the requested sequence. |
| Astra adversarial review | Pending final main candidate. |
| Ready fast lane | Pending review fixes. |
| Main merge | Pending green lane and exact-candidate qualification. |
| Deployment | Outside this build; no production action authorized. |

## 3. Decisions to reconcile

The user requested tests only after the final review; `AGENTS.md` and the
implementation handoff require a focused regression before each task push.
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
