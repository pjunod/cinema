# Watch and browse — implementation and acceptance record

**Status:** open · **Design:** Fable v4 document approval, 2026-09-19 ·
**Initial implementation base:** `0f1e5e43a5726817d7a2d83add06ecbb40a6942f` ·
**Effort:** `effort/watch-and-browse` ·
**Review:** [PR #383](http://192.168.4.7:3000/noirr/plurx/pulls/383), unmerged

Companion to [PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md) (what inputs
mean), [WEB-SHELL-LAYOUT.md](WEB-SHELL-LAYOUT.md) (served source order), and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (integration gates).
This records the approved web slice, its implementation, and the acceptance
that still separates a development branch from a releasable feature.

## 1. Scope and source pin

The approved v4 proposal gives movie and episode playback the Live TV pattern:
a compact picture beside title information, browsing below, a content-width
Larger mode, and browser fullscreen that returns to the same browser. Home
and the stopped item-page bodies stay as they were restored before this work.
Native layouts are explicitly deferred behind separately reviewed owner
designs. Shared native routing vocabulary is part of this slice; native view
lifetime changes are not.

The proposal was audited against `0c8214f5`. Implementation was re-pinned to
`0f1e5e43` before editing. The intervening subtitle reliability changes remain:
late subtitle-track arrival, ready native sidecars, and subtitle reapplication
when a prepared replacement adopts its media element. The retained web host
must not interfere with those legitimate media replacements.

Three prerequisite errors were reproduced on this base: a stray `f` before
a comment in `directed-change.js` threw during asset loading; three Android
subtitle tests omitted the newly required delivered-range argument; a Rust
subtitle test used a Boolean `assert_eq!`, rejected by pinned Clippy. Their
small corrections preserve the subtitle behavior and have separate evidence.
The Rust 1.97.1 compiler loop was established before source changes.
The final containment check also found two pre-existing fractional grids
(Activity and playback stats) without a zero floor; both now use minmax(0,1fr).

Merge-readiness follow-up: current main `15324dd9a52367e09b5fe18e34b666866738f25e`
was integrated into the task candidate. Its Rust workspace and both Apple
clients compile; Android compile/routing/subtitle tests and fine/coarse watch
browser acceptance pass again. Apple build is now 173 and Android versionCode
110. The first hosted effort run (2303) failed on the regression ledger.
The ledger anchors and manual-dispatch scope are corrected; the complete
web gate and a fresh hosted effort result are still pending. Earlier focused
evidence below is not a substitute for that remaining gate.

## 2. Task ownership and integration

These files overlap across milestones, so the disjoint-file exception does
not apply. One web integration task carries M0–M3 in ordered commits into the
effort. Future native tasks branch from the then-current effort after their
owner designs have been reviewed. No task merges directly to main.

| Task | Files owned | Completion evidence |
|---|---|---|
| Prerequisites | `player/directed-change.js`, the Boolean assertion in `http/hls.rs`, missing arguments in Android `SubtitlePolicyTest.kt` | Served asset load; pinned Clippy; subtitle fixture tests |
| M0–M3 web slice | `web/player/watch-presentation.js`, `web/detail/watch-browser.js`, `web/detail/preplay-selection.js`, `web/player/{decode-tiers,projection-chrome,transport,autoplay-next,stats}.js`, `web/router.js`, `web/index.html`, `web/app.css`, `http/web.rs`; shared input fixture/generator/policy, input contract documentation, Apple/Kotlin input reducers and their outcome consumers/tests; focused web tests and this document/index | Shared matrix, native compile/tests, web integration browser, asset and ownership fences |
| Apple owner design, then layout | A separately reviewed design must name the actual retained-owner, detail, player and surface files at that task's base | Owner approval and physical layer/fullscreen/PiP/rotation evidence before layout acceptance |
| Android owner design, then layout | A separately reviewed design must name retained-controller, navigation, PlayerView and adapter files | Owner approval and physical surface/fullscreen/PiP/rotation evidence before layout acceptance |
| Promotion | Evidence records and any integration corrections identified against current main | Effort development gate; frozen current candidate; Main promotion gate and qualification receipt |

`web/` and `http/` above are under `crates/plurxd/src/`. Native file paths
are the existing platform source trees. Native layout ownership is deliberately
not fabricated before its prerequisite design exists.

## 3. Implemented behavior

`WATCH` holds presentation, selected browse state, accepted item identity,
request generations, and a ResizeObserver. It does not own a decoder, HLS
instance, playback session, recovery policy, or progress timer. Existing
`PLAYER`, `play`, and the Playback Surface presenter retain those jobs.

The persistent `#modal/#player/#video` tree remains outside replaceable
`#app`. In watch mode it is a nonmodal region following the page's empty
`#watch-slot`, with viewport clipping when the slot scrolls offscreen. Compact
stacks below 760 px; at 550 px and below the picture stays 16:9 and controls
occupy two 44 px rows below the timeline. The 56 px caption opens source-file
facts, and the next episode's Play button replaces the omitted phone Play
next caption action. Coarse pointers receive 44 px targets at tablet widths
as well. The size preference is `plurx_watch_player_size=compact|wide`.

Larger only changes geometry. Fullscreen uses the existing browser APIs on
`#player`, retaining the Safari video-only fallback. `fullscreenchange`
restores size, scroll and focus. Slotless finite playback uses page-filling
full mode without automatically requesting browser fullscreen; its return
outcome exits through the existing owner and preserves Library Channel return
context. There is no VOD dock or sticky picture.

Episode details and season selection do not play. A separate Play/Resume
action runs the full item loader before `play`; its current-request guard
runs before shared preplay/file-map mutations. Only an accepted playback
attachment changes the playing marker and replaces the current history entry.
Play next uses the existing cross-season lookup and the same in-place action.
Current-episode Play is disabled in both the browser and details sheet. Row
and card views share those actions. The browser selection survives a playing
item change.

Movie chapters use the selected file's existing chapter DTOs. Clicks call
`seekTo`; position updates only change the current marker. Empty chapter
lists omit the lower section. Missing art gets a simple placeholder. Source
facts reuse `specBlock`; delivered playback information retains the existing
stats surface. Compact title information is always visible; wide title info
opens a contained dialog; fullscreen retains the existing overlay preference.

Close synchronously invalidates open attempts and tears down through the
existing owner once. It waits for the outgoing final progress attempt to
settle before re-reading the last accepted item. Route leave starts the same
finalization without a close-to-item refresh. A new playback or route invalidates
any pending Close repaint/focus restoration. No fixed refresh timer remains.

The shared input fixture adds presentation and chrome placement around the
seven existing interaction states. Web remains desktop at every width.
Browser root Back, including failure, ignores on desktop; fullscreen returns
to the retained browser. Local scrub/menu/info interactions resolve first.
Inline idle ignores; overlay idle retains the playing/state exclusions.
Native reducers transcribe the same watch table, and adapters explicitly
fall back to exit when no retained native browser exists. Their normal
presentation and owner lifetime remain unchanged.

## 4. Reproduce the focused checks

Use the repository-pinned compiler explicitly on hosts whose default Cargo
is a different version:

```bash
rustup run 1.97.1 cargo check -p plurxd --locked --all-targets
rustup run 1.97.1 cargo clippy --workspace --all-targets -- -D warnings
rustup run 1.97.1 cargo test --locked -p plurxd --bin plurxd http::web::tests -- --test-threads=1
node scripts/player-contract-table --embed
node scripts/player-contract-table --write
node tests/playback/player-input-contract.test.js
node tests/playback/web-policy.test.js
node tests/web/asset-load.test.js
node tests/web/asset-order.test.js
node tests/web/asset-layout.test.js
node tests/web/player-dom.test.js
node tests/web/calm-library.test.js
node tests/web/nav-keyboard.test.js
node tests/web/library-channels.test.js
node tests/web/live-tv.test.js
python3 scripts/player-input-fence
python3 scripts/playback-surface-fence
python3 -m unittest tests.operations.test_docs_index tests.operations.test_player_input_fence tests.operations.test_playback_surface_fence
```

The two generator invocations are separate: this generator returns after
`--embed`, so combining both flags does not regenerate documentation.

The browser regression loads the actual served assets and uses real Chromium
media elements with an intercepted fixture API. It needs Playwright on Node's
module path and an H.264 MP4 at least two minutes long:

```bash
ffmpeg -f lavfi -i 'testsrc2=size=640x360:rate=12' -t 120 -an \
  -c:v libx264 -preset ultrafast -crf 35 -pix_fmt yuv420p \
  -movflags +faststart /tmp/watch-fixture.mp4
WATCH_MEDIA=/tmp/watch-fixture.mp4 node tests/web/watch-and-browse.browser.cjs
WATCH_MEDIA=/tmp/watch-fixture.mp4 WATCH_COARSE=1 node tests/web/watch-and-browse.browser.cjs
```

Set `WATCH_SCREENSHOTS` to an output directory for actual app captures.
Fixture playback proves browser mechanics; it does not prove production
HLS, Safari, physical remote focus, or native surface continuity.

Apple: generate `clients/apple/project.yml`, build both `plurx-iOS` and
`plurx-tvOS`, and run `AppleClientTests`' shared input and watch presentation
fixture tests. Android: run `:app:testDebugUnitTest` filtered to
`PlayerInputPolicyTest` and `SubtitlePolicyTest`, plus `:app:assembleDebug`.
Use the affected compile and version requirements of the current effort gate.

## 5. Acceptance ledger

| Evidence | State |
|---|---|
| Rust pinned compiler and workspace Clippy | Passed locally |
| Rust served asset tests | 10 passed locally |
| Shared web routing fixture and generated documentation | Passed locally; all watch cells and unmounted return fallback |
| Existing web policy regressions | Passed locally |
| Asset order/load/map, static player DOM, restored Home/item layout | Passed locally |
| Navigation, Library Channels, Live TV, grid containment and ownership/document fences | Passed locally |
| iOS and tvOS simulator builds | Passed locally |
| Apple shared routing and watch matrix | 2 focused tests passed on iPhone 17 Pro simulator |
| Android shared routing/subtitle fixtures and debug build | Passed locally |
| Chromium fine/coarse browser acceptance | Passed locally in both contexts: 11 widths, 20 compact/wide cycles, browser fullscreen return, stale loads, accepted/failed episodes, Play next, final save ordering, chapters and slotless exit |
| General page-read-budget test | Initial base failed; current-main integration carries its fixes. Complete web gate still pending |
| Real server/media, Chrome and Safari | Pending |
| Physical remote focus and native owner/layer continuity | Pending; native layout gate remains closed |
| Effort/Main gates and qualification receipt | Pending on PR #383; no release or deployment claim |

Real-media acceptance must cover a movie and series, playing and paused,
loading/failure, audio/subtitles/quality changes, and at least twenty repeated
compact/wide/fullscreen returns per browser. Record session/open/attachment
counts and media position around geometry changes. A prepared quality handoff
may legitimately adopt new media; geometry alone may not. Verify Browser Back,
Close after an episode sequence, interrupted/failed model loads, seek gestures,
first-frame subtitles, and source-versus-delivery facts. Safari browser and
video-only fullscreen need real execution; synthetic events cannot count.

## 6. Native gate and promotion

Fable's approved §7 requires, before any native layout PR, a **separately
reviewed one-page owner design per client**. Each design must identify owner
creation/replacement/end, explicit exits versus child disappearance, one
presenter/input adapter, one Apple PlayerSurface/AVPlayerLayer attachment or
Android PlayerView, and measured fullscreen/PiP/rotation continuity. Keeping
an AVPlayer reference alone is not evidence that layer re-hosting is seamless.
No native layout acceptance or gate waiver is implied by the passing M0 tests.

After the web task's focused evidence is complete, commit normally with the
tracked hook, submit into the effort, and require the Effort development gate.
Before main promotion, freeze task integration, merge then-current main,
requalify the exact candidate, and require the Main promotion gate and receipt.
A moved base invalidates earlier candidate evidence. Deployment is a separate
authorized action.
