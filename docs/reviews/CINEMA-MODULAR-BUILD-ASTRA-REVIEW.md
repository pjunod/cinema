# Cinema modular builds — Astra findings and dispositions

**Status:** done — Astra confirmed all six plan findings addressed; phase-entry gates retained.
**Written:** 2026-11-04 · **Reviewer:** Astra (`gpt-6-astra`, high reasoning).
**Source:** `f30dfbbc9237046a0f6a6f20d893044593f56600` plus the initial plan draft.

Companion to the [phased plan](../ci/CINEMA-MODULAR-BUILD-PLAN.md), which owns
implementation and acceptance. This record preserves the independent review
and each correction. Astra read source and the plan; it did not edit, build or
qualify an implementation. Codex applied the dispositions below. An accepted
plan does not establish that a future extraction or build improvement passes.

## 1. Independent verdict — request changes before implementation

Astra found the phased approach, baseline capture, prerequisite PR and
effort-to-main promotion model sound. It requested two P1 corrections and two
explicit phase-entry gates, plus two measurement/cache clarifications.

| ID | Severity | Original finding | Requested correction |
|---|---|---|---|
| A1 | P1 | The graph gate covers the daemon but the Docker recipe also builds and ships `plurx-cluster-check`, whose manifest directly depends on Hiqlite/WAL. A clean daemon alone cannot qualify a standalone distribution. | Define supported distributions, recipes, targets, capabilities and every bundled executable. Gate complete package/build closure, including host-side build/proc-macro edges; separate checker/migration artifacts; retain Windows support explicitly. |
| A2 | P1 | Version matching and a shared directory lease do not specify the new cross-process helper protocol. The current in-process converter transfers one continuously held descriptor; a launcher can deadlock its child or race another daemon. Exact supported schema cohorts also matter. | Define artifact/protocol/receipt/schema/layout compatibility, launcher/helper ownership sequence and durable revalidation. Cover competing launchers, crashes, wrong-build/same-version, unsupported cohorts and accepted writes after publication. |
| A3 | P2 | Moving a sealed trait into contracts while implementations move into external backend crates cannot preserve its private seal unchanged. The plan lacks an authority minting owner and valid dependency arrows. | Make a source-backed authority ownership/API design a P3 entry gate. Preserve sealed construction, prove no cycles, add downstream compile-fail cases and real native/replicated refusal tests. |
| A4 | P2 | Optional capabilities can make builds faster simply by deleting functionality; “same workload” does not establish comparable packages. | Bind timing pairs to a capability manifest; distinguish equal-capability optimization from smaller distributions. Specify inference defaults, unavailable UI, retained settings/data and re-enable behavior before P5. |
| A5 | Clarification | A source/cache lease covering only extraction is insufficient; Docker currently holds a cache lease through the build. | Hold exclusive ownership through staging, Cargo execution and artifact/receipt collection. |
| A6 | Clarification | An empty target directory can still hit a warm compiler-result cache. | Record/control compiler wrappers and cache state; label cold target and cold compiler results distinctly. |

Relevant reviewed source: [Docker packaging](../../Dockerfile),
[cluster-check dependencies](../../crates/plurx-cluster-check/Cargo.toml),
[storage selection/conversion](../../crates/plurxd/src/storage_runtime.rs),
[native ownership](../../crates/plurx-core/src/storage/native.rs),
[export cohorts](../../crates/plurx-core/src/cluster/migration/standalone_export.rs),
[sealed authority](../../crates/plurx-core/src/sharing_source_sessions.rs),
and [inference dependencies](../../crates/plurxd/Cargo.toml).

## 2. Dispositions — bind corrections to executable phase gates

