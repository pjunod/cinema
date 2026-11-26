# Cluster facade slice 5 — own bounded peer calls and fresh owner lookup

**Status:** built — implemented and independently reviewed. **Owner:** Codex.

Delivery and qualification evidence is tracked in
[promotion #1811](http://forge.lan:3000/noirr/plurx/pulls/1811). Earlier branch
names and checkpoint receipts below are historical. Fleet acceptance remains
after Paul’s deployment.

Companion to [the facade contract](CLUSTER-FACADE-BUILD.md) and
[slice 4](CLUSTER-FACADE-SLICE-4-BUILD.md). This task follows slice 4's
checkpoint `e398583eb`; final qualification uses the current integrated
effort, preserving C7 and other owners' changes. Integration target:
`effort/cluster-facade`.

## Contract and boundary

Move the existing peer transport implementation into an inline `calls`
module in `cluster.rs`. This puts the raw manager inside the existing
facade boundary without adding census exemptions or changing matchers.
Expose an owned cloneable call capability through the facade; keep its
constructor private to cluster composition. Ordinary feature constructors
receive the capability, and feature methods clone it rather than clone raw
membership to construct a transport. Keep the old HTTP module only as
transparent compatibility re-exports where server helpers still need them.
Do not add a second request forwarding implementation.

Preserve expected node identity separately from URL, all four distinct
auth modes, fresh nonce and exact target/method/path/raw-body signing,
redirect refusal, one absolute deadline and byte bound, Retry-After limits,
static capability headers, streaming backpressure and existing body custody.
Streaming still refuses complete-response-verification modes. Preserve
clock ticket checks and T4 capture before creating the verification future;
failed proof discards timing metadata. Member responses include learners;
voter responses retain voter-only authorization.

The existing manager already shares `peer_http()` and configures a connect
timeout. This task centralizes ownership; it does not claim to create a new
connection-pooling fix. Bootstrap clock observation precedes AppState and
continues through narrow cluster-owned composition, without a fence or
monitor requirement. Membership replacement must rebuild capabilities from
the replacement manager. Optional hydration transports use optional typed
capabilities; separately needed raw authority debt remains counted honestly.

Remove Live TV's five-second owner-address cache. Resolve the already
selected owner's identity through the stamped directory on each lookup,
retaining reachable-voter/base eligibility and time-dependent reachability.
An exchange failure is not a roster change; do not invent directory
invalidation or health suppression. Remove obsolete hit/resolved/invalidated
owner-cache metrics and composition handles rather than report false cache
evidence. Directory read counters retain actual hit/reload/uncached evidence.
Supersede the old TTL regression record with accurate new coverage.

This bounded slice does not implement B5's cached authorization proofs,
peer-health policy, richer outcomes or first-success holder races. Those
remain explicit follow-up work with separate contracts and regressions.

## Ownership

| Files | Work |
|---|---|
| `crates/plurxd/src/cluster.rs`, `http/peer_transport.rs` | One implementation, facade composition, existing helper re-exports and preserved transport/security regressions. |
| `state.rs`, `clock_offset.rs`, `http/internal_activity.rs`, `media_pool.rs`, `media_sessions.rs`, `shared_cache.rs` | Inject long-lived capabilities; preserve all separate authority and shutdown ownership. |
| `copy_index_v3.rs`, `fragment_index_cluster.rs`, `subtitle_ranges.rs`, `subtitle_source.rs`, `transcode.rs`, `transcode/manager/construct.rs`, `vodserve.rs`, `vod/serve/construct.rs`, `diagnostics.rs` | Migrate transport-only parameters and per-use construction; retain directory freshness. |
| `http/activity_starts.rs`, `http/transcode_copies.rs`, `http/internal_auth_revocation.rs`, `http/remote.rs`, `http/remote/invitations/snapshots.rs`, `http/cluster_operations.rs` | Typed call composition with unchanged operation/authentication semantics. |
| `http/live_tv.rs`, metrics composition, associated tests and regression record | Remove owner TTL, resolve through directory, truthful telemetry and behavioral regression replacement. |
| `cluster/census.rs`, ownership inventory and formal mappings if measured | Lower actual pins only, retain matcher/exemptions and review actual source/owner shifts before repinning. |
| This plan, cluster index, project backlog and functionality catalog | Delivery ledger and evidence. |

Transport-only stored membership in subtitle access, transcode subtitle
composition and VOD is replaced by the typed call capability. Job-manager
membership remains for reconciliation; hydration receives a separate call
capability alongside its existing directory. This does not change reconcile,
capacity, custody, publication, or source-reader ownership.

## Validation and delivery

Establish pinned Rust 1.97.1 compiler loop before editing Rust. Preserve and
run bounded/streaming transport tests, nonce/parser cases, Retry-After,
redirect/declared-and-chunked body bounds and clock T4 rejection. Replace
TTL tests with directory reuse, immediate applied address/removal change,
time-only expiry, identity isolation, retry after failed resolution, and
unchanged owner verdict versus ingress transport-failure behavior.

Run affected all-target compilation, formatting, Clippy and focused
regressions. Measure census reductions without a speculative ceiling;
run ratchet, ownership, formal source correspondence and paperwork. Record
actual environment limitations without suppressions or weakened tests.
After integration, obtain one adversarial whole-diff review, fix findings,
run the Effort gate once on its exact head and land only on a pass with
regression trailers. Main promotion and deployment/acceptance are separate.
Implementation and local checkpoint evidence follow; review, gate and
qualification remain pending.

## Local checkpoint — 2026-11-17

The pinned Rust 1.97.1 baseline all-target check passed before Rust edits.
The transport implementation and tests now live in inline `cluster::calls`;
construction is cluster-private and the old HTTP module only re-exports
names. The production code differs only in constructor visibility, the
relocated clock module path and a rustfmt signature comma. Behavioral
security tests remain the execution evidence.

Transport-only handles in subtitle access, transcode and VOD are typed call
capabilities. Job-manager reconciliation retains its membership evidence
and hydration gets a separate call capability. Replacing AppState's fixture
membership also rebuilds its plain Live TV call handle. The extra owner TTL
and obsolete cache metrics are removed; every lookup takes the current
stamped voter roster.

All 22 relocated transport/security and Live TV cases pass, including clock
T4 rejection, exact nonce parsing, redirect/body limits, fresh address
lookup, identity/removal/reachability and retry after unreadable lookup.
Thirteen copy-index, subtitle cancellation/timeout and operation collector
regressions pass. Strict census passes ten cases at 369, down from 430;
matchers/exemptions are unchanged. Ratchet seven and ownership inventory
seven pass without inventory changes. Three formal source mappings pass
after eight unique unchanged 42-line anchor windows were reviewed; this
claims source correspondence only, not new model execution.

The first pin rewrite damaged its declaration header; the local compiler
and parser caught it, and both passed after correction. That failed attempt
is not evidence of a census pass. Real feature-enabled clock causality and
remaining static checks are still pending at this checkpoint. Integrate
the actual current effort/C7 source before final review and qualification.
No gate, landing, deployment or fleet acceptance is claimed.
