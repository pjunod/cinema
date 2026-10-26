# Adaptive playback resources — implementation and evidence ledger

**Status:** open · implementation incomplete · resumed 2026-10-25. Work stays
on `effort/adaptive-playback-resources`, effort PR #1248. Published source
`0fc539264d5a191db476abb5503f03a7ebaea32f` remains the last remote checkpoint;
the preserved integration extends beyond it and is not qualified.

**Owner:** direct Codex harness `/root`, sole integration source/index/commit
writer, following the user's resumption request. The previous Claude worker
ended at its iteration limit; its source is preserved. Isolated helpers deliver
bounded changes for serial adoption. The original Hermes implementation card
remains parked to prevent a competing writer. Final independent review remains
actual Fable 5.1 before behavioral qualification.

**Planning settlement:** the 2026-10-24 thinker decision is implemented in the
worktree. G1 fixes the journal contract: live multi-member V2 settlements use
an exact member/envelope check and durable scratch-fold validation; only the
single retained scratch manifest uses its retained-lineage branch. G2 permits
monotonic discovered-source copy growth under the ordinary SourceV1 rules.
Neither change permits copy removal, identity replacement or DTO-only refunds.

Positive P settlement derives facts from bound original native closure tokens,
durably enriches its four roots and both tasks, closes the original source,
then submits one seven-member settlement. M remains charged. Its admitted
structural sum replaces the arbitrary 64 MiB book allowance; settlement refuses
when actual survivor capacities exceed that sum. Uncertain native completion
retains a durable Hold and all charges. Known limitation: UnknownHeld P pins M
until daemon restart. Never-entered accepted P now uses an original sealed
no-task witness, original source cleanup and the same seven-member settlement.
Generic cancellation preflights P/M together; it cannot refund P while its
inputs survive. P member-vector backing is dropped before refund, and Arc flag
padding enters M's structural sum. A definite prepare rollback with live P/M
payloads remains conservatively held. These paths are compiled, not executed.

**Evidence:** source checkpoint `ab2f95b3d` includes the integrated planning,
copy, static and original-controller repairs and the exact-capacity M survivor
vector. Pinned Rust 1.97.1 host core+daemon all-target+cluster compilation passed
with stable source hashes in 42.657 seconds; log SHA-256
`71eab1bae088cbb0012d58bbd19d37ff70d74fff6ba0fd318bdf1ed0e1f0954c`.
The same committed checkpoint passed native x86_64 Linux compilation in
362.845 seconds, source stable; log SHA-256
`3ffb5068ef9a09fb3199e462f187f867f9b079cbc5badc2ec8f7ebc6e5aaba18`.
Both checks include all targets and cluster fixtures. Linux-cfg receipts
use stubbed C dependencies and do not establish native qualification. Sixty-two
new regression anchors resolve statically alongside the original 490 fields;
behavioral tests remain NOT RUN. Static paperwork passed 14 scopes; its failed
catalog mapping was repaired and catalog lint passed separately. Strict native
Clippy on the same `ab2f95b3d` source failed with 80 core diagnostics in 61.803
seconds (unused adapter paths, owner-value layouts and API lints); log SHA-256
`f499814a8f3cbf4defe1785cab5004066e94e8e00d8824f4022ea4b7e4508818`.
Complete-candidate Fable review and qualification remain pending.

**Remaining production work:** ordinary VOD partial-start closure, complete
copy actor/mailbox/command allocation bounds, typed static-S eviction on policy
changes, selective recovery and the other agreed pipeline,
worker/placement/policy/operations work. Static setup now constructs fallible
application owners before accepted R and installs response budget, launch and
lifetime before exposing its original preparation. Static admission returns the
original owner and application lease together. Conservative preacceptance
preparation/refund uncertainty retains AdmissionUnknown; explicit pre-reservation
refusals can close only after the same original controller/Store barrier. Automatic
SDR/AVC jobs now share the pure selected predicate, admitted no-follow source
open and exact claim/intent/caps/recipe validation; the stored carrier is pinned
across retries. M waits for known same-claim Store closure, original heartbeat
join and destruction of the original outer payloads. Cancelled waits retain the
same handle. Strict no-follow source acquisition can refuse configured symlink
roots. These source connections do not establish behavioral acceptance.
The copy actor frame projection still excludes dynamic mailbox, command/reply and executor
storage. Six producer evidence slots now use fixed inline storage, executor-loss
cleanup streams the original mailbox prefix, and the selected application lease
survives through original actor join and selector destruction. Deep-cloned
command/reply payloads and private Tokio allocation bounds remain open.
Helper existence and compiler success do not close the ordinary request path.