| ID | Codex correction | Remaining proof |
|---|---|---|
| A1 | Plan §2.3 adds Linux/macOS/Windows standalone, explicit cluster, and separate migration-bundle rows. Every executable has an allowlist/hash/build/capability receipt. Cluster-check and migration helper are absent from fresh standalone packages. §6 now gates the full distribution, not one root. | P4 implements graph and artifact-inclusion negatives, host build/proc-macro coverage and actual recipes. Windows keeps existing manual release acceptance without restoring disabled PR jobs. |
| A2 | Plan §2.4 defines protocol/request/receipt versions, exact expected helper commit and executable hash, accepted AUTH81/AUTH82 canonical cohorts and output formats/schema. The launcher releases its lease before spawn; helper acquires/revalidates, owns through terminal drain/publication; launcher reaps and reacquires/revalidates before any writer. | P4 entry supplies exact request/describe/process APIs; actual competing-launcher, failure-boundary, mismatch, interrupted request, timeout/reap and post-publication write tests must pass. Existing in-process tests earn no cross-process credit. |
| A3 | Plan §2.5 assigns sealed proofs/private representations and trusted native/optional replicated implementations to a small authority owner, with no daemon/full-Store reverse import. Unchecked public minting is explicitly forbidden. Actual constructor APIs and no-cycle/compile-fail fixtures are mandatory before P3 code. | This is an enforced design entry gate, not a claim that the sketch already compiles. Astra's finding is addressed in the plan; implementation remains open. |
| A4 | Plan §2.5 requires per-pair capability manifests and separate result categories. Full distributions retain inference; minimal recipes/defaults are fixed before P5; absent features preserve models/data/settings and have explicit unavailable presentation and re-enable acceptance. | Every benchmark/packaging receipt enforces this; intentionally smaller functionality is never reported as equal-capability speedup. |
| A5 | Plan §3.1 extends exclusive slot ownership across staging, compilation and receipt collection. | P2a tests concurrent ownership and interrupted staging; wrappers must honor the same slot lease. |
| A6 | Plan §3.1 requires compiler-wrapper/cache state in receipts and separate cold-target/cold-compiler labels. | P1 report and controlled tests verify these distinctions. |

Astra also noted that same-size/mtime edits, deletion/rename/vendor invalidation,
interrupted staging refusal, truthful identity, Thin LTO retention, separate
package invocations, native sharing, explicit effort gates and current-main
qualification were already adequately specified. These requirements remain.

## 3. Reviewer confirmation — P1 and then P2 may begin

Astra confirmed the dispositions on 2026-11-04: "all six findings are addressed
sufficiently in the revised plan. No remaining plan blocker prevents P1,
followed by P2 after the relevant baseline is captured." It explicitly retained
the prerequisite PR/merge and later authority/helper phase-entry requirements.
This confirms plan corrections only, not implementation or benchmark results.

## 4. Evidence limits — review is not execution

Knowledge context had no reviewed coverage declaration for this manifest
boundary. A broader worktree obligation query included an existing decode-limit
expiry obligation; it does not prove this plan or implementation satisfies that
unrelated behavior. Normal repository obligation checks still apply to each
actual candidate.

No build optimization has acceptance credit from this review. Baseline timings,
before/after measurements, final package dependency omission, source freshness,
helper process handoff and platform/runtime acceptance remain the plan's gates.

## 5. Primitive sequencing — narrow admission, authority gate retained

Astra reviewed the source-backed boundary decision against runtime `5484d7986`.
It permits only the enumerated primitive-only P3a extraction; P3c remains
blocked. Two requested corrections are applied in the canonical plan and
[boundary decision](../ci/CINEMA-CONTRACTS-BOUNDARIES.md): explicitly limit the
early exception, and identify validators' intentional cross-crate visibility
while retaining Core's old visibility and exact clock/helper ownership.

No proof representation, trusted factory, credential owner, SDK hook or backend
selection may move under that exception. The complete original authority gate
applies regardless of task name. The admitted direction is acyclic; StoreError
moves with its SQLite conversion, while Core's local row-to-value conversions
stay legal beside local row types. Private viewer-key construction, checked
UUID parsing, wire values and old type paths survive through genuine reexports.
Contracts-only consumer/graph checks cannot rely on workspace unification.
This was source review, with no Rust edits, builds or authority acceptance.

## 6. P1 runner review — source mutation and timing validity

Astra requested three implementation corrections: protect child launch and
journal registration before source restoration, refuse unexpected probe bytes
instead of adopting them into an allowlist, and require the intended Cargo
package/target to rebuild. All three were fixed, with actual cancellation and
registration-failure injection, preservation of unrelated edits and rejection
of unrelated/stale artifact events.

A second source review identified restoration qualification and inherited
compiler-selection gaps. Restoration now requires owner/dependent rebuilds;
compiler metadata records the actual explicit compiler and target selection.
Root review also requires disjoint source/target directories so a nested cache
cannot hide source inputs. All 21 focused runner tests passed. The controlled
real-Cargo pair separately validates source restoration and equal observed
features/profiles; test fixtures alone do not establish real build behavior.
The outer legacy compiler wrapper's cooperation limit remains explicit in the
measurement ledger. This review provides no final package omission or release
qualification credit.
