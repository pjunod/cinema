# Adaptive playback resources — implementation and evidence ledger

**Status:** open · M0 source contracts in progress; M1–M7 not complete.
**Owner:** Sol 6.1 (`hdr-sol`, `gpt-6.1-sol`, `openai-codex`), task
`t_161f6652`. Manager owns design checks and final review scheduling. One writer,
one integration branch `effort/adaptive-playback-resources`, one final PR.
No deployment. No implementation units, integration campaigns or qualification
have run. Final independent Fable 5.1 implementation review, fixes and explicit
manager handoff precede qualification and landing.

Normative [plan](ADAPTIVE-PLAYBACK-RESOURCE-PLAN.md),
[acceptance](ADAPTIVE-PLAYBACK-RESOURCE-ACCEPTANCE.md) and
[design review](ADAPTIVE-PLAYBACK-RESOURCE-REVIEW.md) are unchanged.

## Current source and compiler receipt

Runtime base `a06be68b90db5e32527f8f72a512ab09cce643fe`;
document import `86687104def4cfd55f53dcf4a4eee08b171bdcfe`.
Approved plan SHA-256 `be993867c7b1db568663f9d793fefbf87c9679e027f67064b7884d281209f77f`
and acceptance SHA-256 `16868e4e3eca9d870f3f32e3bff5d47aa11d89ef8c00eecbed18d2c1fe55d6e0`
were verified against the clone before work.

Initial `rustup run 1.97.1 cargo check` commands returned success, but their
subprocess compiler provenance was not adequately captured: a later Clippy
command reached Homebrew 1.98.0. Those initial commands are not pinned receipts.
This was discovered after the first observation edits; no tests ran and no
unrelated baseline lints were fixed. The prerequisite was repaired by compiling
a pristine `git archive` of the exact baseline with explicit toolchain-bin PATH,
RUSTC, RUSTDOC and cargo bindings, then the current source with the same bindings.
Baseline all-target check passed (170 s); current all-target check passed (2 s);
current `plurx-core --all-targets --features hiqlite-store` Clippy `-D warnings`
passed (72 s). Actual rustc `1.97.1 (8bab26f4f 2026-07-14)`, commit
`8bab26f4f68e0e26f0bb7960be334d5b520ea452`, cargo `1.97.1
(c980f4866 2026-06-30)`, host `aarch64-apple-darwin`. Explicit `rustup run`
is necessary: unqualified binaries resolve a different Homebrew version.
Actual clippy `0.1.97 (8bab26f4f6 2026-07-14)` is bound explicitly too.
Private task evidence: `evidence/compiler-baseline.json` (initial unverified
provenance, superseded) and corrected receipt `evidence/compiler-pinned.json`.
Corrected full logs end `212528.log` (pristine baseline), `212837.log` (current
check), `212839.log` (current Clippy) under `repo/target/quiet`.
Owned target and temporary directories are durable siblings of the clone.
This is macOS compile-only evidence, not native Linux or runtime acceptance.
Ripwire was attempted first and refused because the binary is absent; bounded
symbol search and `scripts/outline` are the fallback, not an absence proof.

## M0 current anchors and coverage

Paths below are relative to `crates/plurxd/src` unless stated otherwise.
Each row must acquire an adapter and a regression before M3 can be complete.
Instrumentation in the shared launcher covers child observations, not source,
network, writer settlement, admission or a complete pipeline adapter.

