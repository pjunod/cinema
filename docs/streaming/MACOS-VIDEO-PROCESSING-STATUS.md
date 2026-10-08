# macOS video processing — execution status and decisions

**Status:** active · **Updated:** 2026-10-07 · **Owner:** managing agent with
three GPT-6.1 Sol builders · **Integration:** `effort/macos-video-processing`.

Companion to the [design](MACOS-VIDEO-PROCESSING-DESIGN.md) and
[implementation plan](MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md). This page is
the execution ledger: current work, evidence, decisions and remaining work.
A plan or compiled change is not hardware qualification. The
[experiment evidence](MACOS-VIDEO-PROCESSING-EVIDENCE.md) retains exact
measurements, negative results, provenance and reproducible raw receipts.

## 1. Current position

M0–M3 implementation is integrated at `93658e211`; independent adversarial
review of the main-bound candidate is active. No installed packages or
live services have changed. The initial delivery remains measured SDR scaling
and HDR10-to-SDR processing; P5 experiments occur early without prematurely
enabling Dolby processing. Extensions retain their independent acceptance.

| Workstream | Owner | State | Evidence or next dependency |
|---|---|---|---|
| Integration and status | Manager | active | Separate clone from `09577b9f3`; pinned Rust 1.97.1 and Cargo 1.97.1 available |
| M0 runtime / M1 FFmpeg packaging | Sol native builder | integrated | Official Jellyfin `v8.1.3-1` arm64 package acquired and checksum verified; `e193b0670`: checksum-pinned complete official package; 38 required declarations; SDR/Metal smoke evidence |
| M0/M1 measurement harness | Sol harness builder | integrated; revised HDR measured | `44050ab2a`: inventory/run CLI; five valid SDR pairs show 87.80% lower CPU cost and +0.71% throughput on synthetic material |
| M1 embedded smoke corpus | Sol fixture builder | integrated; sustained sources built | `0cd06e92b`: three 320×180, 12-frame clips, 8,846 aggregate bytes; repeat generation hashes match; separate synthetic 60-second 4K sources |
| M2 core / shared M4–M5 argv | Sol native builder | integrated, including metadata correction | `47eab3a75`: immutable optional Mac context, only demonstrated SDR/Metal graphs, pinned compile and normal hook passed |
| M3 probes / manager / settings | Sol fixture and harness builders | integrated; independent review active | `532e78d5f`: bounded probe module; `93658e211`: registered all-targets compile and normal hook passed; isolated settings, repair/restart and actual SDR/HDR VOD bytes observed |
| M6 acceptance / main promotion | Manager and builders | isolated daemon evidence recorded; full acceptance open | Exact integrated candidate and adversarial review, then coordinator-owned validation/merge |
| E1–E4 extensions | subsequent scoped work | not started | Separate evidence and demand; no capability inherited from an HDR10 result |

Jellyfin FFmpeg is the required implementation baseline. Local Homebrew
FFmpeg is not a comparison target or substitute. The official package is an
experiment candidate; its capability inventory is not a performance result
or proof of the deployed daemon version. No local daemon has been found.
The SDR result establishes synthetic processing feasibility, not production
capacity or perceptual qualification. The original HDR comparison is invalid:
the CPU output retains stale HDR side data, while Metal tone-maps before
scaling and therefore processes four times as many pixels. Both causes are
corrected explicitly in a separate revised comparison: five valid paired
HDR runs show 97.40% lower CPU cost and +2.14% median paired throughput.
Four of five pairs meet the CPU/throughput consistency bound; the fifth
is 5.51% slower and remains recorded. Energy and
copy counts remain unmeasured. The four-way artificial P5 diagnostic retained
metadata, without establishing RPU visual influence or hardware reconstruction.
Small SDR/Metal outputs have factual geometry, color-signaling and ramp
observations; these are not full visual or playback qualification.

## 2. User-directed workflow and file ownership

On 2026-10-07 the user authorized parallel Sol 6.1 builders, proper commits,
batched PRs, and autonomous routine decisions. These explicit instructions
supersede the earlier sequential-task and per-task test/review language:

- All work occurs in task-owned separate clones. User checkouts are untouched.
- Builders commit normally; the manager integrates onto the effort branch
  and prepares a batched main-bound PR. No per-builder PR/review is required.
- Adversarial review happens once the main-bound PR is ready. Address its
  findings before running the fast lane. Do not run unit suites while building.
- **Merge handoff amendment:** the user subsequently assigned queueing,
  batched validation and main merging to
  [Coordinate PR merge batches](codex://threads/01a11907-f720-71b1-8c51-89902b919e6f).
  This effort hands off the reviewed commits/PR, regression commands, evidence
  and limitations; it does not independently run the merge queue or duplicate
  that session's unit validation. The coordinator may fix superficial test
  failures without behavior changes. Behavioral failures return to this
  effort for a proper fix and review before rejoining the queue.
- Compiler, formatter and lint feedback remains available during building;
  hardware experiments establish feasibility and are distinct from unit suites.
- Each required check needs one valid passing result for the code being merged.
  Fix failures and rerun affected failed checks, retaining valid passing evidence
  where supported. Full unrelated unit failures belong to the separate process.
- No hidden feature gates or additional playback watchdogs. Any necessary
  manual switch belongs in Developer with advisory readiness and a saved choice
  that is always accepted. Existing owners handle real runtime incompatibility.
- Retain only useful source/evidence and remove task-owned transient artifacts
  during cleanup. No credentials, private media or real infrastructure identifiers
  belong in committed receipts.

The session supports four concurrent agents including the manager, so three
Sol builders are active. Subsequent work reuses those slots. Builders have
received and acknowledged the full user instruction packet.

| Owner | Initial exclusive file ownership |
|---|---|
| Manager | This status page, document index and workflow amendments; integration and PR metadata |
| Native builder | `scripts/build-macos-video-ffmpeg` and `scripts/macos-video-build/` helpers if needed |
| Harness builder | `scripts/bench-macos-video`, `scripts/macos_video_bench/`, focused harness test source |
| Fixture builder | `crates/plurxd/fixtures/macos-processing/`, fixture generator and focused fixture test source |

The initial files above are committed. Current application ownership is:

| Owner | Current exclusive ownership |
|---|---|
| Native builder | Core `transcode/{pipeline,decode,mod,vod,recipe}.rs`, associated core regressions and decoder-selection test source; atomic exhaustive daemon metric/count-array rows |
| Harness builder | Daemon main registration, transcode manager construction/planning/recovery, subsequent diagnostics, settings HTTP and Developer UI integration; one stored-key constant and reprobe route registration |
| Fixture builder | New daemon `macos_video.rs` probe module, embedded-fixture preparation and its focused test source |
| Manager | Documentation, integration, final review coverage and validation/PR records |

Agents do not edit another workstream's files. Shared catalog rows integrate
as separately owned hunks. Hardware benchmarks run serially without concurrent
compiler load to avoid contaminating measurements.

## 3. Decisions and limitations

| Decision | Reason | Revisit when |
|---|---|---|
| Start three disjoint M0/M1 workstreams | Packaging, harness and fixture construction can proceed independently before planner integration | First usable graph and corpus are available |
| Keep deployed services/packages unchanged during experiments | Private candidate builds allow attribution and preserve incumbent AC-4/Dolby behavior | Qualification supplies a deployment decision |
| Use explicitly pinned Rust executables | The interactive shell selects a different Homebrew compiler | Every Rust integration/compile session |
| Use official checksum-verified Jellyfin FFmpeg for all candidate and baseline experiments | Preserve the required AC-4 and Dolby functionality; stock Homebrew FFmpeg is irrelevant to this effort | A demonstrated package defect requires a different pinned Jellyfin build |
| Prefer the official complete Mac archive to a source rebuild | The archive already exposes the needed filters and Apple-only dynamic library closure | A required behavior cannot be supplied by the official package |
| Record negative or unavailable results explicitly | Missing evidence cannot justify performance or color claims | Hardware/corpus/provenance becomes available |
| Scale HDR at 10-bit precision before the Metal mapper | Original graph performed tone mapping on four times as many pixels as the production CPU comparator | Revised full comparison and temporal/visual acceptance |
| Fix stale MDCV/CLL only on the shared CPU + zscale HDR-to-SDR chain | That route reproduced incorrect residual HDR signaling; `tonemapx` already removes it, so unrelated Dolby/Metal paths need no change | Separate corrective commit and focused regression review |
| Version identity only for routes changed by consumed-HDR metadata cleanup | This root-cause fix changes legacy output bytes; preserving their previous cache key would be incorrect | Verify unaffected SDR and HDR-preserving hashes remain stable |

## 4. Validation and handoff ledger

- Pinned compiler availability verified: Rust 1.97.1, Cargo 1.97.1.
- First three builder commits plus harness correction `308921d3f` integrated
  on the effort branch. Each normal
  tracked hook passed catalog lint, pinned Rust format/Clippy and JavaScript
  syntax. No standalone unit suite was invoked.
- Unit suites: not run for this effort; deferred per user direction.
- Official Jellyfin source tag: `v8.1.3-1`, source commit
  `253db2a7b0a8045c54ce68ce33d7f601229b1822`.
- Official arm64 archive SHA-256 checked against the release API:
  `22445d7299742749ad2eeb9ce87963d50def0357e45b3e6c7b69987c8365dbf6`.
- Capability listings show `scale_vt`, `tonemap_videotoolbox`,
  `tonemapx` with `apply_dovi`, and AC-4 decoding. SDR and Metal HDR10 graphs
  executed; listings alone do not establish AC-4 or Dolby sample acceptance.
- Fixture generation/probe: 12 decoded frames each for SDR8, SDR10 and PQ;
  the PQ source includes explicit VUI and mastering/content-light metadata.
  The generator was corrected after probing showed x265 omitted primaries and
  transfer unless explicitly supplied in its VUI options.
- Native graph smoke checks: SDR scaling and Metal HDR10 each produced three
  decodable H.264 frames at 160×90, BT.709 matrix/transfer/primaries and TV
  range, with ordered timestamps and no retained HDR/Dolby side data.
- Literal retained surface format: `videotoolbox_vld`. The native color-scale
  HDR alternative remains diagnostic; it is not an accepted production path.
- Execution required ordinary host IOSurface access outside the tool sandbox;
  sandbox denial is recorded as an environment limit, not a codec failure.
- Hardware evidence: VideoToolbox filter/encoder surfaces, Metal device and
  hardware-required H.264 encoding observed. Pinned FFmpeg only requests,
  rather than requires, hardware HEVC decoding; actual HEVC hardware use is
  not proved by the surface spelling and remains unavailable evidence.
- Output gray ramps were observed to be monotone. SDR scale gray values
  matched the analytic input within one code value. These small observations
  do not replace full SDR/HDR perceptual or temporal qualification.
- Harness integration smoke caught two-flag versus three-flag FFmpeg filter
  table differences; parser corrected before repeating only the failed smoke.
- Harness wiring smoke: four 1-second runs passed expected output contracts.
  They are reported as smoke-only, with zero sustained pairs.
- Sustained hardware benchmarks: synthetic 60-second, 2160p24 SDR/PQ
  sources built. Five warm SDR pairs pass output contracts: median paired
  CPU cost reduction 87.80%, throughput change +0.71%. CPU reduction is
  consistent in all five pairs. These fast runs lack enough steady 5-second
  windows for capacity qualification.
- Original HDR rows retained: CPU outputs fail stale MDCV/CLL checks;
  candidate outputs pass, but throughput is approximately 19% lower by the
  median paired comparison. Neither its CPU reduction nor its throughput
  establishes an accepted HDR result. Revised scale-before-Metal graph
  passed a three-frame correctness smoke. Revised five-pair results pass
  output contracts and meet the synthetic CPU criterion in four of five
  pairs; the 5.51% throughput regression in pair four is retained explicitly.
- Status/checklist and merge handoff committed as `fcbef06ac`; normal hook
  passed. The merge coordinator owns subsequent batch validation/landing.
- Mac core contract committed as `47eab3a75`: pinned core check with
  `--features hiqlite-store --all-targets`, plus normal hook passed. Tests
  compiled, not executed. Runtime module `532e78d5f` is unregistered at that
  commit; its standalone hook is not module type-check evidence. The daemon
  builder now owns registration and the integrated compiler check.
- Registered daemon all-targets check and executable build passed on
  `a17de21fe` plus daemon changes. Normal hook identified denied unwraps in
  new test source; these were corrected without executing tests.
- The final registered daemon check passed after narrowly invalidating stale
  `plurx-core` artifacts in the shared target. The earlier failure reported a
  staged constant missing from cached metadata produced by another clone;
  no application workaround was added. Builders use exclusive compiler-target
  ownership during the final integration checks.
- Isolated daemon settings, pending/available observations, admin reprobe,
  corrupted fixture repair, persistence and orderly restart passed.
- Actual non-normalized encoded VOD delivered the first SDR and HDR10
  fragments through the canonical Mac plans: 48 decoded 1080p BT.709 frames
  each. Normalized continuous VOD retains its native source-probe refusal;
  public live presentation is removed. Neither is claimed qualified.
- Native bound-source investigation found reusable source/cache/lifetime
  ownership but no small supported equivalent for immutable parser execution
  and descendant containment. This is an explicit parser-packaging and
  confinement follow-up, not permission to bypass the source boundary.
- Adversarial implementation review: active on `93658e211`, with independent
  core/package, harness/daemon integration and fixture/runtime reviewers.
- Main PR: not opened.

Later entries will record commit IDs, root-cause observations, commands,
accepted graphs, failed checks and their targeted reruns, qualification
limits, review dispositions and cleanup. Completion requires honest closure
of the applicable acceptance rows, not merely a successful compilation.

## 5. Full scope checklist

This checklist traces the complete agreed design, including deferred work.
**Integrated** means committed to the effort, not merged or qualified.
**Building** means an assigned implementation is active. **Awaiting evidence**
means proof is still owed; it never means passed. **Planned** is initial-scope
work not yet started. **Follow-up** is an explicitly separate extension,
not cancellation or completion. A measured negative result may retain the
incumbent, with its reason recorded. The linked design and implementation
sections remain the detailed acceptance specifications.

### 5.1 Inventory, native processing and experiments

| ID / plan reference | Agreed item | State / remaining evidence |
|---|---|---|
| M0-01 · implementation §3 | Exact deployed daemon FFmpeg/FFprobe and service environment | Awaiting evidence: no local daemon found; official Jellyfin candidate must not be called the deployed baseline |
| M0-02 | Executable/library hashes, source/patch provenance, arch, SDK/minimum OS | Integrated package acquisition; arm64 archive and Apple-only library closure observed; x86 runtime unmeasured |
| M0-03 | Incumbent feature inventory, including AC-4, Dolby, subtitles, mux/audio/pacing | 38 required declarations checked; end-to-end sample matrix awaiting evidence |
| M0-04 | Existing SDR/HDR10/P5 baseline outputs or documented failures | Reconstructed SDR/HDR10 exercised; stale HDR metadata failure retained; deployed and P5 baseline awaiting evidence |
| M1-01 · implementation §4 | Bounded reproducible A/B harness, raw failures retained, explicit tools and cache conditions | Integrated; first-use and five paired warm runs recorded separately |
| M1-02 | Lawful embedded SDR8/SDR10/PQ corpus, recipe, hashes and expectations | Integrated; genuine PQ values and explicit VUI, not merely SDR relabeled HDR |
| M1-03 | Complete decode/process/encode graph and observed representations | SDR and Metal smokes observed; final HDR order is 10-bit VT scale, then Metal tone map |
| M1-04 | Native Apple HDR color mapping alternative | Diagnostic only; no accepted production variant; execution and retagging alone are insufficient color proof |
| M1-05 | SDR benefit at equal output and encoder controls | Five valid synthetic pairs support CPU benefit; broader workload and production evidence still owed |
| M1-06 | HDR10 benefit with fair work ordering and correct output | Five revised pairs complete; CPU feasibility criterion met in 4/5, visual/production acceptance open; original invalid comparison retained |
| M1-07 · design §§2, 6 | Unified-memory attribution: mapping, allocation, conversion, synchronization and copies | Awaiting instrumentation; shared physical memory does not prove zero-copy, and CPU reduction is not a measured copy count |
| M1-08 | Actual hardware reconstruction, separate from VT surfaces/hardware encode | H.264 hardware-required encoder observed; actual HEVC hardware reconstruction remains unproved |
| M1-09 · implementation §§1.1, 9.1 | Early P5 four-way software/hardware decode × CPU/Metal renderer experiment | Four-way artificial FATE transport smoke passes all ten frames; physical hardware reconstruction, RPU pixel influence, strict/seek semantics and genuine content still await evidence |
| M1-10 | Same-build A/B plus deployed-build baseline when replacing package | Same official Jellyfin build used; deployed comparison awaiting access/evidence |
| M1-11 · design §3.3 | Direct Core Image/VideoToolbox helper, custom Metal kernels, Vulkan/libplacebo alternatives | Deferred alternatives only if existing filters fail a concrete requirement; no parallel processing stack being added |

### 5.2 Core contracts, runtime and production delivery

| ID / plan reference | Agreed item | State / remaining evidence |
|---|---|---|
| M2-01 · implementation §5 | Typed VT surface and measured SDR/Metal pipeline variants | Integrated; only `VtScaleSdr` and `VtToneMapMetal` admitted initially |
| M2-02 | One immutable resolver and shared rolling/VOD argv | Integrated; no mutable filters behind recipe identity or second planner |
| M2-03 | Decoder overrides, continuation restrictions, source facts and output grade | Integrated; explicit software choice and incompatible presentations retain incumbent |
| M2-04 | Conditional Mac identity: graph, binary/libraries, OS, arch, hardware class | Integrated; excludes host name, probe time and benchmark score |
| M2-05 | Unaffected plan/recipe hashes preserved | Integrated; separate scoped identity revision for genuine consumed-HDR metadata fix recorded in §3 |
| M2-06 | Audit `on_gpu`, surface consumers and copy-related comments | Integrated; frame representation must not imply physical memory placement |
| M3-01 · implementation §6 | Bounded asynchronous post-startup probes and immutable generations | Integrated; one graph at a time, cancellation/reaping through existing child owner, no watchdog |
| M3-02 | Embedded fixture integrity, private cache, atomic corrupt-cache repair | Integrated; no network/developer files/runtime encoding dependency |
| M3-03 | Preparation/probe deadlines, disk/permission/cancellation failures and reprobe | Integrated; explicit unavailable diagnostics and restart/reprobe recovery |
| M3-04 | Saved processing switch and readback/restart/new-plan semantics | Integrated; absent default false, saved choice always accepted on all hosts |
| M3-05 | Developer readiness and concrete graduation requirement | Integrated; readiness advisory only, no offline benchmark receipt in enable/runtime path |
| M3-06 | Selected graph/decoder/surface/grade/revision and fallback reason diagnostics | Integrated; bounded metric labels and one accepted-start counting boundary |
| M3-07 | Worker-local resolution and capability change between offer and acceptance | Planned integration verification; no new required graph protocol field |
| M3-08 | Old→new/new→old dispatch, takeover and recipe/cache isolation | Awaiting evidence; unknown diagnostic names do not prove old-worker rejection |
| M4-01 · implementation §7 | SDR VT decode → native scale → H.264 VT encode | Core integrated; actual non-normalized VOD first-fragment delivery observed through the canonical plan |
| M4-02 | Same-size/downscale, SDR8/10, seek/resume and long-GOP behavior | Existing continuous High 5.0 VOD envelope passes short SDR/HDR smoke; seek/resume, long-GOP and production evidence still owed |
| M4-03 | SAR, rotation, odd dimensions, VFR, interlace and burn controls | Integrated eligibility boundary; preserve incumbent semantics instead of dropping required processing |
| M4-04 | Prepublication retry excludes failed Mac graph, new plan and asset identity | Integrated in existing retry owner; no CPU fallback immediately upgraded back to failed graph |
| M4-05 | Postpublication replacement/error, cancellation and resource release | Awaiting production evidence; no changed bytes appended to published artifact |
| M4-06 | Native bound-source qualification for normalized continuous VOD | Architectural dependency discovered in isolated daemon: production `DecodeProbeIdentity` is Linux-only; normalized geometry correctly refuses without it. No small supported confinement-equivalent mechanism found; separate parser-packaging design required. No fixture-mode or stored-facts bypass. Actual non-normalized VOD fragments passed; public live presentation is removed |
| M5-01 · implementation §8 | HDR10 → SDR with 10-bit precision through scale and explicit BT.709 output | Core integrated from observed scale-first Metal graph; actual non-normalized VOD HDR first fragment decoded cleanly |
| M5-02 | Consumed HDR metadata removed only after actual SDR conversion | Native output observed clean; scoped shared CPU/zscale-boundary correction integrated |
| M5-03 | Peak provenance, missing/1000/4000-nit metadata, gamut/gradients/scenes | Awaiting broader corpus and visual evidence; do not claim CPU Hable controls applied to Metal |
| M5-04 | Segment restart versus continuous tone-map temporal consistency | Awaiting independent VOD segment/seek evidence |
| M5-05 | P5/P7/P8, HLG, HDR10+ and HDR-preserving boundaries | Incumbent controls retained; no wildcard HDR or generic PQ fallback for P5 |

### 5.3 Qualification, packaging and acceptance

| ID / plan reference | Agreed item | State / remaining evidence |
|---|---|---|
| Q-01 · design §6.1 | Target production Mac, second Apple Silicon generation and separate Intel matrix | M3 Max experiment host observed; deployed/second-generation/Intel evidence unavailable so far |
| Q-02 | 1080p light workload, 4K24/60, SDR8/10 and varied HDR corpus | Synthetic 4K24 and tiny probes built; rest awaiting evidence |
| Q-03 · design §6.3 | Five paired ≥60-second source runs and precise first-use/warm labels | SDR and revised HDR complete; no claim of cold disk cache |
| Q-04 | One/two/four sessions, steady-window p10 and 30-minute soak | Awaiting evidence; short high-speed runs do not prove capacity |
| Q-05 | ≥30 starts, first publishable media and first presented frame, p95 | Awaiting production/client evidence |
| Q-06 | CPU, throughput, peak RSS/memory pressure, energy, copy/sync instrumentation | CPU/throughput measured on synthetic workload; remaining measurements unmeasured, not zero |
| Q-07 · design §6.4 | SDR aligned quality metrics plus ringing/aliasing/banding inspection | Analytic gray smoke only; full quality acceptance awaiting evidence |
| Q-08 | HDR blinded review on named calibrated display, highlight/shadow/gamut/temporal findings | Awaiting human/display evidence; synthetic ramp and VMAF alone are insufficient |
| Q-09 | Real plurxd rolling/VOD playback, two-minute play, seek/resume, errors/rebuffer/A/V | First SDR/HDR non-normalized VOD fragments produced and decoded; two-minute client playback and the remaining delivery matrix await evidence |
| Q-10 | Correctness first, then ≥20% throughput or ≥20% CPU/≥15% energy benefit within throughput bound | SDR/revised HDR synthetic CPU criterion met; visual/production correctness still unqualified |
| M6-01 · implementation §10 | Full incumbent AC-4 and P5 package acceptance in daemon environment | Awaiting decodable lawful samples and actual environment; declarations insufficient |
| M6-02 | Native install, runtime libraries, signing/distribution and architecture compatibility | Complete official package retained; actual install/signing workflow verification planned |
| M6-03 | Fresh offline install, empty/corrupt/full/unwritable cache and reprobe | Isolated daemon reached both graphs Available and repaired deliberately corrupted cache; distribution install/full/unwritable cases still pending |
| M6-04 | Upgrade/rollback while workers active, old binaries retained or workers drained | Planned; no installed package changed yet |
| M6-05 | Switch off affects new plans, existing sessions retain captured implementation | Isolated false→true→false→true saves, explicit reprobe and restart persistence passed; live-session behavior still pending |
| M6-06 | Graduate switch only with claimed workload/client evidence, preserve saved value | Planned; stays in Developer while evidence incomplete; no automatic default flip |

### 5.4 Explicit extensions and cross-platform follow-up

These rows remain visible even when the initial SDR/HDR10 batch ships.
Separate scope does not imply that implementation or qualification is done.

| ID / plan reference | Agreed item | State / dependency |
|---|---|---|
| E1-01 · implementation §9.1 | P5 hardware decode feeding existing CPU `tonemapx` independently of GPU renderer | Follow-up production work; early four-way evidence remains M1-09 |
| E1-02 | Software decode + Metal and hardware decode + Metal Dolby rendering | Follow-up; prove RPU pixel influence, no double processing and real complete-graph benefit |
| E1-03 | Strict per-frame effective Dolby metadata at renderer boundary | Follow-up prerequisite: valid reuse/reordering/seek association; initial or midstream absence fails before affected frame is encoded/published |
| E1-04 | Minimal pinned FFmpeg strict-mode patch if existing mode insufficient | Follow-up; `apply_dovi=1` alone is insufficient; bind patch/enforcement into recipe |
| E1-05 | Lawful Dolby runtime smoke, P7/P8/direct-play/remux controls and Dolby-aware fallback | Follow-up; P5 result cannot qualify other profiles or output grades |
| E1-06 | HLG reference-white, color and temporal qualification | Independent follow-up; no inheritance from HDR10 |
| E2-01 · implementation §9.2 | Native scale/tone-map then CPU subtitle burn at output resolution | Follow-up first experiment when burn is measured bottleneck |
| E2-02 | GPU compositing of prepared text/bitmap images | Follow-up only if beneficial; keep libass shaping/fonts and explicit alpha/color semantics |
| E2-03 | PGS color, ASS animation/position, active-cue seek, EOF/free intervals and cancellation | Follow-up acceptance; no new HDR burn policy |
| E3-01 · implementation §9.3 | Hardware BWDIF where supplied; separately judged YADIF alternative | Follow-up; deinterlace before scale, preserve parity and progressive bypass |
| E3-02 | Native Live TV plan, frame/field cadence, rational rate, bitrate and manifests | Follow-up; no VOD argv borrowing |
| E3-03 | 1080i TFF/BFF, 720p59.94, A/53, late audio, AC-4, reconnect/stop/start | Follow-up hardware/delivery matrix |
| E3-04 | Scoped live `-a53cc 0` and caption-bearing VOD limitation | Follow-up or explicit supported-scope exclusion; live success is not file-caption repair |
| E4-01 · implementation §9.4 | HEVC SDR and HDR10 Main10 output separately | Follow-up; negotiated codec/container/manifest/client/cache/cluster contracts together |
| E4-02 | Explicit HEVC output setting with advisory Developer readiness | Follow-up; independent from processing switch and independent graduation |
| E4-03 | Actual HDR presentation, metadata semantics, no implicit Dolby passthrough | Follow-up; preserve existing VOD B-frame policy and promised output grade |
| E4-04 | Apple/native/web client play/seek/quality/recovery and bitrate-quality comparison | Follow-up; H.264 compatibility fallback through existing negotiation |
| X-01 · implementation §9.5 | Intel native VA-API/DXVA and NVIDIA NVDEC P5 investigations; other device tuples individually | Separate cross-platform follow-up; do not globally remove or make permanent the software-decode restriction from one Mac result |

### 5.5 Review, merge and cleanup obligations

| ID | Agreed item | State / remaining work |
|---|---|---|
| R-01 | All builders receive user constraints and have disjoint ownership | Done for active builders; packet applies to subsequent assignments |
| R-02 | Normal commits, tracked hooks, pinned compile loop before pushing Rust | Application candidate `93658e211` integrated; registered compile and normal hook passed |
| R-03 | Explicit adversarial coverage of native/package/core builder | Active: independent core/package reviewer on frozen integrated candidate |
| R-04 | Explicit adversarial coverage of harness/manager/settings builder | Active: independent harness/daemon reviewer, including measurement validity, retry and saved choice |
| R-05 | Explicit adversarial coverage of fixture/runtime builder | Active: independent fixture/runtime reviewer, including integrity, bounds and lifetime |
| R-06 | Cross-builder integration review, root causes and no watchdog/gate cruft | Active: all reviewers check cross-builder interfaces; no builder self-review substitutes |
| R-07 | Address findings, then hand off for coordinator-owned fast lane/regressions | Pending; no unit suites during building; coordinator retains valid evidence and reruns failed/invalidated checks only |
| R-08 | Current-main integration, exact-tree gate/qualification receipt and regression landing lines | Merge coordinator owns queue and main landing; effort supplies reviewed code and handles behavior-changing fixes; no PR open yet |
| R-09 | Retain reproducible sanitized receipts, exact commands, limits and autonomous decisions | Initial/revised experiments plus VOD/P5 diagnostics retained in indexed evidence document and 1.20 MB raw archive, including isolated settings/probe and actual SDR/HDR VOD receipts; full production qualification pending |
| R-10 | Remove own transient clones, benchmark media, caches and obsolete branches after retention | Pending completion; do not remove user files or useful unmerged work |

## 6. Completion accounting

The initial batch is M0–M6 with explicit honest disposition of unavailable
qualification rows; incomplete evidence keeps the feature's graduation
pending and remains visible here. It must not be relabeled a fully qualified
release. E1–E4 and X-01 retain their separate scope and acceptance. Update this
ledger when ownership, evidence, a decision or a PR disposition changes, and
link the actual integrated commits and durable receipts before closing rows.

A reviewed implementation handed to the merge coordinator is not completion
of M6. The control remains in Developer while qualification is incomplete,
with every unresolved acceptance row still open. No missing row is converted
to a pass merely to close the implementation PR.

The design's explicit non-goals remain excluded: client/player rewrite,
direct-play policy changes, source-media modification, Linux access to Apple
engines, interpolation/denoising/super-resolution/sharpening, assumed AV1
encoding, and generic non-Mac backend rewrites. They are not missing delivery
tasks. Priority and effort estimates remain in the companion plan/design;
this page records execution and proof rather than replacing those estimates.

## 7. Coordinator validation packet

These commands are **not execution results**. Unit suites remain unrun by this
effort. After the independent review and its corrections, the merge coordinator
owns their scheduling on the composed tree, required promotion checks and
preservation of valid source-bound passes. Run only failed or invalidated
cohorts again; a passing check from a different effective implementation is not
proof of the new behavior.

Use the pinned Rust 1.97.1 environment. The core tests require `hiqlite-store`.

```bash
cargo test --locked -p plurx-core --features hiqlite-store --test decoder_selection macos_processing_
cargo test --locked -p plurx-core --features hiqlite-store --test decoder_selection cpu_hdr_to_sdr_consumes_only_static_hdr_metadata_in_both_producers
cargo test --locked -p plurx-core --features hiqlite-store --test decoder_selection consumed_hdr_metadata_policy_leaves_unaffected_routes_unchanged
cargo test --locked -p plurx-core --features hiqlite-store --lib transcode::recipe::tests::planned_v4_recipe_hash_is_a_golden_fixture
cargo test --locked -p plurx-core --features hiqlite-store --lib transcode::recipe::tests::output_codec_contract_is_in_recipe_identity_without_invalidating_legacy_bytes
cargo test --locked -p plurxd --bin plurxd macos_
python3 tests/operations/test_macos_video_bench.py
python3 tests/operations/test_macos_video_fixtures.py
node tests/web/settings-sections.test.js
```

The web file has its own queued test runner; Node's `--test-name-pattern`
does not select its internal cases. Its correct scheduling granularity is the
whole file. Runtime probe tests have platform-specific cases: a Linux pass is
not execution of the Mac-only child/cache/graph cases. Record the host with each
result. Actual display, source confinement, client playback, concurrency and
soak gaps remain the open acceptance rows above, not unit-test substitutes.

The PR carries concrete `Regression-Test:` fields that must survive in the
landing commit. Superficial test repairs may be made by the coordinator;
behavior-changing findings return to this effort for implementation and review.
