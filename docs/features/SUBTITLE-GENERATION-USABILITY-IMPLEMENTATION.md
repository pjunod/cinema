# Subtitle generation — finish setup and make background captions usable

**Completion effort, 2026-10-26:** [current implementation and final qualification](SUBTITLE-COMPLETION-STATUS.md) extends the initial scope with progressive transcription, native clients, multiple audio tracks, additional delivery routes and existing-text translation. The evidence below retains its original candidate and scope. The user owns deployment after the new merge.

**Status:** merged to main through PR #1136 as `220157aba`; remaining
qualification explicit below · **Written:** 2026-10-15 ·
**Reviewed implementation tree:** `20589a21` ·
**Executes:** P14 usability follow-up · **Implementation owner:** GPT-6.1 Sol build sessions

The user authorized merging after passed basic checks and monitoring remaining
checks after merge. The exact d440/tree `20589a21` isolated macOS package passed
the primary Settings installation/generation/web playback flow 13/13 with no
browser, console or HTTP errors. Prior f8c72 cache/restart/Test/Stop/retry
lifecycle evidence remains separately retained, not relabeled as a new-base
run. Normal hooks, focused Rust/storage and full Python/web checks passed.
Full post-merge CI and Linux package lifecycle remain pending. Android direct
acquired/generated caption attachment, broader cluster/native/long-form
quality and deployed-fleet qualification remain open. Four hours is an
admission ceiling, not a measured completion promise.

Read the [subtitle completion ledger](../clients/SUBTITLE-RELIABILITY-COMPLETION.md)
for the engine's previous acceptance and [Operations](../OPERATIONS.md#generate-subtitles-offline)
for the current managed setup. This document owns the new setup, worker-selection
and usability work. Build the milestones in order; preserve the existing
queue, cancellation and atomic caption publication. If a milestone appears
to require replacing those systems, revisit this contract before proceeding.

## 1. Original finding — installation was left to the operator

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
   publication. A failed/cancelled transfer must not appear installed. Complete
   checksum-valid bytes may remain as unverified cache after Stop convergence;
   no automatic job may turn that cache into an accepted installation without
   a prior successful receipt for the exact current file identity. Audio
   never leaves the server; model installation is the only added network step.
3. **Start with `base.en` and multilingual `base`.** These are provisional
   first-release choices, subject to the quality/throughput pass. Keep `tiny.en`
   for quick pipeline diagnostics; do not equate its synthetic success with
   movie-quality acceptance. Offer `small.en`/`small` only when measured workers
   meet the chosen performance budget. Do not download every model by default.
4. **Store models by verified digest outside disposable runtime cache.** Show
   disk usage and installed nodes. Bound concurrent installation per node;
   retain the previous working model after an upgrade failure. This release
   exposes Install and Verify only; it has no model-removal API or UI. Any future
   removal operation must refuse an artifact referenced by accepted/running
   jobs and explain why. Keep an Advanced existing-installation mode for
   operators with local models.

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
The managed GET then reports Advanced mode; the legacy stored JSON keeps its original fields. A stale old browser is therefore a competing admin write, never a second
active mode. An old server writing legacy settings cannot update the managed
revision; new workers detect divergence and report configuration conflict
instead of asserting mixed-version safety.

`GET /api/v1/subtitle-transcription/models` returns `version`,
`catalog_revision`, reviewed `models`, and live `workers`. Each model has
`id`, `label`, exact `languages`, `download_bytes`, `sha256`, `source`,
`license` and `adapter_contract`. Worker records have `node_id`, `label`,
`eligible`, `observed_at_ms`, `expires_at_ms`, `engine` and `models`.
Engine status is `available`, `missing` or `unqualified`; model status is
`missing`, `unverified_cache`, `installed`, `verified`, `verification_failed`
or `unknown`. Cache without a successful exact-model-file receipt requires an
explicit Install/Test; it is neither an accepted installation nor `verified`.
An accepted unchanged model may be `installed` while owned requalification
runs for the new boot/engine. Automatic Verify dedupe identity binds its typed
payload, boot, model and engine; stale queued work retires before claim, and
held/named identities are rechecked after admitted hashing and self-test.