| Work | Current physical owner / source anchor | Domains / phases | Planned regression anchor |
|---|---|---|---|
| All launched helpers | `plurx-core/src/process/mod.rs:spawn_job_owned`, `output_job_owned`, `status_job_owned`, `output_job_owned_blocking`; `process/priority.rs:Registration` owns registry lifetime | CPU/memory/process handles; startup, active, retiring; helper evidence never substitutes for packaged output | `plurx-core/src/process/observation.rs::tests::counter_identity_and_missing_samples_never_imply_idle` |
| Finite and rolling HLS transcode | `transcode/producer/spawn.rs`, `transcode/rolling/session.rs`, `producer_spawn.rs`; owned slot in `prodrun.rs:ProducerSlot` | CPU/device/read/scratch/egress, prepare/produce/pacing/retire | `prodrun.rs::tests::adaptive_reservation_survives_reaper_failure` |
| Continuous VOD / family audio/video/companion | `vod/serve/create.rs:try_reserve_media_group`, `vodencode.rs:EncodeWorkerPermit`, `vod/generation.rs`, `vod/driver.rs` | shared family residency and up to two-family overlap; count retained roles once | `admission.rs::tests::adaptive_overlap_is_atomic_for_four_and_six_roles` |
| Frozen ordinary / processed HDR and DV graphs | `vodencode.rs:try_admit_frozen_bundle`, `admit_frozen_preparation`, `ActiveProductionWindow`, `dv_runtime.rs`, `macos_video.rs` | whole helper graph, startup vs packaged steady production, known/unknown memory | `adaptive_resources/tests.rs::processed_graph_unknown_memory_cannot_expand` |
| Copy/remux / cached VOD | `vodencode.rs:EncodePermit::try_source_copy_under`, `copyseg.rs`, `progressive.rs`, VOD source/session lifetime | read, audio/demux, scratch, retained handles; cached delivery excludes past encode cost | `adaptive_resources/tests.rs::copy_and_cached_service_use_bytes_not_encode_speed` |
| Direct/range delivery | `http/stream` handlers and held source body lifetime | source/handle/egress; confirmed body close, not a fake process reap | `adaptive_resources/tests.rs::nonprocess_close_is_identity_bound` |
| Live TV audio-only/copy/video/timeshift | `live_tv.rs:LiveTvProcess`; source shared with DVR transport | tuner/source, queue/memory, CPU/device only when used; input-paced deadlines | `adaptive_resources/tests.rs::paced_capture_forbids_unknown_shared_trial` |
| DVR transport and each recording sink | `live_tv/dvr.rs:DvrTransport` and recording writer/fanout | count shared transport once and each sink write/queue/disk separately | `adaptive_resources/tests.rs::shared_capture_counts_transport_once_and_each_sink` |
| Fragment/index/source probes | `transcode/manager/construct.rs:admit_fragment`, `source_probe.rs`, `fragindex.rs` | currently whole-budget background; needs bounded demand and owned yield/reap | `adaptive_resources/tests.rs::foreground_yield_retains_capacity_until_reap` |
| Subtitle extraction/transcription/translation | `subtitle_work.rs:work_subtitle_queue`, `subtitle_models`, `subtitle_translation`, `subtitle_transcription.rs` | currently shares fragment permit; CPU/read/memory/helper graph, preparation deadline, owned cancellation | `adaptive_resources/tests.rs::background_yield_budget_is_not_restarted` |
| Preview/pretranscode/census/conversion | `transcode/manager/produce.rs`, `transcoder_optimization/comparison.rs`, `dv_disk.rs`, core scanner helpers | preemptible optional work; retain physical residency through yield | `adaptive_resources/tests.rs::background_registration_without_reap_cannot_release` |
| Remote source / shared NAS | `media_pool.rs:observe_media_io`, source/sharing source owner lifetimes | shared storage read and inter-node path independent of worker CPU | `adaptive_resources/tests.rs::shared_domain_unknown_claim_refuses_competing_trial` |
| Ingress HLS relay | `media_sessions.rs:relay_response_with_limits_observed`, `http/hls/relay.rs` | ingress CPU/memory/egress and inter-node bytes; processing relocation retains ingress owner | `adaptive_resources/tests.rs::remote_processing_never_erases_relay_commitment` |

The existing executable census
`plurx-core/src/process/census.rs::every_production_spawn_goes_through_the_launcher`
and clippy disallowed-methods constrain production launcher bypass. Its scope is
four daemon crates, with test-only code removed. M0 must retain this anchor and
complete a call-site census; raw search results include tests and are not a
verified production-spawn count.

### Reverified ownership constraints

