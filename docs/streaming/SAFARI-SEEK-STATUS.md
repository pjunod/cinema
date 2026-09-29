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
| M0 — browser fixture and stage diagnostics | In progress | Added bounded client route and settlement traces. The `scripts/playback-lab safari-seek-fixture` native Safari run and [four served-manifest reports](../evidence/safari-seek-native-2026-09-29.json) reproduce the clamp; growing/sliding real-server evidence and server stage diagnostics remain. |
| M1 — viewer preparation | In progress | Both Store claims filter incompatible engines. The playback path joins an exact, 120-second waiter through analysis, build and targeted hydration; active session ownership refreshes it at most every 30 seconds, and terminal sessions retire their own interest. Analysis priority follows live waiters without changing claim fences or attempts. Server stage diagnostics and an integrated remote hydration case remain. |
| M2 — source I/O admission | In progress | The fenced analysis claim now reserves pre-artifact source reads by storage domain. Common job claims count those reservations and admit at most one reader without live interest across the two-slot domain; existing holders drain without revocation. Multi-node behavior, transition diagnostics and measured maintenance throughput remain. |
| M3 — reproducible media runtime | In progress | Added a read-only `plurxd media-runtime-identity` record backed by the cache-key digest and a package manifest emitted by the image build. The release workflow retains a per-architecture image/digest/package record. Immutable runtime inputs and a two-clean-build comparison remain. |
| M4 — bounded rolling coverage | Pending measurement | Widen only if native range and resource measurements satisfy §8 of the implementation contract. |
| M5 — integrated evidence | Pending | Compare the same source, recipe, engine and cache state. |

## 2. Validation and promotion

| Check | Result |
|---|---|
| Pinned compiler | Rust 1.97.1 confirmed on the separate clone host; baseline `rustup run 1.97.1 cargo check -p plurxd --all-targets --locked` passed 2026-09-29. |
| M0 compile and syntax | Pinned `plurxd` check, `cargo fmt --check`, and JavaScript syntax checks passed after the first trace edit. The added Rust log regression has not been executed. |
| M1 compatibility compile | Pinned `cargo check --workspace --all-targets --locked` passed on 2026-09-29; the backend-neutral regression is written and deferred for the final review sequence. |
| M1/M2 compile | Pinned `cargo check -p plurxd --all-targets --locked` passed after the viewer-interest wiring; a core check with `hiqlite-store` also passed. The new SQL migrations and claim behavior await the final test phase. |
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

## 4. How to read this page

The branch and base name identify the source snapshot, not a qualified
candidate. Only a green ready fast lane tied to the final review fixes and a
recorded merge establish that the code reached `main`. Physical Safari and
fleet results remain explicitly unavailable until captured.
