# Playback principal census — retained ownership and remaining source admission

**Status:** open · **Scope:** S3 backend and caller ownership audit

This answers which session operations preserve a complete playback principal,
which callers still refuse Shared execution, and what evidence remains before
Source admission can be enabled. It supplements [the implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md)
and [the replicated runtime receipts](SHARED-LIBRARIES-REPLICATED-PRINCIPALS.md).
Canonical Local compatibility and readable Shared retention do not establish
Shared grant, scope, lifetime or member-floor authority.

## Audited snapshots and boundaries

The integrated backend and caller snapshot is root
`f65eaace4b5b6372f3212cd8912c0620da26c910`, extracted with `git archive`
without Git metadata or credentials. It includes the Hiqlite terminal/ACK
checkpoint `81f4cd09e7e96e4b004395b244844d52beb78b09` and the caller repairs.
Both backends implement the same 36 Store methods. SQLite's two residual reads
are closed there: `desired_within` selects the canonical Local key and decodes
the complete persisted principal with `from_projection`;
`validation_playback_pointer_desired_revision` uses the canonical ownership
helper. The malformed retained desired-metadata regression rejects the row
instead of reconstructing Local metadata. Terminal acknowledgement pointer
cleanup now correlates canonical owner/playback in both backends.

The initial immutable caller snapshot was
`e99099a170a0610d7b3450bb1356e62eb4897b0a`; the lower-level copy/VOD repairs
are qualified in the integrated snapshot above. The Source witness seam is
S4 commit `ae626d8e6235268a1b32cac25bf07c40adb765f6` and remains separate from implemented Source admission.

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
corresponding SQLite methods in root `f65eaace4b5b6372f3212cd8912c0620da26c910`. “Refused” means an explicit
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
and guarded fixed-alias revision projection are committed read seams, not
installed admission authority. A wire file revision, recipe JSON or size/mtime pair
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

## Proposed server-only claim API and transaction

The complete worker admission and producer retirement API below remains a
design proposal. The narrower candidate reservation API described at the end
of this document now implements initial binding/claim and proven
never-dispatched release. It preserves the existing incarnation namespace and
the seven principal families; it does not enable a Shared adapter.

The proposed Core API is deliberately separate from Local request admission:

```rust
async fn claim_source_media_session(
    &self,
    intent: &SourceSessionIntent,
    proof: &SourceAdmissionProof,
) -> Result<SourceClaimOutcome, StoreError>;

async fn source_session_binding(
    &self,
    incarnation: &SessionIncarnationId,
) -> Result<Option<SourceBindingHandle>, StoreError>;

async fn release_source_reservation(
    &self,
    proof: &SourceProducerRetirementProof,
) -> Result<SourceReleaseOutcome, StoreError>;
```

These are new proposal names, not existing signatures. `SourceSessionIntent`
has private fields containing the validated Sharing principal, request ID,
fingerprint, playback ID, existing incarnation ID, Source/epoch/library/item/
file identity and authenticated `FileRevision`. Its factory derives those
identities from an authenticated Source grant and current `SourceFileWitness`,
checks the wire revision against the Source-purpose HMAC and compares every
request identity component. Neither this type nor the proof or handle has
`Serialize`, `Deserialize` or `Debug` exposing its private witness.

`SourceAdmissionProof` owns current authenticated grant context, the private
file witness, the bounded committed-member roster/freshness observation and
the current server-derived resource policy. Private constructors accept
results from the Source authentication, S4 witness and membership paths;
they do not accept a recipient-created witness, boolean floor or free-slot
count. An immutable `SourceBindingHandle` identifies the persisted binding;
it is not reusable current authority. Assignment, preparation, activation,
publication, renewal, recovery and takeover must obtain fresh proof and
re-evaluate its predicates in their own mutations.

The minimal membership seam factors the existing quorum-observation helper
into a private observation-returning implementation. Existing bool readiness
methods remain advisory wrappers. The proposed
`MembershipManager::observe_source_admission_members()` hardcodes
`SharingMemberFloor::PrincipalAndCatalogue` and obtains the actual local Raft
ID from `replicated_inner().identity`, not a caller argument. Its opaque result
has private bounded roster JSON, membership log/config identity, query and
completion timestamps and freshness cutoff. The factory retains before/after
leader/config/ID checks and freshness at quorum-read completion. Only a
crate-visible method exposes binds and the closed composition of
`sharing_member_guard_predicate(PrincipalAndCatalogue, ...)` with
`sharing_member_transition_absence_predicate()` for the mutation. No public
constructor accepts JSON, a caller-selected Raft ID or a cached bool. The
object needs a freshness deadline checked at use; it still cannot authorize
a write without the SQL predicates.

`SourceClaimOutcome` needs separate acquired, original in-flight, original
resolved, conflict, unavailable and capacity variants. Capacity names the
Source slot, grant slot, unresolved-start or resource bound. A mismatched
retry is conflict; missing schema, scope, witness or member floor is
unavailable. Exact replay is checked before caps because it adds no slot,
but it still needs current authority before returning a playable result.
It returns the original incarnation, never the retry's new proposed ID.

The proposed FK-free `sharing_source_session_bindings` has incarnation as its
primary key and a unique `(owner_key, request_id)` identity. It stores complete
grant/viewer/request/fingerprint/playback and Source/epoch/library/item/file/
revision identity, `reservation_state` (`held` or `released`), nullable
`start_resolved_at_ms` and an exact release receipt. IDs and the HMAC revision
are persisted; the potentially 2 MiB private projection is regenerated and
passed only to each atomic comparison. Current source identity and a revision
equal to the immutable binding are required on every extending write.
`start_resolved_at_ms` is set by exact publication or authoritative terminal
resolution, so maintenance cannot erase the grant's two-start bound merely
by deleting an expired request row.

The proposed addition to the contract's §4 adjunct table is:

