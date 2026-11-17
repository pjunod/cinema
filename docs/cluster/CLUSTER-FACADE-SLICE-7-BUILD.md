# Cluster facade slice 7 — close ownership and prove honest zero

**Status:** open — plan committed before implementation. **Owner:** Codex.

Companion to [slice 6](CLUSTER-FACADE-SLICE-6-BUILD.md),
[the facade contract](CLUSTER-FACADE-BUILD.md), and
[the boundary decision](CLUSTER-FACADE-BOUNDARY-DECISION.md). The
preparatory base is checkpoint `2f39bc76a`; every reviewable task is ported
to the actual current `effort/cluster-facade` before final checks/review/gate.
This is the final facade stage, not completion of the wider cluster audit.

## Acceptance contract

Feature code has no raw membership, serving-fence or replication handles.
Remove those AppState fields rather than keep duplicate private owners.
The facade owns bootstrap and runtime composition and gives feature code
narrow typed capabilities. Existing C2/C7 shutdown ownership, clock startup,
request admission, fresh learner job decisions, opaque custody proofs,
backup sequencing and settings-change proof logic retain their rules.

The final source census has an empty measured table and zero ceiling. Its
classification distinguishes proven local DTO/watch/counter fields from
raw handles without renaming fields, broad method exclusions or new file
exemptions. Unknown receivers remain counted. Strengthen negative controls
for aliases, renamed fields, clones, shorthand/destructuring, split lines,
macros, raw-returning accessors and new files. Actual sibling-module
compiler probes must reject direct raw AppState access.

Core role/roster methods become module-private only after real consumers
use the explicit core capability bridge. Rust cannot allow one foreign
module to call a private method: the boundary decision records that limit
and the chosen high-level public core contract. No raw-manager getter,
Deref or public renamed equivalents are introduced to fake privacy. Keep
explicit ROLE/ROSTER census detection after visibility changes. Actual
source arrays currently name seven role and eleven roster methods.

## Sequential ownership and delivery

All pieces target one effort branch. Cluster.rs has one integration owner,
and these are serialized reviewable tasks, not file-disjoint main changes.
Each piece receives focused regressions, one complete-diff adversarial
review, exact-head Effort gate and landing before the next final task base.
Preparatory source checkpoints do not establish landing or qualification.

| Piece | Files and ownership | Result |
|---|---|---|
| 7a — incoming authentication and admission | `cluster.rs`; `diagnostics.rs`; HTTP `activity_starts`, `internal_activity`, `internal_auth_revocation`, `internal_live_tv`, `internal_media`, `internal_media_sessions`, `remote`, `cluster_operations`, `mod`, `stream`; HLS `control`, `create`, `fence`, `preparation`, `response`; `offline.rs`, `state.rs`, associated fixtures | Reuse PeerAuthentication; preserve distinct legacy/read/voter/exact algorithms. Inject owned restart admission into OfflineManager. HLS response observation retains the same authority source and arrival/answer sampling. Route classification stays stateless and exact. |
| 7b — job, backup and applied-change services | `cluster.rs`, `background_jobs.rs`, `background_jobs/analysis_reconcile.rs`, `cache_preparation.rs`, `state.rs`, `subtitle_models/mod.rs`, small remaining role callers, `backup.rs`, `settings_changes.rs` | Owned ClusterJobAuthority with fresh learner authority, typed reconciliation that preserves unreadable refusal, exact backup apply/anchor/apply/snapshot deadline sequence, typed applied-index/liveness/change source with unchanged settings proof. |
| 7c — bootstrap and core boundary | `cluster.rs`, `main.rs`, `state.rs`; scoped core membership/access APIs, SelectedBackend factory; cluster-check and external startup observer fixture | Cluster-owned composition from selected mechanisms, typed Boot/AppStateConfig, high-level core bridge and private raw role/roster methods. Coordinate the small SelectedBackend addition after the active listener owner's integration; no competing vendor/startup/listener repair. Preserve C7 on its actual landed base. |
| 7d — privacy and semantic census | `cluster.rs`, `state.rs`, `cluster/census.rs`, ratchet/ownership tests and fixtures, final plan/audit/backlog ledger | Remove last raw fields/reach-arounds, exact non-raw receiver classification, strengthened negative controls, compiler probes, empty measured pins and zero ceiling. |

Associated formal source mappings and ownership inventory are updated only
after reviewing actual modeled statements/owners. Matcher sentinels and
existing raw-type inventories are retained or strengthened. No unrelated
B3/C1/source-reader mechanisms are rebuilt.

## 7a contract

Incoming authentication reuses the existing inline calls owner. Add narrow
delegates for the original legacy activity, internal read and internal
voter request algorithms; do not merge them into a boolean mode or replace
fresh checks with a cached answer. Preserve exact target/method/path/body,
nonce consumption, clock/freshness/member floors, signature/status/body
binding, error mapping and caller deadlines.

Serving replacements use the existing authority/subscription/admission
mechanisms. The OfflineManager capability returns the original owned
RestartAdmission token; it is retained and released at the same points.
HLS response telemetry must sample the same authority at arrival and
answer, not freeze a readiness boolean. Keep cancellation and response
observation ownership unchanged.

## Validation

Use the owned normalized source-only compiler slot and dedicated target.
Verify every tracked archive blob before/after each command; share only
dependency/compiler caches, never CARGO_TARGET_DIR with another checkout.
Use pinned Rust 1.97.1 and the existing CI test stack of 8 MiB. A prior
shared-target result is not exact-tree qualification.

Before Rust edits establish the working compiler loop. For 7a retain/run
existing exact/legacy/read/voter authentication, clock/nonce/body/redirect
regressions, HLS arrival/answer authority and restart-admission/cancellation
cases, plus focused offline token ownership. Later pieces add their
existing job/backup/settings/bootstrap/privacy regressions and core,
daemon, cluster-check and external fixture compile coverage.

Run formatting, relevant strict Clippy, all-target compile, focused tests,
census/ratchet, ownership, source-mapping checks and exact-body paperwork.
Keep baseline/environment failures explicit; never suppress warnings,
weaken proof validators, widen pins or infer fleet acceptance. Final effort
promotion freezes task merges, integrates actual current main, qualifies
the new tree and passes Main promotion gate. Deployment remains Paul's
action. No implementation, review, gate or acceptance is claimed here.
