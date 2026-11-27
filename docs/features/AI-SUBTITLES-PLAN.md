# AI subtitles — one feature, automatic route, self-installing models

**Status:** open; plan written, build started · **Written:** 2026-11-27 ·
**Owner:** Claude (effort E-3C179E1FBE) · **Branch:** `effort/ai-subtitles`

Plurx has three AI subtitle features: whole-file generation, translation of
an existing subtitle track, and live captions/translation from the audio.
All three are built and switched on, and none of them reaches a viewer. This
plan merges them into one feature, **AI subtitles**. It has one settings page,
one entry in the player's subtitle list and one control on the item page. The
owner approved the design and its mock-ups on 2026-11-27, and those mock-ups
are the UI spec for this plan.

Canonical project ledger: [PROJECT-BACKLOG.md](PROJECT-BACKLOG.md) row P63. This
plan supersedes the user-facing parts of
[SUBTITLE-GENERATION-USABILITY-IMPLEMENTATION.md](SUBTITLE-GENERATION-USABILITY-IMPLEMENTATION.md)
(P14) and
[PROGRESSIVE-SUBTITLE-TRANSLATION-PLAN.md](PROGRESSIVE-SUBTITLE-TRANSLATION-PLAN.md)
(P27). Their engine, admission and evidence rules still apply.

## 1. What is wrong today (measured 2026-11-27)

| Symptom | Root cause |
|---|---|
| Every node shows its models as "unverified" after any restart. Speech translation is verified on 0 of 4 nodes. | Every verification record is bound to the process `boot_id`. Model receipts (`subtitle_models`), translation capabilities (`subtitle_translation`) and the text model (`subtitle_text_translation`) all go stale on each restart. A stale record makes the workers refuse work with `model_verification_failed` or `translation_unverified`. Getting them back takes a per-node, per-purpose admin button press. |
| The same model is verified twice. | `transcription` receipts and `translation` profile capabilities are separate records with separate self-tests. |
| Two generators with different results. | Whole-file generation (`subtitle_transcription.rs`, `subtitle_models/`) publishes a `DownloadedSubtitle`. Progressive generation (`subtitle_translation/`) publishes into its own `subtitle_translation_*` asset store, which only the client-drawn caption readers can show. |
| Confusing UI. | Three enable switches on two settings tabs, four panels on the item page, and player entries that leave the player. Every start asks for a model and an "I confirm this is the spoken language" checkbox. Offline generation is admin-only and web-only. |
| Poor output. | Base model, 2 CPU threads, `--no-gpu` hard-coded, no speech detection ("You" over silence), 42–50% word mismatch on a film ([qualification](SUBTITLE-RECOGNITION-QUALIFICATION.md)). |

**The queued-extraction finding is corrected.** The handoff named 608
`subtitle_extract` jobs refused by `maintenance_reader` as the reason viewers
see nothing. On 2026-11-27 the live queue was read again with
`GET /analysis/jobs?component=subtitle_source`. All 612 queued rows have
`trigger=background` and `priority=normal`: they are the overnight extraction
backfill, which is maintenance and is classified correctly. A viewer's request
goes through `subtitles::cluster_vtt_into`. That call promotes the request to
`foreground`/`playback`, which `claim_maintenance_reader!` admits as live
demand, and it falls back to inline extraction (`extract_vtt_without_demux`, then
demux). Translation input (`subtitle_text_translation::input_bytes`) uses that
same path. No classification change is needed for viewers.

The backfill's slow progress is a separate throughput question: it waits
behind about 3,300 queued `fragment_index` rows, and 480 of its rows were
cancelled as `artifact_lost`. It is recorded as a follow-up, not as part of
this plan. What this plan does need is for every AI job that a viewer is
waiting on to be live demand. §3.1 makes sure the merged pipeline keeps that
property.

**The real refusal is authorization.** `translation_admission!` requires the
demanding user to have `is_admin=1`, and `text_admission!` requires
`payload.admin_user_id` to be an admin. Both conditions are in the claim and
settle predicates of `store/background_jobs.rs`. A non-admin's job would be
enqueued and then never claimed. §3.3 replaces both conditions with the
`who` setting.

## 2. The shape

The viewer asks for subtitles in their language, and the server picks how to
make them:

1. **Text route.** The title has a usable text subtitle in another language.
   Translate it with the text model. It is fast and the timing is already right.
2. **Audio route.** There is no usable text track. Listen to the audio, starting at the
   playhead and staying ahead of it. English speech is transcribed. Other
   speech becomes English subtitles.