| Adjunct | Key and purpose | Lifecycle owner |
|---|---|---|
| `sharing_source_session_bindings` | Existing canonical incarnation; immutable Source file/request binding and remote capacity reservation | The existing seven families retain session, route and producer lifecycle; this FK-free row only fences identity, dispatch/resolution accounting and exact reservation release. |

This row is necessary because neither existing adjunct nor a media request
persists immutable Source file authority, and ordinary request/route cleanup
cannot prove physical resource release. It must not grow another session,
relay or producer state machine. A monotonic `dispatch_generation` reservation
fence, initially zero, is advanced in the same guarded assignment before the
first queue/producer dispatch. The never-dispatched release CAS requires zero
and no existing dispatch authority; it cannot rely on an absent process-local
registry entry. Held rows survive grant/user deletion and maintenance until
explicit release; no FK cascade can erase an uncertain reservation. Exact
receipt and released-row retention are bounded independently of active caps.

The capacity query is a template shared by both backends. Named parameters
are explanatory; Hiqlite must lower them to numbered parameters in actual
first-appearance order and validate the resulting SQL. The `UNION` removes
duplicate representations of one obligation; inconsistent grant projections
must fail closed rather than authorize a new reservation.

```sql
WITH obligations(incarnation_id, grant_id) AS (
  SELECT incarnation_id, share_grant_id
    FROM sharing_source_session_bindings WHERE reservation_state = 'held'
  UNION
  SELECT incarnation_id, share_grant_id FROM media_session_requests
    WHERE principal_kind = 'sharing' AND state = 'starting'
  UNION
  SELECT incarnation_id, share_grant_id FROM media_sessions
    WHERE principal_kind = 'sharing' AND state IN ('starting', 'active')
  UNION
  SELECT staged_incarnation_id, share_grant_id
    FROM media_session_preparations WHERE principal_kind = 'sharing'
), counts AS (
  SELECT count(*) AS source_slots,
         coalesce(sum(grant_id = :grant), 0) AS grant_slots
    FROM obligations
), pending AS (
  SELECT count(*) AS grant_starts FROM sharing_source_session_bindings
    WHERE share_grant_id = :grant AND reservation_state = 'held'
      AND start_resolved_at_ms IS NULL
)
SELECT source_slots, grant_slots, grant_starts FROM counts, pending;
```

There is no lease, claim-expiry, preparation-deadline, current-pointer or
catalogue-epoch exclusion. Old-epoch held obligations still occupy Source
resources. Admission also refuses unbound or inconsistently bound Shared
obligations: a partially installed binding schema cannot authorize new work.

For a fresh request, the first statement inserts the immutable binding using
`INSERT ... SELECT ... WHERE`, requiring all of these in that statement:

- No existing binding/request for the same `(owner_key, request_id)` and no
  existing incarnation belonging to any other request or principal.
- Installed schema and the same-write membership-floor predicate.
- Current active export, Source/epoch, effective library scope, movie/episode
  item, movie/show library, file ownership and `importing = 0`, using S4's fixed
  `e/s/x/l/i/f` joins and `file_revision_guarded_projection_sql()` equality
  against the server-private witness. The loose legacy raw-size predicate is
  insufficient.
- `source_slots < 8`, `grant_slots < 4`, `grant_starts < 2`, applicable lower
  server resource policy, and independently specified retained-row bounds.

Concretely, reuse `obligations`, `counts` and `pending` above, append this
authority CTE and replace the final count `SELECT` with the binding insert.
The three brace fragments are closed, server-generated SQL builders, not wire
strings: installed-schema proof, the opaque combined floor/absence predicate,
and S4's guarded fixed-`f/i/s` file projection. Retained-row and any configured
lower replicated Source policy predicates must also be implemented before this
template can authorize work; they are not supplied by a free-slot parameter.

```sql
, authority AS (
  SELECT 1 FROM sharing_exports e
    JOIN sharing_identity s ON s.singleton = 1
    JOIN sharing_export_libraries x ON x.grant_id = e.id
    JOIN libraries l ON l.id = x.library_id
    JOIN items i ON i.library_id = l.id
    JOIN files f ON f.item_id = i.id
   WHERE e.id = :grant AND e.token_hash = :credential_hash
     AND e.state = 'active'
     AND s.server_id = :source AND s.catalogue_epoch = :epoch
     AND CAST(i.library_id AS TEXT) = :library
     AND CAST(i.id AS TEXT) = :item AND CAST(f.id AS TEXT) = :file
     AND i.kind IN ('movie', 'episode') AND l.kind IN ('movies', 'shows')
     AND EXISTS (SELECT 1 FROM item_identity_watermark
                 WHERE singleton = 1 AND importing = 0)
     AND {GUARDED_FILE_PROJECTION} = :private_projection
     AND {INSTALLED_SCHEMA_PROOF} AND {COMBINED_MEMBER_FLOOR_AND_ABSENCE}
)
INSERT INTO sharing_source_session_bindings
  (incarnation_id, owner_key, share_grant_id, share_viewer_key,
   request_id, request_fingerprint, playback_id,
   source_server_id, catalogue_epoch, library_id, item_id, file_id,
   file_revision, reservation_state, start_resolved_at_ms,
   dispatch_generation, created_at_ms)
SELECT :incarnation, :owner_key, :grant, :viewer,
       :request, :fingerprint, :playback,
       :source, :epoch, :library, :item, :file,
       :revision, 'held', NULL, 0, :now
  FROM counts, pending
 WHERE EXISTS (SELECT 1 FROM authority)
   AND :owner_key = 'share:' || :grant || ':' || :viewer
   AND source_slots < 8 AND grant_slots < 4 AND grant_starts < 2
   AND NOT EXISTS (SELECT 1 FROM sharing_source_session_bindings
                    WHERE incarnation_id = :incarnation
                       OR (owner_key = :owner_key AND request_id = :request))
   AND NOT EXISTS (SELECT 1 FROM media_session_requests
                    WHERE owner_key = :owner_key AND request_id = :request)
   AND NOT EXISTS (SELECT 1 FROM media_sessions
                    WHERE incarnation_id = :incarnation);
```

