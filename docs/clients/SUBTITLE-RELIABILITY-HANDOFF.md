# Subtitle reliability — build handoff

**Status:** ready to build · **Executes:** the repairs in
[SUBTITLE-RELIABILITY-ASSESSMENT.md](SUBTITLE-RELIABILITY-ASSESSMENT.md) §3
· **Baseline:** `origin/main` @ `c9e4edf4` (v0.3.0-2633) · **Written:**
2026-09-16

Read the assessment first — it carries the file:line evidence for every
defect below and the confidence label on each ("confirmed in source",
"runtime hypothesis", "not verified"). This document is the build order.
Work milestone by milestone, one PR each, in the order given; §3 milestones
depend on §2's wire changes and nothing depends on §7. Every line number
below was read at the baseline commit — **re-verify against the file at
build time**; `main` moves several times a day.

The standing instruction: if a step seems to require a "burn to SDR anyway"
override, a lossy ASS/SSA→WebVTT rendition, deploying to a node, or enabling
the PGS overlay in production, **stop and flag it** — those are decisions the
owner has not made (see §9 non-goals). Do not gate any behaviour behind a
code flag; where an operator choice is genuinely needed it is a Settings →
Developer switch with an advisory readiness section, never a blocker.

## 1. Objective

Three viewer-visible failures on every client, all with a confirmed source
mechanism at the baseline:

| Symptom | Mechanism (assessment §) | Fixed by |
|---|---|---|
| "That subtitle requires an SDR burn-in. HDR playback was kept unchanged." with no user action, or on a track that should not need a burn | §2.1 — server defaults a burn-only track; web auto-applies it; the two guards disagree; Apple's stale legacy check reopens SRT as a burn | §3, §4, §6 |
| Selected, nothing shows, no error | §2.2 — server answers "warming" with a valid empty segment; each client's readiness retry is broken | §5, §6 |
| Showing, then stops, still selected | §2.3 — Apple successor commit does not reconcile the selection; empty segments past a window boundary / past midpoint are cached | §5, §7 |

Done means: on nynuc, an HDR remux with a default-flagged English PGS track
opens on all three clients with **no notice and no subtitles**, an SRT track
turned on mid-play shows cues within one segment on all three, a
server-driven quality handoff on Apple keeps the selected SRT showing, and
every acceptance command in §3–§8 is green. What "done" does **not** include
is in §9.

## 2. Contracts — the interfaces these milestones touch

Copied from the baseline; re-verify at build time.

### 2.1 Track selection (`crates/plurx-core/src/tracks.rs`)

```rust
pub struct TrackSelection {              // tracks.rs:21
    pub audio_index: Option<i64>,
    pub subtitle_index: Option<i64>,
}
pub struct LangPrefs {                   // tracks.rs:57
    pub audio_lang: String,              // ISO 639, default "eng"
    pub sub_lang: String,                // default "eng"
    pub sub_mode: SubMode,               // Off | Always | Auto, default Auto
}
pub fn is_bitmap_subtitle(codec: &str) -> bool      // tracks.rs:175  hdmv_pgs_subtitle|pgssub|dvd_subtitle|dvdsub|xsub
pub fn is_pgs_subtitle(codec: &str) -> bool         // tracks.rs:187  hdmv_pgs_subtitle|pgssub
pub fn is_native_text_subtitle(codec: &str) -> bool // tracks.rs:195  subrip|srt|webvtt|vtt
pub fn subtitle_requires_burn(codec: &str) -> bool  // tracks.rs:205  !is_native_text_subtitle
pub fn select_tracks(                                // tracks.rs:242
    audio: &[AudioStream], subs: &[SubtitleStream],
    prefer_original: bool, prefs: &LangPrefs,
) -> TrackSelection
```

`select_tracks` is codec-blind: `sub_in_lang` (`:219`), `forced_or_default`
(`:230`), the `prefer_original` fallbacks (`:253-255`) and the `Always`
fallback (`:286`) look only at `language`, `forced`, `default`.
Callers at the baseline: `http/dto.rs:465` (item detail), `http/stream.rs:1997`
(`/decision`), `http/stream.rs:2410` (audio-only), `http/offline.rs:185`,
`transcode.rs:13325` (session creation, via `TranscodeManager::select_tracks`).

### 2.2 The wire (`crates/plurxd/src/http/stream.rs`)

