# Playback principal census — retained ownership and remaining source admission

**Status:** open · **Scope:** S3 backend and caller ownership audit

This answers which session operations preserve a complete playback principal,
which callers still refuse Shared execution, and what evidence remains before
Source admission can be enabled. It supplements [the implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md)
and [the replicated runtime receipts](SHARED-LIBRARIES-REPLICATED-PRINCIPALS.md).
Canonical Local compatibility and readable Shared retention do not establish
Shared grant, scope, lifetime or member-floor authority.

## Audited snapshots and boundaries

The Hiqlite terminal/maintenance and acknowledgement checkpoint is
`81f4cd09e7e96e4b004395b244844d52beb78b09`. The initial immutable caller
snapshot is root integration `e99099a170a0610d7b3450bb1356e62eb4897b0a`;
caller files are unchanged through root's `bed8d0ae1` cleanup integration. Both
SQLite and Hiqlite implement the same 36 Store methods there. SQLite
terminal/maintenance predicates have been reconciled to that committed source;
caller repair qualification passes in this census checkpoint. The combined
integration repairs both remaining SQLite reads: `desired_within` selects the
canonical Local owner key and decodes the complete retained principal;
`validation_playback_pointer_desired_revision` uses the same canonical owner
predicate. A corrupt retained projection now fails decoding rather than
reconstructing a Local principal. SQLite terminal acknowledgement also preserves
a foreign owner's pointer that names the ended incarnation. The caller snapshot was extracted with
`git archive`; no Git metadata or credentials were copied.

The original Hiqlite census at `9e2e0e55c2f3946d6df7a14bd6e73f0e5662503f`
contains 61 numeric-owner SQL literals: 55 runtime literals, three validation
literals and three unit-test expectations. There are 36 methods in the
`MediaSessionStore` implementation. The earlier count of 65 predates the
pointer/desired-reader conversion and describes a different tree.

Runtime predicates now select the canonical ownership key on rebuilt tables.
The controlled legacy helper and original schema retain real numeric Local
IDs. The intentionally historical pointer validation writer retains its old
conflict target so upgrade tests can prove refusal. A text search containing
`user_id` therefore does not by itself identify an unconverted runtime write.

## Seven persisted ownership families

| Family | Rebuilt key and projection | Authority boundary |
|---|---|---|
| Request ledger | `(owner_key, request_id)`; full persisted principal on decode | Fresh/reacquired Local admissions check the real user in the same write; foreign resolved replay returns Conflict. |
| Session recipes | `(owner_key, request_id)`; Local insert explicitly binds all principal columns | Incarnation binding is immutable; request agreement is canonical. |
| Desired selection | `(owner_key, playback_id)`; complete fallible decoder | A deleted Local owner cannot recreate a desired row or receive a stale successful write result. |
| Session routes | Unique incarnation/session IDs plus complete principal projection | Principal predicates fence admission and dependent effects; unique incarnation alone does not authorize foreign cleanup. |
| Playback pointers | `(owner_key, playback_id)`; principal agreement with routes | CAS, predecessor, desired revision and receipt session UUID must agree in the same transaction. |
| Preparation ledger | `(owner_key, playback_id)`; complete fallible decoder | Abort and expiry retirement require ledger/session principal and playback agreement. |
| Recovery budget | `(owner_key, playback_id, recovery_epoch)`; complete fallible decoder | Failed incarnation and terminal settlement are scoped to the same principal; Shared reservation stays closed. |

Hiqlite decoders use `try_get` for principal metadata and propagate errors for
incomplete/wrongly typed owned rows. They validate principal kind, nullable real
user, grant UUID, viewer key and canonical owner-key agreement. Shared inventory,
desired, staged and recovery reads preserve distinct grant/viewer namespaces.
Terminal ack rows are outside these seven ownership families: their durable
identity is the exact session/incarnation/node/epoch tuple, not a numeric user.

## Finite Store method inventory

The following is the Hiqlite checkpoint inventory, reconciled against the
corresponding SQLite methods in root `bed8d0ae1`. “Refused” means an explicit
Local adapter/validator or same-write stored Local/current-user predicate;
Shared worker ingress remains refused. “Retained read” preserves metadata on
the candidate layout and refuses Shared on the original layout. Terminal
retirement can reduce retained Shared authority without admitting a producer.