The second statement creates the canonical starting request from that exact
binding, with every immutable field matched and the authority predicates
re-evaluated. `ON CONFLICT(owner_key, request_id) DO NOTHING` cannot treat a
different binding as successful replay. The final transaction assertion
requires both exact rows to exist; a missing second row rolls back the first
insert. Both backends need this assertion, including Hiqlite transactions
whose individual guarded writes can return zero affected rows. Classification
reads after failure may explain refusal; they never authorize allocation.

Its insert is likewise conditional, not `VALUES` followed by an optimistic
post-read. `authority` below means the same current closed predicates in the
same transaction, with the fixed aliases and freshly derived private witness:

```sql
-- Prepend the authority CTE again: CTEs are statement-local.
INSERT INTO media_session_requests
  (owner_key, principal_kind, user_id, share_grant_id, share_viewer_key,
   request_id, request_fingerprint, playback_id, state,
   claim_expires_at_ms, incarnation_id, owner_node_id, response_json,
   updated_at_ms)
SELECT b.owner_key, 'sharing', NULL, b.share_grant_id, b.share_viewer_key,
       b.request_id, b.request_fingerprint, b.playback_id, 'starting',
       :claim_expires, b.incarnation_id, NULL, NULL, :now
  FROM sharing_source_session_bindings b
 WHERE b.incarnation_id = :incarnation AND b.owner_key = :owner_key
   AND b.request_id = :request AND b.request_fingerprint = :fingerprint
   AND b.playback_id = :playback AND b.share_grant_id = :grant
   AND b.share_viewer_key = :viewer
   AND b.source_server_id = :source AND b.catalogue_epoch = :epoch
   AND b.library_id = :library AND b.item_id = :item AND b.file_id = :file
   AND b.file_revision = :revision AND b.reservation_state = 'held'
   AND b.dispatch_generation = 0 AND b.start_resolved_at_ms IS NULL
   AND EXISTS (SELECT 1 FROM authority)
ON CONFLICT(owner_key, request_id) DO NOTHING;
```

Before returning acquired, the conditional rollback assertion checks exact
binding and request agreement, including the same incarnation, fingerprint,
playback, grant and viewer. The assertion's deliberate constraint failure
must be distinguished from arbitrary SQL errors. A newly inserted binding
with no exact request is never committed. The binding and request checks are
also required at subsequent assignment, with a monotonic dispatch fence set
before queue/producer ownership can leave the selected worker.

Existing lower hardware/software admission uses process-local `Admissions`
in `transcode/manager/start.rs`; there is no SQL inventory of all ordinary
Local and remote hardware permits. The daemon therefore must acquire and
retain the actual selected worker resource permit before enqueue/producer
allocation, in addition to the atomic Source/grant reservation. A proposed
replicated lower Source-slot policy must be read and rechecked in the claim;
an observed process free-slot count is not such a policy. Resource refusal
before allocation must reconcile the reserved incarnation using an explicit
never-started receipt. Cancellation or a timed-out worker response leaves a
held ambiguous obligation until reconciliation, not an automatic release.

`SourceProducerRetirementProof` must correlate canonical owner, incarnation,
session UUID, node/owner epoch and resource generation with a recorded exact
stop/never-dispatched receipt. Release requires confirmed producer reap and
writer settlement, or an exact proven-never-dispatched CAS. Route-ended state,
terminal projection, timeout and raw lease expiry alone are insufficient.
Release is a same-write
held-to-released CAS plus durable receipt, idempotent only for that exact
receipt. A user control acknowledgement is not by itself proof that an
encoder returned its hardware permit. Binding cleanup must retain held rows;
released bindings and exact replay identities need a bounded retention policy
that cannot recreate an ambiguous original producer after maintenance.

The concrete rolling callback seam is
`transcode/rolling/retirement.rs::own_rolling_retirement`. Its detached owner
holds the exact Session Arc, actor Terminal settlement and child-transition
gate. It retries termination until the supervised child is confirmed reaped;
`finish_rolling_retirement_after_reap` then releases hardware/software
admission. It settles scratch/copy writers and removes only the matching
registry Arc before publishing `retirement_cleanup_finished` and the shared
settlement. A durable Source release retry owner can capture the exact binding,
session/node/epoch and producer attempt after those physical facts, before
the completion signal. Failed Raft release keeps the reservation held and
retries independently of the stop caller or event telemetry. It must never
hold registry/child-transition locks across the Raft call.

`manager/control.rs::stop_session_until` transfers ownership before waiting;
a timeout stops only the caller's wait while detached teardown continues.
Its false/timeout result is not a release proof. Likewise
`settle_exact_published_child` confirms one attempt and releases its permits
but deliberately preserves the published generation for readers or future
respawn; it is not whole-session retirement and must not free the remote slot.

VOD has a distinct seam in `vod/serve/end.rs::spawn_terminal_cleanup`: exact
reader detach, optional terminal commit, matching cleanup/rendition Arc
compaction and heavyweight capture drop precede its completion guard.
`end().await` joins that cleanup; `begin_end_detached` merely transfers it.
A common rendition producer can remain for other readers/cache, so this is
not proof of global encoder reap. Shared VOD remains refused until the
reservation contract distinguishes exact remote consumer/writer settlement
from shared rendition producer ownership and qualifies both. One remote
tombstone cannot release another reader's physical resource permit.

Qualification must race Source nine across grants, grant five with active/
prepared occupancy, and the separate two-start bound on actual voters. It
must exercise full-capacity exact replay, changed-identity conflict, expired
ambiguous claims, prepared/unpublished/draining/displaced producers, stale
witness/scope/floor refusal without allocation, rollback of a failed second
statement, and exactly one slot reopened by an authoritative release. These
are required tests, not receipts already obtained.

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


## Candidate Source reservation checkpoint

`SharingSourceSessionStore` is a separate native-backend trait. Its three
implemented methods are `prepare_source_session_intent`,
`claim_source_media_session` and `release_source_never_dispatched`. The common
`MediaSessionStore`, HTTP ingress, forwarded worker requests and allocation
queues remain unchanged by this checkpoint. No schema version, installer or
advertised capability is changed.

