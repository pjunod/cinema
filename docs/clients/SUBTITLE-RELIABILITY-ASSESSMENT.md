# Subtitle reliability assessment — 2026-09-16 (v2, revised after review)

**Status:** done · **Reconciled:** 2026-09-20

**Baseline:** origin/main `c9e4edf451e12247a7aa4188903e5ba36888e7e9`. Every anchor is file:line in that commit. This is a source review; no device playback, no test execution, no client-build inventory. v1 overstated several conclusions; the review's R1–R5 are applied below and each claim is now labelled **[confirmed in source]**, **[runtime hypothesis]** or **[not verified]**.

## 0. What changed from v1

- R1: the `/decision` transcode exemption is no longer presented as the correct model for the guard; §3.5 now states the contract as "judge against the delivery *without* the burn".
- R2: the Apple successor finding is downgraded from "every handoff loses subtitles" to "selection is not explicitly reconciled and is not guaranteed to survive"; the successor `AVPlayer` is built with automatic media selection at its default (`true`), an extra uncertainty.
- R3: "the window runs out and subtitles vanish for the rest of the film" is replaced by the three actual paths (boundary before midpoint, demand past midpoint, whole-track failure), each with its own confidence.
- R4: `tests/playback/web-policy.test.js` **does** exist at the baseline (v1's snapshot omitted `tests/`); `web-control.test.js:283` asserts the retry's `[-1, n]` writes through a mocked setter. Test-debt statement rewritten.
- R5: source status, feature enablement, deployed versions and device acceptance are now separate facts; the "half the library" frequency explanation is labelled a hypothesis.

## 1. Status — four separate facts

**Merged in source [confirmed]:** Apple native text subtitles M1–M4; Android §5.4; server-side default selection with client veto; HDR burn guard on all three clients plus server 422 `hdr_subtitle_burn_refused`; M7 readiness / bounded windows / seek coalescing / burn-join (#741 #742 #754 #794 #789 #830); PGS overlay M1–M3.

**Feature enablement [confirmed]:** PGS overlay is behind `subtitles.pgs_overlay`, default off (`crates/plurxd/src/state.rs:731-736`). `PLURX_HLS_CLOSED_CAPTIONS_NONE` and `PLURX_HLS_FORCED_AUTOSELECT` are read-once env rungs, off (`hls.rs:11398-11417`). PGS overlay M4 (auto-select an overlay) and M5 have no implementation in the tree.

**Deployed server versions [confirmed, timestamped 2026-09-16 ~20:15 UTC via `/api/v1/server`]:** nynuc and m6 report build `v0.3.0-2633-gc9e4edf4` (built 19:37 UTC); nuc4 reports `v0.3.0-2609-g74bd6631` (built 15:36 UTC); nuc3 is a learner and does not answer that route. No subtitle-related commit lies between `74bd6631` and `c9e4edf4`.

**Deployed client builds [not verified]:** which Apple / Android builds are installed on the physical devices was not checked. Everything below about client behaviour describes the source at the baseline, not necessarily what is on the Apple TV.

**Device acceptance [not verified]:** the plan docs record no result for the Apple M5 matrix, Android §5.4/§9.4, PGS M0/M2/M3, or R-M2 directed retry. Absence of a recorded result is not proof the runs never happened; it is proof the tree carries no evidence of them.

## 2. The three symptoms

### 2.1 "That subtitle requires an SDR burn-in. HDR playback was kept unchanged."

**The server's default pick is codec-blind [confirmed].** `select_tracks` (`crates/plurx-core/src/tracks.rs:241-302`) chooses by language / forced / default flags only; `/decision` stamps `default:true` on the pick (`http/stream.rs:2129-2132`). `forced_or_default` (`tracks.rs:224-236`) will select a default-flagged or forced PGS track in the viewer's language.

**Web auto-applies that default with no user action [confirmed].** `index.html:10237-10242` schedules `setSub(defSub.index)` 400 ms after play when the method is not transcode; `setSub` runs the guard (`:13865-13872`) and raises the notice. Apple (`PlayerController.swift:3580`) and Android (`Controller.kt:206-208`) drop the auto-pick silently and raise the notice only on a user pick.

**Frequency [hypothesis].** v1 said this explains "half the time" because half the library is HDR remuxes with a default-flagged PGS track. No library query supports that; it is the leading hypothesis and is checkable with one catalogue query (HDR titles × subtitle tracks where `default || forced` and codec is bitmap).

Amplifiers, each **[confirmed in source]**:

1. **Two definitions of "requires burn".** `is_native_text_subtitle` is `subrip|srt|webvtt|vtt` only (`tracks.rs:195-200`), so ASS/SSA/mov_text are `native:false`; `/decision`'s `subtitle_requires_burn_in` uses `is_bitmap_subtitle` (`stream.rs:843-856`), so those same tracks report `requires_burn_in:false`. Apple (`PlayerController.swift:7359-7363`) and Android (`SubtitlePolicy.kt:133`) route on `native` → burn → HDR guard; Android tests `!isNativeHls` before the `planMode == "direct"` arm (`:137`), so an embedded ASS/mov_text track on direct play is routed to burn, contrary to `CLIENTS-REMEDIATION-PLAN.md:568-570`. Note: the ASS exclusion from native renditions is deliberate (positioning, typefaces, karaoke are lost in WebVTT — `tracks.rs:192-194`); fixing the metadata inconsistency does not by itself decide that tradeoff (§3.4).
2. **Server create-time guard keys on the source, not the delivery.** `hdr_subtitle_burn_is_refused` (`hls.rs:1334-1343`) refuses any `subtitle_burn` on a file whose probe says HDR unless `subtitle_burn_sdr:true`. Web never sends that flag (absent from `index.html`), so every web burn on an HDR source is refused with this sentence via `failPreparation`. Apple computes the ack from the decision's range (`PlayerController.swift:3839-3842`) rather than the session's delivered range (`:1957-1965`).
3. **Apple legacy-server detection is stale.** `playlistAdvertisesNativeSubtitles` requires a `native` query item (`PlayerController.swift:7530-7533`); the server now returns `/api/v1/hls/{id}/master.m3u8[?subtitle=N]` (`hls.rs:2533-2546`). `serverServesNativeSubtitles` is therefore always false, and any failure to load the legible group trips `recoverFromFailedNativeSelection` (`:7648-7685`), which sets sticky `forceLegacySubtitleBurn` and reopens an SRT as a `subtitle_burn` — refused on HDR with this message, silently re-encoding on SDR. `AppleClientTests.swift:6727-6777` pins the old `?native=1` shape.

### 2.2 Enabled, nothing shows, no error

**Server answers "not ready" with a valid empty track [confirmed].** A subtitle segment whose whole-track or window sidecar is absent returns 200 `WEBVTT\n\n` `no-store` (`hls.rs:10574-10588`); a failed extraction is memoised 120 s and answered the same way (`subtitles.rs:44`, `1121-1129`). `subtitle_readiness` reports `warming` for `Absent` as well as `Warming` (`hls.rs:4650-4652`). The `warming→ready` control edge is the designed recovery; the client half of that recovery has a defect on each client:

- **Apple [confirmed]:** `retryNativeSubtitleAfterReadiness` requires `activeNativeSubtitle != nil` (`PlayerController.swift:8264-8268`); that field is assigned only inside `open()` (`:3797`, `:3943`) and reset at `:3409`. An in-place selection from Off — the normal case under the default `Instant` readiness — never arms it.
- **Web [confirmed in source; engine behaviour is a hypothesis]:** `retryReadyNativeSubtitle` writes `subtitleTrack=-1` then `=n` (`index.html:8912-8913`). `tests/playback/web-control.test.js:283` proves those two writes happen through a mocked setter; it does not instantiate hls.js. Whether hls.js 1.6.16 re-fetches fragments it has already recorded as buffered on a same-track reselect is the open question — reading the bundled subtitle-stream-controller suggests it does not (bookkeeping is reset on media detach or a changed track list), but that needs a runtime regression, not a source read.
- **Web, additionally [confirmed]:** `hls.subtitleTrack=n` set before the rendition list has loaded is discarded by hls.js; `_subOff` is then set (`:13899`) so `pbTick` (`:12779`) never retries; `DEFAULT=YES` rescues only non-forced tracks (`hls.rs:11538`). `adoptPlaybackMediaElement` sets `_subOff=null` (`:8386`), which disables the re-apply the comment says it triggers.
- **Android [runtime hypothesis]:** `textTrackAt` flattens all `TRACK_TYPE_TEXT` groups in `currentTracks` order (`Controller.kt:1993-2003`). On a TS transcode session, Media3 with `exposeCea608WhenMissingDeclarations` may synthesise a CEA-608 group when the master lacks `CLOSED-CAPTIONS=NONE` (`hls.rs:11414, 11630`), shifting every ordinal by one. Consistent with the code; not reproduced.

Also **[confirmed]**: a prepared successor is staged with `native_subtitles = Some(native_subtitle.is_some())` (`hls.rs:7946`); if subtitles were Off when staged, the successor's master carries no `SUBTITLES` group and a later in-place selection has nothing to select (`SubtitlePolicy.kt:150` routes NativeSession→NativeSession with `reopen=false`).

### 2.3 Displaying, then stops, still selected

**Apple prepared-successor commit does not reconcile the selection [confirmed]; loss on every handoff [not established].** `commitPreparedSuccessor` calls `player.replaceCurrentItem(with:)` (`PlayerController.swift:8659`) and returns without `reconcileNativeMediaSelections`, which `open()` (`:4089`), failover (`:6520`) and native seek (`:2983`) all call. The incumbent has `appliesMediaSelectionCriteriaAutomatically = false` (`:2561`, `:2613`); but the successor is primed on a separate `AVPlayer(playerItem:)` (`:8454`) whose automatic selection is left at Apple's default of `true`, so the item may carry an automatically-chosen legible selection (or none) when it is transferred. The honest statement: the viewer's selected track or Off state is not guaranteed to survive the handoff, and what it becomes depends on the priming player's automatic choice. Repair candidate: reconcile after `replaceCurrentItem`; validate forced, non-forced, alternate-language and Off by reading the item's `currentMediaSelection` and observing cues before claiming it fixed.

**Bounded windows [three paths, separate confidence].** Windows are anchored on a grid of `SUBTITLE_WINDOW_SECS` (default 200; `subtitles.rs:246-249`) and the segment handler derives the demand's anchor and may `warm_window` for it (`hls.rs:10460`, `:10555`) — so the server does request successive windows; one window ending is not by itself the end of subtitles.

- *Boundary before the midpoint* **[confirmed mechanism, gap length is a hypothesis]:** the first segments past a window boundary are served empty while the next window warms; players that cache the empty segment (AVPlayer per the retry's own comment at `:8261-8263`; hls.js/Media3 per the hypotheses above) show no cues for those segments until the readiness retry — which is broken as in §2.2 — refetches them.
- *Demand past the midpoint* **[confirmed]:** `windowing_is_worthwhile` refuses any window whose anchor×2 ≥ duration (`subtitles.rs:266-277`). Past the midpoint only the whole-track sidecar can answer.
- *Whole-track extraction fails or times out* **[confirmed mechanism; frequency unknown]:** 600 s timeout (`subtitles.rs:34`), empty/>8 MiB rejected (`:1752-1762`), failure memoised 120 s. With no whole-track sidecar and no window past the midpoint, the second half of the film is served empty. Whether real NAS remuxes hit the 600 s bound is unmeasured (PGS M0 recorded a 179 s / 600 s case on a 25 GB episode — a different extractor).

Lower-confidence contributors **[hypothesis]**: web fetch-path cue offset uses `start_seconds` (`index.html:9259`) not `media_origin_ms`, so copy sessions may lead by a GOP; Android `retryNativeSubtitleAfterReadiness` disables text first and re-arms only if fence/generation/selection are unchanged (`Controller.kt:2251-2263`).

## 3. Proposed repairs — ordered by leverage, each a proposal

1. **Deliverability-aware server default.** `select_tracks` (or the `/decision` layer above it) should know which tracks are native, overlay-capable, or burn-only and whether the base delivery is HDR, and never stamp `default` on a track no client can show without a refused burn; prefer a native text track in the same language, else none. Removes the web auto-notice and the silent auto-drops. Server-only; fixture-testable.
2. **Apple successor reconcile** after `replaceCurrentItem`, with the four-case validation in §2.3 done against the item's actual selection.
3. **Readiness retry on all three clients:** Apple — arm on any native in-place selection (set `activeNativeSubtitle` there or gate on route instead); web — force a real refetch (detach/re-attach the subtitle track, or bump the rendition URL with a generation query on `warming→ready`) and prove it with a runtime regression that observes cues after an empty segment; Android — audit the re-arm predicate. Ship `CLOSED-CAPTIONS=NONE` unconditionally and retire the env rung so Media3's text-group order is stable (and disprove or confirm the CEA-608 hypothesis on a device).
4. **One classification of "requires burn"** shared by `/decision`, create and the clients; fix the Android route order so direct play keeps the embedded track; delete or repair the Apple `?native=` legacy check (no legacy servers exist). Whether ASS/SSA/mov_text should additionally be offered as lossy WebVTT renditions is a **separate presentation decision** — the current exclusion is deliberate — and is not implied by fixing the metadata.
5. **Unify the HDR guard against the delivery *without* the burn.** The current `/decision` guard (`stream.rs:952-969`) exempts every `PlaybackMethod::Transcode` even when `delivered_dynamic_range` is `hdr10`, and `apply_selected_subtitle` (`:916-948`) then rewrites the range to `sdr` — so judging the resulting burn session is circular. Contract: compute the base plan's delivered range with no subtitle burn; if it is DV/HDR10/HLG, a burn is refused (or, if an explicit override is ever adopted, requires it); if it is already SDR (an HDR source tone-mapped for a height cap, or an SDR source) the burn is allowed. Cases to state and test separately: HDR copy (refuse), HDR10 transcode (refuse — the burn would change the grade), HDR source already tone-mapped to SDR (allow), SDR source (allow). Apply the same predicate at create (`hls.rs:1334-1343`, currently keyed on `file.hdr`), and have web send `subtitle_burn_sdr` only when the base plan is SDR.
6. **Stop serving emptiness as "warming".** Hold the subtitle media playlist's segments until the demanded window exists (or a 503 + `Retry-After` behind clients that handle it) so players never cache an empty segment; allow windows past the midpoint when the whole-track warm has failed.
7. **PGS overlay:** enable `subtitles.pgs_overlay` on one node, run the Apple/Android acceptance procedure (note the procedure's §2 still names the retired env gate), then build M4 auto-select — the only route that restores PGS on DV/HDR under the no-override policy.
8. Run the open hardware matrices (Apple M5, Android §9.4/§5.4, R-M2 directed retry) and record results in the plan docs.

## 4. Test debt — stated precisely

Four levels exist: pure policy tests (e.g. `SubtitlePolicyTest.kt`, Apple guard truth tables, `web-policy.test.js`), calls into mocked engines (`web-control.test.js:283` asserting `[-1, n]` writes on a stubbed `subtitleTrack` setter), actual engine state (AVPlayer `currentMediaSelection`, Media3 `currentTracks`, hls.js buffered-fragment bookkeeping), and visible device playback. At the baseline the first two levels are well covered; **no test at the third or fourth level establishes that a selection results in cues being fetched and shown, or recovered after an empty segment.**

Specific gaps with no test at any level: `/decision` stamping a bitmap default on HDR; create guard vs `/decision` guard disagreement; M6 preparation setting `subtitle_burn_sdr` unconditionally (`hls.rs:7945`) and staging without the rendition group; whole-track failure past midpoint; Apple successor selection after commit; Apple retry gating on `activeNativeSubtitle`; Android `textTrackAt` ordinal mapping on TS sessions; Android direct-play ASS routing vs the plan doc.