All V1–V5 and R1/M0–M7 remain OPEN. Actual independent Fable review and fixes
precede once-only affected behavioral qualification, fast lane and authorized
merge. No deployment. Bounded direct restart recovery retains its explicit
held-lock and positive same-boot/PID-namespace old-daemon exit requirement;
uncertain cases retain holds. Obsolete recovered-cache eviction is unchanged.

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

Earlier published checkpoint `4d094726a55f14f3f236085d348b2a079bb0321d`
passed native Linux core+daemon compilation in 218.172 seconds (log SHA-256
`02366bf282056ce847dcce09584fbb56954b8b1c9a1415f7d5a790084dea7c48`); its
415 authored anchors were not run. The parser/metadata/strict-import prototype
then passed host compilation in 87.870 seconds (log SHA-256
`ea8c7d9c7ce1e18f0e55679db4661e4f42f0c3357c3f529b92bc8b009635a745`,
`../evidence/direct-check-20261023-141059.json`); 423 local anchors at that point
were not run. These receipts predate the published source below.

Published checkpoint `dd0a5b4cb068abb051678186648d5968488c67fe` passed exact
native Linux core+daemon all-targets compilation with pinned Rust 1.97.1 (EXIT
0, 230.430 seconds; `../evidence/direct-linux-dd0a5b4cb068-RESULT.json`, log
SHA-256 `37b14a4bceb08c322eef719b155e5ebad8e3f6e5eafb4dc745f2154da6cf65ca`).
Its 415 authored regression anchors were not run. The published source has the
ordinary branch, same-held-source before/after-publication drift latch and
root-only cancellation. Current uncommitted work extends cancellation to the
original registered writer and the no-root/all-sources-registered/unstarted
bootstrap case, including once-only close of the same file on identity
rejection. Typed capacity routing, finite progress belief and advisory output
measurement are integrated. Earlier partial startup traversal, possible child,
restart/adoption/seeks and the full allocation profile remain open. Host
compilation is running and has not passed. All V1–V5 and R1/M0–M7 remain OPEN.
No behavioral tests, final Fable review or qualification have run.

The selected executable now uses a fixed eight-entry verification cache keyed by
all seven S1 identity fields and digest. Only a prior successful held-file native
ELF/hash scan can populate it; a cache hit rechecks the same held descriptor before
and after lookup. Miss/identity change/digest change never grants cached authority.
This avoids repeated image reads without treating pathname metadata as a native
launch proof. Private tests use isolated fixed caches, avoiding global eviction races.

The retained runtime now owns response reader cells and exact OpenReader/ReadRange/
CloseReader operations. The same held child opens and verifies the published file;
original latched jobs retain the File and one fixed buffer. Only known durable
read observation exposes an opaque chunk. Chunk drop returns that SAME buffer,
and waits for it release the bundle lock. Drop requests closure rather than
claiming it: normal completion, later serving and eviction use the same close
operation; only positive original close permits removal of the lease. HTTP
`ReadyFile` counted-range selection and native-error causality now use this
interface and pass host compilation. Active native reader owners and their exact
cursor/lease inventory now move with the same bootstrap owner through provisional
S and known R→S; the old output selection resolves to S after rebind. The runtime
also explicitly preserves its actual ledger durable tag: the initial prepare-
bundle ID is distinct from the journal obligation ID.

