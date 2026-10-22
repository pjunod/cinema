# Adaptive playback resources — implementation and evidence ledger

**Status:** open · M0/M1 partial; M2 capacity and M3 custody slices compiled.
**Updated:** 2026-10-22 at committed checkpoint `ffd7f58379856a3ad31ff9789aff3520cf26cf6e`.
**Integration owner:** direct Codex harness (`/root`; actual model unspecified),
primary and sole integration writer. Lower-model assistants may take bounded,
non-overlapping tasks. Former Hermes Sol task `t_161f6652` is parked; its worker
exited and its heartbeat remains paused. One integration branch
`effort/adaptive-playback-resources`, draft PR #1248. No deployment, new source
closure, tests, review or qualification are claimed. Complete implementation,
actual independent Fable 5.1 review and fixes precede qualification and landing.

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

Historical M0 proposals and manager direction are kept in the
[contract history](ADAPTIVE-PLAYBACK-CONTRACT-HISTORY.md); current source and
acceptance status are recorded here.

## Direct harness integration — 2026-10-22

The active harness is connecting the first ordinary finite VOD path. Core exposes
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
lease without reopening the legacy pathname. The finite-output slice following that checkpoint
adds an explicit `PublishMetadata` journal operation for opted-in
`identity.json` and `encoded-process` outputs; the software-transcode profile
requires its marker, while copy omits it. The same-child native sync cell retains
the original job through known Arm/Observe and bounds each batch to 16 files.
The daemon [`PendingWrites` finite adapter](../../crates/plurxd/src/renditiondir/finite_publication.rs)
connects metadata durability, and startup passes the already-open runtime Arc to
`VodServe::Shared`. These are bridge components: ordinary
`build_rendition_with_storage` still starts with legacy admission, and the
first metadata-marker creation caller is not fully wired.

All V1–V5 remain **OPEN**. Still missing: ordinary entry and root producer
ownership/task composition; the first metadata-marker caller; complete
memory/index and durable-debt accounting; actual reader leases; retained-output
transfer, conserving R→S and exact cleanup/settlement/capacity reuse.

The latest committed/pushed checkpoint
`ffd7f58379856a3ad31ff9789aff3520cf26cf6e` passed pinned Rust 1.97.1 host
core+daemon all-target compilation in 28.406s (EXIT 0, stable source). Static
bookkeeping and 326 regression anchors resolve against that candidate; draft PR
#1248 is updated. The next source slice adds ten authored sync/metadata
regressions, not run. Its corrected pinned host all-target checks passed core
in 50.486s and daemon in 139.073s (EXIT 0 each, stable source hashes). The
receipts are `direct-check-20261022-180814.json` and
`direct-check-20261022-180506.json` in the task evidence directory. Earlier
compiler failures and their fixes remain retained. No behavioral tests, final independent review, qualification, fast lane
or merge have run. Linux/Windows current-source compilation, strict Clippy,
restart evidence and behavior qualification remain open.

## Current source and compiler receipt

`7b783effa` extends the SAME prearmed native root cell through no-follow component
opens and exclusive child mkdir/open: actual descriptors, original job/latch,
known Arm/Observe retries, retained parent+child and Linux descriptor mount ID.
Entered errors/partial creation/EEXIST retain facts; no implicit adoption. The
internal runtime can reach ParentReady/SinkReady, NOT ordinary application entry.
`7184dcd01` rejects foreign owner/boot labels before arm. `735387e96` adapts the
existing native publisher to consume a moved held directory and selected I/O ID,
with no path fallback/local UUID; actual ordinary caller still uses legacy input.
These mechanisms are NOT permission/proof constructors or completed V2 closure.

Pinned1.97.1 current host core all-target check PASS11.885s; combined core/daemon
108.631s retains unchanged daemon source. Seven new host bodies COMPILED NOT RUN;
two Linux-only bodies compiled by controller Linux check, NOT host. No bodies run.
Warnings, NOT strict Clippy; initial closure-type failure preserved/fixed.
Controller exact7b783effa Linux117.758s and Windows132.363s checks PASS; unchanged
native step/statx/test sources retain those scopes. Later common guards and held
publisher have host checks above, not claimed exact7b Linux/Windows coverage.
No behavior/restart qualification. Earlier source/evidence remains retained.

The authoritative [V1–V5 checklist](ADAPTIVE-PLAYBACK-DELIVERY.md#authoritative-first-ordinary-vod-source-checklist)
has ALL FIVE OPEN. Missing: ordinary pre-acquisition/source/producer composition,
actual held-child + known-output outcome before manifest/readers, markers/sync,
bounded native/index memory and overage debt/fence, retained snapshot/private-proof
ONE atomic R→S and same-ledger swap, exact settlement/capacity reuse. Conservative
restored holds remain; R1 selective/positive old-domain recovery and later M0–M7
remain separate required integration. No ordinary finite/physical-quota pass.

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