Either route **finishes as an ordinary stored subtitle track**
(`DownloadedSubtitle`). That track reaches every client through
`/files/{id}/subs/{n}`, including AirPlay and picture-in-picture. While the
work runs, the client shows the cues produced so far through the same
manifest/segment reader the progressive feature already has.

## 3. Design decisions

### 3.1 One pipeline (merges A into B)

- The progressive range pipeline (`subtitle_translation/`) becomes the only
  speech generator. A request with no viewer (scheduled "ahead of time" work)
  is the same operation without a playhead demand. Its ranges run from 0
  at background priority.
- **Finish demand.** Today a range can be claimed only under a live viewer
  demand whose playhead window covers it, or under a continuity row, so the
  tail of a film after the viewer stops never runs. Each operation gets one
  durable server-owned *finish* demand. It has no playhead, it admits every
  core, and it is classified as maintenance. The viewer's demand still wins
  the head of the queue as live demand.
- When every core range is published, the server renders the segments into
  one compact WebVTT and publishes it as a `DownloadedSubtitle` with
  `transcription` provenance. It does this in the transaction that marks the
  operation complete, and clients then switch to the stored track.
- **Size cap.** `MAX_DOWNLOADED_SUBTITLE_BYTES` (256 KiB) lives in the
  replicated `files` row, and a talky 3–4 h film can exceed it. Work is never
  lost to the cap. If the rendered track fits, it is published as above. If
  it does not, the completed asset stays the durable form and is listed and
  served as a stored track from its segments. A 4 h fixture pins this case.
  Raising the cap is rejected, because every byte of it is replicated in the
  `files` row up to 8 times per file.
- Asset rows are retired only after the track is published *and* the last
  reader's demand has expired, so a viewer in mid-playback keeps its
  manifest.
- B's lease, fence and read-attestation machinery stays. It is what makes
  range work on a shared source safe.
- Whole-file managed generation (`SubtitleTranscribeManaged`), the Advanced
  custom-command mode (`SubtitleTranscribe`, `subtitles.transcription.config`)
  and their routes and settings are removed. Queued or running rows of those
  kinds settle as `superseded` on upgrade.
- A viewer-awaited range keeps its `subtitle_translation_range` waiter, which
  `claim_maintenance_reader!` already treats as live demand. A regression
  test pins that, and pins that a demand-less (scheduled) range is
  maintenance.

### 3.2 Models install, verify and repair themselves

The live model state on 2026-11-27 was:

| Node | `base.en` | `tiny.en` | `small` |
|---|---|---|---|
| 1 | verified | missing | missing |
| 2 | verified | verified | `unverified_cache` |
| 3 | verified | missing | missing |
| 4 | missing | missing | missing |

The text model was missing on every node.

The automatic re-verify after boot works: `refresh_local` → `requalify_accepted_model` re-runs the self-test once per boot for any model with an accepted receipt, and that is why `base.en` reads verified after restarts. **The root cause is that nothing installs a model by itself.** Install is a per-node, per-purpose admin button. A cancelled install leaves `unverified_cache`, which by design only an explicit install may qualify.

`boot_id` binding is therefore kept. Re-running a seconds-long self-test per boot is a sound proof that this host and engine still work, and Fable's review lists what it protects.

- **One record covers every purpose.** The progressive pipeline's per-profile capability rows (`subtitle_translation_capabilities`) are replaced by the model receipt (`subtitle_model_receipts`). A multilingual model's self-test is the German clip translated to English, which runs the decoder, the language path and the translate task; it also produces the receipt that transcription accepts. An English-only model keeps the English fixture.
- **Self-install.** When the audio route is on, every capable node installs the current tier's model and the VAD model. A capable node has the qualified engine and platform, free memory and disk for the model, and is found through `state.cluster`. When the text route is on, the node installs the text model. These installs are server-owned *install* jobs, the same fenced job an admin's Install runs. So `unverified_cache` and missing files repair themselves, and only a durable successful receipt counts as ready.
- **Repair.** A missing or changed file triggers a reinstall on the next pass, and the status line reports it. The "Download and test" / "Test model" buttons and the worker checkboxes are removed.

### 3.3 Route and language

- `POST /files/{id}/ai-subtitles` with `{target: "en"}` is the only start.
  It takes optional corrections: `route: "audio"` ("Use the audio instead")
  and `spoken_language` ("Wrong language?"). `GET /files/{id}/ai-subtitles`
  returns the route the server would take, its one-line explanation and any
  operation in progress. Both are open to every signed-in user unless
  Settings says "Admins only".
- Text route candidate: a non-bitmap, non-forced text track whose language is
  not the target. The server prefers a non-SDH track and the one whose
  language matches the audio.