Retained cleanup vocabulary fences new readers, removes exact owned files, syncs
the child, unlinks the child relative to the held parent, syncs that parent, then
closes the exact folded descriptor-copy set. Failed/uncertain removals and closes
retain charge and cannot retry a numeric descriptor. The S cleanup state machine,
typed settlement and dormant eviction now use the original native jobs and
nonclone closure capability. Actual closed inventories are disposed before
refund; unresolved cleanup preserves the rendition registry and charge. Host
compilation passes; native compilation and behavior qualification remain open.
Typed S settlement reconstructs the latest closed fold in the journal transaction,
checks exact retained lineage and empty files/readers/handles/storage, and retains
the original physical receipt through known COMMIT. The selected Linux ordinary
software-transcode branch now connects counted HTTP serving, S cleanup and
native R→S transfer; other recipes remain on the legacy path. Full quotas/cold
memory policy, partial-start ownership, restart/cache adoption and broader
selection remain open. Source/reference JSON defaults preserve historical
encoding.

The active harness has connected the selected Linux ordinary software-transcode
path. Core exposes
[`finite_vod`](../../crates/plurx-core/src/adaptive_resources/runtime/finite_vod.rs):
`SoftwareVodOwner` reserves the whole role before source/output acquisition, and
nonserde `FiniteOutput` uses exact retained publication tickets. The shared native
publisher is in core and reused by the daemon. Its retained bootstrap owner binds
payload capacity, selected arm/observation IDs, original native job, same child
descriptor and durable observation before success; cancellation keeps the
operation in its runtime cell. Native publication is single use, including after
partial failure.

A private same-ledger overage API adds only newly observed debt and fences fresh
admission. A repeated or smaller observation cannot add duplicate charge or
refund it. This is live accounting, not durable overage/restart qualification;
the bounded audit projection still holds an over-limit result rather than
acknowledging a fabricated bounded success.

The prearmed source adapter retains the actual file and original native job,
checks scanner and `s1` identity using shared core code, and exports a nonclone
counted lease only after the exact acquisition observation. Existing
[`SourceFence`](../../crates/plurxd/src/fragment_index_cluster.rs) can hold that
lease without reopening the legacy pathname. The finite-output slice adds an
explicit `PublishMetadata` journal operation for opted-in `identity.json` and
`encoded-process` outputs; the software-transcode profile requires its marker,
while copy omits it. The same-child native sync cell retains the original job
through known Arm/Observe and bounds each batch to 16 files. The daemon
[`PendingWrites` finite adapter](../../crates/plurxd/src/renditiondir/finite_publication.rs)
connects metadata durability, and startup passes the already-open runtime Arc to
`VodServe::Shared`.

The VOD generation engine, identity check and conversion are core-owned; the daemon re-exports them while retaining its existing test bodies. The `finite_vod` command, pump and initial-unit reader are joined to the physical graph's concrete `SoftwarePump` task and guarded actual pipes. The pump reads the exact init boundary, then queues the encoded-process marker, identity, init and segments through selected publication tickets and syncs them with bounded progress and diagnostics. Known durable Observe ends pending-I/O payload accounting; actual payload is cleared and the same directory returned under the original bundle lock before another queue operation can enter. The whole role reservation remains held until retirement.

The source-bound AVC route now preflights and accounts the bounded Segmenter prefix, merge, finish split and published batch through `GenerationMemoryLimits`/`Generation`. Init parsing has an explicit four-copy bound; parser reads, fragment inventory allocations and plan-boundary vectors are fallible. A canonical grid command and producer clock alignment are compiled. These are bounded components, not an end-to-end quota: parser metadata, native process, launcher and runtime-journal memory remain unresolved. The retained task-start latch is released only after known typed task Observe and same-ledger entry recheck. The pump waits for publication/sync bundle ownership through the same locked-queue helper; public sync queue behavior remains fail-fast. Finite control retains stop intent across root registration and uses the exact original control without waiting on Child or holding the journal lock, but whole closure is not proven.

The core finite-request path requires an immutable `SoftwareAvcCommand` and `SoftwareAvcPumpPlan` in the same reserved bundle before I/O, verifies actual source identity/path/scanner inputs, prearms source cells without exporting a lease, and carries the same `File` through spawn. It enriches the source-copy observation only after actual root launch, then registers and observes the retained pump task once. A cancelled graph-lock wait preserves the root-entry latch. The SourceCell parent-close job records Linux `close(2)` success only on return 0; an error or unknown outcome is never retried on a potentially recycled descriptor, and cancellation retains the original job/latch. This is parent-file closure only: whole SourceMember/root closure and refund proof are not wired. Shared-fd helper extraction preserves daemon behavior.