`POST /api/v1/subtitle-transcription/models/{id}/install` and `/verify` take
`{version: 1, node_ids: [...]}`: nonempty, bounded, unique current members.
They return `operations: [{node_id, job_id, outcome}]`, including per-node
refusals when only part of a request is eligible. The server selects every
URL and destination; no caller URL/path enters a download.

`GET /api/v1/files/{id}/subtitles/transcribe` returns `version`, exact string
`file_id`, `source_size`, `source_mtime`, selected config summary,
`audio_tracks: [{index, language, normalized_language, title, default}]`, public
`subtitle_streams`, and
`captions: [{index, artifact_key, source_size, source_mtime}]` with string
revision fields. Version 1 POST carries those expected revision strings plus
`audio_index`, `language`, `model_id`. `{}` retains the default legacy entry
point. Unknown request versions and source changes are refused. Successful
admission returns exact string `file_id`, `job_id`, `artifact_key` and source identity in
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

### 9.3 Integration amendments and current execution — 2026-10-15

**Single-server completion amendment — accepted 2026-10-15.**
After T1 passes its exact-base gate and lands, consolidate the remaining
interdependent slices into one integration task PR targeting the current effort:
T6 corpus attribution, T2 durable contracts, combined T5/watch UI, T3/T4 runtime
(including known51866 Advanced compatibility), essential regressions and
maintained Operations/API/Features/ledger updates. Preserve original commits,
review dispositions and all Regression-Test trailers; reconcile only identical
fixture/status hunks. The ownership table and leaf ownership below still apply.
This replaces separate remaining task gates with one normal hook, exact
integrated-tree focused checks, review and Effort development gate. It does not
replace the final current-main merge, Main promotion gate or qualification
receipt. Superseded PRs close only after the surviving integration visibly
contains their changes.

The acceptance claim is a usable single-server managed setup in the web player: Settings saves
an enabled choice before readiness; a named local worker installs and verifies
allowlisted weights; generation during playback publishes the exact requested
source/audio/language artifact; Off and the current player survive publication;
explicit Use shows a cue; Activity reloads the exact job. Repeat from the final
committed package with an empty model store, then prove cancelled first-install
cache cannot become Ready after restart without fresh intent, explicit recovery
works, successful unchanged-model restart requalifies automatically, and running
inference Stop reaps its observed child and scratch without publishing.

Essential local checks cover SQLite and replicated-store admission/publication,
mode/CAS/source/Stop/provenance fences; manager/config/API/artifact/runtime suites;
the actual packaged native inference helper; web behavior and both shipped-UI
browser fixtures. Duration/model/output/publication limits remain safety guards:
a four-hour admission ceiling is not a four-hour performance or quality promise.
Test exact refusal boundaries rather than treating unfinished duration runs as
success. The pressure-stopped two-hour run remains a failed completion attempt.

A nonroot Linux single-server functional package/image run remains essential
for the Linux installation path; a native macOS pass cannot stand in for it.
Broader three-node/fleet routing, native-client campaigns, representative
long-form quality and throughput, release-throughput measurements and upgrade
campaigns remain explicit follow-up qualification. Their prepared drivers and diagnostic
results are retained; they do not delay the bounded single-server claim and are
not represented as supported, qualified fleet behavior. No optional probe or
translation implementation is added to this integration. Progressive English
translation followed as its own project; see
[PROGRESSIVE-SUBTITLE-TRANSLATION-PLAN.md](PROGRESSIVE-SUBTITLE-TRANSLATION-PLAN.md).

The original task boundaries above remain the review history. T3/T4 now land
as one compiling integration because manager orchestration, runtime inference
helpers and configuration validation depend on each other. Review still covers
three slices separately: HTTP/configuration and queue ingress; secure artifacts
and inference; worker routing, admission and publication. No inert worker stub
or disconnected-module test substitutes for the integrated daemon.

