# Adaptive playback resources — implementation and evidence ledger

**Status:** open · implementation active · updated 2026-11-20.
Draft effort #1248; release qualification and deployment remain open.

[Plan](ADAPTIVE-PLAYBACK-RESOURCE-PLAN.md) ·
[finite delivery](ADAPTIVE-PLAYBACK-DELIVERY.md) ·
[policy](ADAPTIVE-PLAYBACK-PRODUCTION-POLICY.md) · [containment](ADAPTIVE-PLAYBACK-OWNED-CONTAINMENT.md) ·
[support ADR](ADAPTIVE-PLAYBACK-FINALIZATION-SUPPORT.md) ·
[collector](ADAPTIVE-PLAYBACK-ORDINARY-COLLECTOR.md) ·
[attempt root](ADAPTIVE-PLAYBACK-ATTEMPT-ROOT.md) ·
[retained exit](ADAPTIVE-PLAYBACK-RETAINED-EXIT.md) ·
[capture](ADAPTIVE-PLAYBACK-CAPTURE-CUSTODY.md).

## 2026-11-20 continuation — singleton capture ordering and service observations

The normal embedded Hiqlite route in `34eecfab` can consider an ordinary finite
software trial when the local daemon is the sole committed voter and there are no
learners. The original trial retains the membership admission hold until
confirmed cleanup or handoff to its surviving, fully charged producer. The
same original media and output descriptors must prove local noswap tmpfs
before the contained producer starts.

A single capture census checks durable recording and rule obligations. An
applied change to either table or to replica authority invalidates that census
and wakes the original driver. Settings and rule saves can commit while the
trial drains. Capture entry keeps its original local protection, and a new
member must wait for the membership hold to clear before it can serve. An
unknown cleanup result retains the hold.

Multi-member or unproven Hiqlite authority keeps normal conservative
admission. Singleton runtime acceptance and shared-domain trial authority
remain open.

`bd53089a` binds supported adaptive attempts to the original file stamp
and applied shared-source mapping. The existing attempt holds that mapping
through confirmed physical close. `8140ad6c` merges current main
`f771a063f`, including MPV short startup, and retains the verified-media
check before a first-publication unpaced capacity refusal.

`46dcc51a` adds input-paced Live TV observations from committed segments
and separates the relay pump's downstream queue and upstream body waits.
Egress capacity remains unknown. `98edcd2d` adds the native singleton
fixture and routes Activity Stop through retirement of the exact finite
owner.

The normal pinned macOS hooks passed through `98edcd2d`. Linux Rust 1.97.1
compiled the daemon and Core fixture targets at `05e9203e` (tree
`37b3077f`) under the capped non-voter profile. Linux compilation of the
new layers, including the `adaptive_two_node` target with
`cluster-integration-tests`, and native runtime acceptance remain open.
The native fixture requires a non-root process, Landlock ABI 6 or later,
`clone3`, native FFmpeg and FFprobe, a private noswap tmpfs and an isolated
delegated cgroup v2. The fixture takes the supplied mount and cgroup paths
from `PLURX_TEST_FINITE_NOSWAP_TMPFS` and
`PLURX_TEST_FINITE_NATIVE_ROOT`.

## 2026-11-19 continuation — integrated source, acceptance open

The current-main integration through `93e4e292` passed the normal pinned
macOS hooks. On exact committed `93e4e292` (tree
`e4619ada`), Linux Rust 1.97.1 passed the daemon production check and Core
fixture test-frontend check in 281.47 seconds, under the 8 GiB/four-CPU
profile. These are compile checks. No Rust unit or test bodies ran.

The bounded M2 trial controller is integrated for the supported memory-backed
SQLite recovery scope. Runtime acceptance remains pending. Normal Hiqlite
speculation remains conservative until durable capture ordering through physical
entry and safe mixed writers are established. Selected finite admission protects
capture and delivery during trials. Cold-origin FEL placement can defer and
adopt. Worker ranking distinguishes measured common source pressure from
Unknown and reports relay occupancy separately.