The intent factory loads a current grant/credential/scope-authorized
`SourceFileWitness` and the existing sealed CatalogueRevision purpose key.
It opens that key with the local `CredentialKey` and compares the received
`FileRevision` with the HMAC of the actual current private witness. The intent
and binding handle have no wire encoding or Debug representation. Source item,
file and library IDs remain exact bounded positive decimal strings. The
conditional write independently correlates credential hash, grant, effective
library, current Source/epoch, importing state, the guarded private file
projection and the same persisted key envelope. A captured witness alone
cannot authorize another grant.

Every proposal checks the exact seven frozen principal table definitions after
SQLite's quoted rename and the exact Source adjunct table, indexes and
triggers. The opaque server-created `SourceAdmissionMembers` supplies the
combined principal/catalogue capability floor and unresolved membership
transition predicates. Admission also checks the saved `sharing_enabled`
setting in the same write. Its bounded SQL comparison accepts text of at most 64 bytes and the
canonical enabled spellings `1`, `true`, `yes`, and `on` with surrounding ASCII
spaces; other persisted representations refuse admission without changing the
saved setting.

A transaction inserts the immutable held binding, inserts the matching
canonical Sharing request, and executes a conditional NOT NULL assertion. If
either guarded insertion cannot establish the exact pair, that assertion rolls
back the transaction. Only that named assertion failure is classified as a
lost proposal; other database failures propagate. An existing immutable
request binding is examined before all caps. Exact retries return its original
incarnation; changed immutable identity conflicts. Missing or cross-principal
request records refuse usable replay and are not repaired. A retained resolved
response additionally requires a matching principal/owner live published route;
that branch is not qualified as a complete Shared worker lifecycle here.

The occupancy union deduplicates held binding IDs, Sharing starting requests,
Sharing starting/active routes and Sharing preparations. It excludes no entry
because its deadline, lease, owner epoch or pointer has changed. Thus active
routes with drain deadlines or displaced pointers and held older-epoch rows
remain obligations. Limits are eight Source slots, four per grant and two
unresolved starts per grant. Independent retained bounds are 4096 Source binding
rows and 4096 Sharing request rows; full bounds preserve exact replay. Ordinary
request expiry or failed request state does not release a held slot.

The adjunct stores identity and capacity bookkeeping, not a parallel media
lifecycle. Immutable identity cannot change, dispatch/resolution proof cannot
move backwards, and a held row cannot be deleted. The only implemented release
is an exact never-dispatched CAS: dispatch generation zero, unresolved start,
no route of any state, no preparation/recipe/pointer, no cache pin or session
lease even if expired, and no assigned, resolved or foreign-principal request.
It records an internal deterministic release receipt and fails the matching
unassigned request atomically. Exact release retry succeeds after sharing is
disabled or revoked. A resolved start, ended route, timeout or expired lease
cannot mint this proof. No reaped-producer callback is implemented.

The selected worker's actual RAII admission permit, producer dispatch fence,
proof-bearing activation/renewal paths and reaped-producer/writer-settlement
release remain prerequisites before this reservation can authorize work. The
candidate calls the opaque floor factory's clock check with fresh execution-entry
wall time, but cannot prove an arbitrary Raft queue delay stays within its five
second observation window. Hiqlite forbids nondeterministic SQLite time
functions. The integrated membership factory now supplies a monotonic generation
singleton and exact table/trigger-shape checks in the conditional write. It
invalidates an observation across completed intent or directory identity
mutations even after their visible floor is restored. Current capability
predicates also recheck every active SQL member. The deterministic queued-clock
limitation and remaining worker proof remain explicit; Shared front doors stay
closed.

Focused candidate commands (Rust 1.97.1, `hiqlite-contract-tests` enabled):

```bash
cargo test --locked -p plurx-core --features hiqlite-contract-tests \
  --lib store::sharing_source_sessions -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests \
  --test store_contract sharing_source_reservations_three -- --nocapture
cargo check --locked -p plurx-core --features hiqlite-contract-tests --all-targets
cargo clippy --locked -p plurx-core --features hiqlite-contract-tests \
  --all-targets -- -D warnings
```

The SQLite matrix exercises memory and pooled stores, pending-start/Source/grant
limits, expired/failed request holds, exact and retired retry, switch/file races,
missing request and altered schema refusal, injected second-insertion rollback,
foreign expired pin preservation, and the retained bound. Resolved-start rows
in the grant-cap test are explicit fixture bookkeeping, not evidence of a live
producer being reaped. The replicated fixture uses three actual voters and the
full quorum observation factory; its reservations allocate no producer.

The initial checkpoint is based on `b84f87e142974d34c4e01d84e4d978b34947d54d`
(the pinned S4 witness plus opaque floor checkpoint). The six focused library
regressions passed with zero ignored tests in 8.51 seconds. The actual
three-voter Source race/capacity regression passed with zero ignored tests in
9.83 seconds. Catalogue lint and all four documentation index tests passed.
The library matrix also verifies that a changed retained request cannot return
conflict after its grant is disabled; current authority refusal takes priority.
These are reservation receipts; they do not qualify live producer dispatch,
physical retirement or schema activation.


The generation follow-up integrates `a6a60d53f` as `71e9958ca`. Its actual
three-voter Source regression passed in 10.20 seconds with zero ignored tests.
A valid, production-guarded voter-promotion intent is inserted and resolved
after the observation; the actual committed configuration already proves the
voter outcome. With no open intent and a restored visible floor, the old
observation creates zero Source rows. A fresh actual quorum observation admits
work. This is a completed guarded SQL-intent race, not a new live Raft membership
configuration change. The same test additionally races identical request keys
with distinct proposed UUIDs: exactly one acquires and the other returns its
original in-flight incarnation. All six Source library tests passed with zero
ignored tests in 8.43 seconds, including oversized saved-switch refusal without
changing the saved value. The exact combined feature all-target check passed
in 29.39 seconds. Earlier receipts remain scoped to their earlier factory.