| Method | Predicate, conflict or decoder responsibility | Shared checkpoint behavior |
|---|---|---|
| `claim_media_session_request` | Canonical request key, live-user admission and bounded Local counts | Refused |
| `record_library_channel_session_recipe` | Canonical request/recipe key; immutable incarnation; live-user insert | Refused |
| `assign_media_session_request_owner` | Canonical request key and live-user write | Refused |
| `activate_media_session` | Canonical request/recipe/session/pointer predicates; explicit metadata; lease ownership | Refused |
| `prepare_media_session` | Same-principal predecessor; canonical ledger; live-user three-statement admission | Refused |
| `record_desired_selection` | Canonical desired conflict target; complete Local metadata; live-user insert | Refused |
| `validation_playback_pointer_desired_revision` | Controlled Local ownership helper | Refused validation call |
| `validation_write_legacy_playback_pointer` | Intentional historical conflict target; candidate rejection is the regression | Refused validation call |
| `desired_selection` | Canonical key and complete persisted principal decoder | Retained read |
| `rejoin_media_session_preparation` | Canonical guarded delete; atomic assertion rollback; same-principal successor | Refused |
| `staged_media_session_for_playback` | Canonical key; complete decoder; expected-owner comparison | Retained read |
| `commit_media_session_preparation` | Canonical pointer CAS/ledger/desired; predecessor principal; actual receipt session UUID | Refused |
| `abort_media_session_preparation` | Ledger/session principal agreement fences pointer, lease and pin effects | Refused ingress |
| `settle_media_session_activation` | Canonical request/session/pointer; principal-fenced lease and pin cleanup | Refused ingress |
| `publish_media_session_activation` | Canonical request/route joins and live-user write | Refused |
| `complete_media_session_handoff` | Stored Local/current-user gate in mutation and replay read | Refused |
| `arm_media_session_handoff` | Stored Local/current-user gate in mutation and replay read | Refused |
| `arm_media_session_terminal_projection` | Exact incarnation/node/epoch; ended-row projection only | Actual Shared ended-only retirement qualified |
| `complete_media_session_terminal_projection` | Exact incarnation/node/epoch; ended-row projection only | Actual Shared ended-only retirement qualified |
| `fail_media_session_request` | Canonical request key; terminal state update | Refused ingress |
| `media_session_route` | Unique session lookup; complete persisted principal decoder | Retained read |
| `media_session_route_by_incarnation` | Unique incarnation lookup; complete persisted principal decoder | Retained read |
| `media_session_route_for_playback` | Canonical key and complete expected-owner comparison | Retained read |
| `record_media_session_terminal_ack` | Exact session/incarnation/node/epoch and sequence; terminal mutation | Actual Shared retirement, foreign-grant isolation and replay after cleanup qualified |
| `media_session_terminal_ack` | Unique session ack lookup; ack carries exact incarnation/node/epoch | Retained terminal read |
| `renew_media_sessions` | Canonical request/pointer joins; live Local-user gates on session, lease and pin | Refused |
| `expired_media_sessions` | Canonical request exclusion; complete route decoder and bounded cursor | Retained inventory |
| `claim_media_session_takeover` | Canonical request counts/assignment; live Local-user gates on session, lease and pin | Refused |
| `end_media_session_if_owner` | Exact ownership generation and canonical stored principal for dependent cleanup | Terminal retirement |
| `end_media_session` | Canonical stored principal; re-read actual owner/epoch inside lease cleanup | Terminal retirement |
| `maintain_media_sessions` | Same-owner/playback ledger retirement and retention joins; bounded cleanup | Terminal/expiry cleanup |
| `owned_media_sessions` | Canonical request/pointer joins; fallible complete lease decoder | Retained inventory |
| `reserve_producer_recovery` | Canonical budget conflict/key; complete Local metadata; live-user reservation | Refused |
| `settle_producer_recovery` | Canonical owner key and failed-incarnation/state fence | Refused ingress |
| `producer_recovery_for_epoch` | Canonical key; complete decoder and expected-owner comparison | Retained read |
| `validation_corrupt_recovery_restriction` | Controlled Local ownership helper | Refused validation call |