| Task | Current owner and branch | State and remaining evidence |
|---|---|---|
| T0 | Build lead, effort documentation | Reviewed and committed; historical design source retained. |
| T1 | Packaging Sol session, `codex/subtitle-engine-package` | Gate5210 passed and PR1111 landed as `a378ab927`; essential integrated service acceptance remains. |
| T2 | Build lead, `codex/subtitle-managed-job-contracts` | Built; pinned formatting/Clippy, schema pair and SQLite/three-voter store contracts passed. Included in the single consolidated candidate; layout fixtures do not qualify cluster restore. |
| T3/T4 | Build lead, `codex/subtitle-managed-runtime` | Corrected provisional source `1a6b971529` committed normally after `85de7adee`; pinned all-target Clippy, model33/33, HTTP3/3 and UI26/26 pass. The new actual SQLite/reopen/worker-pass regression fences cancelled cache and queued automatic work. Included in the single consolidated candidate; exact-tree checks, package acceptance and its gate remain. |
| T5 | UI Sol session, `codex/subtitle-usability-web` and `codex/subtitle-model-reasons` | Built with unit/static and actual watch-controller fixtures; provisional real-package browser flow preserved Off/video identity, selected an active cue and reloaded Activity. Maintenance graduation and exact final-tree repetition are included in the single consolidated candidate. |
| T6 | Build lead and reviewer | Open; final merge tree, promotion gate and scoped qualification receipt required. Movie-length quality/throughput, native Intel and broader deployed fleet remain separate unmet acceptance. |

Leaf ownership in the combined T3/T4 integration is explicit: the packaging
session owns `subtitle_models/artifacts.rs` and `subtitle_transcription.rs`;
the UI session owns `subtitle_models/config.rs` and `subtitle_models/api.rs`;
the build lead owns manager orchestration, state/routes, signed observations,
queue admission, `JobFence`, process launcher integration and integration tests.
The Maintenance graduation additionally reserves `web/pages/settings-panels.js`,
the setup link in `web/detail/preplay-selection.js` and corresponding settings
tests; moving the permanent setup controls does not imply broader qualification.
The watch follow-up additionally owns `web/detail/watch-browser.js`,
`web/player/watch-presentation.js`, `web/player/projection-chrome.js`, the narrow
`web/app.css` interaction rule, and their web/playback tests. These remain
reviewed slices. The original setup/completion and watch follow-up now form
part of the single remaining server integration candidate, preserving original
commits, ownership and regression trailers. Prior task PRs are superseded only
after that consolidated candidate visibly carries their changes. It must pass
the normal hook and one required effort gate on its exact integrated base.

Native qualification ownership remains with the UI session on
`codex/subtitle-native-qualification`: Apple
`Tests/GeneratedSubtitleAcceptanceTests.swift` and the minimal read-only
accepted-selection observation in `Sources/PlayerController.swift`; Android
`GeneratedSubtitleAcceptanceTest.kt`, `GeneratedSubtitleObservationTest.kt`
and the corresponding internal observation seams in `player/Controller.kt`
and `player/PlayerScreen.kt`. The demonstrated Android test-host PiP crash
also reserves the test-only `src/capabilityProbe/AndroidManifest.xml` overlay;
its correction remains in review and does not change the production manifest.
No Apple project-file change is required. Simulator/emulator evidence must distinguish the requested menu choice from
server-accepted selection and the actual current native player.

Managed discovery retains the common `subtitle-transcription:sweep` lease and
bounded eight-file pages. Its single disposable settings record,
`subtitles.transcription.managed.schedule.v1`, contains version, configuration
revision, next-run time and cursor. A record from another revision is due
immediately at cursor zero: an old sweep cannot postpone a changed or reenabled
configuration by publishing an obsolete timestamp. Interval zero performs no
sweep. Discovery chooses only known, matching spoken-language tags; untagged
audio still needs an explicit title request. Existing queued payloads retain
their accepted profile when defaults change.