```rust
pub struct SubTrackDto {                 // stream.rs:375
    pub index: i64, pub codec: String,
    pub language: Option<String>, pub title: Option<String>,
    pub default: bool, pub forced: bool,
    pub text: bool,      // !is_bitmap_subtitle — a sidecar can be extracted
    pub native: bool,    // is_native_text_subtitle — may be an HLS rendition
    pub overlay: Option<String>, // "pgs-v1" when PGS and the overlay is on
}
// /decision selection block (stream.rs:~2126, PLAYBACK.md:392)
"selection": { "subtitle_index": 3,
               "subtitle_requires_burn_in": true,     // is_bitmap && !(overlay && pgs)  stream.rs:843
               "subtitle_burn_in_blocked_by_hdr": false }
fn subtitle_burn_would_discard_hdr(decision: &Decision, requires_burn_in: bool) -> bool // stream.rs:952
    // requires_burn_in && decision.method != Transcode && delivered_dynamic_range ∈ {dolby_vision,hdr10,hlg}
fn apply_selected_subtitle(decision, file, selected, requires_burn_in)            // stream.rs:916
    // if !refused: method=Transcode, transcode_audio=true, preserve_dolby_vision=false,
    //              delivered_dynamic_range="sdr"
```

`Decision.delivered_dynamic_range: &'static str` (`playback/mod.rs:699`) is
the base plan's grade **until `apply_selected_subtitle` rewrites it** — that
ordering is what §4 relies on.

### 2.3 Session creation guard (`crates/plurxd/src/http/hls.rs`)

```rust
const HDR_SUBTITLE_BURN_REFUSAL: &str =                         // hls.rs:1326
    "That subtitle requires an SDR burn-in. HDR playback was kept unchanged.";
fn hdr_subtitle_burn_is_refused(                                  // hls.rs:1334
    source: Option<&MediaFile>, subtitle_burn: Option<i64>, subtitle_burn_sdr: Option<bool>,
) -> bool   // burn requested && ack != Some(true) && file.hdr ∈ {dolby_vision,hdr10,hlg}
// call site hls.rs:1820 → ApiError::Unprocessable {"code":"hdr_subtitle_burn_refused", "error": …}
// M6 preparation: hls.rs:7945  subtitle_burn_sdr: Some(!requested_hdr10); the
// preparation path (:7970-7982) calls resolve_plan directly and never runs the guard —
// the ack's only reader is :1340, on the create path
```

Request body fields on `POST /files/{id}/hls/sessions`: `subtitle` (native
rendition index), `subtitle_burn` (index), `subtitle_burn_sdr` (bool ack),
`native_subtitles` (bool). Non-native codecs asked for as `subtitle` are a
400 `"the selected subtitle requires burn-in"` (`hls.rs:1602-1611`).

### 2.4 Master playlist and subtitle segments (`hls.rs`)

- `master_playlist_with_shape` (`:11493-11660`): one `EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID="subs"` per native track in `sub_tracks` order; `DEFAULT=YES` only for `?subtitle=N` and `!forced` (`:11538`); `CLOSED-CAPTIONS=NONE` only when `PLURX_HLS_CLOSED_CAPTIONS_NONE` is set (`MasterRungs`, `:11398-11417`, `:11630`).
- Segment handler `subtitle_vtt_local_before_with_source` (`:10344`): whole-track sidecar → slice; else window sidecar (anchor = demand rounded to the `playback.subtitle_window_secs` grid, `subtitles.rs:246`) → slice; else kick `warm_whole`, maybe `warm_window` (only if the settled target covers the anchor, `:10543-10552`, and `windowing_is_worthwhile`, `subtitles.rs:266`), then **200 `WEBVTT\n\n` `Cache-Control: no-store`** (`:10574-10588`). The `timeout_at` around `warm_window` (`:10554-10566`, `RESPONSE_PUBLICATION_LIFECYCLE_BUDGET = 5 s`, `:60`) bounds only the ownership/settlement locks — the extraction itself is never awaited (handler comment `:10513`).
- `SubtitleSegmentSource` trait (`:~10230`; `warm_window` at `:10254`) — `read_whole` · `read_window` · `warm_whole` · `warm_window(session, sequence, dir, file, index, anchor, window) -> bool`. `warm_window` **spawns and returns immediately** (`subtitles.rs:1431-1570`); nothing in the handler awaits an extraction, and the comment at `:10501-10504` says why: AVPlayer gives a subtitle segment ~2 s and blocks video while it waits. Tests substitute the trait (fixture impl around `:15900-15960`).
- Readiness: `subtitle_readiness ∈ {ready, warming, unavailable}` on `DeliveryView` (`playback_control.rs:967-987`); `Absent` maps to `warming` (`hls.rs:4650`).

