# Adaptive playback resources — implementation and evidence ledger

**Status:** open · M0/M1 partial; M2 capacity and M3 custody slices compiled.
**Integration owner:** Sol 6.1 (`hdr-sol`, `gpt-6.1-sol`, `openai-codex`),
`t_161f6652`. Owner-authorized isolated Sol lanes deliver journal and M4 Store
authority; integration retains runtime, shared registrations/docs/catalog and
serialized adoption. Controller coordinates scope/review. One integration branch
`effort/adaptive-playback-resources`, one final PR; no overlapping writers.
No deployment. No implementation units, integration campaigns or qualification
have run. Complete implementation, actual independent Fable 5.1 review and fixes
precede qualification and landing. The owner confirms management transfer is
already complete: no additional pre-test handoff or controller-stop gate exists.

Normative [plan](ADAPTIVE-PLAYBACK-RESOURCE-PLAN.md),
[acceptance](ADAPTIVE-PLAYBACK-RESOURCE-ACCEPTANCE.md) and
[design review](ADAPTIVE-PLAYBACK-RESOURCE-REVIEW.md) retain their historical
approval pins below. An explicit manager-accepted, source-grounded zero/service-
budget clarification now changes their bytes; review §6 records that delta
without inventing new independent approval. Current plan/acceptance/review
SHA-256 respectively:
`0a3a26eaab79e9b1852d16dc005394cfca59e676fae7319cbc34b0e64ca1386c`,
`73067de47ac10b0855637540e0e3d84724e0361aa72340de9c867bd4cf1159fd`,
`3c5a4b08f83617e73273e520aa4a9a78a60e69ab550618c97f2966756898e8ed`.

## Current source and compiler receipt

`f51a5475f` connects typed journal preparation to the SAME retained runtime bundle
and reservation/family/cancel/refund path. Real root/pipe adapters enrich matching
typed envelopes; the old all-closed path refuses typed inventory before consuming
proof. Scratch-directory member/ArmIo commands retain exact IDs/results, apply
the local fold only after known commit, acknowledge without double application,
and permit bounded local IO after outer seal without reopening members. No native
parent/observation permit escapes. `5d12668d1` corrects a nontrial-only fixture.

Pinned1.97.1 affected core all-target check PASS7.948s; preceding17.332s/10.759s/
8.347s scopes were source-invalidated, retained without replay. Nine new bodies
COMPILED NOT RUN; warnings, NOT strict Clippy. Unchanged daemon81.476s retained;
controller reports exactb37d changed-daemon Windows PASS151.38s, not Unix execution.
`1584a357c`/`a4465e4b0`/`521a6d5fb` retain actual Unix no-follow/no-clobber native
publication and partial-object/drain-error/cancellation ownership. Non-Unix is
legacy. `3e9edcdf4`/`cdf580eb0` provide bounded event/reader audit; old codec/DDL
unchanged. Admission STILL LEGACY. Missing: actual prearmed root/init and native
outcome/manifest composition, source/copy/HTTP leases, bounded native/index memory,
overage debt/fence, snapshot/RetainedAssetsV2/private-proof atomic R→S, positive
old-domain restart and remaining M0–M7. No finite/physical-quota/behavior pass.

Retained `83ec55303` shares root/source handle and read/drain memory bounds;
cancelled/completed-unconsumed reads remain charged. `92dd41a48` arms before
audited task creation; actual abort+join proves ONLY those pipes. `66291c562`
owns prepare/root/settle-before-refund; `c39c30542` opens the lock-bound runtime
before scratch/probes. Full recovered costs/edges/epochs/trials/paths remain
cleanup-only UnknownHeld. Pending startup refusal is safe interim behavior,
NOT selective recovery or unaffected serving. These bounds do not estimate
process/containment kernel overhead or production demand.

Released M4 `6169e9b18` remains integrated through `5d7eba0f1`: Store installers,
Source guards and claim/Prepared/winner/adoption/fence authority, not worker RPC,
advertisement or a normal Closed writer. Preserve ALL earlier anchors/receipts
and the strict-default child failure. See [custody limits](ADAPTIVE-RESOURCE-CUSTODY.md).

See the [finite delivery table](ADAPTIVE-PLAYBACK-DELIVERY.md): implemented/missing
M0–M7 deliverables, dependencies, owners, observable behavior and first finite VOD
closure target. No milestone is accepted end-to-end; ordinary playback is still
legacy. Complete source/review/fixes precede once-only qualification/merge, with
no new handoff gate/deployment. Unsupported Windows has NO ledger, not zero usage.

Runtime base `a06be68b90db5e32527f8f72a512ab09cce643fe`;
document import `86687104def4cfd55f53dcf4a4eee08b171bdcfe`.
Approved plan SHA-256 `be993867c7b1db568663f9d793fefbf87c9679e027f67064b7884d281209f77f`
and acceptance SHA-256 `16868e4e3eca9d870f3f32e3bff5d47aa11d89ef8c00eecbed18d2c1fe55d6e0`
were verified against the clone before work.

Historical `rustup run` receipts lacked subprocess provenance and are NOT pinned.
Corrected `evidence/compiler-pinned.json` supersedes `compiler-baseline.json`;
baseline170s/current2s/Clippy72s and logs212528/212837/212839 remain retained.
Current checks use explicit toolchain-bin PATH/RUSTC/RUSTDOC/cargo1.97.1. Durable
build/scratch only. Ripwire absent; bounded source fallback is not absence proof.

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

## Remaining milestone work

- M0: finish production call-site inventory; incorporate manager's read-only
  settings writer/form inventory; obtain bounded check of proposed contracts;
  refine schema guards and assign runnable fixtures for current storage layout.
