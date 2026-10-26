# Adaptive playback contract history — M0 proposals and manager direction

**Status:** done · historical contract proposals and manager direction, not current implementation or acceptance evidence.
**Written:** 2026-10-22.

Companion to the [current adaptive playback resource ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md), which records implementation and evidence. This file preserves the two former M0 contract sections verbatim so their historical constraints and limits remain available without presenting them as current ownership or completed behavior. The approved implementation scope remains in the [resource plan](ADAPTIVE-PLAYBACK-RESOURCE-PLAN.md).

## M4 request intent decision — 2026-10-26

Fable 5.1 decision `t_9095ec22` fixes the version-1 `intent_digest` meaning as
the complete original worker wire envelope with route and request IDs scoped
to the root incarnation. The origin hashes it before attempt identity rewrite;
the worker rederives it before planning admission. A borrowed projection retains
the existing field order, principal aliases and omission rules without cloning
the envelope. This unreleased effort changes neither its schema nor wire version.

A retry must reuse the exact immutable request binding and original deadline.
The predecessor must be durably Cleanup, Losing or Closed; origin Fence never
proves native cleanup and the origin never issues Close. Worker cleanup may use
a foreign committed fence only after its original native/application owners
settle. Bounded coordinator retry is connected: it retains original Claim/Fence operations, confirms a fresh durable predecessor phase, and re-fetches each quote within the common deadline. Worker stale/foreign-fence cleanup is connected; behavioral qualification has not run.
Worker-node domains remain the only available identity; shared source/storage
domains and a production consumer for unclosed provisional rows remain open.
The integrator preserves the stricter plan §7.1 boundary where the design's
blanket HTTP-failure retry would classify authentication/protocol failures as
load: explicit 400/401/403/404/405/413/415/422 responses and deterministic
Prepared protocol/owner/digest/preview mismatches Fence the original attempt
and then hold. Ambiguous 409/429/500/503/local-string or transport failures
still require a confirmed original Fence before retry. This implementation
delta awaits final Fable review; no new wire error classification is invented.

This decision is design evidence, not complete-candidate review or execution.

## M0 proposed exact contracts for manager check

These are implementation contracts awaiting bounded manager confirmation, not
implemented wire/schema or approval of future code. M1 instrumentation is
independent of the remote schema and can proceed without enabling new dispatch.

### Local types and observation authority

Shared portable child counters belong in `plurx-core::process::observation`,
called by the existing `priority::Registration`/launcher. A `ProcessIdentity`
binds pid and OS creation/start marker. `Metric<T>` is tagged `Known(T)` or
`Unknown(reason)`; no null/zero conversion. Counters are cumulative CPU ns and
current resident bytes, accompanied by tree-coverage completeness. Sample once
per second per owned observer; no global scheduler lock spans OS reads.
Descendants' own CPU counters are summed once, never combined with recursive
child totals. Missing/inaccessible/reused process identity invalidates evidence.

Node-local `adaptive_resources` owns `WorkloadDemand`, `ResourceDomain`,
`ResourceVector`, `AdmissionAssessment`, `AdmissionRefusal`, `WorkSample`,
`ResourceReservation` and bounded `ResourceHistory`. CPU demand is CPU ns per
service unit, thread ceiling separate; memory/handles/process/device/tuner/scratch
are hard dimensions. Identity includes executable/helper/device/limit epoch and
exact recipe; generic class history cannot mint a recipe or permit. Assessment
is advisory; reserve rechecks epoch/hard limits atomically. One optional trial
per node/shared domain; unknown peak memory or cleanup/start bounds bars trials
beside incumbents. Three verified output samples span >=2 s; paced state never
trains an encoder as slow. History <=1,024 classes, seven-day priors, fifteen-
second expansion evidence, 60 s actual-failure cooldown, 30 s growth stability.

Reservation state: `Reserved -> SpawnPossible -> Reaping -> Released`;
uncertain Drop/panic/wait failure leaves `UnknownHeld`. A matching nonserializable
reap/close receipt is the only post-spawn release. Never-spawned cancellation
requires retained exclusive no-spawn custody. Family overlap reserves only missing
roles; retained predecessors remain counted and are not stolen or mutated.

### Durable request / provisional route extension