T6 qualification ownership is also isolated. The engine builder owns
`codex/subtitle-recognition-qualification`: the recognition driver, attributed
corpus catalog/reference files, its operations tests and qualification document
with its index row. The web builder owns `codex/subtitle-native-qualification`:
`clients/apple/Tests/GeneratedSubtitleAcceptanceTests.swift`, the minimal
read-only accepted-selection seam in `clients/apple/Sources/PlayerController.swift`,
`clients/android/app/src/androidTest/java/tv/plurx/app/player/GeneratedSubtitleAcceptanceTest.kt`,
the corresponding internal observations in Android `player/Controller.kt` and
`player/PlayerScreen.kt`, and their focused stale-observation regressions. No
project-file registration change is required. These observations bind actual
accepted server selection to the current player/session/item; requested menu
state alone is not proof that alternate audio was applied.
Any minimal Android player test seam is separately reviewed. Native test builds
wait for the first packaged web acceptance and target only simulator/emulator;
physical-install builds retain the committed merged-main requirement. The build
lead owns integrated package/service evidence and the final ledger reconciliation.

Further adversarial findings and dispositions:

| Finding | Required disposition and evidence |
|---|---|
| macOS rejects Mach-O execution through `/dev/fd` | Copy the held verified executable into the private per-attempt directory with exclusive/nofollow creation, exact size/hash and original identity checks. Execute the owner-only absolute copy; retain descriptors until reap, then clean it. Linux keeps held executable descriptor execution. A real production-helper package test is required. |
| Cleared child environment was repopulated by the common launcher | Add an explicit CPU child path with LC_ALL only and absolute executable enforcement. An actual child test seeds hostile/sentinel ambient loader, GPU and proxy values and proves omission; existing launcher behavior remains covered. |
| One worker lacks the source mount | Check local source access before claim and revalidate the held source after claim. Local access failure yields for another worker; authoritative source revision changes retire obsolete work. Test both worker eligibility and globally changed source. |
| First ineligible queue page starves later work | Keep a bounded keyset cursor across passes and wrap at the end. More than 32 ineligible jobs must not hide a later model operation forever. |
| Failed automatic Verify admission becomes permanently suppressed | Mark scheduling only after accepted/current existing admission. Queue-full/error remains retriable; bound remembered identities per catalog model and distinguish replaced local identities. |
| Cold or replaced model hashing bypasses CPU admission | Observe model identity with bounded metadata only, and hash only after admission under the owned cancellable operation. Compare the verified identity with the preclaim receipt. Cancellation checks each bounded read and joins the blocking task; refresh timeouts cannot detach large model hashing. |
| Shutdown cancels a sibling token rather than the active child | Pass the manager-owned cancellation token through the inference helpers and derive their child token from it. Test an actual running child is reaped before private scratch and admission retire. |
| Long inference expires worker observations | Refresh bounded cached local facts while the owned operation is pending; bound each refresh and preserve Stop/shutdown and child cleanup. Expired or missing peer evidence remains unknown. |
| Managed runtime adapter disagrees with caption storage validation | Admit only the exact qualified managed adapter with closed engine SHA/platform execution provenance. Managed publication matches its payload adapter; legacy publication remains the literal historical adapter with no execution extension. Test actual SQLite and replicated publication/readback plus SQLite reopen and historical JSON compatibility. Logical artifact/profile identity stays unchanged. |
| Real recognizer output extends beyond decoded media EOF | Managed inference intersects validated cue intervals with the exact decoded PCM sample domain: clip straddling ends, discard wholly later cues, preserve text/IDs/order/overlap, and reject malformed/decreasing starts or empty output. Bind `mono16k-s16-digital-zero-pcm-eof-v1` into the managed pipeline digest and refuse incompatible recipe claims. Advanced output and digest retain prior behavior. The retained CC BY short-film raw output and attribution prove the observed boundary, not universal recognition quality. |
| Silence produces a hallucinated cue | Reject only nonempty decoded PCM whose actual samples are all digital zero, with bounded cancellable reads before inference. Return `no_speech_detected`; do not treat valid speech/noise quality as model verification failure or add a VAD model. Test actual FFmpeg silence and cancellation/header bounds; broader non-speech classification remains unqualified. |
| Cancelled install cache becomes Verified after restart | Require a durable successful receipt matching model id/SHA and exact current file identity before automatic verification. Label orphan bytes `unverified_cache`; explicit Install/Test can qualify them. Fence already queued automatic work to its typed payload, boot/model/engine identity, compare held identity after admitted hash, and recheck named identity before receipt. Preserve cancelled-first-install and replaced-inode restart reproductions; successful unchanged installs still reverify automatically. |
| Publication retry rewrites verification time | Keep receipt bytes immutable, use a fresh ownership-check clock and resolve only the exact successful job/fence receipt. Test lost-reply-style readback with a later clock. |


