# Subtitle generation — finish setup and make background captions usable

**Status:** implementation claimed; contracts reviewed; acceptance open ·
**Written:** 2026-10-15 · **Source:** `9eb7f6738` ·
**Executes:** P14 usability follow-up · **Implementation owner:** GPT-6.1 Sol build sessions

Read the [subtitle completion ledger](../clients/SUBTITLE-RELIABILITY-COMPLETION.md)
for the engine's previous acceptance and [Operations](../OPERATIONS.md#generate-subtitles-offline)
for today's manual setup. This document owns the new setup, worker-selection
and usability work. Build the milestones in order; preserve the existing
queue, cancellation and atomic caption publication. If a milestone appears
to require replacing those systems, revisit this contract before proceeding.

## 1. Finding — the engine exists; installation is left to the operator

The field is an **absolute path to an existing model file on the server**,
not an AI service's model name. The implementation invokes the local
`whisper-cli` executable from whisper.cpp. Neither the executable nor model
is provisioned by the transcription feature. Typing `base.en` into the model
field will not install or select it; a bare name fails path validation.

There is a substantial working implementation, merged in #961. The retained
[qualification archive](../clients/evidence/subtitles-20261009/final-qualification.tar.gz)
contains `app/transcription-pass.json` and its driver. That receipt records a
real queue transition through queued/running/succeeded, model/audio/source
provenance, WebVTT surviving restart, and a visible cue in Chrome. The driver
creates a **20-second synthetic video**; the model was `tiny.en`, and the
ledger identifies whisper.cpp 1.9.2. This proves the basic path, not movie
accuracy, feature-length throughput, fresh installation or deployed-fleet
readiness. These are historical results inspected for this review, not tests
rerun against this checkout.

This review inspected source, packaging, documentation and retained evidence.
It did not inspect or change production settings, install dependencies,
download a model, or run transcription against the user's library. Whether
any deployed worker already has a usable installation remains unverified.

### 1.1 What “on the fly” means here

Today **Generate subtitles** requests background processing of a complete,
finite file. The worker first decodes the entire selected audio track, then
runs inference, and only publishes after the complete WebVTT passes checks.
The viewer refreshes the title and selects the generated track afterwards.
There is no partial-caption publication or live speech recognition path.

This plan first makes that existing workflow usable: install from Settings,
request generation, understand its status, and select the result without a
manual page reload. Immediate captions while watching and Live TV generation
are a separate extension in §7; they cannot be promised by finishing setup.

## 2. What is built — retain these parts

Re-verify symbols against the linked files at build time; line numbers and
the checkout base can move.

| Surface | Implemented behavior and evidence |
|---|---|
| Settings | [`Settings`, `settings`, `update_settings`](../../crates/plurxd/src/subtitle_transcription.rs) persist enablement, command, absolute model path, language and scan interval. Save deliberately permits missing dependencies. |
| Request UI | [`prePlayPickers`, `generateSubtitles`](../../crates/plurxd/src/web/detail/preplay-selection.js) expose an admin action on local video details, distinguish refused queue outcomes, and link to Activity. |
| Queue | [`JobPayload::SubtitleTranscribe`](../../crates/plurx-core/src/store/background_jobs.rs) binds file revision, audio ordinal, language, model hash and pipeline hash. Admission caps active transcription jobs at 32. |
| Worker | [`pass`, `transcribe`](../../crates/plurxd/src/subtitle_transcription.rs) execute under background admission and renewable job ownership; held source/model descriptors, source-clock normalization, strict decode checks, cancellation and scratch cleanup already exist. |
| Publication | [`publish`](../../crates/plurx-core/src/store/background_jobs_transcription.rs) atomically stores caption/provenance and settles work, conditioned on the current source and claim. Stop or lost ownership cannot publish stale output. |
| Playback | Generated captions enter the durable acquired-caption store and ordinary subtitle delivery. They carry a machine-transcription title; no media-directory sidecar or scanner rediscovery is required. |
| Activity | [`activity.js`](../../crates/plurxd/src/web/pages/activity.js) provides durable job state, worker, attempt history and Cancel. This is not transcription percentage progress. |
| Automatic discovery | `automatic_page` visits eight candidates per interval; zero means manual only. Matching-language subtitles suppress automatic generation. Manual requests can ask for an alternative. |
| Tests | Worker tests cover held descriptors, source timestamps/gaps, decode-error rejection, cancellation, audio selection and malformed output. The store contract covers publication, cancellation and source replacement; web tests cover advisory readiness and queue refusal. |