### 2.5 Client selection entry points

| Client | Selection entry | Guard | Readiness retry |
|---|---|---|---|
| Apple | `selectSubtitle` `PlayerController.swift:3026` → `subtitleSelectionRoute` `:7975` | `subtitleBurnWouldDiscardHDR` `:7038` = `isHDRDelivery` && `subtitleRequiresBurn` (`:7359`, = `!isNativeHLS && !isPGSOverlay`) | `retryNativeSubtitleAfterReadiness` `:8264`, gated on `activeNativeSubtitle` (written only in `open`, `:3797`/`:3943`) |
| Android | `switchSubtitle` `Controller.kt:1291` → `subtitleRoute` `SubtitlePolicy.kt:111` | `subtitleBurnWouldDiscardHdr` `SubtitlePolicy.kt:70` | `retryNativeSubtitleAfterReadiness` `Controller.kt:2244`; readiness field `PlaybackControlReporter.kt:668` |
| Web | `setSub` `index.html:13855`; auto-apply `:10237-10242` | `PlaybackPolicy.subtitleBurnAction` `playback-policy.js:1076`, `subNeedsBurn = text===false` `:13551` | `retryReadyNativeSubtitle` `:8907` (`subtitleTrack=-1; =n`) |

## 3. M1 — the server never defaults a track the viewer cannot see

**Defect.** `select_tracks` stamps a PGS/VobSub track as `default` whenever
language/forced/default flags say so. On an HDR base delivery that default is
undeliverable (burn refused, overlay off), so web raises the notice at
+400 ms and the native clients silently show nothing.

**Change.** Add an eligibility predicate to selection and have `/decision`
pass one derived from the base plan. Keep `select_tracks`'s signature so its
existing matrix stays valid:

```rust
// tracks.rs — new
pub fn select_tracks_with(
    audio: &[AudioStream], subs: &[SubtitleStream],
    prefer_original: bool, prefs: &LangPrefs,
    eligible: impl Fn(&SubtitleStream) -> bool,
) -> TrackSelection;
pub fn select_tracks(...) -> TrackSelection { select_tracks_with(..., |_| true) }
```

**Every** subtitle `find` in the function filters on `eligible` first — the
two helpers `sub_in_lang` (`:219`) and `forced_or_default` (`:230`), the
`prefer_original` fallbacks (`find(|s| s.default)`, `find(|s| !s.forced)`,
`subs.first()`, `:253-255`) and the `Always` fallback (`find(|s| !s.forced)`,
`:286`). Miss one and the "Always + only ASS → None" case below fails. A
candidate that fails `eligible` is skipped, not vetoed — the next candidate
in the same rule is tried, then the next rule, then `None`.

`/decision` supplies the predicate. It needs the base plan's grade **before**
the subtitle is chosen, so restructure `decision` (`stream.rs:~1990-2140`):
select audio → build the base `Decision` with no subtitle → derive
`base_range = decision.delivered_dynamic_range` → select the subtitle with:

```rust
let overlay_on = pgs_overlay;                       // already read once per request
let base_is_hdr = matches!(base_range, "dolby_vision" | "hdr10" | "hlg");
let eligible = |s: &SubtitleStream| {
    is_native_text_subtitle(&s.codec)                                    // rendition
    || (overlay_on && is_pgs_subtitle(&s.codec))                         // overlay
    || (is_bitmap_subtitle(&s.codec) && s.forced && !base_is_hdr)        // forced burn on SDR
};
```

The third arm is the clients' existing veto ("never auto-start a burn for a
non-forced track"; forced may) expressed server-side, so all four parties
agree. ASS/SSA/`mov_text` are deliberately **not** eligible as a default:
their only session-mode route is a burn, and web's sidecar route is a manual
pick. Then `apply_selected_subtitle` runs as today.