Existing anchors are `plurx-core/src/domain.rs:MediaSessionRequestClaim`,
`MediaSessionRoute`, `MediaSessionActivation`, and Store `claim_media_session_request`,
`assign_media_session_request_owner`, `activate_media_session`. Both
`store/sqlite/sessions.rs` and `store/hiqlite_sessions.rs` implement them.
Current request states are `starting/resolved/failed`, route states
`starting/active/ended`; do not add strings old decoders might treat as active.
Current owner assignment is single-worker sticky and has no attempt epoch;
claim expiry can reacquire an old starting request. It is NOT already sufficient
for bounded multi-worker preparation. Existing `RemotePrepareRequest` is for a
previously staged successor and requires owner epoch 1. Do not repurpose it as
unfenced initial preparation.

Proposed additive versioned table `media_resource_attempts` is subordinate to the
existing request claim and holds: owner_key, request_id, request incarnation,
attempt_id (UUID), ordinal (1..3), route incarnation/session UUIDs, worker node,
owner_epoch (>0), optional predecessor incarnation/epoch, principal JSON,
source digest, settings generation/digest, recipe digest, deadline Unix ms,
state (`provisional/prepared/winner/losing/cleanup/closed`), boot/serving identity,
prepared handle digest and updated time. Unique request+ordinal and attempt_id;
at most two nonterminal attempts, only on distinct admitted domains. Separate
`media_resource_requests` extension holds one nullable winner attempt and the
immutable original deadline/source/settings binding. Use request identity and
principal, not a bearer token, as authorization.

Store operations: claim attempt creates its blocked `starting` route and epoch
BEFORE spawn, atomically under the existing `starting` request; mark prepared CAS
checks all bindings; select winner CAS changes null winner once; activation
requires that winner and its exact epoch and ordinary predecessor/output checks;
losers become cleanup obligations. Request reacquisition and legacy activation
must reject any live/unknown attempt. Add guards for old writers (including
legacy request-owner assignment, claim reacquisition, activation and maintenance)
so a rolling old daemon cannot bypass the extension. Expiry fences adoption but
does not delete held attempts or their routes. Cleanup closure requires a
worker-bound physical receipt, not route removal. Shared-domain trial claims use
the same durable bounded authority, not high-frequency samples through consensus.

Install the same schema and guards through the repository's SQLite and Hiqlite
migration mechanisms; fresh and upgraded layouts must agree, including rebuilt
principal-owner schemas. No existing request state CHECK is widened. An old
schema or mixed cluster unable to enforce these guards refuses this protocol;
ordinary legacy playback remains on its existing path. Schema version and
writer-capability refusal need explicit migration fixtures before M4 is complete.

### Capability-negotiated wire

Use fresh authenticated `MediaNodeSnapshot` optional capability
`adaptive_prepare: 1` (absent means unsupported), not a global protocol bump
which would disable existing ordinary peers. Capability is accepted only within
the current snapshot TTL and matching boot/serving generation. Versioned new
Prepare/Adopt messages have strict `deny_unknown_fields`; send only after that
advertisement, never probe an old strict parser with unknown fields.

Prepare v1 fields: version=1, attempt_id, request_id, request incarnation,
route incarnation/session, typed principal, expected worker/owner epoch,
optional predecessor incarnation/epoch, source/settings/recipe digests, absolute
deadline plus bounded remaining_ms, boot/serving identity, existing validated
candidate intent. Worker resolves the authoritative row before admission and
again before sealing, reconstructs local executable/credit itself, and returns
only a handle bound to all these identities, output digest and deadline. No
serialized resource reservation or prepared executable is accepted.
Adopt v1 fields: version, attempt_id, exact handle, expected owner epoch and
winner binding; it rechecks drain and durable winner before ordinary activation.
Cancel/status use exact same attempt/epoch/principal. No adoption after deadline.
Authenticated pre-handler protocol rejection is confirmed, not capacity failure;
transport errors remain unknown and require status reconciliation before conflict.

### Transaction crash matrix / concrete regression targets

Tests are authored and compiled now; execute after complete-candidate review/fixes.
`store/adaptive_resource_attempts_tests.rs` is the planned shared behavioral
contract exercised on SQLite and Hiqlite with `--features hiqlite-store`.