### Qualification ledger · integration snapshot, 2026-10-15

These rows distinguish implementation evidence from acceptance. Passing a small
fixture does not close a longer workload, different platform or client row.
Final qualification must name the integrated committed tree and package.

| Required surface | Evidence available | Remaining acceptance |
|---|---|---|
| macOS clean Settings workflow | Corrected committed source `1a6b971529` in a production-cfg functional package passed all 13 actual browser checks from missing weights: save enabled, install/automatic verification, generation during playback, exact artifact/VTT fetch, Off/video preservation, explicit Use with active cue and Activity reload | Repeat the final integrated merge tree with the shipping package profile; the unoptimized functional package does not qualify release throughput |
| Linux clean nonroot image | Pinned CPU engine recognized real speech as a nonroot user; package tests execute on Linux without skips | Complete daemon image with empty persistent models and normal Settings/API install/generate; repeat after upgrade |
| Download, decode and inference cancellation | Bounded chunk/hash and owned child lifetime regressions pass; provisional service inference Stop/reaping/no-caption and retry passed. Corrected committed-package first Install Stop retained unverified cache, stayed unverified after restart without Install/Test, then explicit Test qualified the same bytes | Final integrated-tree inference/decode and transfer repetition with exact job/fence evidence; preserve the original pre-correction failure separately |
| Low storage | Preallocation failure/low-budget tests leave no published partial artifact; actual physical reservation succeeds on local storage | Actual isolated ENOSPC or quota-limited filesystem run, with recovery; synthetic budget refusal is not ENOSPC |
| Restart and stale readiness | Corrected committed package retained saved enable, model SHA and exact caption/VTT across same-data restart without Install/Test, reverified after the new boot, and refused automatic readiness for cancelled first-install cache. Provisional corruption refusal, queued-generation block and explicit repair also passed; SQLite reopen/identity tests pass | Repeat on the final integrated shipping package; retain exact package/boot/source identities and the original pre-correction failures |
| Cluster routing and races | Actual SQLite/three-voter publication, cancellation, source revision and mode/CAS fences pass; two-worker local-source eligibility and queue pagination regressions pass | Isolated multi-process routing with one missing mount/model, worker restart/lease takeover, stale peer observations and partial install refusals |
| Movie and episode duration | Real short-film and German sample engine timing is retained separately; exact real-film EOF regression uses attributed raw output. A two-hour repeated-speech service attempt stopped at the host-pressure guard after 553.97s, with no publication and independently observed child/scratch retirement | End-to-end managed movie/episode workflow with source-clock bounds and recorded CPU/wall time; representative quality remains a separately scoped claim |
| Language and audio selection | Actual HTTP tests bind nondefault ordinal, unknown-tag explicit language and exact source identity. Corrected committed service installed Base multilingual, generated the referenced German FLEURS utterance and fetched the exact published VTT; cue end12000ms stayed within12960ms PCM | The one-sample automatic comparison has6 word edits/28 publisher-reference words, with no local human review. Broader language/dialogue quality and final shipping-tree repetition remain open |
| Web playback and Activity | Corrected committed `1a6b971529` package added the exact artifact without reload, preserved Off/player/attachment/source, rendered an active cue after Use/seek and reloaded the exact Activity job | Repeat on the final integrated shipping package. Raw console receipts retain two established disabled remote-control 503 probes; scoped subtitle passes are not clean-console claims |
| Apple and Android playback | Native qualification remains open. A retained Android emulator run passed direct1080p with Off, then failed to render selected generated caption1: the fixed direct source exposed only embedded French text and later stalled | Android direct acquired/generated external captions need a separate safe dynamic text owner; a metadata marker or startup preload does not solve publication during playback. No partial correction or native qualification is claimed. Broader Apple/Android/physical acceptance remains follow-up |
| Upgrade and rollback | Transactional bundle staging, corruption refusal, old-bundle restoration and model retention installer regressions pass | Isolated supported installation upgrade/restart/rollback evidence; never replace the operator's running service for acceptance |
| Promotion | Task slices reviewed and local focused evidence recorded | Green exact-base effort gates, integrated main merge, full promotion gate and qualification receipt; no red-gate merge or production rollout is authorized |