- M1: implement portable observations and truthful diagnostics, no policy changes.
- M2: atomic dimensioned policy, retained physical custody, history and settings migration.
- M3: connect core custody to every adapter above; positive containment, typed
  nonprocess closure and restart persistence; real background yield/reap, Live/DVR.
- M4: guarded durable worker preparation/adoption and mixed-version behavior.
- M5: same-quality ranking/budgets, shared NAS/relay domains and maintenance.
- M6: settings/client/operator docs and accurate drain/placement diagnostics.
- M7: acceptance implementation, complete-candidate independent review/fixes,
  one affected batch and real-process/cluster/platform
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

Subsequent source commits: `6e2104efb` adds dimensioned useful-service windows,
verified VOD package/phase diagnostics, effective-limit diagnostics and a daemon-
shutdown-owned sampler; `449747f36` associates actual finite/streaming DV managed
helper/encoder launcher registrations with node-local observation graphs;
`068f56f22` uses the repository-required pinned atomic `try_update` API. The
first graph Clippy failed on banned `fetch_update`; corrected all-target plurxd
Clippy `-D warnings` passed on exact `068f56f22` (67 s, log ending `215330.log`).
No implementation tests have executed. Managed grouping is not graph-total
measurement, custody or descendant closure. Unix file limits and visible Linux
cgroup-v2 ancestor ceilings are explicit dimensions; hidden/unsupported/unbounded
containment is Unknown, not host totals. `resource_limits` and process diagnostic
fields are documented in the API reference. M1 whole-graph accounting and full
adapter/phase/overhead/platform evidence are still owed.

### Checked M0 lexical launcher inventory

Manager-supplied Luna read-only inventory at `8081f025c` follows
`process/census.rs` governed four-daemon-crate scope (core, daemon, PGS,
compat-Plex), excluding harness and test-only modules. Its cfg/test stripping,
recursive include traversal and typed Clippy complement source search. These
are lexical anchors, NOT positive admission/lifecycle/descendant proof; none of
the inherited tests were executed. Do not repeat this inventory as acceptance.

| Area | Actual baseline call-site anchors (symbol; line) |
|---|---|
| Core launcher/bounded | process/mod.rs spawn_job_owned81, output_job_owned126, status_job_owned150, output_job_owned_blocking211; process/bounded.rs output_command_environment161/197 |
| Core probes/metadata | scan/probe.rs self_check28, probe_reporter_identity210, probe_with_threads292, probe_first_frame478; metadata/book.rs collect_attached_picture686; metadata/local.rs generate_thumb_with389 |
| Core backend probes | transcode/avc_qualification.rs probe_output255; decoder_inventory.rs advertised_backends447/probe_clip520/probe_decode570/measure_request_decode642; encoder.rs detection/benchmark/yielding1532/1611/1710/1774/2030 |
| FFmpeg wrappers | ffmpeg.rs spawn236/spawn_piped_output259 (return owned child/job), stamped_with_this_reporter723, bounded_command_output_with_limits3265; Dovi/HDR/burst3815/3909/3990/4084/4121/4179/4215/4480 |
| Bound probes / source | transcode/source_preparation.rs run_child_with631; decode_facts.rs spawn_configured_probe2645/probe_version3737/collect5213; transcoder_optimization/comparison.rs run666; media_pool.rs spawn_library_root_probe1723 |
| HDR / disk / census | dv_disk.rs215/1542/1647/5883; hevc_census.rs sample220; pipeprobe.rs ffmpeg387/ffprobe407; producer_spawn.rs verify145 returns child/job |
| Live / subtitles / indexing | live_tv/caption_probe.rs media_command265/run_live_graph702; live_tv.rs probe_live_source7983/spawn_live_ffmpeg8238/run_graph_probe10092; fragindex.rs build_with_args_owned2419; state.rs run_subtitle_source_pass3180; subtitle_ride_along.rs judge_text_mks1653/run_ffmpeg2854; subtitles.rs extract_vtt3022 |
| Remaining source paths | http/stream.rs run_chapter_probe2084; transcode/content_encoding.rs measure_content485/sample_probe_document785; transcode/media_origin.rs probe56 (enclosing symbol unresolved) |

No production matches found in PGS/compat-Plex within that bounded scan, not an
absolute absence proof. Outstanding caller tracing: actual admission/work-class
association, capacity retained through wait/pipe settlement, returned-child
factory ownership, unmanaged descendants, non-Rust/separate-binary boundary,
media_origin symbol and Live factory chain. The current DV finite stage owner
retains Arc source/admission/offset resources through registered child wait;
streaming renderer and encoder share those resources through separate existing
supervisors. That does not by itself prove every possible descendant is gone.

Manager lifecycle resolution: managed-graph custody must gather every actual
identity-matched managed-child wait plus writer/non-process close. Recipe lists,
leader receipts, job Drop/kill-on-close and elapsed time do not prove unmanaged
descendant closure. Unsupported closure keeps UnknownHeld and bars speculation;
reconciliation belongs to the SAME existing cleanup owner using positive OS
job-empty/group/descendant evidence where available, never a watchdog. Distinguish
unknown expansion evidence from actually live residency so supported ordinary
graphs do not permanently leak reservations. Cancellation/panic followed by
confirmed reconciliation and the positive normal-cleanup path need real tests.

Clock anchors verified: `cluster/clock.rs:PeerClockOffset::Bounded` carries signed
offset, uncertainty and monotonic observation age (25 s maximum); continuity and
membership/admission generations are serialized in `capture_with`. Existing
public acquisition may soften Unknown in advisory mode: it is NOT a strict
adaptive-deadline proof. The new consumer must use strict bound/revalidation
independent of advisory mode, or refuse new adaptive Prepare/Adopt.

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
