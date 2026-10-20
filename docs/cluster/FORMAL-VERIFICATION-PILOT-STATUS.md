# Formal verification pilot — status

**Status:** implementation active; no milestone or proof accepted · **Owner:** Codex coordinator, thread `01a15aca-f39a-7ca3-9899-bf5bced43755` · **Base:** `2c0e8b14df` · **PR:** [#1241](http://forge.lan:3000/noirr/plurx/pulls/1241) · **Merge readiness:** not ready

GPT-6.1 Sol owns model implementation. GPT-6 Luna owns bounded tooling and mechanical source inventory. Following Paul's clarification, Fable 5.1 provides design advice and final adversarial review; it performs no further coding. Earlier authored files and commits are preserved, with their actual authorship intact. All work uses isolated agent-owned clones; the owner's checkout is untouched.

The monolithic model draft is unqualified. Early exploration exceeded the 60-second smoke budget (65,236,225 distinct states and 37,650,419 queued at the last sample) and was stopped incomplete. A later calibration completed, but Sol found contradictory assignments disabling time/poll transitions, collapsed time samples and loss of aged traffic. That calibration supplies no accepted evidence. Sol is replacing the representation with seeded starting states and a shared transition relation, preserving authority interleavings. Fable supplied two text-only design consultations; finite limits and non-vacuity remain explicit acceptance conditions. Baseline exploration exposed a state-cost problem. Sol corrected unequal queue/domain expiry and quotiented obsolete histories without removing authority transitions. The model is frozen; the cost decision uses a two-job eight-event baseline plus a single-job eleven-event lifecycle check. Both still require completion. The larger incomplete run is retained as negative feasibility evidence.

## Current implementation and acceptance

| Scope | Owner/model | State | Evidence still owed |
|---|---|---|---|
| Toolchain/bootstrap | GPT-6 Luna | Committed; TLC 1.7.4 and Temurin 21.0.12+8 fetch/resolve feasibility passed | Final synthetic tests and review |
| Protocol/model and contract | GPT-6.1 Sol | Frozen model, all controls/witnesses observed as intended; revised baseline/lifecycle feasibility pending | Complete named configurations, clocks/source contract, intended controls and non-vacuity |
| Source inventory | GPT-6 Luna | 97 candidate anchors across 33 files prepared | Semantic mapping review and current integrated hashes |
| Backend insertion bridge | Coordinator integration; earlier Fable authorship | Integrated through `14a881a4f`; real-backend cases and per-store acknowledgement seam authored | Final review and actual SQLite/three-voter execution |
| Compiler/static checks | Component author and coordinator | Pinned Rust 1.97.1 contract compile-only, both feature variants' Clippy and touched-file rustfmt passed | Final changed-candidate fast gate |
| Runner/receipts | GPT-6 Luna | Frozen mapping and temporal parser integrated; registry extending to 16 configs; 9-config smoke unchanged | Config metadata and declaration parser integrated; final model hash, observed control output and final synthetic tests pending |
| CI | Coordinator | Existing main/manual preflights provision pinned Java/TLC, check source drift and affected smoke | Final fast lane and retained receipts |
| Review and promotion | Designated Fable reviewer; coordinator | Not ready | Complete candidate, adversarial review, fixes, once-per-unchanged-code evidence, merge |

No unit or Store contract suites have run. Development model exploration and compile/static checks are separate from final qualification. The build remains one substantial PR to `main`: finish implementation, request final adversarial Fable review through the designated session, fix findings, then run the affected fast lane and focused qualification. Preserve passing evidence on unchanged checked inputs; rerun failed or invalidated checks only.

The authoritative blocking workflow is `main-fast-lane.yml` (15-minute preflight). The older full `ci.yml` is manual. The pilot adds no product feature gate and claims neither whole-cluster deadlock freedom nor permission to remove existing tests.