### Prepared cluster follow-up — not included in this candidate

The build lead retains a prepared three-node driver and protocol tests on the
separate `codex/subtitle-usability-operations` workspace. They are not files or
runnable commands in this candidate, and no multi-process result is claimed.
The follow-up must pin an immutable Linux image, use distinct private model
paths and source mounts, preserve quorum during observed owner loss, and prove
exact attempt/source/artifact identity, stale readiness and single publication.
It remains broader fleet qualification rather than a single-server completion
gate. Existing replicated-store regressions do not substitute for that run.

The macOS Stop experiment retains its original stricter failed assertion:
complete checksum-valid cache bytes appeared after Stop acknowledgement, while
the job settled cancelled without a success receipt. The accepted contract
permits residual unverified cache after bounded Stop convergence, but not
installation/readiness or automatic completion of stopped work. The separate
restart experiment found a real bug on provisional `85de7adee`: those orphan
bytes became Verified without a fresh action. The correction requires a prior
successful exact-model-identity receipt for automatic enqueue and execution;
old queued automatic work also carries and checks its boot/model/engine
identity. Explicit Install/Test may intentionally qualify cache. An unchanged
previously accepted model remains installed when a redundant operation stops
(Stop is not uninstall). The original red receipts remain the failing baseline;
the focused corrected-source regression passed on `1a6b971529` (actual SQLite
reopen and real `worker_pass` retirement, within model33/33). A separate empty
root then reproduced the same stopped first-install/cache scenario on the
corrected committed package: the job stayed cancelled with no result, restart
without Install/Test left the cache `unverified_cache` with no verification
time, and explicit Settings Test subsequently qualified the unchanged bytes.
The corrected package also passed all 13 actual subtitle browser workflow
checks. These scoped passes preserve, rather than rewrite, the old red
receipts. Successful experimental receipts already produced by the unreleased
buggy version are not retrospectively invalidated or claimed to be migrated.
Final integrated-tree and shipping-package repetition remain required.


Current provisional task evidence is bound to commit `1a6b971529cb9a1406a35034ada13cd9b2362c8f`
and tree `3c435604ae2c1447ab56280978d6c6136fe772b6`. Rust 1.97.1 compiled the
registered daemon tests; the normal tracked hook passed workspace formatting,
all-target Clippy with denied warnings, catalog lint and served JavaScript.
Focused counts are model33/33, HTTP3/3 and web26/26. The reusable cluster driver's
nine isolation/protocol tests and docs-index four tests also pass, but its
three-node run is still pending. A standalone corrected-source macOS package
also built with Rust 1.97.1, one compiler job, explicit source stamp and the
production-cfg `test` profile; all memory-pressure samples stayed normal.
The copied daemon's signature and pinned engine/FFmpeg/parser were verified,
and `--version` reports the exact commit. Fresh-root Stop/cache/restart, explicit recovery and the full browser workflow
passed on that corrected package; this unoptimized functional package does not
qualify release throughput.
These are task-source results; merging a new effort/main base requires
exact-tree revalidation and normal gates.

### Advanced vocabulary compatibility clarification

Advanced header capability accepts the known GGML vocabularies 51864 (English),
51865 (99 languages) and 51866 (100, including Cantonese `yue`). This follows
`whisper_vocab::num_languages` and the language-token table in the pinned
whisper.cpp source 306c88f4d1286aec1bf96e544632897886af5501. Unknown headers remain
unadvertised and the 2 GiB bound remains unchanged. This preserves manual-model
language admission; it does not qualify additional weights. The managed Base
multilingual catalog remains 99 languages and excludes `yue`.