Other callers: item detail (`dto.rs:465`) has no plan, so pass
`|s| is_native_text_subtitle(&s.codec) || (overlay_on && is_pgs_subtitle(&s.codec)) || (is_bitmap_subtitle(&s.codec) && s.forced)`
— the same predicate without the HDR term; `/decision` is what the clients
act on and it will refine. `offline.rs:185` keeps `select_tracks`.

**`TranscodeManager::select_tracks_with_prefs` (`transcode.rs:13325-13350`)
is not exempt.** An explicit `subtitle`/`subtitle_burn` override wins there,
but with no override and `prefer_original` true it burns the *policy default*
whenever `subtitle_requires_burn(codec)` — a dual-audio anime HDR file with a
default English PGS is burned (tone-mapped) with no guard and no client
choice. That is the M1 class in a second place. Two changes: the manager's
implicit pick uses `select_tracks_with` and the item-detail predicate above,
and the implicit burn additionally runs §4's `burn_would_discard_hdr` against
the session's no-burn grade — refused means no subtitle, never a downgrade.
Add a manager-level test for exactly that file shape.

**Web behaviour change, on purpose.** Apple (`PlayerController.swift:7812`)
and Android (`SubtitlePolicy.kt:196`) already veto a non-forced burn default;
web does not (`index.html:10237-10241` auto-applies any `default`), so today
an SDR remux with a non-forced PGS default auto-burns on web at +400 ms. After
M1 the server never sends that default, so web stops doing it. Say so in the
PR and add a `web-policy.test.js` case pinning that a non-forced bitmap
`default` is no longer auto-applied (the server should not send one; the
client should still not act on one from an older server).

**Tests.** Add to the `tracks.rs` matrix: HDR base + default-flagged English
PGS + English SRT → SRT; HDR base + forced PGS only → `None`; SDR base +
forced PGS → PGS; overlay on + HDR + PGS → PGS; Always mode + only ASS →
`None`; anime `prefer_original` + default PGS + SRT → SRT. Add an HTTP
regression in `stream.rs` tests that `/decision` (no `?subtitle=` — the
`selection` block is only present when a choice was sent, `stream.rs:2138`)
on an HDR fixture with a default-flagged PGS track returns **no subtitle DTO
with `default: true`**, and that with `?subtitle=<pgs>` it returns
`subtitle_burn_in_blocked_by_hdr: true` (a non-optional bool, `:665`) — the
manual pick still gets the honest answer.
Prove the tests reject the old code: run them once with the predicate
short-circuited to `true` and record the failures in the PR.

**Docs.** PLAYBACK.md row `server.track-selection` (`:102`) gains the
eligibility sentence; `docs/FEATURES.md` playback defaults paragraph (`:382`).

**Acceptance.** `cargo test -p plurx-core tracks` and `cargo test -p plurxd
decision` green; the PR body quotes the pre-fix failing run.

## 4. M2 — one HDR burn guard, judged against the delivery without the burn

**Defect.** Two predicates disagree. `/decision` exempts every transcode even
when the negotiated grade is HDR10 (`stream.rs:952`), which is wrong when the
burn would change that grade; create keys on the *source* flag (`hls.rs:1334`)
and refuses an already-tone-mapped session unless the client sends an ack that
web never sends and Apple computes from the wrong range. The M6 preparation
path (`hls.rs:7970-7982`) runs no guard at all.

**Contract.** One function, one question — *would adding this burn change the
dynamic range the viewer is otherwise getting?*

```rust
// crates/plurx-core/src/playback/mod.rs — new, next to Decision
pub fn burn_would_discard_hdr(base_delivered_range: &str, requires_burn: bool) -> bool {
    requires_burn && matches!(base_delivered_range, "dolby_vision" | "hdr10" | "hlg")
}
```

where `base_delivered_range` is the grade of the plan **with no subtitle
burn** — never the grade of the resulting burn session, because that session
is SDR *because of* the burn.

| Case | base range (no burn) | Verdict |
|---|---|---|
| HDR copy / remux / direct | dolby_vision · hdr10 · hlg | refuse |
| HDR source, transcode negotiated to HDR10 (M4 grade) | hdr10 | refuse — the burn would drop the grade |
| HDR source, transcode already tone-mapped (display `hdr=0`, height rung on SDR display) | sdr | allow |
| SDR source, any method | sdr | allow |

**Changes.**