- `admission.rs:try_admit_bundles_claiming` rejects empty or >3 roles before
  mutation, checks sum overflow, then performs a single locked acquisition.
  `try_admit_family` adopts pointer-checked retained permits and preserves family
  identity. Keep both legacy bounds. Add a separate bounded overlap transaction,
  at most two explicit family identities and six distinct roles; no vector widening.
- Current `Priority::Live if background_active` refusal exists in bundle admission;
  fragment/subtitle work still uses whole-budget admission. Merely changing that
  estimate without a physical yield owner is unsafe and does not complete M3.
- `prodrun.rs:owned_reap` keeps opaque resources through wait retries and writer
  settlement. Without a runtime it deliberately forgets the owners. Panic/aborted
  cleanup can still drop opaque resources. New reservation custody must mark
  `UnknownHeld` at Drop after spawn rather than equate Drop with release.
- Current `ConfirmedProducerReap` proves matching generation and ended writer task;
  `WriterSettlement::Abandoned` means the task is gone, not a live writer.
  Descendant job closure is not itself proof of every descendant exit on Unix.
  Do not claim whole-tree measurement/cleanup from the leader's counter alone.
- `media_pool.rs:CreateStartupBudget` caps startup at 10 s; stage work currently
  retains 250 ms for final work. Use the original deadline and that activation
  reserve, never reset the budget on a remote retry.

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

Tests are to be authored and compiled, not executed before final handoff.
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

## Remaining milestone work

- M0: finish production call-site inventory; incorporate manager's read-only
  settings writer/form inventory; obtain bounded check of proposed contracts;
  refine schema guards and assign runnable fixtures for current storage layout.
- M1: implement portable observations and truthful diagnostics, no policy changes.
- M2: atomic dimensioned policy, retained physical custody, history and settings migration.
- M3: every adapter above, two-family overlap, real background yield/reap, Live/DVR.
- M4: guarded durable worker preparation/adoption and mixed-version behavior.
- M5: same-quality ranking/budgets, shared NAS/relay domains and maintenance.
- M6: settings/client/operator docs and accurate drain/placement diagnostics.
- M7: acceptance implementation, complete-candidate independent review/fixes,
  explicit manager handoff, one affected batch and real-process/cluster/platform
  qualification, unavailable hardware list, promotion/merge. No physical deployment.

## Actual M1 delta (not complete)

`plurx-core/src/process/observation.rs` implements OS-creation-bound leader CPU
and resident bytes for Linux proc stat, macOS proc_pid_rusage and Windows
GetProcessTimes/GetProcessMemoryInfo. The launcher registry retains one observer
per child, cached for one second; OS reads happen outside the registry lock.
Unknown reads and pid reuse break positive windows; no descendant completeness
or useful media throughput is invented. Running-child diagnostics now carry the
additive tagged observation. Authored tests cover identity/missing samples,
dimensioned rate/stale/regressing/zero windows, and Linux parser units excluding
recursive child CPU totals. macOS all-target compilation and pinned core Clippy
pass; tests have NOT executed. Native Linux/Windows compilation, whole-graph
sampling, output/phase adapters, effective constraints and bounded-overhead
qualification remain. Admission policy is unchanged.

## M0 contract refinement for manager check (revision 2)

These decisions supersede ambiguous prose above. They remain proposed until
the manager resolves them; no M4 behavior has been enabled.

### Database enforcement, including old SQL and retention

Implement shared schema SQL in `store/adaptive_resource_schema.sql`, installed
atomically with its triggers in a new SQLite migration and replicated migration
after current AUTH schema 82. Both current layouts already have principal
`owner_key`; adaptive transactions require the rebuilt principal layout (sharing
source layout 71), otherwise return protocol unavailable without spawning.
Fresh installs and upgrades use the same SQL. No daemon-local Rust check is
substituted for these persistent guards.

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
  delete+reinsert (`REPLACE`) from detaching its physical obligations.
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
silently expands. Zero retains its existing disabled meaning in enforced mode.

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
