# Linux player — Dolby Vision on Linux through plurx

**Status:** plan, owner-approved direction; Fable review folded in (2026-10-25) · **Owner:** Claude
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
- Profile 7 FEL titles keep their enhancement layer, composed on the server
  and re-encoded as P8.1 (§3.3). For those the stream is not the original.

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
 mpv (private build) ── libplacebo (private build: closed-form DM)
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
| `video[]` HEVC | `profiles:["main","main10"]`, `max_height:2160`, `present:["sdr","pq","hlg"]`, `dv_profiles:[5,8]` | mpv/libplacebo reconstruct P5 and P8 from the RPU. P7 is not claimed, because the server's P7 → P8.1 routes (§3.3) are what make it playable. |
| `video[]` AV1, H.264 | as decoded by the bundled FFmpeg | mpv decodes them in software or with VA-API. |
| `display` | `{hdr:true, dolby_vision:true}`, no `max_nits` | On the preserved route the player maps locally, so the server never needs the peak. Sending none means Unknown, so the server never display-targets this client. Sending a guess would be the over-claim the peak contract forbids. |
| `hls_hevc_sample_entries` | `["hvc1","hev1"]` | The P8.1 route uses `hvc1` plus `dvvC`, and mpv's demuxer reads `dvvC`. |
| `dv_transport` | `"hls"` | The same as Apple. |
| audio / sinks | every codec mpv decodes, `passthrough:false`, `max_channels:8` | mpv decodes and downmixes to the real sink. Reading the PipeWire channel count needs more than the standard library, and passthrough is out of scope. |
| `subtitle_overlays` | `[]` in v1 | Text subtitles load as sidecar tracks. PGS is in §6. |

### 3.3 Profile 7 FEL in v1 — reconstructed P8.1 from the server

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

- **The setting:** Settings → Playback → "Open titles in": **This browser**
  (the default) or **plurx Player (Linux)**. It is stored per browser in
  `localStorage["plurx.openIn"]`.
  - It is not a feature gate. It is a viewer preference, like the HDR display
    peak, because only the viewer knows whether the player is installed.
- **What Play does when it is set:**
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

## 5. Closed-form mapping in mpv/libplacebo

The rog_hdr_dv card `t_d4b5c4f8` ports `dm_closed_form.h` (plurx main,
byte-identical in #1275/#1276) into a libplacebo/mpv stage. Owner direction,
recorded on that card on 2026-10-25:

- ordinary `vo=gpu-next` playback of DV P5/P8 with an RPU uses the closed
  form, CM v4.0 and v2.9, trims included;
- it targets the compositor's preferred description;
- libplacebo's built-in DV tone mapping is not also applied;
- the mapper that ran is visible.

Acceptance is ΔE_ITP against `cm_offline` on the E2 real-title frames, at
least matching plurx (CM v4 about 0.005–0.011, CM v2.9 about 0.14–0.46).
The remaining equation outliers belong to the HDR10-E session. Their fixes
reach the player through plurx main's `dm_closed_form.h`.

## 6. Non-goals for v1

- **No browsing UI in the player.** The web app is the browser, and a second
  library UI would drift from it.
- **No client-side FEL.** The server composes FEL (§3.3), so one
  implementation serves every client.
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
| M2 | Reconstructed P8.1 serving (§3.3) | A test client claiming `[5,8]` gets `ReconstructedProfile81` for a P7 FEL fixture. ffprobe shows a P8.1 `dvvC` and an RPU on every AU, with L1/L2/L8 equal to the source. **Composition is proved:** ΔE_ITP/PSNR of the output BL against an offline FEL reference on the E2 frames beats the EL-dropped output. The session `enhancement` reports FEL used. The Activity row names the work and stops it. The Developer tab line is updated. |
| M3 | Web setting + handoff action (§4.1) | `tests/web` covers the setting, the code mint, the scheme URL, and the redeem-status poll for both opened and not-installed. |
| M4 | Launcher (§4.2) | Python tests against a fake server: the call sequence, caps, heartbeat/progress cadence, progress = mpv time + origin, seek outside the range re-creates the session, 404/410 control stops playback, teardown and token revoke on exit and on crash, an unknown origin is not redeemed without confirmation, a malformed URL is refused, no token on disk. |
| M5 | Private mpv/libplacebo with the closed form (§5) + `.deb` | `t_d4b5c4f8` acceptance. The `.deb` installs on Ubuntu 26.10, coexists with the system mpv, registers the scheme through the desktop database, and uninstalls cleanly. |
| M6 | Acceptance on the ROG | One P5, one P8 CM v4, one P8 CM v2.9 and one P7 FEL title, each opened from the web app. Checks: HDR presented, the mapper line, the target 616, seek inside and outside the window, track switching, progress saved, the session torn down on close, and Stop in Activity ends playback within one heartbeat. **No double mapping:** the surface description mpv commits equals the compositor's preferred one, checked with 616-nit and 1000-nit PQ patches. The owner's visual check is last. |

M1, M2a, M3 and M4 are independent. M2 follows M2a, M5 waits on
`t_d4b5c4f8`, and M6 waits on all of them. PRs:

- one for M1+M3 (handoff and web);
- one for M2;
- one for M4+M5 (launcher and packaging).

M2 is its own PR because it carries the most risk.
Re-verify every path and field named here against main at build time.