## Caller audit and repair boundary

| Surface | Exact ownership behavior | Remaining Shared work |
|---|---|---|
| `transcode/session_request.rs` | `ClusterRecoveryIdentity` carries the typed principal; supersession and durable intent encodings use its canonical key while preserving the historical Local wire encoding. | Source admission proof must travel with Shared dispatch. |
| `media_sessions.rs` | Forwarded start/preparation wire adapters reject mixed principal plus user fields and preserve Local numeric wire compatibility. Takeover compares envelope and persisted principals and refuses Sharing before account lookup. | Typed Source authority replaces the explicit takeover refusal. |
| `http/internal_media_sessions.rs` | Peer authentication precedes payload validation. Shared start/preparation refuses before resource allocation and Local account lookup. Route/recipe identity comparisons retain the principal. | Atomic grant/scope/lifetime/floor admission is required before removing refusal. |
| `http/hls/session_guard.rs` | Guards, request claim settlement and deferred cleanup retain the principal rather than rebuilding it from a numeric user. | Shared Source request/producer admission remains closed. |
| `transcode/manager/create.rs` | Shared recovery cannot become numeric VOD viewer demand; the manager returns an explicit typed-authority refusal. Ordinary Local VOD demand retains the real user. | Propagate typed Shared viewer demand with server-derived authority proof. |
| `vod/serve/create.rs`, `vod/serve/construct.rs` | Audit found that lower-level typed Shared viewer demand could be silently filtered after queue work. Early refusal now covers the common create funnel and cluster-index entry before pool changes, source probing or queue writes. | Actual create/index refusal passed; Shared VOD admission still requires full proof. |
| `state.rs` copy preparation helper | Audit found queue insertion before Shared demand was silently filtered. Early refusal now preserves explicit ownership rather than creating anonymous work. | Actual no-queue refusal passed; proof-bearing Shared background demand is not implemented. |
| `store/background_jobs.rs` | Analysis/artifact viewer joins require a real positive Local principal before serializing user ID and canonical consumer identity. | Shared background admission must not invent an account or discard the principal. |
| Ordinary authentication/telemetry | Authenticated ordinary endpoints and account telemetry remain numeric Local-user surfaces; no Shared grant UUID or zero sentinel is substituted for a user. | Source delivery telemetry needs its separate typed attribution, not an account adapter. |
| Account/grant deletion | Candidate SQL retires Local rows on user deletion and Shared rows on export revocation/deletion, removes pointers/preparations/desired rows and clamps leases. | These candidate triggers remain uninstalled; coordinated migration and restart qualification is required. |

No caller repair is authorized to replace an absent Shared proof with a Local
account, grant UUID, user zero, cached capability boolean or anonymous viewer.
The repairs above only make the current refusal explicit at lower-level entry
points; they do not enable Shared playback. `PlaybackViewerDemand::require_local_authority`
distinguishes Sharing from a nonpositive Local user without substituting an
account. The common VOD create guard runs before the pool cap changes; the
copy/index guards run before engine inspection, source probing or queue writes.

## Source admission has no durable binding yet

The existing sharing schema definition contains Source grant identity and scope in
`sharing_identity`, `sharing_exports` and `sharing_export_libraries`.
Current file authority requires the same query to join `libraries`, `items`
and `files`, correlate the Source/epoch/library/item/file identities and check
`item_identity_watermark.importing = 0`. The S4 server-only `SourceFileWitness`
and guarded fixed-alias revision projection are candidates, not installed
admission authority. A wire file revision, recipe JSON or size/mtime pair
cannot replace the private witness in the actual mutation.

Neither existing adjunct table is a Source admission binding:
`sharing_relay_upstream` records the Recipient's upstream import, generations
and remote session identities; `sharing_delivery_grants` records token,
incarnation, state and deadline. The seven ownership families preserve
principal/request/incarnation/playback identity but do not bind an immutable
Source item/file revision. A Source-only durable binding therefore needs to
use the existing incarnation namespace and retain exact canonical principal,
request/fingerprint/playback and Source/epoch/library/item/file/revision
identity. It must be constructed on the server and rechecked against current
grant/scope, private file witness, installed schema and member floor in the
same admission write. This is an API requirement, not an implemented table.