| Arrow / race | Required durable / physical behavior | Proposed test name |
|---|---|---|
| claim -> provisional | no spawn without committed blocked owner/epoch; principal conflict refused | `provisional_route_precedes_spawn_and_is_never_serveable` |
| provisional -> prepare | duplicate resolves same held worker; changed source/settings/epoch refuses | `duplicate_prepare_reuses_exact_worker_custody` |
| spawn -> sealed | cancelled/expired/crashed worker stays counted and drain-visible | `expired_preparation_stays_held_until_confirmed_reap` |
| prepared -> winner | concurrent results select only one durable winner; losers fenced | `two_attempts_have_one_winner_and_no_partial_activation` |
| winner -> adopt | duplicate adoption idempotent; predecessor change refuses without harming incumbent | `adopt_rechecks_winner_epoch_predecessor_and_maintenance` |
| response lost at any write | status read resolves exact outcome; no fresh conflicting spawn | `unknown_prepare_and_adopt_outcomes_require_reconciliation` |
| old/new versions | no new fields to old peer; old writes cannot bypass live attempt | `legacy_writer_cannot_reacquire_or_activate_a_live_attempt` |
| drain | provisional, expired, losing and unknown attempts remain blockers | `drain_includes_every_unclosed_attempt` |

Local `adaptive_resources/tests.rs` additionally supplies fake-clock regression
anchors for stale/nonfinite/duplicate evidence, exact caps including zero,
pacing state changes, protected runway, real-only cooldown, bounded history,
known peer first, common B/A/R budget, three-worker search bound, and no silent
manual fallback. These names become executable anchors only when authored;
this table does not claim tests already exist or pass.

## M0 contract refinement for manager check (revision 2)

These decisions supersede ambiguous prose above. Manager accepted the direction
for implementation with the mandatory INSERT-guard refinement below. This is
not final adversarial review or M0/M4 completion; no M4 behavior is enabled.

Draft PR #1248 is open, owner task `t_161f6652`; controller owns authenticated
transport and ongoing coordination. Keep it draft through complete implementation,
independent Fable review and fixes. Paperwork-only draft CI is allowed. The owner
superseded the former extra handoff gate; no additional transfer or controller
stop is required before post-review tests/qualification/authorized merge.

### Database enforcement, including old SQL and retention

Implement shared schema SQL in `store/adaptive_resource_schema.sql`, installed
atomically with an independent versioned marker and verified object shapes.
Manager supersedes the unimplemented AUTH83 proposal: support Hiqlite baseline81
and Source82 without bumping AUTH or implicitly activating Source. SQLite uses
its own independent installation convention, not a shared ordinal. Verify actual
principal/route columns; frozen private Source layout71 is not composed ordinal71.
Fresh/install/import/restore parity, partial/restart and unknown-future refusal
are required. No adaptive advertisement before complete shape/hash verification;
ordinary playback remains supported. Rust checks do not replace persistent guards.

`media_resource_requests` primary key `(owner_key, request_id)` stores immutable
`claim_incarnation`, `playback_id`, `fingerprint`, `principal_json`, `deadline_ms`,
`source_digest`, `settings_generation`, `settings_digest`, nullable `winner_id`,
and monotonic `revision`. No cascading FK deletes. `media_resource_attempts`
primary key `attempt_id`, unique `(owner_key,request_id,ordinal)` and unique
`route_incarnation`/`session_id` stores the fields specified above. Add
`domains_digest`, exact `recipe_json`, `response_json`, `cleanup_nonce`, nullable
`cleanup_receipt_digest`, and state `unknown_held`/`adopting` to the state set.
All states except `closed` count against the two-nonterminal physical-attempt
limit, including expired, losing, cleanup and unknown-held attempts. Ordinal
1..3 is the lifetime search limit; a third attempt requires one of the first
two to have closed physically. Closing is not deletion. Route IDs are separate
per attempt; the request's original claim incarnation remains immutable in the
extension until retention deletion after all attempts close.

Persistent named guards (row-local `RAISE(IGNORE)` for legacy cleanup, not a
whole-batch abort that would break unrelated ordinary playback):

- `resource_request_identity_bu`: on request incarnation/owner/principal/
  fingerprint/playback/state changes, ignore a legacy write while any unclosed
  attempt exists unless it is the exact recorded winner activation transition.
  Claim-expiry renewal alone does not authorize a new attempt or move its deadline.
- `resource_request_retain_bd`: ignore deletion of a request with any unclosed
  attempt, even after failed/resolved/expired retention. This also prevents
  an explicit delete from detaching its physical obligations. DELETE triggers
  alone do NOT protect implicit REPLACE deletion with recursive_triggers OFF.