## Candidate local worker assignment

`assign_source_dispatch(binding, CredentialKey, SourceAdmissionMembers)` commits
this actual local worker's assignment before producer queueing. It resolves the
opaque observation's local Raft ID to a current active SQL node, rereads the
canonical starting request and immutable held binding, obtains the current
stored grant credential and file witness, and verifies the existing Source
purpose key against the bound FileRevision. The old peer token is not retained
as binding identity: benign credential rotation does not invalidate this worker
assignment while the grant, effective scope and bound file remain authorized.

One transaction advances dispatch generation from zero to one and assigns the
canonical request owner. Its final assertion requires the complete pair plus
current saved switch, exact candidate shapes, original member observation and
generation, active local node identity, removal fence, current grant/scope,
private file projection and sealed key. A lost predicate rolls back both rows;
an incomplete retained pair refuses without repair. Exact retry returns the
same opaque assignment only for the same actual local worker. A different
actual member cannot use the retained binding to acquire ownership.

The assignment carries the original opaque observation. Its freshness check
must succeed again before actual producer queue admission; a successful SQL
assignment does not prove that a delayed Raft apply happened within five seconds.
The assignment is neither an encoder permit nor an active route lease. It does
not authorize activation, renewal, takeover, remote scheduling or physical
capacity release. The existing never-dispatched callback refuses once the
assignment generation advances. Shared lifecycle entry points remain closed.

The assignment checkpoint is based on
`4a82e9cbc14e72b4fb393f63a4d897433668bb76`. All seven Source library tests
passed with zero ignored tests in 13.77 seconds, including memory and pooled
assignment/rotation, exact replay, saved-switch refusal, no repair of an
incomplete pair and expiry of the original observation before queue admission.
The actual three-voter regression passed with zero ignored tests in 10.29
seconds: concurrent assignment/replay at a full Source eight-slot bound returns
the same actual node and original incarnation; a second actual voter refuses
ownership, current credential rotation remains valid, and held capacity stays
eight. No producer, route or lease is allocated by assignment. The exact
feature all-target compiler check passed in 29.75 seconds. These tests qualify
candidate assignment, not the still-closed activation or physical worker path.

## Candidate proof-bearing first activation

`prepare_source_activation_authority` refreshes the actual worker assignment's
full held binding, canonical starting request, effective grant/current stored
credential, Source file witness, sealed purpose key and opaque original member
observation. It returns a closed `SourceSessionWriteAuthority` with no wire
constructor. A retained binding alone cannot mint it. Source activation on the
ordinary `activate_media_session` entry remains refused.

The narrowly named `activate_source_media_session` has candidate SQLite and
Hiqlite implementations; the default method remains unavailable for other
backends. Each backend's Local and proof-bearing Source entries delegate to its
existing activation algorithm.
The Source selector binds the canonical owner key, inserts literal NULL
`user_id`, and persists the full Sharing principal. It accepts only a first
start fenced against an absent predecessor. Replacement, preparation, renewal,
takeover and recovery require their own qualified authorities and remain closed.

`source_activation_guard` supplies the first assertion in the same lifecycle
transaction. It checks the immutable binding and current assignment generation,
canonical request owner, original membership freshness/generation, saved switch,
grant/scope/file/key snapshot, actual local node and removal fence. Existing
target route, lease and pin rows must have the exact Sharing principal, node,
epoch and session lineage; foreign rows refuse before any lease extension. A
known assertion refusal returns unavailable and rolls back every effect, while
unrelated database faults propagate. Exact replay checks current authority and
returns the retained route without extending its lease.

The actual three-voter case interleaves saved switch-off after the preliminary
pointer read and before the committing transaction: no session, pointer or lease
is created. It also injects a foreign target lease and pin, verifies both
activation and proof mint refusal, and checks the exact expiry/pin rows remain.
A fresh valid authority then creates one fully typed blocked Sharing route and
read-only exact replay. All eight Source obligations remain held, and the start
stays unresolved. Publication and physical producer queueing remain separate
unqualified seams; this checkpoint advertises no capability and installs no
schema.

On the assignment base `7b8d25fd70c44e39bc362388955ba44157e5935e`, the actual
voter case passed with zero ignored tests in 10.63 seconds. All eight Source
library cases passed with zero ignored tests in 12.87 seconds; memory and pooled
SQLite prove assertion rollback, file replacement refusal and preservation of a
genuine storage fault. The existing Local activation/preparation/settlement
contract through `dyn Store` passed with zero ignored tests in 11.64 seconds.
The exact feature all-target compiler check passed in 28.16 seconds.


The SQLite counterpart is based on
`5982f541bd9ffa22c22909de7f004f0b512cee17`. Its owned writer closure carries a
clone of the original closed authority, without resetting observation times,
and calls the guard after acquiring the writer connection and starting the
transaction. Both backends insert the same complete Sharing metadata with NULL
local user ID. The authority exposes its original-observation freshness check
for the required postcommit check before actual producer queue admission; it
remains separate from current authorization and the actual physical permit.

All nine Source library regressions passed with zero ignored tests in 17.80
seconds. The new memory and pooled SQLite activation case covers the ordinary
Shared entry refusal, saved-switch and private-file races, foreign lease/pin
preservation and mint refusal, foreign viewer tuple refusal, genuine writer
fault rollback, one blocked Sharing route, and exact replay without lease
renewal. The held binding and unresolved start remain; the never-dispatched
callback cannot release an assigned incarnation. The existing Local lifecycle
contract through `dyn Store` passed with zero ignored tests in 11.72 seconds.
Exact feature all-target check passed in 29.79 seconds and Clippy in 33.68
seconds. The prior actual-voter receipt remains scoped to the Hiqlite checkpoint;
this counterpart changes its SQLite algorithm and closed snapshot cloning only.

## Candidate current-owner renewal