Integrated observations report rolling encoder service from committed media,
shared AAC output from committed output, and DVR service from original capture
writes. Subtitle admission can reclaim admitted work before foreground refusal.
Relay admission remains held through physical drain. Rolling service coverage
remains incomplete.

The continuation through `d8e6fdf2` adds bounded Auto deferred FEL peer retry.
The next peer must advertise retry support, and the original attempt must have
confirmed `Closed` after capacity refusal. Retry uses the original deadline,
and the incumbent remains current until viewer commit. Admitted local Linux
file bodies also report bytes emitted from verified ranges and slack against
their original read deadline. This observation carries no cleanup authority.
The combined pinned macOS hook passed. Linux compilation of those additions
and their runtime acceptance are pending at this checkpoint.

Shared durable domains, all-path adapters, independent Astra review of the exact
candidate and runtime acceptance remain open across M0–M7. The physical Linux cases require an isolated environment with
Landlock ABI 6 or later and existing delegation; the inspected VM has ABI 4.
Qualification, promotion and deployment remain open.

## 2026-11-19 continuation — source integration, not acceptance

The current-main merge `e54d5d3e` passed normal hooks. Production native v2
`c4d87256` passed normal hooks, exact Linux production compilation with zero
warnings, and the core fixture frontend at one job in 77 seconds under an
8 GiB cap (5.84 GB actual peak, zero OOM counters). An earlier four-job
SIGKILL remains recorded; its cause is unproven. No new Rust unit or runtime
tests have run for this continuation.

The selected unprivileged Linux x86-64 finite AVC path now integrates restricted
entry with original domain/pidfd custody and partial-object cleanup. Delegation
and common-ancestor permissions are verified before selection and pinned at
preparation. Finite cgroup memory and task limits are charged, while unknown
producer, executor and native peaks remain Unknown; no trial permit is minted.
[Native entry](ADAPTIVE-PLAYBACK-NATIVE-ENTRY.md) records the exact boundary.

Background reclaim `f59b00b0` and its display fix `3a54ed193` passed normal
hooks and exact Linux production compilation; its physical fixture is unrun.
Placement `2900a6ed` is committed but its binding follow-up, native composition
and Linux checks remain pending. Named processed-peer `affa571a8` passed normal
hooks; its predecessor passed the development C18 Linux binary check, while the
later proof changes still need a combined check. Auto/staged work remains active.

Physical kernel cases are authored but environment-required and ignored by
default; qualification needs an isolated Linux environment with Landlock ABI 6
or later. The inspected VM reports ABI 4 and is unsuitable. Full M0–M7,
V1–V5, true local trials, all families, source/ingress accounting, diagnostics,
independent Astra 6.2 review, qualification and promotion remain open. Handoff
to Sol precedes unit execution. No deployment is claimed.

## Earlier continuation — source-bound task qualification