**Existing bounds:** probed finite sources of at most four hours; model at
most 2 GiB; audio decode at most 30 minutes; inference at most four hours;
temporary PCM approximately 460.8 MB plus header; output at most 256 KiB;
at most eight acquired captions per source revision. Inference uses two CPU
threads, one processor and `--no-gpu`. These are enforced ceilings, not
performance estimates or a guarantee that every four-hour title succeeds.

## 3. Gaps — implementation work versus missing acceptance

| Priority | Finding and source evidence | Required outcome |
|---|---|---|
| P1 | **No install path.** `Settings::default` names `whisper-cli` and leaves `model_path` empty; `readiness` explicitly says models are never downloaded. Reviewed Docker/install assets have no Whisper provisioning. | Ship a pinned executable and offer a managed model installation workflow with progress and a self-test. |
| P1 | **Request admission depends on ingress-local files.** `enqueue` resolves the executable and opens/hashes the model on the server receiving the HTTP request. A different worker having the model cannot rescue that request. | Resolve a verified logical model/profile independently of the HTTP ingress; claim only on a compatible worker that can open the source. |
| P1 | **Compatibility includes a local path.** `pipeline_digest` hashes `command.display()` but not executable content/version. Equivalent binaries at different paths cannot share a job; replacing a binary at the same path does not change that part of identity. | Define versioned engine/profile identity independent of install location and bind it to the qualified binary build. |
| P2 | **Readiness is existence, not a working pipeline.** `readiness` checks an executable file and nonempty model size. It does not load the model, run the decoder, test service permissions/scratch, or prove a capable worker can read this title. | Separate installed, verified, worker-available and title-eligible observations; report failures with a next action. |
| P2 | **Configuration changes strand old work.** `pass` considers only the current settings/model and skips any queued pipeline mismatch. The operator docs acknowledge these jobs can remain waiting. | Keep immutable job profiles usable until settlement, or visibly block legacy jobs with explicit Cancel/recreate recovery. Never silently rewrite an accepted job. |
| P2 | **Disable copy disagrees with execution.** The form says enablement applies to new requests and directs users to Activity to stop work; `watch_enabled` cancels a running attempt when disabled, which then yields. | Adopt and test the precise disable semantics in §4.4; make the UI match. |
| P2 | **Completion is a manual handoff.** `generateSubtitles` does not retain a job subscription; it tells the user to refresh. Its request body is empty, so language/audio come from global configuration rather than the title's selected audio. | Show the chosen spoken audio/language, follow the job, refresh caption metadata and offer selection when ready. |
| P2 | **Failures and waits are too generic.** Inference/decode failures settle as `subtitle_transcription_failed`; model mismatch and missing-worker skips do not publish a specific wait explanation. Activity's progress lookup is for analysis telemetry. | Publish bounded transcription phases, wait reasons and actionable error codes through existing Activity observations. |
| Acceptance | **Only short synthetic generation is evidenced here.** Production setup, full movies, multilingual recognition and generated-caption selection on the complete native-client matrix are not established by the retained pass. | Record the separate installation, quality, throughput, cluster and client cases in §6. |

Background admission also matters. `admit_fragment` and `fragment_worker_idle`
in the [transcode manager](../../crates/plurxd/src/transcode/manager/construct.rs)
prioritize foreground work; `watch_enabled` checks capacity every two seconds.
Yield has no inference checkpoint, so an interrupted attempt starts over.
Do not market the feature as running continuously during playback. First
measure its behavior under playback load and expose the waiting/restart reason.