The producer-join slice freezes graph entry while allowing the original writer to finish its admitted publication/sync work. The bundle lock is released before the same writer/root joins; terminal diagnostics stay in the original graph record and callers receive copy-only summaries. Stop intent retains an abort handle for the original task and the original root registration, including stop-before-registration. Existing source/root/publication/sync jobs are joined in place without launching or observing replacement work. After writer/root joins, the same ledger fences fresh entry and positively observed source parents close through their original close jobs. This is producer quiescence, not a physical-close receipt: inherited copies, Linux fixed-image/known-helper conclusion, unobserved partial starts and retained output/reader ownership remain unresolved; R is still charged.

Durable output Fence/Freeze, the V3 frozen inventory, proof-bearing journal R→S mutation/recovery and post-COMMIT same-ledger swap are connected. Adopted output is read-only and reader cost growth is checked against the same stored S resources. Native publication records one positive writable-file close before releasing its held directory or payload; close failure/unknown stays charged without retry. The selected Linux ordinary software-transcode branch connects these components to original fixed-process/source closure, counted HTTP serving, cleanup and a provisional same-native S owner. Full native/index/runtime memory accounting is still required; partial-start ownership, restart/cache adoption and broader recipe selection remain open.

The selected Linux ordinary software-transcode branch is connected; other
recipes remain on legacy admission. Same-process seeks reuse one rendition
owner and its full-plan producer; cold-cache reuse remains an R1 gap. Full
memory profile, partial-start handling and broader recipe selection remain
open.

All V1–V5 remain **OPEN**. Still missing: ordinary entry and complete producer
ownership/terminal composition; the first metadata-marker caller; complete
memory/index and durable-debt accounting; actual reader leases; retained-output
transfer, conserving R→S and exact cleanup/settlement/capacity reuse.

The latest published base is `dd0a5b4cb068abb051678186648d5968488c67fe`. Its
native Linux core+daemon all-target Rust 1.97.1 compile passed in 230.430 seconds.
The 415 authored regression anchors were not run. The published source has the
ordinary branch, same-held-source before/after-publication drift latch and
root-only cancellation. Current uncommitted work extends cancellation to the
original registered writer and the no-root/all-sources-registered/unstarted
bootstrap case, including once-only close of the same file on identity
rejection. Typed capacity routing, finite progress belief and advisory output
measurement are integrated. Earlier partial startup traversal, possible child,
restart/adoption/seeks and the full allocation profile remain open. Host
compilation is running and has not passed. All V1–V5 and R1/M0–M7 remain open;
no behavioral tests, strict Clippy, final independent review, qualification/fast
lane, merge or deployment are claimed.

## Current source and compiler receipt

Published checkpoint `269923bb7` adds the selected finite path's bounded client
payload partitions; `ee8cc4674` adds streaming custody history and indexed
family/asset-operation membership. The pinned host all-target core+daemon+
cluster-fixture check passed in 112.938 seconds, and a later core-only check
passed in 12.445 seconds. Their receipts are recorded above. Exact native archive
checking for `269923bb7` is still RUNNING, so it has no result yet.

The selected ordinary Linux software-transcode path is connected through local
admission, source/output custody, producer joins, publication, HTTP readers,
recovery and settlement. Other recipes remain legacy. S now keeps the enforced
immutable metadata, client-state, response and native partitions after known
retirement; strict proof that S is smaller than R has not run. Same-process
seeks reuse the existing full-plan owner, while cold-cache/restart remains open.
The M1 writer samples registered-process CPU/RSS only after durable publication
and exposes advisory diagnostics; M2 policy does not consume those observations.
Direct-source adapter work is active. All V1–V5 and R1/M0–M7 remain OPEN; tests
remain NOT RUN, and there has been no final review or qualification.

Earlier compiler receipts and source scopes remain historical evidence only.
They do not cover the checkpoints above or the in-progress native archive check.
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