- Spoken language: whisper language detection on the first speech window.
  The audio track's tag is the hint. When the tag is present and detection is
  not confident, the tag wins. The model picker and the "I confirm" checkbox
  are removed. "Wrong language?" restarts the operation with an explicit
  spoken language.
- Authorization: the `is_admin` conditions in `translation_admission!` and
  `text_admission!` become the `who` setting. A regression shows that a
  non-admin's range job and text job are claimed. Opening AI work to every
  user needs bounds, so there is a per-user limit on concurrent operations
  and the existing queue caps stay. The same task settles whether a
  viewer-started text job holds a `source_io` resource. If it does, it is
  added to the live-demand list, conditioned on a live viewer, the way the
  range arm is.
- The old start and config routes are removed in T6 together with the old UI,
  so each pull request leaves every client working. The new start keeps the
  request-identity idempotency that the existing starts use.

### 3.4 Quality

- Speech detection (whisper.cpp's Silero VAD) runs before decoding, so
  silence and music produce no cues.
- More threads: the worker takes its CPU budget from software admission
  instead of the hard-coded 2.
- GPU: a Vulkan variant of the engine for the nodes' Intel and AMD iGPUs,
  used only where its self-test passes on that node.
- Tiers (Fast / Balanced / Best) map to models and decode options. They are
  **measured on the fleet before they are fixed**. That includes whether the
  fastest large model (large-v3-turbo, which was not trained to translate) is
  acceptable for foreign speech, or whether "Best" for foreign audio must be
  transcribe-then-translate-the-text. The text model `opus-mt-mul-en` is
  compared against per-language models on real subtitle files. §6 records the
  results.

### 3.5 Settings

One document, `subtitles.ai.v1`, which the server returns with its readiness:
`{download_match, translate_text, from_audio, ahead_of_time, who: everyone|admins,
quality: fast|balanced|best}`. "Subtitle language" and "Show subtitles" are
the existing per-user `subtitle_language` / `subtitle_mode`. On upgrade, the
three old enable flags (`subtitles.transcription.managed.v1`,
`subtitles.translation.config.v1`, `subtitles.text_translation.config.v1`)
and the schedule keys (`subtitles.transcription.managed.schedule.v1`,
`subtitles.transcription.next_run`) fold into it. The new document uses one
compare-and-set scheme, the integer generation that text translation already
uses (`put_settings_if_generation`). The admission SQL reads the old keys by
name, so the fold, the switch of the SQL to the new key and the deletion land
together in one migration transaction. For one release the server also
writes the old keys as a mirror, so a node that is still on the old version
keeps admitting work during a rolling upgrade. Queued rows of the removed
whole-file kinds are settled as `superseded` by the migration. A
mixed-version node that claims one first just finishes it, which is
harmless because its result is an ordinary track. There is no feature gate: each of the three
rows is a real user choice and nothing lives in Developer.

### 3.6 Clients (per the approved mock-ups)

- Settings → Subtitles: a new page with flat sections and hairline rules.
  It has Defaults, "When a title has no English subtitles" (three ordered
  switches, when, who), and AI quality (segmented control, status line,
  per-node Details).
- Player: one "English AI" entry in the subtitle list. Its small line says
  how it will be made or how far it has got, and picking it acts in place.
  An "Options ⋯" entry offers Use the audio instead, Wrong language?, Timing,
  and later Redo at Best and Delete.
- Item page: "+ English" in the Subtitles row with route choices. The
  "Captions" row and its three panels are removed.
- Web, Apple and Android all get the same words. Removed: the Maintenance
  card, the Playback card, the three switches, the model-setup buttons, the
  pickers, the confirmation checkbox, Android's typed language code, and the
  names "progressive", "offline", "text translation" and "captions".

### 3.7 API contract (T5; the clients are built against it)

All routes are under `/api/v1`. All take a signed-in user, and `who` decides whether a non-admin may start work.

- **`GET /files/{id}/ai-subtitles?target=en`** describes what AI subtitles would do for this file:
  `{target, allowed, blocked: null | {code, message}, existing_index: n|null,
  route: "text"|"audio"|null, label, estimate, source: {kind: "subtitle"|"audio",
  index, language, detected}, alternatives: [{route, label}], operation: null | Operation}`.
  - `label` is the small line in the player, such as "Translate from the Japanese subtitles" or "Create from the audio". `estimate` reads like "about a minute" or "starts in a few seconds".
  - `blocked.code` is one of `disabled`, `admins_only`, `no_capable_node`, `no_audio`, `quota`.
  - When `blocked` is set, clients leave the entry out.
- **`POST /files/{id}/ai-subtitles`** with `{request_id, target: "en", route: "auto"|"text"|"audio", spoken_language?, position_ms?}` starts or joins the work and returns an `Operation`. Repeating the same `request_id` returns the same operation.
- **`Operation`** is `{id, route, state: "queued"|"working"|"complete"|"failed"|"stopped",
  ready_to_ms, duration_ms, progress, reason, track_index: n|null, spoken_language}`.
  - `ready_to_ms` is how far ahead the cues are ready. It draws the green mark on the timeline.
  - `track_index` is set once the stored track exists. Clients then select that index as an ordinary subtitle track and stop reading cues.
- **`PUT /ai-subtitles/operations/{id}/viewer`** with `{position_ms, playback_rate}` is the viewer heartbeat (the demand), sent every 10 s while the entry is selected. A seek past `ready_to_ms` moves the work to the new position. `DELETE` on the same route releases it.
- **`GET /ai-subtitles/operations/{id}/cues?from_ms&to_ms`** returns `{cues: [{start_ms, end_ms, text}], ready_to_ms, state}`. It is the one in-progress reader for both routes. For the audio route the server reads the asset segments behind it, under the existing lease and attestation rules. For the text route it reads the partial text cues.
- **`POST /ai-subtitles/operations/{id}/stop`** stops the work. **`DELETE /files/{id}/subtitles/{index}`** deletes a stored AI track; that is the ⋯ Delete. "Redo at Best" is a `POST` with `quality: "best"` and `route: "audio"`.
- **Stored tracks.** A stored AI track carries `ai: true` in the file's `subtitle_streams`. Clients show the AI badge from that field, and the title is the language name, such as "English".
- **Settings.** `GET /ai-subtitles/settings` returns the document, its `revision` and `readiness`:
  `{nodes: [{label, cores, gpu, state: "ready"|"downloading"|"failed"|"unsupported", reason, tested_at_ms}], summary}`.
  `PUT /ai-subtitles/settings` (admin) writes it, with `expected_revision` as the compare-and-set check.

## 4. Tasks and pull requests

Each task is one pull request into `effort/ai-subtitles`, with its focused
regression named in its description. The effort lands on `main` once,
through the requested lane and `scripts/land`.

| # | Task | Area | State |
|---|---|---|---|
| T1 | This plan and the corrected queue finding | docs | in review |
| T2 | `subtitles.ai.v1` with its migration; admission reads it; `who` replaces `is_admin`; per-user bounds | store admission SQL, settings | pending |
| T3 | One verification record per model for every purpose; self-install and repair on enable (`boot_id` kept, see §3.2) | `subtitle_models`, `subtitle_translation`, `subtitle_text_translation`, store | pending |
| T4 | One pipeline: finish demand, completion publishes a stored track (cap-safe), remove whole-file and Advanced generation | `subtitle_translation`, `subtitle_transcription.rs`, store | pending |
| T5 | `/ai-subtitles` API, automatic route and spoken-language detection | server | pending |
| T6 | VAD, thread budget, measured Fast/Balanced/Best tiers, text-model comparison | engine options, runtime | pending |
| T6b | Vulkan engine variant for the iGPUs (optional for landing; nothing in §5 depends on it) | engine build | pending |
| T7 | Settings → Subtitles, player entry and item control on web, Apple and Android; remove the old UI and routes | clients | pending |
| T8 | Confirm the surround `source_clock_unsupported` repair on the deployed build with a real 5.1 film | fleet | after deploy |
| T9 | End-to-end acceptance of all three routes on the fleet | fleet | after deploy |

The settings and authorization task (T2) comes first, because self-install (T3)
is driven by its switches, and because the pipeline merge (T4) removes `SubtitleTranscribeManaged`, whose admission reads the
managed key. A fresh-context Fable review of this plan (2026-11-27) supplied
the authorization gap, the finish demand, the cap rule and this order. Its
`boot_id` concern (find the re-verify stall first) led to §3.2's finding that
there is no stall, so `boot_id` stays.

Deploying to the fleet is the owner's step. T8 and T9 run after the owner
deploys the effort's landing.

## 5. Acceptance

- Generate (English audio), translate an existing foreign text track, and
  translate foreign audio each work end to end on the fleet through the new
  UI on web, Apple and Android. Each one finishes as a stored track that
  survives a restart.
- After a plurxd restart, every node that was ready is still ready, with no
  admin action.
- Silence and music produce no cues on the qualification fixtures.
- A 5.1 film starts live translation without `source_clock_unsupported`.

## 6. Measurements

To be filled by T6 from fleet runs.