## 4. Build contract — managed setup over the existing worker

### 4.1 Preserve the current API and identify new contracts explicitly

The current admin API is:

```text
GET  /api/v1/subtitle-transcription
PUT  /api/v1/subtitle-transcription
POST /api/v1/files/{id}/subtitles/transcribe     body: {}
```

The persisted key is `subtitles.transcription.config`, whose current value is:

```json
{
  "enabled": false,
  "interval_mins": 0,
  "command": "whisper-cli",
  "model_path": "",
  "language": "en"
}
```

Current job inputs are `file_id`, `audio_index`, `source_size`, `source_mtime`,
`language`, `model_sha256` and `pipeline_digest`. Current provenance records
those identities plus adapter and generation time. Keep the acquired-caption
storage and atomic publication contract; never convert generated output into
a provider download or rewrite a media source.

**Proposed additions, not existing APIs:**

| Admin endpoint | Contract |
|---|---|
| `GET /api/v1/subtitle-transcription/models` | Reviewed catalog: ID, language coverage, download bytes, digest, engine compatibility, license/source; per-worker installation and verification observations with timestamps. |
| `POST /api/v1/subtitle-transcription/models/{model_id}/install` | Body `{ "node_ids": ["worker-id"] }`; explicit installation on selected eligible workers; returns durable operation IDs. No caller-supplied URL or destination path. |
| `POST /api/v1/subtitle-transcription/models/{model_id}/verify` | Same node selector; bounded synthetic-audio test through the production decoder/inference/output validator. Return an operation ID, not a long blocking HTTP call. |
| Existing file transcription POST, versioned request | Keep `{}` as the legacy/default request. Add an explicitly decoded request version for chosen `audio_index`, `language` and logical model/profile. Reject unknown versions; preserve exact file-ID handling. |

Use existing authenticated peer transport and durable job/Stop ownership for
node operations. Add typed payloads and registry coverage where necessary;
do not create detached subprocesses or a second job scheduler. Old workers
must ignore unsupported payload versions, not reinterpret them.

The existing settings decoder denies unknown fields. Do not append new keys
to its persisted JSON and assume old workers will accept them. Store managed
profile metadata in a separately versioned namespace, proposed
`subtitles.transcription.managed.v1`, and keep legacy configuration readable.
The build must specify one authoritative selected mode and mixed-version
behavior before writing migration code. Preserve existing enable/language/
interval choices; never silently activate legacy discovery on old nodes when
the operator selects managed mode. Do not require every node to upgrade before
a supported worker can execute a versioned job.

### 4.2 Install a known engine and choose a model without typing paths

1. **Package the executable with the server distribution.** Pin a reviewed
   whisper.cpp release/commit, record build options and identity, include
   notices, and test runtime libraries under the real service account and
   cleared child environment. Start qualification from the historically
   exercised 1.9.2 adapter; this is not a claim that it is the latest release.
   Linux container and native macOS are the first qualification targets.
2. **Download models only after an explicit Install action.** A small reviewed
   catalog pins immutable upstream revisions, expected SHA-256 and exact byte
   lengths. Use bounded HTTPS transfers, bounded redirects to approved hosts,
   service-owned temporary files, disk reservation, verification and atomic
   publication. A failed/cancelled transfer must not appear installed. Audio
   never leaves the server; model installation is the only added network step.
3. **Start with `base.en` and multilingual `base`.** These are provisional
   first-release choices, subject to the quality/throughput pass. Keep `tiny.en`
   for quick pipeline diagnostics; do not equate its synthetic success with
   movie-quality acceptance. Offer `small.en`/`small` only when measured workers
   meet the chosen performance budget. Do not download every model by default.
4. **Store models by verified digest outside disposable runtime cache.** Show
   disk usage and installed nodes. Bound concurrent installation per node;
   retain the previous working model after an upgrade failure. Removal must
   refuse an artifact referenced by accepted/running jobs and explain why.
   Keep an Advanced existing-installation mode for operators with local models.

