# Video quality status — what is built, measured and merged

**Status:** planning; three initial lanes assigned · **Updated:** 2026-10-02

Companion to [the programme](VIDEO-QUALITY-PROGRAM.md), which owns scope,
acceptance and order. This ledger records actual execution. Empty evidence is
unverified, not a pass. Dates use America/New_York unless a receipt states UTC.

## 1. Integration and compiler

| Fact | Value |
|---|---|
| Planning base | Forgejo main `4fa50b79e4b3b196797a9b7a1a7a4abd2184ea43` |
| Planning branch | `codex/video-quality-program` |
| Documentation PR | [#758](http://192.168.4.7:3000/noirr/plurx/pulls/758), one adversarial review complete; two P2 specification ambiguities addressed; fast lane pending |
| Effort branch | `effort/video-quality`, created after the plan merges |
| Compiler | `/Users/pjunod/.cargo/bin/rustc`: `1.97.1 (8bab26f4f 2026-07-14)` |
| Compiler-loop baseline | `cargo check -p plurx-core --lib --locked --offline` passed on planning base; package-only baseline reported pre-existing unused-item warnings. Pinned workspace Clippy and tracked hook passed. |
| Original checkout | Left untouched, including pre-existing uncommitted documents |
| Production changes | None |

## 2. First wave — three independently owned lanes

| Lane | Owner | State | Evidence / next action |
|---|---|---|---|
| A: encoder calibration | `/root/encoder_calibration` | Source census complete; read-only hardware discovery | Reuse existing bench and acceptance policy. Identify isolated capture route that does not write replicated settings; no new default is justified yet. |
| B: content-aware encoding | `/root/content_aware` | C1 contract specified | New offline sample/recommendation tool and focused tests; wait for documentation merge before implementation. C2 durable/runtime integration remains unstarted. |
| C: HDR-to-SDR images | `/root/hdr_calibration` | Source census complete | Complement existing S11 scorer; bounded before/current/reference captures and contact sheets. Avoid duplicating open codec-corpus work. |

Coordinator owns docs, index, validation catalogue and shared integrations.
No benchmark host is reserved yet. Assign one host/window per active calibration
job here before running it; keep three coding lanes but serialize shared GPUs.

## 3. Requested follow-on queue

| Priority | Work | State |
|---|---|---|
| 1 | Next-episode preparation | Queued after the first wave |
| 2 | Apply qualified encoder policies / retain measured baseline | Depends on A; no duplicate campaign |
| 5 | HLS acknowledgement batching | Queued; M1 buffer changes already exist |
| 6 | VOD B-frames | Queued; no-reorder remains production contract |
| 3 | Broader codec/HDR output | Queued; reconcile active S11 work first |
| 4 | Remaining cold-start latency | Queued; reconcile PR #745 and descendants first |

## 4. Evidence interpretation

Each measurement row must name source commit, source/corpus hash, tool build,
recipe, host/encoder, resource limits, command, result and scope. Synthetic and
non-production-tool runs are screening evidence. Failed or unavailable evidence
is retained with its reason. No screenshot or metric substitutes for an absent
client, renderer or physical-display run.

Calibration proceeds without user-operated tests. If a route cannot be measured
autonomously, continue other routes and report that limitation; do not silently
broaden a pass from one hardware family or source to another.