- `resource_request_identity_bi`, `resource_route_owner_bi`, and identity BEFORE
  INSERT guards on every replaceable owner/parent/extension refuse insertion
  when it would conflict with any existing unclosed custody. This protects
  INSERT OR REPLACE independently of DELETE triggers or the connection's
  recursive_triggers PRAGMA. Both OFF and ON legacy-SQL fixtures are mandatory;
  no claim of REPLACE protection is valid from a DELETE-only trigger.
- `resource_route_owner_bi/bu`: block conflicting route insertion and epoch/
  identity/recipe changes for an attempt. Permit `starting -> active` only for
  its durable winner in `adopting`, matching exact worker, session, principal,
  recipe, source/settings binding and blocked publication state. Transition
  request incarnation to winner route incarnation only in the guarded winner
  activation transaction. Old single-owner assignment cannot reserve a
  different attempt or create a second producer on a provisional route.
- `resource_route_retain_bd`: ignore deletion of an attempt's route while it
  is unclosed; terminal/lease-expiry does not bypass this. Terminal projections
  may fence publication without deleting the attempt or physical custody.
- `resource_attempt_retain_bd` and `resource_request_extension_retain_bd`:
  refuse retention deletion of unclosed attempts/extensions. No attempt has a
  cascading parent FK. User deletion must retain the opaque owner-key custody
  ledger until cleanup, even if user/principal authorization has been revoked;
  revocation prohibits adopt but is not a physical-release receipt.
- `resource_attempt_transition_bu`: immutable identity/deadline/digests;
  valid CAS state edges only; `closed` requires a matching cleanup nonce and
  worker/boot/epoch-bound receipt; `winner` unique through request CAS. A stale
  or partitioned worker cannot close a newer attempt using an old receipt.

All adaptive writes use the authority transaction on Hiqlite, and one SQLite
transaction under its existing connection owner. Update/query changes must
verify the exact postcondition (not assume success after a trigger ignored a
row). Ordinary requests with no resource extension retain existing legacy
behavior. New protocol capability requires verified installed guards plus
worker capability/boot/serving generation. No activation extension is sent to
an old worker. New transactions must verify schema/trigger hashes before
advertising. Old SQL replay fixtures include expiry reacquire, owner assignment,
active INSERT/UPSERT, maintenance DELETE, REPLACE, route terminal/delete, and
principal deletion, each while an unknown-held loser remains. No broad cleanup
may silently drop the custody tables. Database restore does not prove children
on other workers disappeared: restore invalidates adoption, retains unknown-held
custody and reconciles each original worker/boot before clearing it.

### Shared-domain trial claim

Domain identity is `(kind, authority_id, domain_id, epoch)` where kind is
`device/source_storage/ingress_network`, authority_id is the configured cluster
resource authority, and domain_id is an opaque stable configured or verified
storage/device/link identifier, never a raw path/movie name. Node-private domains
use node+boot identity. Unknown shared identity is ineligible for speculation,
not assumed independent. No NAS identity is guessed from matching path strings.

`media_resource_domain_trials`: primary key domain identity; fields
`claim_id` UUID, owner_key, request_id, attempt_id, worker_node, worker_boot,
owner_epoch, `generation` (>0, monotonic), deadline_ms, state
`held/cleanup/unknown_held/closed`, cleanup_nonce and nullable receipt_digest.
`media_resource_attempt_domains`: primary key `(attempt_id, domain identity)`;
one bounded set <=8 domains, sorted canonically for acquisition, foreign keys
RESTRICT (no cascade). A graph requiring more/unknown domains takes only its
existing conservative cold path; it cannot speculate with incomplete accounting.

`claim_resource_attempt` atomically validates the original request and <2
unclosed attempts, creates the blocked provisional route, and acquires ALL
domain claims (each absent or physically closed) in the same transaction.
No partial claims survive a refusal. One node trial claim is mandatory even
when all external domains differ. Generation never resets when a closed claim
is reused. `begin_resource_cleanup` fences adoption and changes claims to
cleanup; `close_resource_attempt(receipt)` checks exact attempt/epoch/boot/
nonce and releases every domain in one transaction only after physical reap
and writer/handle settlement. Deadline/lease expiry never overwrites held claims.
An unknown acquisition result must be resolved by claim_id/status before spawn;
a partition refuses a competing claim. An unknown close result retains local
custody until authoritative status proves closure; no lease-based stealing.
At most two prepares can overlap only with genuinely disjoint admitted domains;
cleanup losers continue consuming both physical-attempt count and trial claims.

