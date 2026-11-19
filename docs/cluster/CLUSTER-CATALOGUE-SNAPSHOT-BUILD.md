# Peer catalogue snapshots

**Status:** ready to build · **Executes:** B6 of the
[cluster audit](CLUSTER-LAYER-AUDIT-2026-11-14.md) · **Written:** 2026-11-19

The [facade contract](CLUSTER-FACADE-BUILD.md) owns the directory and peer
transport. This plan owns descriptive catalogue reuse in `media_pool.rs`.
The adaptive resource effort (#1248) keeps reservation and native ownership;
this change must not turn a description into execution authority.

## 1. Current contract

At base `09d4d9502`, `MediaPool::quality_catalog_answer` asks every eligible
peer for the same catalogue on each call. The existing media snapshot holds
capabilities and resource observations, but no source-specific recipe digest,
held executable identity or validated client track selection. Manufacturing
those facts from an encoder name would bypass current correctness checks.

`restore_catalog_context_planned` in `transcode/manager/candidates.rs`
reconstructs the selected owner's recipe before execution. It and the owner's
fresh admission, source binding and resource reservation remain unchanged.

## 2. Descriptive snapshot contract

Cache only a complete, successful, strictly bounded v2 peer description whose
candidate bindings equal the create's exact planning binding. Never cache
refusals, partial answers, legacy responses, unknown bindings or local execution
contexts. Preserve the candidate's original node, recipe and binding.

A key covers the entire validated request, planning binding, peer identity,
endpoint and current media snapshot. A removed member, replaced endpoint,
changed source/settings generation, client audio/subtitle/caps choice or new
snapshot cannot reuse another key. Require a fresh exact-protocol peer snapshot
for reuse. Expiry is monotonic, no later than that snapshot's expiry or the
existing ten-second snapshot interval. Bound the entry count; no new poller,
task or timer owner is introduced.

Warm repeated calls and subsequent creates read the held descriptive snapshot.
A cold or expired key uses the original bounded peer request and stores only
an eligible result. This retains cold discovery rather than guessing an absent
worker's recipe. A cached answer never extends a request deadline or skips the
selected owner's existing fresh recipe/source/admission qualification.

## 3. Ownership and delivery

| Owner | Files |
|---|---|
| Root B6 | `media_pool.rs`, its focused fixtures and this plan |
| Peer lane B5/B3a | peer-call racing, proof caches and token revocation |
| Watch lane B4 | watch API, sharing/offline work wakeups, session lease loop |
| Timing lane C3 | central timing declarations and relationship checks |
| Existing resource effort | native producer/reservation custody and #1248 |

No changes to `media_sessions.rs`, native candidate construction or resource
admission are planned. Isolated index/backlog rows will be reconciled at
integration. The active facade promotion remains independently owned.

## 4. Acceptance

- Same current key reuses one held description; expiry and any scope change
  force the original request path.
- Missing/old/mismatched peer snapshots, partial and refused answers, legacy
  responses and unequal candidate bindings never populate reusable entries.
- Bounded entries are evicted without changing existing request deadlines.
- Existing stale-source, changed recipe/helper and remote-owner admission
  regressions remain intact; cache observations grant no execution authority.
- Pinned exact-source daemon check and focused cache/owner regressions pass.
  The inherited macOS `dv_runtime` compile failure is not a passing check.

## 5. Validation ledger

Implementation, focused execution, final review and qualification are pending.
The source-only compiler slot is available; its exact-base daemon check reports
only the existing two `state.rs` Linux-runtime type errors. Linux or a qualified
platform repair is required before a green daemon result can be recorded.