1. `/decision`: `subtitle_burn_would_discard_hdr` becomes a call to the new
   function with `decision.delivered_dynamic_range` as read *before*
   `apply_selected_subtitle` mutates it (§2.2 ordering). Delete the
   `method != Transcode` term and rewrite the comment: the M4 grade is exactly
   why the transcode exemption is wrong now.
2. Create (`hls.rs:1820`): replace `hdr_subtitle_burn_is_refused(source, …)`
   with the grade the session would resolve to without `subtitle_burn`.
   **Where that grade lives:** `resolve_plan` (`hls.rs:1583`) deliberately
   does *not* answer it (its doc at `:1577-1582`; `ResolvedPlan` says "do not
   grow a second resolver"). The grade is decided by
   `TranscodeManager::encoder_and_grade_for` (`transcode.rs:13568`) →
   `hdr10_grade_for` (`:13466`; `:13487` is the `if subtitle_burn` line that
   forces `Sdr`), reached only from `start` (`:19367`), the offer path
   (`:12682`) and VOD (`:17314`) — all after the create guard. So: factor the
   decision inside `encoder_and_grade_for` into a `pub(crate) async fn
   grade_preview(&self, file, requested_hdr10, target_height, subtitle_burn:
   Option<i64>) -> OutputGrade` that `encoder_and_grade_for` itself calls (one
   resolver, two callers — not a second one), and call it from the create
   handler **after** `resolve_plan` with `subtitle_burn: None`. That moves the
   guard later than `:1820`, past durable-intent fingerprinting; confirm no
   accounting or encoder is created between the old and new positions, and
   flag it in the PR. Do **not** use `Decision.transcode_grade`
   (`playback/mod.rs:718`) for this — it ignores encoder proof and can say
   HDR10 where `start` will deliver SDR, so the 201-`sdr` test row would be
   judged by a different function than the one that delivers.
   Keep the 422 code and message.
3. `subtitle_burn_sdr` stays on the wire for old clients but is **no longer
   load-bearing**: log it, do not consult it. The M6 preparation path
   (`hls.rs:7970-7982`) never ran the guard at all — it calls `resolve_plan`
   directly, and the `Some(!requested_hdr10)` at `:7945` had no reader on that
   path — so run `grade_preview` + `burn_would_discard_hdr` on the preparation
   candidate too, refusing (not silently encoding) a candidate that would
   discard HDR. Drop the `:7945` assignment.
4. Client guards need no logic change — each already reads the *current*
   delivered range, which is the no-burn base while no burn is active. Apple's
   ack computation (`:3839-3842`) and Android's become dead code; delete them
   in the client milestone (§6) rather than here.

**Tests.** A four-row matrix over the new function; HTTP regressions for the
create path on an HDR fixture: copy → 422; transcode with `hdr=0` display →
201 with `delivered_dynamic_range: "sdr"`; transcode negotiated HDR10 → 422;
and one for the preparation path. Reinstate the old predicate once to show the
HDR10-transcode row fails against it.

**Docs.** PLAYBACK.md rows `server.hdr-subtitle-burn-guard` (`:104`) and the
`/decision` prose at `:417-430`; `docs/API.md:1172`.

**Acceptance.** `cargo test -p plurxd hdr_subtitle` green; the four rows are
named in the PR.

## 5. M3 — the server stops answering "warming" with an empty track

**Defect.** A not-ready segment is a 200 `WEBVTT\n\n`. Players cache it in
memory regardless of `no-store`, and every client's recovery from that state
is broken (§6). Windows are refused past the file midpoint even when the
whole-track extraction has failed, so the second half is empty for good.

**Changes.** Two, in one PR.

1. **Answer within AVPlayer's patience, but answer the truth when you can.**
   The handler must not block: AVPlayer gives a subtitle segment ~2 s and
   stalls the muxed video while it waits (`hls.rs:10501-10504`) — that is why
   nothing awaits an extraction today (`:10513`). So the change is bounded and
   small: when a window flight for this anchor is **already live** (owner
   registry, `subtitles.rs:1466-1530`), poll `read_window` every 100 ms for at
   most `SUBTITLE_SEGMENT_PUBLICATION_WAIT = 1500 ms` and serve the real slice
   if it lands; otherwise fall through as today. This wins only the last
   second of a warm, which is the common case at a window boundary once the
   next window was kicked by the previous segment's request. Everything
   longer is the client retry's job (§6). Separately, when the sidecar state
   is `Failed` (memo) answer 503 `Retry-After: <memo remaining>` instead of a
   fake empty track, **behind a Settings → Developer switch**
   `playback.subtitle_not_ready_503` (default off) whose advisory section
   lists which client builds have been observed to keep playing video through
   a subtitle 503. Measure that per engine (AVPlayer, Media3, hls.js) before
   proposing a default flip; the AVPlayer comment above is the reason the
   switch starts off.
2. **Windows past the midpoint when the whole track cannot answer.**
   `windowing_is_worthwhile(anchor, duration, window)` (`subtitles.rs:266`)
   is pure and is called from inside `warm_vtt_window_with` (`:1455`), not
   the handler, so it gains a fourth argument `whole_track: SidecarState` and
   `warm_window`'s trait/impl signatures carry it; `SidecarState::Warming`
   (`:762`) carries no start time, so add `warming_since: Instant` to the
   `extractions()` entry and expose it through `sidecar_state`. Rule:
   past-midpoint windows are allowed when the whole track is `Failed`, or
   `Warming` for longer than `2 × window_seconds` of wall time. The midpoint
   rule's reason ("don't scan the file twice concurrently") holds while the
   whole-track warm is healthy; it does not hold when that warm is dead or
   crawling.

**Tests.** Extend the `SubtitleSegmentSource` fixtures (`hls.rs:~15900`):
live flight publishes inside 1500 ms → real cues; live flight does not → empty
within ≤ 1600 ms wall (assert the bound — this is the AVPlayer constraint);
no flight → empty immediately; `Failed` with the switch off → empty; `Failed`
with the switch on → 503 + `Retry-After`; past-midpoint demand with whole
track `Failed` → a window flight starts (assert via
`peak_window_flights_for_test`). The existing "cold segment is empty and one
window flight" test changes meaning — rewrite it, don't delete it.

**Docs.** CHEATSHEET/OPERATIONS settings tables gain the switch; PLAYBACK.md
gains a row `server.subtitle-segment-not-ready`.

**Acceptance.** `cargo test -p plurxd subtitle_vtt` green; the measured
extraction time and the per-engine 503 observations are in the PR body (or an
explicit "not measured — switch stays off" line).

## 6. M4 — the readiness retry works on all three clients

Each client's `warming→ready` recovery has a different hole. Fix all three in
one PR per client (three PRs), each with a test at the *engine* level, not
the policy level (assessment §4).

### 6.1 Apple

- `retryNativeSubtitleAfterReadiness` (`:8264`) must fire for an in-place
  native selection. Either set `activeNativeSubtitle` in the `.mediaSelection`
  arm of `selectSubtitle` (`:3143-3147`) or gate the retry on
  `subtitleSelectionRoute(...) == .mediaSelection && selectedSubtitle != nil`.
  Prefer the latter — `activeNativeSubtitle` means "the index the session was
  opened with" elsewhere.
- Delete `playlistAdvertisesNativeSubtitles` and `serverIsLegacy`
  (`:7519-7533`) and the `forceLegacySubtitleBurn` path they gate
  (`:7648-7685`, `:1668`): there are no pre-native servers in the fleet and
  the check can only misfire. A failed native selection becomes a visible
  "could not be turned on" (`:7680`), never a silent burn. Update
  `AppleClientTests.swift:6727-6777`, which pins the stale `?native=1` shape.
- Delete the SDR-ack computation (`:3839-3842`, `:7049-7055`) — M2 made it
  inert.
- **Test at engine level:** an XCTest that loads a local HLS fixture whose
  subtitle segment is served empty first and with cues second, drives the
  readiness edge, and asserts `currentItem.currentMediaSelection` names the
  option **and** `AVPlayerItemLegibleOutput` delivers a cue. The build loop
  exists on the Mac runner (`docs/apple-builds/`, DEVELOPMENT_PIPELINE.md);
  the test is not optional.

### 6.2 Web

- `retryReadyNativeSubtitle` (`:8907`) must make hls.js fetch the current
  subtitle fragments again. `subtitleTrack=-1; =n` does not evict fragments
  the subtitle stream controller already recorded. Candidate mechanisms, to
  be tried in that order against the bundled hls.js 1.6.16 and the first one
  that passes the test below wins: (a) `hls.subtitleDisplay=false; true`
  around the reselect; (b) switch to a second rendition and back when one
  exists; (c) re-issue `hls.loadSource` is **not** acceptable (video restart).
  If none passes, add a `?g=<generation>` query to subtitle rendition URIs
  in the master (server change, one line in `master_playlist_with_shape`)
  and bump it on the readiness edge via a new control-wire field — flag this
  before building it.
- Fix the two re-apply holes: `hls.subtitleTrack=n` before the rendition list
  loads is dropped — apply on `SUBTITLE_TRACKS_UPDATED` when
  `curSub>=0`; and `adoptPlaybackMediaElement` (`:8386`) must not null
  `_subOff` (its comment claims the opposite of what the code does at
  `:12779`). Forced native tracks currently rely on nothing (`DEFAULT=NO`);
  the tracks-updated hook covers them.
- **Test at engine level:** a Playwright case under `tests/playback/` that
  runs the shipped `index.html` player against a stub HLS server (empty
  segment, then cues) and asserts `video.textTracks[n].activeCues.length > 0`
  after the readiness edge. `web-control.test.js:283` (mocked setter) stays
  as the policy test; it is not the proof.

### 6.3 Android

- Ship `CLOSED-CAPTIONS=NONE` unconditionally in the master and retire the
  `PLURX_HLS_CLOSED_CAPTIONS_NONE` rung (server, one line + `MasterRungs`
  cleanup; the comment at `hls.rs:11626` already says this is correct HLS
  authoring). This removes the CEA-608 phantom-group hypothesis for both
  Media3 and AVFoundation at the source.
- Then prove or refute the hypothesis: an instrumented test (Media3 test
  utils, TS transcode master without/with the attribute) that
  `textTrackAt(0)` maps to the first rendition. If it was real, the fix above
  already covers it; if not, say so in the PR.
- `retryNativeSubtitleAfterReadiness` (`Controller.kt:2244-2268`): the
  disable-then-re-arm must not leave text disabled when the re-arm predicate
  fails — re-arm unconditionally for the same `selectedSubtitle`, or don't
  disable first.
- Surface `subtitle_readiness == "unavailable"` (`PlaybackControlReporter.kt:672`)
  as the same transient notice the HDR guard uses; a memoised extraction
  failure is not "warming".
- Delete the SDR-ack computation (M2 made it inert).
- **Test at engine level:** a Robolectric/instrumented test with a fake HLS
  source asserting cues are delivered after the readiness edge.

**Acceptance (all three).** Each PR carries its engine-level test green in
CI (`ci-apple`, `web-check` + the new Playwright lane, `ci-android`), and the
version bumps `validation/mobile_versions.py` demands.

## 7. M5 — Apple keeps the viewer's subtitle across a prepared handoff

**Defect.** `commitPreparedSuccessor` (`:8659`) replaces the item and never
calls `reconcileNativeMediaSelections`; the successor was primed on a
separate `AVPlayer` (`:8454`) whose `appliesMediaSelectionCriteriaAutomatically`
is left at its default `true`, so the transferred item carries whatever that
player chose, not what the viewer chose.

**Changes.** Set `appliesMediaSelectionCriteriaAutomatically = false` on the
priming player at `:8454` (same as the incumbent, `:2561`); after
`replaceCurrentItem` in `commitPreparedSuccessor`, `await
reconcileNativeMediaSelections(to: item)` under the same lifecycle guards
`open()` uses at `:4089`; and reconcile Off explicitly (select `nil` on the
legible group) — Off is a selection too.

**Tests.** Four cases, each asserting `item.currentMediaSelection` after
commit **and** a cue observation: forced native · non-forced native ·
alternate-language native · Off. Run them with the reconcile call removed
once to show all four fail.

**Docs.** PLAYBACK.md row `apple.subtitle-route` (`:143`) gains "survives a
prepared handoff".

**Acceptance.** `ci-apple` green with the four cases named in the PR.

## 8. M6 — routing and classification agree everywhere

Small, mostly deletions; one PR per client plus one server PR.

- **Android:** in `subtitleRoute` (`SubtitlePolicy.kt:111`) test
  `planMode == "direct"` *before* `!track.isNativeHls`, so an embedded
  ASS/`mov_text` track on direct play stays in the plan as
  CLIENTS-REMEDIATION-PLAN.md:568-570 says. Bitmap tracks on direct play
  still burn (Media3 does not render PGS from the container reliably — keep
  that arm). Rewrite `bitmapAndStyledTracksBurnInEveryMode`
  (`SubtitlePolicyTest.kt:107-122`), which pins the wrong order.
- **Apple:** confirm whether AVPlayer renders `mov_text` from a direct-play
  MP4; if it does, mirror the Android order for that codec. If not, leave the
  route and say so in the PR.
- **Server:** `/decision`'s `selection` block gains
  `subtitle_route: "native" | "overlay" | "sidecar" | "burn"` computed from one
  function next to `subtitle_requires_burn_in`, so no client re-derives the
  classification from `text`/`native`/`overlay`. Clients may adopt it later;
  this PR only adds and documents it (PLAYBACK.md `:512-532`).
- **Docs:** `docs/FEATURES.md:407` ("SRT/ASS… native track") contradicts
  PLAYBACK.md `:520-530`; fix FEATURES.

**Acceptance.** JVM `SubtitlePolicyTest` green with the rewritten case; the
new `/decision` field has a Rust unit over all four routes.

## 9. Non-goals — do not do these

- **No "burn to SDR anyway" override.** Decided against 2026-08
  (`docs/archive/retro-2026-08-09/BELOW-THE-LINE.md:25-28`). If a milestone
  seems to need it, flag it; the owner decides.
- **No lossy ASS/SSA/`mov_text` → WebVTT renditions.** The exclusion at
  `tracks.rs:192-194` is deliberate (positioning, typefaces, karaoke). M1
  makes those tracks ineligible as *defaults*; it does not change how a
  manual pick is delivered. A separate decision.
- **No PGS overlay M4/M5 here.** Enabling `subtitles.pgs_overlay` on a node,
  running `APPLE-PGS-OVERLAY-ACCEPTANCE.md`, and building auto-select of an
  overlay is the follow-on plan; it is the only route that restores PGS on
  DV/HDR and is worth doing next, not inside this handoff.
- **No deploys.** The owner deploys to the nodes. Each PR ends merged to
  `main` with its version bumps; the deploy prompt for the GPT session is the
  owner's to send.
- **No code-gated features.** The one operator choice (§5's 503 switch) is a
  Settings → Developer switch with advisory readiness, never a flag in code.
- **Do not widen scope into seek coalescing, windows ownership or M6
  preparation semantics** beyond the exact lines named; those have their own
  handoffs (`docs/playback-control/M7-R-M3-CLAUDE-HANDOFF.md`,
  `M6-CALLER-HANDOFF.md`).

## 10. Working rules for this handoff

- **PR lifecycle:** proper commits · fast local lane only until the PR is
  together · open as `WIP:` draft · adversarial agent review · implement
  findings · run the full suite **once** · fix until green · merge it
  yourself. Docs live in the same commit as the behaviour they describe.
- **Every regression test is run once against the old code** and the PR
  body quotes that failing run (assessment §4 is the reason: the baseline is
  green at the policy level while all three symptoms exist).
- **Client changes bump builds** per `validation/mobile_versions.py`, and the
  release notes go in `docs/apple-builds/` as the existing pattern shows.
- **Verify on hardware you cannot reach by writing the prompt**, not by
  skipping it: each client PR appends a section to one physical-verification
  prompt in `docs/clients/` (`SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT`,
  created by the first client PR and indexed then) naming the title, the
  client build, the steps, and what a pass looks like, for the GPT session
  that has the devices.
- **Status page:** add a `Subtitle reliability` block to `docs/STATUS.html`
  with one row per milestone (§3–§8), updated in each PR.
- **Docs index:** this handoff and the assessment are rows in
  `docs/README.md` (added in the PR that introduced them). Any new document a
  milestone creates — the physical-verification prompt above included — gets
  its row in the same commit, or `tests/operations/test_docs_index.py` fails.

## 11. Order and dependencies

```
 M1 server default ──┐
 M2 server guard  ───┼──▶ M4 client retries (3 PRs) ──▶ M5 Apple handoff
 M3 server warming ──┘              │
                                    └──▶ M6 routing (4 PRs)
```

M1–M3 are independent of each other and can be built in parallel; M4 waits
for M2 (the ack deletions) and M3 (the 503 switch's client observations);
M5 and M6 wait only for M4's Apple/Android PRs so build numbers do not
collide. Eleven PRs; the first three are the ones that change what a viewer
sees most.