Capacity must count distinct obligations across held reservations, starting
requests, prepared successors and starting/active routes, including unpublished,
draining and pointer-displaced producers. Neither lease expiry nor an expired
claim/preparation deadline proves an uncertain producer has stopped. The
current maintenance sweep deletes expired starting requests and retired
preparation ledgers; counting only these surviving rows would release an
ambiguous slot. Durable held obligations must survive that cleanup until an
exact authoritative release condition is recorded. Exact replay retains its
original incarnation and consumes no additional slot; predecessor replacement
cannot bypass Source eight or grant four. Retained request/binding bounds are
separate from active-slot caps and must also bound distinct viewer namespaces.

Forwarded start and preparation currently carry principal/incarnation and
recipe context, with session/owner epoch for preparation. Workers need to
resolve the durable Source binding and regenerate private authority after
forwarding, recovery and takeover. They must not accept a serialized private
witness or replace Shared ownership with a numeric viewer. The existing
refusals remain until these proof-bearing paths and atomic capacity behavior
are implemented and qualified.

## Evidence and open completion conditions

The replicated checkpoint has actual three-voter cases for retained Local
request, desired, recovery, preparation, activation, commit, settlement,
renewal/takeover and cleanup. They cover two Shared namespaces, foreign replay,
wrong-session receipt, actual foreign lease/pin preservation and deleted-Local
refusal. Eleven Hiqlite session safety/decoder unit regressions passed. Pinned
all-target check, denied-warning feature Clippy and the normal tracked hook
passed. The companion receipt document records exact commands and timings.

The actual daemon VOD create/index refusal regression passed 1/1 with zero
ignored in 0.16 seconds; it checks unchanged pool cap and empty preparing,
session and rendition registries on absent media. The durable preparation
regression passed 1/1 in 1.54 seconds: Shared and nonpositive Local demand leave
the actual queue empty, then existing Local behavior still joins/promotes the
same durable request. Rust 1.97.1 daemon all-target check passed in 54.69
seconds and denied-warning daemon Clippy passed in 1m26s.

```bash
cargo test --locked -p plurxd \
  shared_vod_demand_refuses_before_pool_queue_or_session_allocation -- --nocapture
cargo test --locked -p plurxd \
  playback_preparation_is_durable_exact_and_independent_of_discovery -- --nocapture
cargo check --locked -p plurxd --all-targets
cargo clippy --locked -p plurxd --all-targets -- -D warnings
```

The Hiqlite Shared terminal case
`sharing_rebuilt_terminal_ack_and_projection_retire_only_exact_shared_owner`
passed against the actual candidate three-voter fixture (1/1, zero ignored,
9.22 seconds). It rejects wrong session/node/epoch acknowledgements, preserves
a foreign grant's corrupt pointer and live lease/pin, replays the exact ack
after maintenance and qualifies ended-only terminal projection. It authorizes
retirement without extending a Shared producer lease. The existing terminal
contract passed 1/1 in 9.66 seconds and all 11 Hiqlite safety/decoder units
passed in 0.01 seconds. Denied-warning feature Clippy passed in 33.46 seconds.

Shared admission remains open until all of these are wired and qualified:

- Installed-schema proof and the coordinated drain/backup/restart/rollback path;
  the bare rebuild is incompatible with actual historical Store writers.
- Server-derived export/grant scope and lifetime proof, atomically rechecked
  with the mutation rather than accepted from a recipient request.
- The all-committed-member floor predicate in the same Raft write, with bounded
  roster and freshness inputs; preflight observations cannot authorize later writes.
- Source cap eight and per-grant cap four, counting prepared, unresolved and
  displaced obligations with the distinct unavailable/capacity classifications.
- Proof-bearing worker, VOD, background and delivery attribution paths before
  advertising `sharing_session_principal_v1` or enabling Shared ingress.

A Local-compatible rebuilt runtime and a typed retained Shared row are only
parts of S3. This census does not qualify a migration installation, mixed
cluster bridge, historical daemon process, rollback or Shared admission.