`prepare_source_owned_route_authority` is a separate read-only factory for a
resolved, ready Source route. It refreshes the current effective grant, stored
credential hash, private file witness and existing Source key; the immutable
binding and assignment supply lineage, not current permission. The canonical
resolved request, binding resolution marker, session capability, actual local
node, epoch one and unexpired job lease must agree. The closed authority captures
both lease expiry and revision and the original start of the newly obtained
membership observation. The dispatch assignment's earlier admission timestamp
does not permanently prevent later renewal.

The proof-bearing `renew_source_media_session` uses the existing backend renewal
algorithm with an assertion first in the same transaction. It rechecks saved
Sharing choice, current grant/scope/file/key, exact candidate schemas, membership
floor/generation, local node/removal fence and the captured route/lease tuple.
Only an already resolved, published, active route can extend its live lease and
frontiers. A reused lease snapshot or delayed member observation refuses. The
ordinary Shared renewal entry remains closed, and the held Source capacity
obligation is retained. No expiration, terminal state or renewal releases it.

This stage does not implement publication or projection completion. Possessing
an owned SQL lease is not proof that a physical producer and its admission permit
exist. The future daemon actor must retain the actual permit, child job and
registered session before invoking a separately qualified readiness transition.
Renewal fixtures explicitly seed a resolved ready route; they do not claim that
production publication or physical dispatch is available.

At the renewal checkpoint, the Source claim's resolved replay query required
`publication_ready_at_ms > 0`, although publication resolves a request at
readiness zero. The SQL publication checkpoint below closes that predicate and
qualifies exact replay. Actual physical readiness remains a separate daemon
owner obligation before production dispatch can use this transition.

The renewal checkpoint is based on
`2925a231b6fde645ccb5194c25233236bfb174bb` and was checked with pinned Rust
1.97.1. All ten Source library regressions passed with zero ignored tests in
22.03 seconds. The memory and pooled case proves switch/file refusal, exact
foreign lease and unrelated pin preservation, rollback of lease extension when
the route writer faults, successful frontier/lease renewal, refusal of a reused
lease revision, expiry of the newly obtained five-second observation, and fresh
renewal from an older dispatch assignment. Held capacity remains retained.

The actual three-voter Source regression passed with zero ignored tests in
10.89 seconds, including current-owner renewal at eight held obligations,
ordinary Shared refusal, same-write switch-off refusal without lease extension,
stale lease proof refusal and fresh proof minting. It uses fixture-only resolved
readiness and does not dispatch a producer. The unchanged Local committed
successor renewal contract passed with zero ignored tests in 9.64 seconds.
Feature all-target check passed in 25.24 seconds and Clippy with denied warnings
in 30.01 seconds. The exact focused commands were:

```sh
cargo test --locked -p plurx-core --features hiqlite-contract-tests --lib store::sharing_source_sessions -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract sharing_source_reservations_three_voters_atomic_claim_caps_replay_and_release -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract media_session_a_committed_successor_can_renew_and_outlives_its_preparation -- --nocapture
cargo check --locked -p plurx-core --all-targets --features hiqlite-contract-tests
cargo clippy --locked -p plurx-core --all-targets --features hiqlite-contract-tests -- -D warnings
```


## Actual VOD producer lifetime checkpoint

The VOD segment producer now registers an opaque identity while attaching its
actual child, descendant job and encode permit. Retirement transfers those
objects into one detached process owner, so cancelling the caller waiting for
retirement cannot drop admission. A failed child wait retries with the same
owner and retains its resources. A cancelled or panicked writer task cannot
certify settlement: its missing confirmation leaves the bounded owner
quarantined. Successful child wait and joined stdout/diagnostic readers are
both required before the owner drops admission and mints its generation
receipt. An old receipt cannot retire a successor generation.

The stdout generation task requests exact retirement before waiting for its
own writer barrier. Diagnostics are bounded by a five-second deadline after
retirement starts, preserving ordinary long-running producer diagnostics.
Lifecycle outcome handling follows confirmed process and raw-reader
settlement. The existing scheduling policy, metrics and Local VOD namespaces
are retained.

This receipt proves one actual process generation only. It neither authorizes
Source binding release nor proves viewer demand, durable terminal projection,
route cleanup or all downstream writers settled. Source creation and
publication remain closed. Missing-init head regeneration is a separate helper
and is not qualified by this segment-producer receipt. A future Source actor
must retain each immutable binding and every associated physical generation,
then require its own last demand and complete terminal settlement before a
retirement writer can release its durable obligation.

The checkpoint merges frozen Root
`4ebb852268cff912f21e8e3bbf95917baf0186ca` as
`7f1b1c200e2a058b6ad92c962cb674efbebfd0dd`. Pinned Rust 1.97.1 checked the
actual daemon. The new cancellation regression uses an actual spawned child,
actual encode admission, an injected first wait error and an independently
paused retry. Admission remains held after caller cancellation, wait error and
successful wait before writer settlement; it is released after both barriers,
and the old exact receipt leaves a new live generation untouched. It passed
with one test and zero ignored tests in 9.81 seconds on the final source. All twelve existing
process-slot contracts passed in 0.03 seconds. Their macOS process-state
inspection requires running outside the restricted sandbox. The actual ffmpeg
held-capacity regression passed in 2.10 seconds and the NTSC forward/backward
restart regression passed in 3.08 seconds, each with one test and zero ignored
tests. Daemon all-target check passed in 54.92 seconds. Clippy with denied
warnings passed in 1 minute 10 seconds.

```sh
cargo test --locked -p plurxd --bin plurxd source_registered_producer_retains_real_permit_until_wait_and_writers -- --nocapture
cargo test --locked -p plurxd --bin plurxd prodrun::tests -- --nocapture
cargo test --locked -p plurxd --bin plurxd encoded_vod_held_capacity_keeps_cached_gets_open_and_rechecks_seek_after_reap -- --nocapture
cargo test --locked -p plurxd --bin plurxd encoded_vod_ntsc_gets_decode_after_forward_and_backward_restarts -- --nocapture
cargo check --locked -p plurxd --all-targets
cargo clippy --locked -p plurxd --all-targets -- -D warnings
```