### Four-state settings and every writer boundary

Luna's bounded read-only inventory confirmed `http/system.rs` numeric DTO fields
2501–2502, default synthesis 2687–2700, selective optional PATCH fields 3260–3261
and bulk writes 4610–4622. No native/CLI numeric capacity form was found by its
bounded search (not absence proof). `web/pages/settings-system.js` has no capacity
form yet. Sol verified generic `SettingsStore` arbitrary-key writes at
`store/sqlite/mod.rs:put_setting/put_settings/put_settings_if_generation` and
`store/hiqlite.rs` equivalents. Import is real: `store/hiqlite_import.rs` settings
TablePlan copies key/value/updated_at with only instance.id excluded. Full
cluster restore copies the database image in `cluster/migration.rs:
restore_cluster_backup_archive`; its migration/fencing stage must retain or
normalize provenance before serving. There is no proof all generic consumers
are covered until these storage-boundary fixtures exist.

For each numeric key K, companion K`.provenance` encodes schema=1,
mode=`automatic/explicit/legacy_ambiguous`, and bound stored numeric string
(or absent). Four read states: absent K and no record = automatic/Absent;
K without valid bound provenance = enforced LegacyAmbiguous; valid Explicit =
enforced saved ceiling; valid Automatic = ignore the numeric rollback cache.
Malformed numeric or inconsistent explicit provenance fails closed, never
silently expands. Zero is kind-specific: hardware zero disables hardware
sessions; software-pool zero retains the existing serialized/idle-pool oversize
semantics, not a software ban (`SwPool::try_take`, admission.rs352–377, and
store/mod.rs2062–2066). Do not normalize these two meanings into one zero cap.

Shared persistent settings INSERT/UPDATE triggers on either numeric key always
invalidate its provenance to legacy-ambiguous, including writes by an old binary,
raw SQL, same-value UPSERT and generic imports. DELETE removes bound provenance;
absent numeric is absent rather than an invented zero. A typed new operator
transaction writes numeric first, then the schema/bound provenance LAST; explicit
mode selection is required to restore Automatic. Unchanged GET/PATCH echoes are
filtered BEFORE writing: keep omission/mode, never materialize a resolved local
CPU default. GET reports stored numeric/mode separately from effective node value;
new controls send only dirty fields. New-install initialization records automatic
without writing a resolved numeric default. An operator's Automatic selection
retains existing conservative numeric cache for rollback readers; no per-node
CPU count is newly replicated as an explicit cap.

Importer must load numeric keys first, validate any provenance in a recognized
schema manifest against exact imported values, then install valid provenance
last. Unversioned/mismatched generic restore is legacy-ambiguous and enforced;
it cannot replay a stale Automatic record. An intact full-image restore retains
the matching bound record and triggers; a restored older schema has no trusted
provenance and stays ambiguous after migration. Add tests
`legacy_numeric_write_invalidates_automatic_even_after_restart`,
`unversioned_import_cannot_restore_stale_automatic`,
`untouched_roundtrip_preserves_absence_and_node_local_defaults`,
`explicit_zero_and_rollback_cache_are_enforced` on both stores/API.

### Deadlines across clocks and retries

The coordinator owns one original monotonic deadline D and activation reserve
A=250 ms from the existing create contract. Peer allowance R and trial spend use
that D, never a new duration on retry. Every outgoing message subtracts elapsed
time and carries `remaining_ms`, absolute original deadline and clock-uncertainty
bound from existing authenticated clock health. Worker creates one local
monotonic cutoff on FIRST receive: minimum of receive-now+remaining_ms and the
conservatively translated original deadline minus the worst clock error. Refuse
new adaptive Prepare/Adopt when the translation cannot be bounded. A duplicate
may only SHORTEN that stored cutoff. Boot change invalidates the handle and
adoption; it never resets D. Cleanup has its own custody obligation, not a new
adoption allowance. Coordinator rejects a prepared response or status result
after D−A; worker also checks its cutoff immediately before sealing and adopting.
Status reconciliation after expiry can discover/clean the existing winner but
can NEVER authorize late adoption or a fresh conflicting physical attempt.
