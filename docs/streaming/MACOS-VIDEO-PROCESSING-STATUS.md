# macOS video processing — execution status and decisions

**Status:** active · **Updated:** 2026-10-07 · **Owner:** managing agent with
three GPT-6.1 Sol builders · **Integration:** `effort/macos-video-processing`.

Companion to the [design](MACOS-VIDEO-PROCESSING-DESIGN.md) and
[implementation plan](MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md). This page is
the execution ledger: current work, evidence, decisions and remaining work.
A plan or compiled change is not hardware qualification.

## 1. Current position

M0/M1 are in progress. No production routes, settings, installed packages or
live services have changed. The initial delivery remains measured SDR scaling
and HDR10-to-SDR processing; P5 experiments occur early without prematurely
enabling Dolby processing. Extensions retain their independent acceptance.

| Workstream | Owner | State | Evidence or next dependency |
|---|---|---|---|
| Integration and status | Manager | active | Separate clone from `09577b9f3`; pinned Rust 1.97.1 and Cargo 1.97.1 available |
| M0 runtime / M1 FFmpeg packaging | Sol native builder | active | Official Jellyfin `v8.1.3-1` arm64 package acquired and checksum verified; required Metal, scaling, Dolby CPU and AC-4 capabilities present |
| M0/M1 measurement harness | Sol harness builder | active | Implementing explicit binary inventory, bounded execution and structured experiment receipts |
| M1 embedded smoke corpus | Sol fixture builder | active | Three Jellyfin-generated 320×180, 12-frame HEVC fixtures verified; aggregate media 8,846 bytes; manifest/generator commit in progress |
| M2–M5 production integration | subsequent builder wave | waiting for graph evidence | Add only demonstrated candidates through existing plan/argv/recovery owners |
| M6 acceptance / main promotion | Manager and builders | not started | Exact integrated candidate, review, then required fast-lane evidence |
| E1–E4 extensions | subsequent scoped work | not started | Separate evidence and demand; no capability inherited from an HDR10 result |

Jellyfin FFmpeg is the required implementation baseline. Local Homebrew
FFmpeg is not a comparison target or substitute. The official package is an
experiment candidate; its capability inventory is not a performance result
or proof of the deployed daemon version. No local daemon has been found.
No throughput, CPU, energy, color or P5 metadata claim has been made.

## 2. User-directed workflow and file ownership

On 2026-10-07 the user authorized parallel Sol 6.1 builders, proper commits,
batched PRs, and autonomous routine decisions. These explicit instructions
supersede the earlier sequential-task and per-task test/review language:

- All work occurs in task-owned separate clones. User checkouts are untouched.
- Builders commit normally; the manager integrates onto the effort branch
  and batches a main-bound PR. No per-builder PR/review is required.
- Adversarial review happens once the main-bound PR is ready. Address its
  findings before running the fast lane. Do not run unit suites while building.
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

Task paths in this table are proposed until committed. Shared application
files will receive explicit ownership in the next wave; agents do not edit
another workstream's files. Hardware benchmarks run serially to avoid load
contaminating measurements.

## 3. Decisions and limitations

| Decision | Reason | Revisit when |
|---|---|---|
| Start three disjoint M0/M1 workstreams | Packaging, harness and fixture construction can proceed independently before planner integration | First usable graph and corpus are available |
| Keep deployed services/packages unchanged during experiments | Private candidate builds allow attribution and preserve incumbent AC-4/Dolby behavior | Qualification supplies a deployment decision |
| Use explicitly pinned Rust executables | The interactive shell selects a different Homebrew compiler | Every Rust integration/compile session |
| Use official checksum-verified Jellyfin FFmpeg for all candidate and baseline experiments | Preserve the required AC-4 and Dolby functionality; stock Homebrew FFmpeg is irrelevant to this effort | A demonstrated package defect requires a different pinned Jellyfin build |
| Prefer the official complete Mac archive to a source rebuild | The archive already exposes the needed filters and Apple-only dynamic library closure | A required behavior cannot be supplied by the official package |
| Record negative or unavailable results explicitly | Missing evidence cannot justify performance or color claims | Hardware/corpus/provenance becomes available |

## 4. Validation and handoff ledger

- Pinned compiler availability verified: Rust 1.97.1, Cargo 1.97.1.
- Unit suites: not run for this effort; deferred per user direction.
- Official Jellyfin source tag: `v8.1.3-1`, source commit
  `253db2a7b0a8045c54ce68ce33d7f601229b1822`.
- Official arm64 archive SHA-256 checked against the release API:
  `22445d7299742749ad2eeb9ce87963d50def0357e45b3e6c7b69987c8365dbf6`.
- Capability listings show `scale_vt`, `tonemap_videotoolbox`,
  `tonemapx` with `apply_dovi`, and AC-4 decoding. Graph execution pending.
- Fixture generation/probe: 12 decoded frames each for SDR8, SDR10 and PQ;
  the PQ source includes explicit VUI and mastering/content-light metadata.
  The generator was corrected after probing showed x265 omitted primaries and
  transfer unless explicitly supplied in its VUI options.
- Native graph smoke checks: in progress, serialized and bounded.
- Sustained hardware benchmarks: not started.
- Adversarial implementation review: not started; reserved for ready main PR.
- Main PR: not opened.

Later entries will record commit IDs, root-cause observations, commands,
accepted graphs, failed checks and their targeted reruns, qualification
limits, review dispositions and cleanup. Completion requires honest closure
of the applicable acceptance rows, not merely a successful compilation.
