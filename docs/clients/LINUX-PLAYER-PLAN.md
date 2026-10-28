# Linux player — Dolby Vision on Linux through plurx

**Status:** open: M6 (acceptance on the ROG) next. M1–M5 are in (#1295), and package 0.3.1 with its server change fixes the four faults the owner's first real playback found (2026-10-26) · **Owner:** Claude
(Opus 5.5, Linux Dolby Vision session) · **Written:** 2026-10-25

Companion to [HDR-DISPLAY-PEAK.md](HDR-DISPLAY-PEAK.md) (the browser half) and
[the display-targeted HDR10-E build](../streaming/DISPLAY-TARGETED-DV-BUILD.md)
(the server mapping). This is the plan for a native player on Linux. Read §2
first. Work milestone by milestone (§7). If a step seems to need a new client
UI beyond what §4 names, stop and flag it instead.

## 1. Objective

Make Dolby Vision on Linux work as well as it does on the Apple TV:

- P5 and P8 titles reach the machine as their original Dolby Vision stream;
- the player maps each one locally with plurx's closed-form display
  management, at the screen's real target from the compositor;
- Profile 7 FEL titles reach it as their original dual-layer stream, and the
  player composes the enhancement layer on the GPU before mapping (§3.3), so
  nothing is re-encoded.

A browser cannot do this, for three reasons:

- **No luminance:** no browser exposes the display's luminance.
- **No DV decode:** Chrome on Linux does not decode Dolby Vision. Chrome 152
  on the ROG reports `dvh1.08.06` unsupported.
- **No FEL:** nothing in a browser composes FEL.

Chrome stays the browser path. #1280 made it claim HDR correctly, and the
server maps DV for it. It still has the limits above.

Owner decisions (2026-10-25):

- browsing stays in the web app, and a "Play in plurx Player" action hands
  the title to a native player;
- Profile 7 FEL is in v1;
- it ships as an Ubuntu `.deb`.

## 2. Shape — mpv with a plurx launcher, not a new player

```
 plurx web app (Chrome)                 plurx server
   │ Play in plurx Player                  │
   │── POST /playback-handoffs ───────────▶│ one-time code (120 s)
   │◀────────────────────────── code ──────│
   │ navigate plurx-player://play?server=…&code=…
   ▼
 plurx-player launcher (x-scheme-handler, user session)
   │── POST /playback-handoffs/redeem ────▶│ scoped token + item, start, tracks
   │── POST /files/{id}/decision (caps) ──▶│ DV 5/8 preserved, or P7 → P8.1 with FEL composed
   │── POST /files/{id}/hls/sessions ─────▶│ playlist_url (session-authorized)
   │ start mpv (JSON IPC) on playlist_url
   │   every 5 s: POST /hls/{s}/control (lease) · every 10 s: POST /items/{id}/progress
   │   on exit:   DELETE /hls/{s}
   ▼
 mpv (private build: closed-form DM, plurx/dv_dm.h) ── libplacebo (private build)
   └── vo=gpu-next, Wayland wp_color_manager_v1 → GNOME HDR (target from surface feedback)
```

mpv already does HDR correctly on the ROG. It receives the compositor's
preferred description (PQ, BT.2020, target 616 nits, panel primaries) and
presents PQ. It also decodes P5/P8 RPUs through libavcodec and hands them to
libplacebo. That leaves three new pieces:

- the closed-form mapping in that libplacebo/mpv path (§5);
- a launcher that speaks plurx;
- a server handoff.

## 3. Server contract

### 3.1 Playback handoff — a one-time code, never a bearer in a URL

There is no existing way for a web session to give another local app
playback access. API keys refuse user routes. File grants (`purpose:
"open_in"`) are books-only bytes. A `?token=` in an external URL is the
[D5 finding](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md). So add one
narrow mechanism, modelled on `http/file_grants.rs`:

- **Mint:** `POST /api/v1/playback-handoffs`
  - Request: `{file_id, start_ms?, audio?, subtitle?}`, with a user bearer.
  - Response: `{code, expires_in: 120}`.
  - The code is 32 random bytes, base64url, single use, and stored hashed.
- **Redeem:** `POST /api/v1/playback-handoffs/redeem`
  - Request: `{code, device: "plurx Player (Linux)"}`. No bearer.
  - Response: `{token, file_id, start_ms, audio, subtitle, user}`.
- **The redeemed token** is a new `TokenAudience::PlaybackHandoff`, not a
  native user token.
  - It is accepted only on these routes, and only for the handed-off
    `file_id`: decision, HLS create/control/teardown, item progress and
    client-log. It also works on one revoke route,
    `DELETE /api/v1/playback-handoffs/self`.
  - It expires 6 h after redemption. It is also revoked with the minting
    token (as file grants are) and by the launcher on exit.
  - It shows in the account's sessions as "plurx Player (Linux)".
  - Why so narrow: anything can register a desktop scheme handler without
    privilege, so whoever redeems the code must get one title's playback,
    not the account.
- **Status:** `GET /api/v1/playback-handoffs/{id}` (with the minting bearer)
  answers `{redeemed: bool}`. The web app uses it to tell "opened" from "not
  installed" (§4.1).
- **Errors:** a second redemption, an expired code, or a code minted by a
  revoked token answers 410.
- **Why a code and not a token in the URL:** scheme URLs land in shell
  history, `ps` and desktop logs. A 120 s single-use code there is harmless
  once redeemed, and the long-lived credential never leaves the two HTTPS
  bodies.

### 3.2 What the player claims

The caps v2 document follows the Apple template (`Caps.swift`), except where
this table says otherwise.

| Field | Value | Why |
|---|---|---|
| `video[]` HEVC | `profiles:["main","main10"]`, `max_height:2160`, `present:["sdr","pq","hlg"]`, `dv_profiles:[5,7,8]` | mpv/libplacebo reconstruct P5 and P8 from the RPU, and P7 with its full enhancement layer (§3.3). Claiming 7 makes the server deliver the original stream (`dolby_vision_converts_to_p81` is false for a client that lists 7). |
| `video[]` AV1, H.264 | as decoded by the bundled FFmpeg | mpv decodes them in software or with VA-API. |
| `display` | `{hdr:true, dolby_vision:true}`, no `max_nits` | On the preserved route the player maps locally, so the server never needs the peak. Sending none means Unknown, so the server never display-targets this client. Sending a guess would be the over-claim the peak contract forbids. |
| `hls_hevc_sample_entries` | `["hvc1","hev1"]` | The P8.1 route uses `hvc1` plus `dvvC`, and mpv's demuxer reads `dvvC`. |
| `dv_transport` | `"hls"` | The same as Apple. |
| audio / sinks | every codec mpv decodes, `passthrough:false`, `max_channels:8` | mpv decodes and downmixes to the real sink. Reading the PipeWire channel count needs more than the standard library, and passthrough is out of scope. |
| `subtitle_overlays` | `[]` in v1 | Text subtitles load as sidecar tracks. PGS is in §6. |

### 3.3 Profile 7 FEL in v1 — composed in the player

**Changed 2026-10-26 (owner's call).** The server route below re-encodes the
composed picture, and on lab6 and media1 that loses more than the EL adds
for both measured titles (FEL-P81-RECONSTRUCTION.md §7;
`claude/fel-quality-risk-2026-10-26.md` in the project). Players that do
FEL well compose it on the device. plurx Player now does too:

- the caps list Dolby Vision profile 7, so the server delivers the original
  dual-layer stream, by direct play or a copy;
- mpv (master, with libplacebo 7.387's `pl_frame.enhancement_layer`) splits
  the EL out of the HEVC stream, decodes both layers, pairs them by PTS, and
  libplacebo composes the NLQ residual during reconstruction, before the
  closed-form mapping (§5);
- on *Michael*, the player's FEL output matches an independent reference
  (FFmpeg decode of both layers, libplacebo composition,
  `dv_dm_cpu_render`) to ΔE_ITP mean 0.008, max 0.052. Turning the EL off
  moves the picture by ΔE mean 3.4.

The server route remains for clients that cannot compose (Apple, Android,
web), gated per title by M2's qualification.

#### The server route (M2), for clients that cannot compose

The server already holds every piece:

- `fel_renderer.h` composes BL+EL into a PQ master.
- `author_p81.c` and `dv_author_profile81_window` convert each source RPU
  with `To81` and remove the mapping that reconstruction already applied.
  They keep L1/L2/L3/L4/L5/L8 unchanged and write a P8.1 record.
- `manager/candidates.rs` routes `DvDestination::Profile81` →
  `ReconstructedProfile81` when `playback.dolby_vision_fel_reencode` is on.

The Developer tab said that route "is not yet available on playback
workers". v1 finishes it. *(M2, 2026-10-25: built on `claude/lp-fel-p81`;
root causes, admission and proofs in
[FEL-P81-RECONSTRUCTION.md](../streaming/FEL-P81-RECONSTRUCTION.md). M2a
picked the live design: one reconstruction per worker.)* The deliverable is a client claiming
`dv_profiles:[5,8]` playing a P7 FEL title, receiving a P8.1 stream:

- the BL carries the FEL-composed picture;
- the RPU carries the original display-management metadata.

Every DV client benefits: Apple TV, Android TV and this player. It is one
server feature, not a Linux one.

**This is the largest unknown in v1, so it starts with a measurement.** Before
the serving design is fixed, M2a runs the reconstruction on a worker GPU and
reports frames per second for 4K compose plus Main10 encode. The result
decides between two designs:

- **Real time with headroom (≥ 1.25× the title frame rate):** a live
  session, the same as processed HDR10-E today.
- **Not real time:** a cache-producer job per title. It gets its own
  Activity row naming the title, node, GPU and reason, with a stop, under
  the owner's attributability rule. Until the job finishes, the title plays
  through the existing P7 → P8.1 conversion with the EL dropped, and the
  session's `enhancement` detail says "FEL used: no, enhancement layer being
  prepared".

Either way, the session response's `enhancement` is the truth. It says "FEL
used: yes" only when the BL really came from the composed picture.

## 4. Client contract

### 4.1 Web — one Playback setting, one action

- **The setting:** Settings → Playback → **Dolby Vision playback on Linux**:
  **This browser** (the default) or **plurx Player (Linux)**. It is stored per
  browser in `localStorage["plurx.openIn"]`. The setting is disabled outside
  Linux, and a saved `player` preference is ignored there. Android, ChromeOS
  and unknown client platforms use browser playback. The server OS is irrelevant.
  - It is not a feature gate. It is a viewer preference, like the HDR display
    peak, because only the viewer knows whether the player is installed.
- **What Play does when it is set:** only a cold start of a local Dolby
  Vision file in a Linux browser uses the external player. The selected
  file’s `hdr` / legacy `hdr_format` determines its range; SDR, HDR10, HLG
  and unknown formats stay in the web UI. Each version’s Play button uses
  that version’s metadata, so a DV version never redirects an HDR10 version.
  Retries, internal reopens, reserved Watch attempts, Library channels,
  audiobooks and shared-library sources retain browser playback. For an
  eligible Dolby Vision start:
  - mint a handoff (§3.1);
  - set `location.href` to
    `plurx-player://play?server=<origin>&code=<code>`;
  - show "Opening in plurx Player…".
- **Not installed:** poll `GET /playback-handoffs/{id}` once a second for up
  to 15 s. If the code was redeemed, say "Playing in plurx Player". If not,
  show "plurx Player didn't open". Offer a link to the install page and a
  "Play here instead" button.
  - Why poll the server instead of watching page focus: Chrome's first-use
    "Open xdg-open?" dialog takes focus whether or not anything opens, and
    the viewer may click it after any timer. The server is the only party
    that knows.
- **Copy:** the setting's help says that signing out of the web app also
  stops plurx Player playback, because the handoff token is revoked with the
  minting token.

### 4.2 Launcher — `clients/linux-player/`

The launcher is Python 3 using only the standard library: `urllib`, `json`,
and a Unix socket for mpv IPC. Two reasons:

- It installs with no dependencies on any Ubuntu.
- `clients/desktop-remote` already sets the Python precedent.

Its work, in order:

1. **Parse, trust and redeem.**
   - Parse the scheme URL. `server` must be an http(s) origin with no path.
   - Check `server` against a per-user list in
     `~/.config/plurx-player/servers.json`.
     - The first time a new origin appears, ask once in a desktop dialog
       (`zenity`, which the package depends on), naming the origin. Refuse
       if the user declines.
     - Why: any web page can navigate to a `plurx-player://` URL. Without
       the list, a hostile page could point the player at its own server
       and feed mpv and libavcodec whatever it likes.
     - Plain http is allowed only after that same confirmation, because the
       owner's LAN servers are plain http.
   - Redeem the code. The redeemed response's canonical origin must equal
     `server`.
   - `--ca FILE` adds a CA for servers with a private certificate. Python
     uses the system CA store, not the one Chrome uses.
2. **Decide and create.** Run decision → HLS create with the caps in §3.2,
   then execute `delivery`: direct, or remux/transcode through
   `sessions_url`. The rules are in [API.md §7.2](../API.md).
3. **Play.** Start the private mpv with `--input-ipc-server` and
   `--start=<media_origin_ms-adjusted>`, then select the handed-off
   audio/subtitle tracks. Sidecar subtitles load with `sub-add`.
4. **Keep the session alive.**
   - Control heartbeat every 5 s: passive, no `supported_actions`. Fences
     come from the create response.
   - Progress every 10 s while playing, and on pause, seek and exit.
   - Progress is always mpv's playback time plus `media_origin_ms`. mpv's
     timeline starts at the session's origin, so adding the origin once,
     here, is the only place it is applied.
   - **Server stop:** if control answers 404 or 410, stop mpv and show the
     server's reason on the OSD for 3 s. This makes the stop in Activity
     work on this client.
5. **Seek.**
   - Watch mpv's `seek` and `playback-restart` events.
   - **Inside the session's available range:** mpv seeks within the
     playlist.
   - **Outside it** (a rolling or still-growing session): pause, send
     `DELETE` for the session, create a new one with `start` set to the
     target, `loadfile` the new playlist, and take the new
     `media_origin_ms`.
   - Why not let mpv seek: a remux or transcode playlist grows behind the
     encoder, and seeking past its end stalls.
6. **Tear down.** On exit or crash:
   - send `DELETE /hls/{session}`;
   - revoke the handoff token with `DELETE /playback-handoffs/self`.

   The token is in memory only and is never written to disk.
7. **Log the mapper.** The OSD and log show what libplacebo reported: the
   DV profile, the mapper (`closed-form CM v4.0` or `CM v2.9`) and the
   target nits. One `POST /client-log` line per playback carries the same
   facts, so Activity shows them.

**Desktop integration.**

- A `.desktop` entry declares `MimeType=x-scheme-handler/plurx-player;`, and
  dpkg's `update-desktop-database` trigger registers it.
- No `xdg-mime` call in `postinst`, because that runs as root and would edit
  root's associations, not the user's.
- The launcher also runs standalone (`plurx-player <url>`) for testing.

**HDR depends on the compositor.**

- HDR output needs a compositor with `wp_color_manager_v1` in HDR mode, such
  as GNOME in HDR mode.
- An accurate target needs a compositor that reports the panel's real
  luminance, as the rog_hdr_dv nativehdr Mutter does. Stock compositors may
  report a default instead.
- The package description says this, because "HDR on Ubuntu" without it
  would be an untruthful claim.

## 5. Closed-form mapping in mpv

Owner direction (2026-10-25):

- ordinary `vo=gpu-next` playback of DV P5/P8 with an RPU uses the closed
  form, CM v4.0 and v2.9, trims included;
- it targets the compositor's preferred description;
- libplacebo's built-in DV tone mapping is not also applied;
- the mapper that ran is visible.

**Built as one mpv patch** (2026-10-26, owner's call: it replaced the
rog_hdr_dv port, which sat on about 9,000 lines of research code).
`clients/linux-player/packaging/patches/mpv/0003-…` adds
`video/out/placebo/plurx_dv.{c,h}` and about 200 lines in `vo_gpu_next.c`.
The private build is libplacebo 7.387.0 and mpv master `e59d90c84`
(2026-10-26), the first with FEL composition (§3.3), with two upstreamable
fixes before it: 0001 sends no zero MaxCLL/MaxFALL in the Wayland image
description (Weston 16 ends the connection on one), and 0002 splits the EL
without FFmpeg 9's `dovi_split` BSF (Ubuntu ships FFmpeg 8.1). A third,
0004 (2026-10-26), starts Wayland Vulkan swapchains pass-through: the NVIDIA
615 driver gives an sRGB swapchain a colour surface of its own and keeps it
after the swapchain is gone, so mpv's own request, made when HDR video
arrives in an already open window, ended the Wayland connection.

libplacebo is linked into mpv statically, its global symbols renamed
`plurx_pl_*` and hidden. Ubuntu's FFmpeg (`libavfilter`) loads the system
libplacebo, another major version, into the same process, and libplacebo's
symbols are unversioned, so a private `libplacebo.so` would take over every
call `libavfilter` makes into libplacebo, with the wrong ABI. The linker
refuses to hide a symbol a linked library references, so the names are
changed (`objcopy --redefine-syms`; mpv sees them through the staged
`libplacebo/config.h`). The build and the package verification fail if mpv
needs a shared libplacebo, exports a libplacebo symbol or imports a `pl_*`
one.

1. **The equations are the server's.** The package build copies
   `tools/dv_processing/dv_dm.h` and `dm_closed_form.h` into mpv's tree, so
   the player compiles exactly the code the plurx helper renders with, and
   every equation fix on main reaches the player at its next package build.
   The patch carries no copy of them.
2. **RPU.** libdovi (Ubuntu's `libdovi3`) parses the frame's raw RPU, as the
   helper does. Ubuntu's `libdovi-dev` ships an empty C header, so the
   package carries the one the helper pins
   (`packaging/libdovi/rpu_parser.h`, sha256-checked by a test).
3. **Reconstruction.** libplacebo applies the RPU's reshaping and colour
   conversion to BT.2020/PQ with the helper's own render parameters
   (identical 0.005–10000-nit volumes, clip tone and gamut mapping), so no
   tone mapping happens there. Chroma is upsampled bilinearly.
4. **Target.** The compositor's preferred description (mpv's `target_csp`,
   with `--target-peak` applied if set): its luminance range, and the
   largest of BT.2020, P3-D65 and BT.709 inside the panel's primaries,
   within one step of the precision the six primary coordinates were
   reported in (DisplayID 12-bit, else EDID 10-bit; rounding x and y can
   move a vertex by up to √2/2 of a step, and some encoders truncate). The
   ROG selects P3-D65 at 616 nits; the compositor finishes P3 to the panel.
   - **Brightness is an option** (owner, 2026-10-26), `dv-brightness` in
     plurx Player's `mpv.conf`:
     - `relative` (default) maps on mpv's reference-white scale (the
       compositor's values rescaled to a 203-nit reference), so the desktop's
       HDR brightness setting applies.
     - `absolute` maps for the panel's real peak, using the compositor's
       reference luminance (which the patch makes `wayland_common` keep), and
       scales the result by 203/reference, which the compositor undoes:
       Dolby's absolute rendering.
     - Checked on Weston with a 300-nit reference: relative targets 416.8
       nits; absolute targets 616 and matches the reference mapping at 616
       scaled by 203/300, ΔE_ITP mean 0.007.
     - The mapper line names the mode and the reference.
5. **Presentation.** The mapped frame (16-bit UNORM) and the one swapchain
   hint per frame both carry the target's volume (BT.2020/PQ container, the
   named gamut as mastering primaries, peak = MaxCLL = MaxFALL), so the
   final render has nothing to map and the compositor does not tone map it
   again. The reconstruction is 16-bit UNORM too, half the memory traffic
   of float.
6. **Visible.** One line per configuration, `plurx mapper: closed-form CM
   v4.0 target=<min>-<max> nits gamut=P3-D65 (trim …, primaries …,
   tolerance …)`, which the launcher posts to Activity. When the display is
   not in HDR mode, the frame has no RPU, or a frame cannot be mapped, the
   line says `none (<reason>)` and libplacebo's own handling renders it,
   with the display's own hint. A mapping failure holds for the rest of
   that configuration, so playback never alternates between mappers; the
   GPU formats the mapping needs are checked once.
7. **Switches.** `--dv-cm4-trims` and `--dv-cm29-trims` (default yes), as
   `DV_TARGET_TRIMS_CM4/_CM29` on the server. Screenshots of a DV frame use
   the same mapping.

**Evidence** (2026-10-26, lab3, Ubuntu 26.10 container, libplacebo 7.360.1,
lavapipe):

- plurx's `controls/test_dv_dm.c` passes against Ubuntu's libdovi 3.3.2
  with the pinned header (real RPU fixtures and pinned golden values), so
  the parser ABI is the helper's;
- `controls/run_dm_parity.py` passes: the shader against the CPU reference,
  every case inside its tolerance;
- end to end, the patched mpv on a headless HDR Weston 16 with the ROG's
  DisplayID primaries and 616 nits logs `closed-form CM v4.0 … gamut=P3-D65
  (… primaries displayid-12bit …)` and the CM v2.9 equivalent. Its unscaled
  16-bit PQ screenshot of a real frame against an independent reference
  (FFmpeg decode, the same reconstruction, `dv_dm_cpu_render` in double
  precision): Companion (CM v4.0) ΔE_ITP mean 0.008, p99 0.021, max 0.053;
  Dumb Money (CM v2.9) mean 0.006, p99 0.016, max 0.073;
- an SDR desktop logs `none (the display is not in HDR (BT.2020/PQ) mode)`
  and an HDR10 file `none (no Dolby Vision RPU)`.

So the player reproduces plurx's own closed form, whose agreement with
`cm_offline` on the E2 frames is the server's (CM v4 about 0.005–0.011, CM
v2.9 about 0.14–0.46). The remaining equation outliers belong to the
HDR10-E session; their fixes reach the player through
`tools/dv_processing`.