Upstream documents GGML model files, a separate `whisper-cli` build, and
approximate model storage of 75 MiB for tiny, 142 MiB for base and 466 MiB for
small. These numbers inform UI estimates; the pinned manifest supplies exact
download sizes. The upstream default large model exceeds this worker's 2 GiB
ceiling. Source checked 2026-10-15:
[whisper.cpp README](https://github.com/ggml-org/whisper.cpp#quick-start).
No new model training, cloud speech API, account or API key is required.

### 4.3 Route by verified identity, not host pathname

The logical profile binds model SHA-256, language/audio selection policy,
decoder/pipeline revision and engine build identity. The worker resolves its
own executable/model locations. Absolute paths are local implementation data,
not cross-worker compatibility keys.

Cache verification by immutable artifact identity. Request handlers must not
rehash hundreds of megabytes per click or require the model on ingress.
Worker observations include profile identity, verification time, capacity
and failure reason. Stale observations are unknown, never proof of readiness;
the worker rechecks source/model identity and authority when claiming work.

Changing the default affects new requests. A versioned accepted request keeps
its original profile; installed artifacts remain referenced until it settles.
Legacy jobs whose path-based profile cannot run get a visible explanation and
explicit Cancel/recreate action. No automatic migration may relabel a result
or duplicate demand. Maintain queue caps and deduplication across ingress
nodes, and test two competing workers against the real replicated store.

### 4.4 Define enablement, language, progress and completion

**Enablement:** default off; scan interval default zero. Disabled refuses new
requests and pauses new transcription claims/discovery. An already running
attempt may finish; Activity Cancel explicitly stops it. Queued jobs show
“Paused: subtitle generation is disabled.” Foreground resource pressure may
still yield work. Never use advisory readiness to reject Save or reset the
saved switch. This deliberately repairs today's mismatch between UI wording
and `watch_enabled` behavior.

**Language/audio:** show the actual spoken audio track and language before
generation, initialized from the title's current selection. Offer only model-
supported languages. For untagged audio ask for the spoken language; do not
silently treat it as English. Preserve the current known-mismatch refusal.
Generation transcribes speech; it is not translation, speaker identification
or a guarantee of hearing-impaired sound descriptions. Keep admin-only
generation for the first release; playback remains available through ordinary
caption permissions. Native request controls are a follow-up, not a reason
to duplicate settings screens now.

**Progress:** report queued, decoding, transcribing, validating, publishing
and terminal state, with current worker, elapsed time and bounded reason
codes. Show percentage/ETA only when supported by measured progress. Keep
observations tied to job/claim/fence so old workers cannot overwrite a new
attempt. At minimum distinguish missing model, failed model verification,
no compatible worker, unavailable source, waiting for playback capacity,
decode failure, inference timeout/failure, output size/format refusal and
source replacement. Keep raw paths/process output out of ordinary error text.

**Completion:** link directly to the specific Activity job, including when
opening Activity on the Jobs tab. Refresh title metadata when that job
succeeds and show “Captions ready — use generated subtitles.” Fence async
results by file revision, page generation, account and current selection.
Do not replace the video element, auto-select captions after the user chose
Off, or silently switch an existing track. Selecting the new track uses the
existing playback-control path. A title must not show Ready until publication
and a selectable track are confirmed.

**Settings destination:** retain the meaningful optional enable switch and
Developer graduation text during construction. After §6 acceptance, move
permanent setup controls to Settings → Maintenance, as the existing card
promises, and remove the temporary Developer card. Preserve old deep links
and saved choices. Do not add toggles for correctness repairs or job telemetry.

## 5. Ordered milestones — each ends in a usable, reviewable result

Use one `effort/subtitle-generation-usability` branch: several milestones
touch the same worker and settings files, so independent main-bound changes
would not meet the file-ownership exception. Task branches use `codex/` and
target the current effort. The table assigns scope, not concurrent agents.

| Milestone | Files/surfaces owned | Deliverable and acceptance |
|---|---|---|
| M0 · reproduce and choose defaults | This ledger; existing retained driver; deployment inspection | Record deployed build, eligible nodes, actual service executable/model availability and source visibility without changing settings. Run an isolated real-engine fixture, then measure base/base.en on a representative excerpt. Freeze engine/model hashes and the versioned settings/payload contract. No claim that all deployed nodes need models. |
| M1 · packaged engine and managed model | `Dockerfile`, `deploy/`, packaging notices/tests; new model manager; typed node-operation jobs and routes | Clean supported installation needs no shell compilation or typed model path. Install, verify, restart, failed download, wrong hash, no disk, Cancel and upgrade rollback all leave truthful status. Installation uses the real service environment. |
| M2 · worker-aware queue identity | `subtitle_transcription.rs`; core job payload/publication/observation contracts; peer capability reporting | Request through a node with no model succeeds via a compatible source-capable worker. Different local install paths work. Settings changes preserve accepted identities. Missing/stale workers and legacy incompatibility produce explanations. Existing Stop/source/lease fences remain enforced. |
| M3 · setup and per-title experience | `settings-developer.js`, `settings-panels.js`, `detail/preplay-selection.js`, `pages/activity.js`, relevant HTTP observations | Choose model/language, install, test, enable, generate, follow the exact job and select its finished track from the UI. Test disable semantics, all refusal paths, retry, page navigation, Off and stale completion. New scripts, if needed, receive `WEB_ASSETS`, shell and layout-table entries together. |
| M4 · movie and fleet qualification | Reusable fixture/acceptance drivers, current engine tests and evidence; targeted performance repairs only if needed | Complete the §6 matrix on the exact candidate; mobile clients installed on physical devices must be built from committed main as required below. Preserve foreground priority. If full-length runtime or repeated preemption is unacceptable, record the measured failure and implement a bounded admission/acceleration change before claiming usability; do not promise immediate output. |
| M5 · graduate and promote | Maintenance settings, Developer lifecycle tests, Operations/API/Features, this ledger and backlog | A fresh installation completes setup and generation without a terminal; supported clients show/select the generated track. Update screenshots/instructions and retain a source-bound qualification receipt. Merge current main into the frozen effort and pass Main promotion gate before landing. |

Before any Rust edits, establish pinned Rust **1.97.1** compilation as required
by [the compile loop](../ci/AGENT-COMPILE-LOOP.md). For every task run the
smallest behavioral regression locally, normal tracked hooks and affected
compilation; dispatch **Effort development gate** manually and merge only
after it passes. User-observable fixes use `fix(` or `perf(` subjects and
carry `Regression-Test: <path>::<test name>` in both PR and landing messages.
Follow the [development pipeline](../DEVELOPMENT_PIPELINE.md) for final
qualification; the October 9 exception in the historical ledger does not
authorize bypassing gates for this new effort.

**Main-only mobile builds for physical devices:** the owner's final
2026-10-15 clarification applies only to mobile app builds intended for
installation on physical devices, and those installations. They must use
committed source already merged into main; verify that provenance. Mobile
emulator/simulator testing, CI/test builds and other builds not intended for
physical-device installation may use branches under the normal review/testing
workflow. Never reuse an unmerged-source test build for physical installation.
Server and other non-mobile work are outside this restriction. This replaces
the earlier broader holds and grants no new deployment authorization.
Record server and client source separately in qualification receipts; do not
claim a main-built physical client proves changes present only on a branch.

## 6. Qualification — what “usable” must prove

| Area | Required evidence |
|---|---|
| Fresh install | Linux container and macOS service: empty model store → Settings install → self-test → request → durable captions. Record artifact hashes and package revision; repeat after a server upgrade. Track Windows/other distributions as unqualified until separately tested. |
| Recognition | Known-reference clean speech, noisy dialogue/music, silence and a supported non-English fixture. Record omissions, hallucinated speech and timing error. Before model selection freezes, propose explicit quality thresholds from these samples; synthetic phrase matching alone cannot close this row. |
| Duration/size | Representative episode and feature film plus synthetic near-limit inputs; record decode/inference duration, CPU/RAM, scratch peak and output bytes. Establish the acceptable wait with those numbers before advertising a default model. Test the four-hour, 2 GiB, 256 KiB and eight-caption boundaries without silently truncating or raising limits. |
| Real playback | Generated captions visible throughout representative early/middle/late scenes; Off, seek, reselect, audio changes and quality handoff in web, Apple and Android players. Distinguish simulator from physical-device evidence. Existing generic WebVTT tests do not substitute for this generated-track path. |
| Cluster | Non-model ingress; two compatible workers racing; differing install paths; source unavailable on one worker; stale readiness; owner restart/removal; model/default change with queued and running jobs; old/new payload workers. One current result, no stale publication, actionable blocked state. |
| Cancellation/resources | Stop during install, decode and inference; service restart; foreground playback pressure; low disk and inference timeout. Child processes are reaped, temporary files retired, durable demand settled or visibly yielded. Repeated playback preemption must not look like advancing transcription progress. |
| UI/auth | Admin-only model installation/generation; escaped errors; wrong-language refusal; queue full; browser reload; page/account/file changes during completion; selected Off preserved; direct link to the right job and final selectable caption. No settings gate inferred from readiness. |

Focused existing commands to start from (not results claimed by this plan):

```bash
# Verify the pin before compiling any changed Rust.
rustc --version
cargo check --locked -p plurxd --all-targets
cargo clippy --locked -p plurxd --all-targets -- -D warnings
cargo fmt --all -- --check

# Exercise the worker's actual subprocess/timing/cancellation regressions.
cargo test --locked -p plurxd --bin plurxd subtitle_transcription::tests

# Run the durable publication contract, including real replicated storage.
cargo test --locked -p plurx-core \
  --features hiqlite-store,hiqlite-contract-tests --test store_contract \
  background_jobs_transcription_publication_is_atomic_namespaced_and_stop_fenced

# This custom runner executes all 65 cases, including setup and queue refusal.
node tests/web/settings-sections.test.js

# Validate documentation registration and public-safe infrastructure names.
python3 -m unittest discover -s tests/operations -p test_docs_index.py
python3 -m unittest discover -s tests/operations -p test_infra_names.py
```

Use the current replicated-contract harness prerequisites when executing the
store command; a skipped backend is not a three-voter pass. Retain source,
engine and model hashes with receipts, and requalify changed source after
merging current main. Add targeted regressions for new behavior rather than
treating the existing commands as exhaustive acceptance.
These server/core Rust checks are non-mobile work and remain part of normal
pre-merge validation. Only mobile builds intended for physical-device
installation retain the separate restriction in §5.

## 7. Boundaries — what needs a separate design

Do not build cloud transcription, translation, automatic language detection,
speaker labels, library-side subtitle files, shared-Cinema remote generation
or native generation controls into this first usability effort. They are
different permissions, semantics or delivery surfaces. Keep online subtitle
downloads separate; they already have their own provider workflow.

**Live captions while watching** require an additional contract: selected
audio decoding in bounded time ranges; overlapping inference context and cue
deduplication; canonical source timestamps; partial versus complete result
identity; seek/audio-change cancellation; ownership of requested ranges; and
renderer delivery without false complete-track publication. Live TV adds
discontinuities and an unbounded source. The current worker explicitly rejects
that source class. Acceleration may help throughput, but does not supply
these missing semantics. Design this extension after measuring the finished
background workflow, or explicitly rescope the project if immediate captions
are the required first result.

## 8. Execution ledger

| Date | State | Evidence / next action |
|---|---|---|
| 2026-10-15 | Review and plan complete; M0–M5 unstarted | Source `9eb7f6738`, historical app receipt inspected, upstream model/build documentation checked. No production inspection, install, code change, new engine run or deployment claimed. Start with M0, then freeze the managed-profile contract before implementation. |
| 2026-10-15 | Existing web behavior checked | Settings custom runner: 65/65 passed, including advisory transcription readiness and queue-refusal handling. Node's test-name filter does not filter this custom runner; the complete file ran once. No Rust or app build executed. |
| 2026-10-15 | Documentation checks passed | Docs-index suite: 4 tests; infrastructure-name suite: 14 tests. The new untracked plan also received explicit relative-link, public-safe-name, index and backlog checks. `git diff --check` passed. |


## 9. Reviewed contracts — implementation starts from one frozen interface

**Reviewed 2026-10-15.** These decisions supersede provisional names in §4.
The implementation must retain the old publication and cancellation fences.
A small fixture proves a pipeline; it cannot close the movie-quality rows.

| Review finding | Disposition and proof required |
|---|---|
| Legacy JSON denies added fields; old nodes could keep sweeping | Keep managed configuration separately versioned. Activate one mode atomically with legacy configuration. Record mode revision; a legacy settings write is a visible competing administrator write and activates Advanced mode. Old binaries writing settings cannot provide universal mixed-version coordination; report detected configuration divergence. |
| V2 payloads never enter current candidates | Add `subtitle_transcribe_managed` and `subtitle_model_operation`, both payload version 1. Extend all closed kind/resource/metric/cancel registries and publication contracts. Old workers ignore unknown kinds. |
| Current defaults can replace accepted audio/profile | Snapshot exact audio ordinal, spoken language, source revision, model digest, adapter contract and pipeline. Execution and publication use that snapshot after defaults change. |
| Bytes present can be mistaken for verified installation | One node-targeted owned install/verify operation publishes a fenced successful receipt bound to engine/model identity and boot/local file identity. Revalidate after restart and reject replacements. |
| Installs could bypass Stop or exhaust resources | Bound total queued/running operations and per-node concurrency. Downloads use no media source reservation. Network reads, shutdown and final publication obey the durable claim. Verification inference respects foreground admission. |
| Host paths and native binaries differ | Logical adapter contract is common; packaged per-platform executable SHA-256 is exact provenance. Changed/unqualified binaries are unavailable. Paths are local only. |
| ASCII language syntax admits unsupported codes | Catalog carries the exact Whisper language list; validate membership. Untagged selected audio requires explicit spoken language. |
| Generic completion can select another result | Use exact job ID and published `result_ref`, then match current source revision and caption artifact before offering selection. Preserve Off and page/account/file fences. |
| Stale observations imply false readiness | Authenticated peer DTOs expire after 60 seconds, bounded fanout, missing members unknown. Execution rechecks local artifacts, source and authority. |
| No-terminal acceptance could accidentally use dev PATH | Start fresh-install acceptance from the built package/image with empty model store and the real service environment. Record engine/package/model hashes. |

### 9.1 Configuration and wire identities

`GET/PUT /api/v1/subtitle-transcription/managed` uses strict JSON:

```json
{
  "version": 1,
  "expected_revision": "0.<legacy-sha256>",
  "mode": "managed",
  "enabled": false,
  "interval_mins": 0,
  "language": "en",
  "model_id": "base.en"
}
```

GET supplies opaque `revision` as `<saved UUID-or-0>.<canonical full legacy
JSON SHA256>` (absence has a distinct canonical identity). PUT requires that
`expected_revision` and rejects stale writes with conflict. Its core transaction
compares both the managed UUID and the exact legacy raw value or absence
observed by that load, so an old server's write cannot slip between token
validation and the atomic update. Invalid legacy JSON fails explicitly. Advanced mode additionally supplies
`advanced: {command, model_path}`. Managed activation atomically writes its
separate configuration and disables legacy discovery while preserving
Advanced command/path. An explicit return to Advanced copies the chosen
on/off, interval and language into legacy JSON. Existing legacy PUT is a
last-writer-wins Advanced activation; its returned state identifies the mode.
A stale old browser is therefore a competing admin write, never a second
active mode. An old server writing legacy settings cannot update the managed
revision; new workers detect divergence and report configuration conflict
instead of asserting mixed-version safety.

`GET /api/v1/subtitle-transcription/models` returns `version`,
`catalog_revision`, reviewed `models`, and live `workers`. Each model has
`id`, `label`, exact `languages`, `download_bytes`, `sha256`, `source`,
`license` and `adapter_contract`. Worker records have `node_id`, `label`,
`eligible`, `observed_at_ms`, `expires_at_ms`, `engine` and `models`.
Engine status is `available`, `missing` or `unqualified`; model status is
`missing`, `installed`, `verified`, `verification_failed` or `unknown`.
Installed bytes without a current validated receipt are never `verified`.

`POST /api/v1/subtitle-transcription/models/{id}/install` and `/verify` take
`{version: 1, node_ids: [...]}`: nonempty, bounded, unique current members.
They return `operations: [{node_id, job_id, outcome}]`, including per-node
refusals when only part of a request is eligible. The server selects every
URL and destination; no caller URL/path enters a download.

`GET /api/v1/files/{id}/subtitles/transcribe` returns `version`, exact string
`file_id`, `source_size`, `source_mtime`, selected config summary,
`audio_tracks: [{index, language, title, default}]`, and
`captions: [{index, artifact_key, source_size, source_mtime}]` with string
revision fields. Version 1 POST carries those expected revision strings plus
`audio_index`, `language`, `model_id`. `{}` retains the default legacy entry
point. Unknown request versions and source changes are refused. Successful
admission returns exact `job_id`, `artifact_key` and source identity in
addition to the existing outcome.

`GET /api/v1/subtitle-transcription/jobs/{job_id}` exposes only a safe DTO:
job/kind/state/phase/reason/node, attempt/fence/time, yield/restart counts,
input artifact and source identity. `result_ref` is published output identity,
never an inference from the input hash. Ordinary errors expose no raw paths or
process output. All these endpoints require administrator authentication.

Phases are queued · downloading · verifying · decoding · transcribing ·
validating · publishing · succeeded · failed · cancelled. Reasons include
disabled · missing_model · model_verification_failed · no_compatible_worker ·
source_unavailable · waiting_for_playback_capacity · worker_interrupted ·
decode_failed · inference_failed · inference_timeout · output_invalid ·
output_too_large · source_changed · legacy_profile_incompatible ·
configuration_conflict. Unknown values render a safe generic fallback.

### 9.2 Task ownership and ordering

| Task | Owner and files | Review boundary |
|---|---|---|
| T0 · frozen plan | Build lead: this plan, index, backlog, subtitle ledger | Documentation only; preserve concurrent unrelated edits. |
| T1 · packaged engine | Packaging session: Dockerfile, deploy/install, package/build scripts, release packaging, notices, catalog JSON, self-test fixture and packaging tests | Exact portable engine pin and native/image artifacts. |
| T2 · durable contracts | Build lead: core job kinds, payloads, registries, resources, publication, observations/receipts and core tests | New version-1 kinds with Stop/source/lease evidence. |
| T3 · managed setup | Build lead: Rust model manager, authenticated routes/DTOs and mode transition | Bounded install/verify and revision-conflict regressions. |
| T4 · managed execution | Build lead: transcription runtime, worker routing and immutable profiles | Exact selected audio, old defaults, disable semantics and publication. |
| T5 · web experience | UI session: settings-developer.js, preplay-selection.js, activity.js, router.js and web tests | Settings install, named workers, exact-job completion fences. |
| T6 · qualification | Build lead and reviewer: integration, measured acceptance, Operations/API/Features and ledger | Exact candidate; unsupported acceptance remains explicitly open. |

T1 and T2 can build concurrently. T3 and T4 branch from the updated effort.
Every task PR targets `effort/subtitle-generation-usability`; parent reviews
the concrete diff before its gate/merge. No deployment is authorized.