## Source actor identity and production store boundary

The actual worker association retains the complete immutable binding: grant and
viewer principal, incarnation, request and fingerprint, playback, Source server
and catalogue epoch, library, item, file and revision. Read-only comparison
checks all those values, independently of the mutable release marker. Dispatch
comparison additionally checks the assigned node and dispatch generation. A
new current membership observation may refresh permission without changing
that immutable lineage. Neither equality nor these read-only accessors mint
write, physical admission, readiness or retirement authority.

`SharingSourceSessionStore` is part of the full Store boundary, allowing the
server-held storage object to invoke the actual current intent, assignment and
activation factories. The concrete backend implementations remain the same.
This does not enable ordinary Shared queue admission.

Ordinary production daemon startup uses `select_daemon_store` and takes its
actual `membership_manager`. The one-server production topology is one real
Hiqlite voter with its actual local identity and current committed roster.
`cluster::open_store` is the legacy/recovery SQLite path; interrupted activation
recovery returns `SelectedBackend::SqliteRecovery` with
`MembershipManager::unavailable()`. That path cannot provide Source admission
and must report the exact readiness prerequisite: an activated replicated
store with the current principal/catalogue member floor. The saved Sharing
switch remains the user's choice. The memory and pooled SQLite candidate
contracts qualify backend atomic behavior using explicit closed test fixtures;
they do not prove a production SQLite membership authority or justify a fake
single-member roster.

The additive identity/bound checkpoint is based on merged Root
`719fde0bd87828322219fc35e55f26126c35ccba`, incorporated as
`8c514e1838d0059d0d8f725d1c445a259943afe3`. The identity regression passed
with one test and zero ignored tests; it changes each association dimension
independently and also distinguishes dispatch node and generation. This is a
pure identity contract, not a Source actor or database admission receipt.
Pinned Rust 1.97.1 daemon all-target check passed in 1 minute 3 seconds,
daemon Clippy with denied warnings in 1 minute 21 seconds, and feature Core
all-target Clippy in 40.27 seconds. Documentation index tests and catalogue
lint passed. The exact commands were:

```sh
cargo test --locked -p plurx-core --features hiqlite-contract-tests --lib source_association_identity_never_collapses_grant_viewer_request_or_file -- --nocapture
cargo check --locked -p plurxd --all-targets
cargo clippy --locked -p plurxd --all-targets -- -D warnings
cargo clippy --locked -p plurx-core --all-targets --features hiqlite-contract-tests -- -D warnings
```


## Source publication transaction and ready-zero replay

Based on `c9bc7bf876ab61ec96dc13db746c2f28ac70b77e`, the candidate
`SourcePublicationAuthority` factory captures the actual current binding,
canonical request, private file/key/grant witness, node/lease/pointer lineage
and a fresh opaque member observation. It distinguishes blocked starting and
published resolved phases. This is SQL permission; it does not prove physical
producer readiness and the ordinary Shared publication entry remains closed.

The guarded transaction changes the exact blocked route to readiness zero,
resolves its canonical request from that route, and records start settlement
without releasing its held Source capacity. A final assertion checks the full
published tuple and actual lease expiry in the same transaction. Suppressing
an accounting update rolls back all preceding writes. Saved-switch changes
refuse without mutation. A fresh published authority provides exact completion
replay, and resolved Source claim replay accepts readiness zero before capacity
checks. A rotated old peer credential remains unavailable; current credential
replay succeeds at all eight held obligations.

Pinned Rust 1.97.1 passed all eleven Source library tests with zero ignored tests
in 23.95 seconds. The actual three-voter contract passed with one test and zero
ignored tests in 11.17 seconds. Daemon all-target check passed in 1 minute
13 seconds, Core feature all-target check in 28.31 seconds, Core feature Clippy
in 34.62 seconds, and daemon Clippy in 1 minute 21 seconds. The focused tests
exercise memory and pooled SQLite and actual replicated transactions, without
allocating or dispatching a producer. Physical readiness, downstream writer
settlement and generation-one failed-start retirement remain closed until the
actual daemon actor owns those barriers. Trusted transaction-entry time keeps
the original observation window; deterministic replicated SQL has no independent
applied-command clock, so the actor must recheck freshness after commit.

```sh
cargo test --locked -p plurx-core --features hiqlite-contract-tests --lib store::sharing_source_sessions -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract sharing_source_reservations_three_voters_atomic_claim_caps_replay_and_release -- --nocapture
cargo check --locked -p plurxd --all-targets
cargo check --locked -p plurx-core --all-targets --features hiqlite-contract-tests
cargo clippy --locked -p plurx-core --all-targets --features hiqlite-contract-tests -- -D warnings
cargo clippy --locked -p plurxd --all-targets -- -D warnings
```

## Complete Source decision and prepared recipe

The daemon now shares its actual file decision engine and recipe resolver
between Local playback and a Source preparation adapter. Source preparation
uses the current peer grant and viewer pseudonym as its canonical principal,
requires the complete current file witness and catalogue revision, and rechecks
both after planning. It supplies no Local account or LAN network prior. The
opaque prepared result retains the actual file, full decision response, recipe,
principal and fingerprint for the physical owner to consume.

The focused preparation regression compares the complete Source response with
the Local engine on the same file, distinguishes fingerprints for Local and two
Shared viewers, rejects injected network priors, and refuses changed Source,
epoch, library, revision, file metadata and private-library movement. It leaves
media sessions empty. This proves preparation and recipe identity; the peer
HTTP consumer, physical producer admission and B relay remain open.

Pinned Rust 1.97.1 passed the preparation test (one test, zero ignored), all 62
stream decision tests, and the three existing recipe regressions. Daemon
all-target check and Clippy with denied warnings passed with
`plurx-core/hiqlite-contract-tests`; the four documentation index tests passed.
The focused commands were:

```sh
scripts/require-test-count cargo test --locked -p plurxd --features plurx-core/hiqlite-contract-tests --bin plurxd http::shared_library::tests::sharing_source_preparation_uses_complete_live_file_and_real_principal_engine -- --exact
scripts/require-test-count cargo test --locked -p plurxd --features plurx-core/hiqlite-contract-tests --bin plurxd http::stream::tests::
scripts/require-test-count cargo test --locked -p plurxd --features plurx-core/hiqlite-contract-tests --bin plurxd http::hls::tests::the_height_resolution_keeps_its_three_promises -- --exact
scripts/require-test-count cargo test --locked -p plurxd --features plurx-core/hiqlite-contract-tests --bin plurxd http::hls::tests::the_intent_fingerprint_ignores_the_review -- --exact
scripts/require-test-count cargo test --locked -p plurxd --features plurx-core/hiqlite-contract-tests --bin plurxd http::hls::tests::a_reviews_notes_reach_the_resolved_plan -- --exact
```

## Assigned worker failure before activation

The candidate `settle_source_assigned_without_activation` callback takes the
closed complete dispatch assignment, not a node string or caller boolean. It
checks the immutable generation-one binding, the exact canonical request and
assigned node, and absence of every incarnation route, lease, pin, preparation,
recipe and playback pointer. It atomically records failed request settlement
and releases only that held reservation. A timestamp CAS makes an ignored
request write fail the final assertion even when revocation had already marked
the request failed. Exact settlement replay preserves the original receipt.

This callback is SQL permission only. Its sole intended production caller is
the private daemon actor after that actor proves it never spawned or queued a
producer. Route absence alone is not physical proof. It deliberately works
while Sharing is disabled or revoked. An activated route refuses this callback;
its physical retirement requires a separate exact owned producer barrier and
terminal route transition. Ordinary generation-zero release refuses assigned
workers. No expiry or terminal ACK releases capacity.

Based on `830e735c8`, the memory and pooled regression passed with one test and
zero ignored tests in 0.97 seconds. It preserves an actual foreign lease,
rolls back a suppressed request write, and settles/replays while disabled and
revoked. The actual three-voter contract passed with one test and zero ignored
tests in 11.53 seconds; it proves activated-route refusal, suppressed-write
rollback, exact assigned no-spawn accounting replay, and preservation of the
other seven held obligations. These fixtures dispatch no producer and do not
qualify the private actor handoff.

```sh
cargo test --locked -p plurx-core --features hiqlite-contract-tests --lib sharing_source_assigned_no_spawn_settlement_is_atomic_and_independent_of_grant -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract sharing_source_reservations_three_voters_atomic_claim_caps_replay_and_release -- --nocapture
```

Pinned Rust 1.97.1 daemon all-target check passed in 1 minute 21 seconds,
Core feature all-target check in 31.00 seconds, and Core feature all-target
Clippy with denied warnings in 35.15 seconds. Documentation index tests,
catalogue lint and diff checks passed.

## Exact terminal worker accounting

The candidate `settle_source_terminal_worker` takes a closed dispatch assignment
and its server-held typed terminal route. It agrees on the full principal,
incarnation, playback, fingerprint, assigned node, generation-one session and
owner epoch, exact terminal reason, lease and route timestamp. It captures the
current request timestamp and lease revision before the same-write assertions.
It rejects foreign lease owner/fence, foreign pin epoch, any preparation,
recipe or pointer, and a changed terminal route. Only matching terminal lease
and pin rows can be removed, in the same transaction as request settlement and
held reservation release. A suppressed accounting writer rolls those deletions
back. Start settlement already recorded by publication is preserved.

This is SQL permission, not physical proof. The sole intended production caller
is the private Source worker actor after actual registered process reap and all
associated reader/writer barriers. The actor first uses the existing exact
`end_media_session_if_owner` lifecycle method; no second media lifecycle is
introduced. A terminal SQL state, zero lease after revoke, expiration or ACK
never establishes physical settlement. Cleanup can complete after disable or
revoke. Disabled published cleanup retains resolved replay metadata; revoke
retains the canonical failed state produced by the existing trigger.

Based on `5c6a0369a`, the focused SQLite regression passed with one test and zero
ignored tests in 2.51 seconds, exercising blocked and published routes in both
memory and pooled modes. It preserves actual foreign lease/pin rows, refuses a
wrong session, proves rollback of terminal lease deletion, and replays exact
settlement. The actual three-voter regression passed with one test and zero
ignored tests in 11.63 seconds, including real revoke cleanup, active-route
refusal, foreign pin preservation, deletion rollback, exact replay and the
other grants' six held obligations. No producer is dispatched by these fixtures;
physical actor installation and its downstream settlement remain unqualified.

```sh
cargo test --locked -p plurx-core --features hiqlite-contract-tests --lib sharing_source_terminal_settlement_fences_physical_rows_and_rolls_back -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract sharing_source_reservations_three_voters_atomic_claim_caps_replay_and_release -- --nocapture
```

Pinned Rust 1.97.1 daemon all-target check passed in 1 minute 19 seconds,
Core feature all-target check in 30.66 seconds, and Core feature all-target
Clippy with denied warnings in 35.81 seconds. Documentation index tests,
catalogue lint and diff checks passed.

## Private Source decision ingress

The private peer router now accepts the bounded POST decision operation at
`/sharing/v1/items/{item}/files/{file}/decision`. Its closed request carries the
complete expected Source reference, v2 capabilities and request-local choices.
A current grant-authorized file witness admits the actual planning snapshot;
the adapter checks the sealed catalogue revision before and after the common
engine and rechecks the current peer credential. It returns the complete engine
decision in a protocol envelope, without a Local user or LAN throughput prior.

The response uses the existing four-MiB JSON bound and accepted-connection
monitor, including the exact library/item/file membership. The decision lane
has sixteen nonqueued permits and a ten-second request deadline. It allocates
no media session, resource admission permit or producer. The focused preparation
regression now also exercises the peer decision adapter and compares every
engine field, refusing a changed Source identity and empty capabilities. The
B file-decision consumer and live Source starts remain separate open work.