**Found on the way:**

- A float render target with `.bits.color_depth = 32` is scaled as 32-bit
  integer samples by libplacebo 7.360.1 (values around 1e7 instead of PQ).
  The helper's `base_dv_renderer.h` sets that on its libplacebo `0d043c7`,
  where its controls pass; moving the helper to a newer libplacebo needs
  that field dropped.
- On the ROG's Intel Arc iGPU, CM v2.9 cost 28 ms per 4K frame (CM v4.0
  14 ms), and a 4K CM v2.9 title dropped a third of its frames (29 of 87;
  measured with the ROG's CI runners paused). The final
  pass evaluated eight output-lattice vertices per pixel, in one shader that
  also held all of CM v4.0. `dv_dm.h` now evaluates the frame's lattice once
  (vertices outside the table's chroma reach still per pixel) and compiles
  each method's passes separately: 16.5 ms, and the title plays without a
  dropped frame on the Arc, as FEL and CM v4.0 do. Same float32 vertex
  computation: `run_dm_parity.py` passes, and the patched mpv's 16-bit
  screenshot of the Dumb Money frame is bit-identical before and after. The
  server's display-targeted renders use the same code.

## 6. Non-goals for v1

- **No browsing UI in the player.** The web app is the browser, and a second
  library UI would drift from it.
- **No HDMI passthrough or bitstreaming,** of either audio or DV. The laptop
  panel is the target, and compositor-managed HDR is the only correct
  output path.
- **No PGS overlays.** Image subtitles use the server's existing burn route.
  Native PGS can follow, using mpv's own PGS renderer.
- **No replacement of the system mpv.** The private build installs under
  `/usr/lib/plurx-player/`, so the distribution's mpv and libplacebo, and
  every program linked against them, are untouched.
- **No remote-receiver mode** ("play on ROG" from a phone). It is a later
  milestone that reuses the same launcher.

## 7. Milestones

| # | What | Acceptance |
|---|---|---|
| # | What | Acceptance |
|---|---|---|
| M1 | Server playback handoff (§3.1) | Rust tests: mint/redeem, single use, 120 s code expiry, 6 h token expiry, revoked with the minting token and by `/self`. Every other user route refuses the handoff token, and so do routes for another `file_id`. The 410 cases, `{redeemed}` status, the device name in sessions. `Regression-Test` lines. |
| M2a | Measure reconstruction on a worker GPU | Frames per second for 4K FEL compose plus Main10 encode on two E2 P7 titles. The result picks the live or cache design (§3.3). |
| M2 | Reconstructed P8.1 serving (§3.3, for clients that cannot compose) | A test client claiming `[5,8]` gets `ReconstructedProfile81` for a P7 FEL fixture. ffprobe shows a P8.1 `dvvC` and an RPU on every AU, with L1/L2/L8 equal to the source. **Composition is proved:** ΔE_ITP/PSNR of the output BL against an offline FEL reference on the E2 frames beats the EL-dropped output. The session `enhancement` reports FEL used. The Activity row names the work and stops it. The Developer tab line is updated. |
| M3 | Web setting + handoff action (§4.1) | `tests/web` covers the setting, the code mint, the scheme URL, and the redeem-status poll for both opened and not-installed. |
| M4 | Launcher (§4.2) | Python tests against a fake server: the call sequence, caps, heartbeat/progress cadence, progress = mpv time + origin, seek outside the range re-creates the session, 404/410 control stops playback, teardown and token revoke on exit and on crash, an unknown origin is not redeemed without confirmation, a malformed URL is refused, no token on disk. |
| M5 | Private mpv/libplacebo with the closed form (§5) and FEL composition (§3.3) + `.deb` | The §5 evidence: the shader parity controls, and the patched mpv's output against the double-precision reference on real CM v4.0, CM v2.9 and P7 FEL frames. The `.deb` installs on Ubuntu 26.10, coexists with the system mpv, registers the scheme through the desktop database, and uninstalls cleanly. |
| M6 | Acceptance on the ROG | One P5, one P8 CM v4, one P8 CM v2.9 and one P7 FEL title, each opened from the web app. Checks: HDR presented, the mapper line, the target 616, seek inside and outside the window, track switching, progress saved, the session torn down on close, and Stop in Activity ends playback within one heartbeat. **No double mapping:** the surface description mpv commits equals the compositor's preferred one, checked with 616-nit and 1000-nit PQ patches. The owner's visual check is last. |

M1, M2a, M3 and M4 are independent. M2 follows M2a, and M6 waits on all
of them. PRs:

- one for M1+M3 (handoff and web);
- one for M2;
- one for M4+M5 (launcher and packaging).

M2 is its own PR because it carries the most risk.
Re-verify every path and field named here against main at build time.