Pre-Winner ownership [PR #1429](http://forge.lan:3000/noirr/plurx/pulls/1429)
merged at `4dabcb100` after all eight jobs in gate 6675 passed on `340b73b8`.
Candidate and landing share tree `b8dd2379`; all 32 regression trailers are
retained. Its [31 scoped passes](ADAPTIVE-PLAYBACK-ATTEMPT-ROOT.md) preserve
original Entry/P-M/task custody and response backing through actual aliases.
Normal hooks, exact committed checks and paperwork passed. The compile uses
the existing 16/18 GiB profile; the new 8 GiB worker remains unchanged and
unqualified for the daemon test frontend.

[Retained S exit](ADAPTIVE-PLAYBACK-RETAINED-EXIT.md) merged as #1419 after
gate 6591, with 14 distinct scoped passes. The
[capture native/writer continuation](ADAPTIVE-PLAYBACK-CAPTURE-CUSTODY.md)
merged as #1435 at `d4a517a94` after gate 6687, with 19 new and 42 existing
scoped passes. Original [SQL claim/detach custody](ADAPTIVE-PLAYBACK-CAPTURE-SQL-CUSTODY.md)
has 71 scoped passes; #1444 merged after gate 6703.

[Restricted native entry](ADAPTIVE-PLAYBACK-NATIVE-ENTRY.md) implements the
atomic launcher policy and finite fd roles under qualification; production
graph integration remains open.

Complete family inventory, pre-M/SQL coordinators, finalizer output/scratch,
production owned-worker containment, constructor/allocator/executor/kernel
peaks, full R/S/F closure and real trials remain open. V1–V5, current-main
integration and Main promotion remain open; no deployment. Earlier selected
bridge, static, driver and collector evidence is preserved in the
[earlier checkpoint](ADAPTIVE-PLAYBACK-CAPTURE-CUSTODY.md#earlier-canonical-checkpoint).

## 2026-10-29 checkpoint — historical source-bound evidence

The earlier implementation and receipt identities are preserved in the
[finite-delivery checkpoint](ADAPTIVE-PLAYBACK-DELIVERY.md#2026-10-29-checkpoint--historical-source-bound-evidence).
They establish only the stated source/scope, not current acceptance.

The copy driver retains the same NeverRoot/RootOnly/Joined witnesses and
one-shot settlement. Positive post-Arm refusal may close the never-entered
root; actual retirement wakes the original parser even at its full output
window. Post-mint identity contradictions retain the witness and full charge
as a permanent hold. A missing task selector never proves no registered writer.
R15 now authors the actual entered-parser/full-window retirement case, without
claiming caller cancellation at every await. Complete preflight still precedes
one-shot witness consumption; capacity returns only after native and actual
application owners close. Copy has no R→S transfer.

Source physical/preparation closure and the real body barrier determine the
factory admission disposition independently of SQL release. Before-reservation
validation is classified precisely; a failed unpublished Direct start returns
its original sealed ticket through actual join/finalization. Known original
planning cleanup remains resumable after cancellation. Ordinary SQL failures
and proven pre-reservation refusals do not invent native uncertainty.
Genuine uncertainty retains its existing operation reservation and blocks
planned-restart drain/commit; unrelated starts use normal capacity, phase,
readiness and identity checks. No sticky uncertainty is cleared or refunded.
C5's source follow-up separates live-ticket custody from repair selection;
SQL and HTTP retirement coverage remains blocked by daemon failures.

Preparation retains its controller through positive join and same-operation
Store proof. Selected Linux software SDR Source planning retains original P/M
through recipe/media retirement. Before-factory availability I/O and non-Linux
preliminary opens remain outside that guard. Complete private/native/allocator/
executor peaks, P/legacy CPU coexistence, shared storage/provisional recovery,
broader adapters and selective recovery remain open. Earlier compiler results
and authored regressions do not close these acceptance gaps.

Earlier compiler receipts remain in the
[copy review record](ADAPTIVE-PLAYBACK-COPY-START-REVIEW.md#15-earlier-published-compiler-receipts).
They cover their original source and environment; they do not qualify this
continuation, the pending cgroup fix or current-task native execution.

Bounded direct recovery retains its held-lock and positive
same-boot/PID-namespace old-daemon-exit requirements. Separate obsolete
recovered-cache eviction retains its existing approval boundary.

Normative [plan](ADAPTIVE-PLAYBACK-RESOURCE-PLAN.md),
[acceptance](ADAPTIVE-PLAYBACK-RESOURCE-ACCEPTANCE.md) and
[design review](ADAPTIVE-PLAYBACK-RESOURCE-REVIEW.md) retain their historical
approval pins below. An explicit manager-accepted, source-grounded zero/service-
budget clarification now changes their bytes; review §6 records that delta
without inventing new independent approval. The coverage map and implementation header now describe current owners; this
administrative refresh does not change the approved requirements. Current
plan/acceptance/review SHA-256 respectively:
`272cacf853c54cf4126ea939e31f4fc1949240e565febced7a5b58bed91ac4d5`,
`73067de47ac10b0855637540e0e3d84724e0361aa72340de9c867bd4cf1159fd`,
`3c5a4b08f83617e73273e520aa4a9a78a60e69ab550618c97f2966756898e8ed`.

Historical M0 proposals and manager direction are kept in the
[contract history](ADAPTIVE-PLAYBACK-CONTRACT-HISTORY.md); current source and
acceptance status are recorded here.

## Implementation history through 2026-10-22

See [Adaptive playback history](ADAPTIVE-PLAYBACK-IMPLEMENTATION-HISTORY.md).

## Current source and compiler receipt

Published checkpoint `269923bb7` adds the selected finite path's bounded client
payload partitions; `ee8cc4674` adds streaming custody history and indexed
family/asset-operation membership. The pinned host all-target core+daemon+
cluster-fixture check passed in 112.938 seconds, and a later core-only check
passed in 12.445 seconds. Their receipts are recorded above. At that historical checkpoint, native archive checking for `269923bb7` had no
result. Current compiler receipts and source attribution are recorded above.

The selected ordinary Linux software-transcode path is connected through local
admission, source/output custody, producer joins, publication, HTTP readers,
recovery and settlement. Other recipes remain legacy. S now keeps the enforced
immutable metadata, client-state, response and native partitions after known
retirement; strict proof that S is smaller than R has not run. Same-process
seeks reuse the existing full-plan owner, while cold-cache/restart remains open.
The M1 writer samples registered-process CPU/RSS only after durable publication
and exposes advisory diagnostics; M2 policy does not consume those observations.
Direct-source adapter work is active. All V1–V5 and R1/M0–M7 remain open.
Current executed results are recorded at the top; final qualification remains open.

Earlier compiler receipts and source scopes remain historical evidence only.
They do not cover later source checkpoints; current native results must be
read from their exact source-bound receipts above.
See the [finite delivery table](ADAPTIVE-PLAYBACK-DELIVERY.md) for remaining
adapters and the [custody limits](ADAPTIVE-RESOURCE-CUSTODY.md) for recovery
scope.

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

## Historical M0 proposal

The original proposed contracts are preserved verbatim in [contract history](ADAPTIVE-PLAYBACK-CONTRACT-HISTORY.md#m0-proposed-exact-contracts-for-manager-check). They are not evidence of implementation or acceptance.

## Remaining milestone work

- M0: finish production call-site inventory; incorporate manager's read-only
  settings writer/form inventory; obtain bounded check of proposed contracts;
  refine schema guards and assign runnable fixtures for current storage layout.
- M1: implement portable observations and truthful diagnostics, no policy changes.
- M2: atomic dimensioned policy, retained physical custody, history and settings migration.
- M3: connect core custody to every adapter above; positive containment, typed
  nonprocess closure and restart persistence; real background yield/reap, Live/DVR.
- M4: guarded worker runtime/retry is connected; complete foreign-fence cleanup, provisional inventory recovery and remaining pipeline adapters. Shared source/storage identity remains unresolved.
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

## Historical manager direction

The revision-2 manager direction is preserved verbatim in [contract history](ADAPTIVE-PLAYBACK-CONTRACT-HISTORY.md#m0-contract-refinement-for-manager-check-revision-2). It records accepted direction, not implementation or acceptance.

## Selected ordinary finite-software VOD ownership map

This table maps the currently connected Linux ordinary software AVC path from
selection through retained serving and cleanup. It records implementation
locations, not V1–V5 acceptance: authored regressions have not been executed,
the requested-capacity accounting is not an RSS bound, and source stability,
restart/cache coverage, other recipes and adapters remain open.

| Stage | Production owner and concrete seam | Relevant regression leaf |
|---|---|---|
| Select the ordinary profile | `plurxd/src/vod/shared.rs::build_rendition_with_storage` calls `vod/finite_software.rs::selected` and `build`; copy, private, ephemeral, source-prefixed, DV, subtitle, shared-audio and non-software profiles stay on other paths. | No dedicated selection regression in this path. |
| Associate and open the original source | `plurx-core/src/adaptive_resources/runtime/finite_vod.rs::SoftwareVodOwner::prepare/open_source` registers source cells; `operations/source_files.rs::SourceCell::open_source` checks the same held descriptor and planned scanner identity. The Linux software profile also requires the planned executable digest/ELF identity before launch. | `adaptive_resources/runtime/operations/source_files/tests.rs::binary_digest_uses_same_source_descriptor_without_changing_its_offset`; `::publication_rechecks_the_same_held_file_after_in_place_mutation`. |
| Build the fixed software command and reserve the graph | `vod/finite_software.rs::build` derives the fixed `SoftwareAvcCommand`, source copies, output slots and `SoftwareAvcPumpPlan`, then prepares the typed graph in the same `NodeResourceRuntime`. This is a selected recipe profile, not an arbitrary command API. | `adaptive_resources/runtime/finite_vod/tests.rs::finite_software_writer_waits_for_original_start_sender_before_reading`. |
| Start and join the original producer | `operations/finite_vod.rs::start_finite_vod/join_finite_producer` and `runtime/finite_vod/pump.rs` keep the registered writer task and child in the runtime until joined; stop/cancellation routes retain the original task and root. | `adaptive_resources/runtime/finite_vod/tests.rs::finite_stop_before_task_registration_aborts_the_late_original_task`; `::finite_stop_aborts_original_task_while_join_mutex_is_held`. |
| Publish output through the held directory | `runtime/finite_vod.rs::SoftwarePump` submits init/segments to the same `FiniteOutput`; `operations/bootstrap/native/publication.rs::queue_scratch_publication_wait` and `sync.rs::queue_scratch_sync_wait` use the original native publication/sync jobs and their journal observations. | `adaptive_resources/runtime/operations/bootstrap/native/tests.rs::bootstrap_native_cancelled_join_waiter_keeps_actual_original_handle`; `::bootstrap_native_joined_result_is_retained_without_relaunch_or_new_ids`. |
| Attach the daemon rendition owner | `plurxd/src/renditiondir/finite_publication.rs::RenditionDir::from_finite_producer/finite_output` carries a selector to the native owner without duplicating its directory descriptor; `renditiondir.rs::join_writes` joins the finite owner. | `adaptive_resources/runtime/finite_vod/tests.rs::finite_software_writer_waits_for_original_start_sender_before_reading`. |
| Serve init and segments | `plurxd/src/vod/serve/serve.rs::open_materialized` and finite-ready construction choose `ReadyFile::Finite`; `read_rendition_artifact` uses bounded reads from the same `FiniteOutput`, not path opens. | No dedicated daemon serving regression cited here. |
| Inspect HLS init | `plurxd/src/http/hls/context.rs::bind_hls_init_context` uses borrowed bounded init inspection; parser metadata remains separately accounted from response and retained-file capacities. | Existing HLS context tests exercise complete and partial init parsing; they are authored, not executed for this status. |
| Hold response read custody | `runtime/finite_vod.rs::FiniteOutput::prepare_reader/finish` and `operations/bootstrap/readers.rs::prepare_finite_reader/finite_reader_step` journal bounded range reads and close the same retained reader lease. | No dedicated production-reader lifecycle leaf is listed in this table. |
| Transfer finished output R→S | `operations/finite_retirement.rs::retire_finite_output` joins the producer, freezes its exact head, and transfers the same bootstrap/native cells only after the durable retained swap; it does not reopen the child path. | `resource_custody/codec_v2/scratch/bootstrap/snapshot/tests.rs::frozen_v3_reference_streams_inventory_larger_than_sql_row` covers frozen inventory shape, not native transfer acceptance. |
| Recover and reuse retained output | `vod/finite_software.rs::build` checks `recovered_software_output` against media identity, object version and segment count before attaching the recovered selector; `runtime/operations/recovered.rs` rebuilds process-local accounting. | Recovered-cache acceptance still needs restart and source-stability qualification. |
| Retire retained S | `runtime/operations/bootstrap/retirement.rs` drives reader drain, owned-file removal, directory sync/unlink and exact retained-handle closure; `operations/finite_retirement.rs` settles the source reservation only after the physical retirement receipt. | Cleanup fold tests cover ordered events; native Linux end-to-end cleanup qualification remains open. |

The ordinary software AVC selection is the only VOD recipe path mapped here.
The map does not close V1–V5, R1 or M0–M7, and it does not establish a full
process-memory ceiling: allocator/runtime overhead, framework parser and
indexing allocations, cold-cache restart policy, other adapters and executed
behavioral regressions remain separate work.
