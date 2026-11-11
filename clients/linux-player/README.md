# plurx Player (Linux)

This is the launcher half of the native Linux player in [the Linux player plan](../../docs/clients/LINUX-PLAYER-PLAN.md). Browsing stays in the plurx web app. When Settings → Playback → **Dolby Vision playback on Linux** says **plurx Player (Linux)**, a cold Play of a local Dolby Vision file in a Linux browser mints a one-time handoff code and opens `plurx-player://play?server=<origin>&code=<code>`. Other formats and clients on other operating systems keep playing in the web UI; the setting is disabled outside Linux. This launcher handles that URL: it redeems the code for a token scoped to one file, asks the server how to deliver the file, plays it in mpv, and keeps the server informed until mpv closes. It is Python 3 with the standard library only (`urllib`, `json`, a Unix socket for mpv's JSON IPC, `subprocess`), and uses the separately installed system `mpv` plus `zenity`.

Status: 0.5.0 source consolidates Cinema onto the system mpv and removes the
bundled renderer from the Debian package. Validation and installation remain
pending in the [release ledger](../../docs/clients/LINUX-PLAYER-CINEMA-UI.md).

## Install and run

Install the package (see [Packaging](#packaging)), or run it from a checkout with a system mpv:

```sh
clients/linux-player/plurx-player 'plurx-player://play?server=http%3A%2F%2Fmedia1%3A8096&code=CODE'
```

`man plurx-player` covers the options, the URL and the files below. `--mpv PATH` picks the mpv binary; otherwise it uses `/usr/bin/mpv` when executable, then `mpv` on `PATH`. `PLURX_PLAYER_MPV` selects an override when `--mpv` is absent. An old `/usr/lib/plurx-player/bin/mpv` is never selected implicitly. `--ca FILE` adds a CA for a server with a private certificate, because Python uses the system CA store rather than Chrome's; the path is remembered for that server.

The package (`debian/`, see below) installs the code under `/usr/lib/plurx-player`, the launcher as `/usr/bin/plurx-player`, and `plurx-player.desktop` with `MimeType=x-scheme-handler/plurx-player;`. dpkg's `update-desktop-database` trigger registers the scheme. There is deliberately no `xdg-mime` call in `postinst`: that runs as root and would edit root's associations, not the user's.

## mpv configuration

The launcher runs mpv with `--config-dir=$XDG_CONFIG_HOME/plurx-player/mpv` (by default `~/.config/plurx-player/mpv`), so plurx Player has its own optional mpv configuration and never reads the general `~/.config/mpv`: a stray `vo=` or `tone-mapping=` there would silently defeat the closed-form mapping. The directory does not exist by default; an `mpv.conf`, `input.conf` or script placed there applies to plurx Player only. For a qualified renderer that implements them, Dolby Vision options belong there: `dv-brightness=absolute` maps for the panel's real luminance (Dolby's absolute rendering) instead of the default `relative`, which follows the desktop's HDR brightness setting; `dv-cm4-trims=no` / `dv-cm29-trims=no` turn off each family's creative trims. Cinema's bundled on-screen controls work without it (the launcher does not pass `--no-config`). The options the player depends on (`--vo=gpu-next --gpu-api=vulkan --hwdec=auto-safe --target-colorspace-hint=yes`, the IPC socket, `--idle=yes --keep-open=yes`) are on the command line, which mpv applies over any config file. `--ytdl=no` is always passed: the player only opens plurx URLs from an allowed server, and mpv's ytdl hook would otherwise run `yt-dlp` on anything else. mpv also starts without `LD_LIBRARY_PATH`, `LD_PRELOAD` or `LD_AUDIT`, which would otherwise override system library selection or inject code into it (see [Packaging](#packaging)).

## Renderer qualification

Cinema owns controls, server sessions and its isolated mpv configuration.
The selected system mpv owns decoding, Dolby Vision reconstruction, FEL
composition, sampling precision and HDR output. Cinema reads its compiled
capability using `--no-config --version` with a three-second deadline and the
same loader-variable cleanup as playback. Only the exact `native-dovi-api-1`
token in `List of enabled features:` enables the native P5/P7/P8 claim. That
renderer contract identifies raw-RPU grading, enhancement-layer pairing and
the required provider SDK 2 boundary; it is a build capability, not a runtime
switch or a version comparison.

Without the marker (including probe failure), Cinema keeps playing with the
selected binary and asks the server for its compatible HDR10 conversion route.
The decision and playback-control capability documents both omit native Dolby
Vision. Explicit overrides and saved quality choices still apply. A marker
reports compiled support, not actual display calibration or physical HDR
acceptance. The ROG renderer must be updated from its unmarked 5.1 build to
the marked 5.2 build before the frontend is installed and qualified.

Historical mpv patches and CPU controls remain in this source tree for
reproducibility. They are neither built nor installed by the 0.5.0 package;
passing those controls does not qualify a system mpv. The launcher retains
`--vo=gpu-next --gpu-api=vulkan --hwdec=auto-safe
--target-colorspace-hint=yes` and loads Cinema through standard mpv Lua/IPC.

## Playback controls

The native player draws Cinema-style controls inside mpv's video surface:
white title/transport controls over dark gradients, a blue full-film timeline,
±10/±30-second buttons, play/pause, Audio, CC, Settings, Playback info and
Fullscreen. The stock OSC is disabled; the system renderer supplies the
video surface and its supported Dolby Vision options.

Drag the timeline to preview; release to seek. Left/Right seek ten seconds
immediately, including when controls are hidden. Tab focuses controls; with
the timeline focused, Left/Right preview and Enter commits. Escape cancels a
preview before closing a menu. Space commits a preview and toggles playback.
Menu arrows or Tab choose a row; Enter activates it. Controls hide after four
seconds while playing, except during a preview or an open menu; paused
playback keeps them visible. Mouse movement reveals them.

Audio and subtitle menus list tracks mpv actually has, including loaded
sidecars. Library metadata comes from the handoff's restricted context:
title, series and episode identity, year, runtime, air date and synopsis.
Both Info buttons open the same scrollable Playback info panel. It reports
mpv's decoded frame, OSD window and active video-area dimensions, codec/pixel format, input color
and Dolby Vision/PQ/HLG observations, VO target color/peak, active renderer
and hardware decoder, estimated FPS, packet bitrate, renderer/decoder drops,
stream/decoded/output audio, sync and demuxer buffer/input-rate estimates.
Delivery method, transport and session kind come from whitelisted launcher
facts. Missing observations read Unavailable; the panel refreshes only while
open, including while paused. Target properties describe mpv's output target,
not measured panel luminance or proof of physical HDR output. The panel does
not expose media URLs, session credentials, private paths or synopsis text.

Quality offers Auto, Original and the server's advertised encode heights.
Changing it retains the full-film position and paused state. A provisional
session has a separate owner ID; the old session stays live until the new
file loads and playback restarts. Admission, decode or timeout failure keeps
or restores the old stream. The toolbar shows the requested mode and actual
decoded resolution separately. Native playback has no browser MSE transport,
so the web's Original · one stream transport experiment is not a separate
quality mode here.

Next episode uses the server's immediate ordered successor, across season
boundaries. It advances only this handoff token's file scope, with an atomic
expected-predecessor check; expiry and sign-out revocation stay unchanged.
It does not grant library browsing. Autoplay advances at the known film end,
with the web player's 15-second tolerance. Early stream EOF retains resume
position and offers Retry instead of marking the title complete or skipping.
Settings saves autoplay and the selected quality in `playback.json` beside
`servers.json`, without credentials. An unavailable saved rung falls back to
Auto on the next title.

Intro/credits markers, browser PiP and watch-and-browse are outside this
native overlay. An older server can still play, but the overlay explicitly
reports that its metadata/next endpoints need a server update.

The script is installed beside the Python modules as
`plurx_player/scripts/cinema.lua` and loaded with an explicit `--script` path.
`script-opts/cinema.conf` under the player's mpv config directory can set
`accent=6EA8FE` (RGB hex) and `hide_ms=4000`. The behavioral scenarios run in
Lua 5.2 during Debian package tests:

```bash
lua5.2 clients/linux-player/tests/test_cinema.lua clients/linux-player/plurx_player/scripts/cinema.lua
```

The virtual-display render checks layout/input only. HDR-display visual
acceptance and installation are recorded separately in the release ledger.

## What it does, in order

1. **Parse and trust.** The URL must be exactly `plurx-player://play?server=…&code=…`; `server` must be an http(s) origin with no path, user information, query or fragment. Anything else is refused before any network or dialog. The origin must be in `~/.config/plurx-player/servers.json` (`$XDG_CONFIG_HOME` respected); the first time a new origin appears, a `zenity --question` names it and the player refuses unless the user allows it. Plain http is allowed only through that same question, which says the connection is not encrypted. Any web page can navigate to this scheme, so without the list a hostile page could point mpv and libavcodec at its own server.
2. **Redeem.** `POST /api/v1/playback-handoffs/redeem` with `{code, device: "plurx Player (Linux)"}` and no bearer. A 410 (expired, used, or minted by a revoked sign-in) says so in a dialog. If the answer carries the server's canonical `origin`, it must equal `server`.
3. **Decide and create.** `POST /api/v1/files/{id}/decision?force=auto[&audio=][&subtitle=]` with the caps v2 document of plan §3.2 (field names from `crates/plurx-core/src/playback/caps.rs`): HEVC Main/Main10 to 2160, with Dolby Vision profiles 5, 7 and 8 only when the selected renderer reports `native-dovi-api-1` (otherwise the server converts), `display {hdr, dolby_vision}` with no `max_nits`, every codec mpv decodes without passthrough, `hls_hevc_sample_entries ["hvc1","hev1"]`, `dv_transport "hls"`, no subtitle overlays. Then execute `delivery` ([API.md §7.2](../../docs/API.md)): `direct` plays the file URL; `remux` creates an HLS copy session through `sessions_url` (`copy`, `aac`, `preserve_dolby_vision` from the plan); `transcode` creates one with no `height`, because Auto is the server's choice.
4. **Play.** mpv starts with `--input-ipc-server` on a socket in a private 0700 directory under `$XDG_RUNTIME_DIR`, `--vo=gpu-next --gpu-api=vulkan --hwdec=auto-safe --target-colorspace-hint=yes` and `--idle=yes --keep-open=yes`. The start position is set as mpv's `start` property, adjusted by the session's `media_origin_ms`. A text subtitle loads as a sidecar (`sub-add` of `/api/v1/files/{id}/subs/{n}`) shifted by `-media_origin_ms`, since its cues are on the source timeline. An image subtitle asks the server to burn it; if the HDR guard refuses (422), playback continues without it and the OSD says why.
5. **Keep the session alive.** A passive control exchange every 5 s: no `supported_actions`, fences from the create response (API.md §10). Progress every 10 s while playing, and on pause, after a seek, and on exit. Positions are always mpv's time plus `media_origin_ms`, added in one place. A control 404 or 410 stops mpv after showing the server's reason on the OSD for 3 s, which is how Stop in Activity reaches this client. A 401 anywhere (the user signed out of the web app) stops it too.
6. **Seek.** On mpv's `seek` event the target is compared with what the session has produced (`published_end_ms` from `/hls/{s}/status`, or mpv's own duration). Inside it, mpv seeks in the playlist. Outside it, the player pauses, deletes the session, creates one that starts at the target, loads the new playlist and takes the new origin: seeking past the end of a growing playlist would stall behind the encoder. A VOD session or a direct file never needs this.
7. **Tear down.** On exit, mpv closing, mpv crashing, SIGTERM/SIGINT/SIGHUP or a launcher exception: final progress, `DELETE /hls/{session}`, then `DELETE /playback-handoffs/self`. A second signal during teardown is ignored. SIGKILL cannot be caught; the token then expires with its 6 h lifetime or with the web sign-in.
8. **Log the mapper.** One `POST /client-log` line per playback (`event: native_mapper`) carries the Dolby Vision profile (mpv's `track-list` `dolby-vision-profile`), the mapper and the target nits (`video-target-params` `max-luma`), and the OSD shows the same. The mapper is what the selected mpv reports: a `plurx mapper: closed-form CM v4.0 target=… gamut=…` line (or `plurx mapper: none (<reason>)`), or a bare `CM vX` from an unpatched libplacebo. When nothing reports it, it says `unknown` rather than guessing.

The token is in memory only. It never enters a file, a log line or mpv's command line (which `ps` shows to every user). When a bearer route must be fetched by mpv itself — the direct file or a sidecar subtitle — the header is set over the private IPC socket. HTTP redirects are never followed, so the header cannot be carried to another host. Session routes use the session id as their capability and carry no bearer.

## HDR depends on the compositor

HDR output needs a compositor with `wp_color_manager_v1` in HDR mode, such as GNOME in HDR mode. An accurate target needs one that reports the panel's real luminance, as the rog_hdr_dv nativehdr Mutter does; stock compositors may report a default instead, and the mapper line will show that target.

## Packaging

```bash
make linux-player-deb  # Build, verify and compare two frontend packages.
```

The disposable containers use the pinned apt snapshot in
`packaging/upstream.lock`. `PLURX_DEB_OUT` selects the output directory and
`PLURX_DEB_APT_CACHE` selects the shared apt cache. Both belong on the configured
external agent drive for agent work. `PLURX_DEB_JOBS` caps container CPUs.

The binary package contains Python modules, the launcher, the desktop entry,
Cinema Lua and documentation. It depends on the system `mpv` package and never
builds, ships or replaces mpv/libplacebo. An upgrade removes the formerly owned
bundled executable through dpkg's normal obsolete-file handling. The source
package has one launcher orig tarball plus the Debian tarball; historical
renderer patches and CPU controls remain source only. `SHA256SUMS` covers the
binary and source artifacts; `.buildinfo` records the build environment.

Verification checks checksums (including a negative self-test), lintian,
installation, package ownership/file list, scheme registration, default binary,
PQ rendering under gpu-next/Vulkan, Cinema's actual Lua/IPC interfaces, isolated
configuration and ytdl suppression with positive controls, launcher scenarios,
and complete frontend removal while system mpv remains installed. PQ rendering
on a software Vulkan device establishes interface compatibility, not physical
HDR output or Dolby Vision/FEL correctness. The selected renderer requires its
own qualification evidence.

## Focused evidence and remaining acceptance

```sh
python3 -m unittest discover -s clients/linux-player/tests -p 'test_*.py'
make linux-player-check
```

The tests run the real launcher in-process against a fake plurx server (`http.server` in a thread) and a fake mpv (`tests/fake_mpv.py`, which speaks the JSON IPC on the socket), on a virtual clock so cadence is exact; one test runs it as a subprocess and sends SIGTERM. They cover the call sequence and which calls carry the bearer, the caps fields against `caps.rs`, the 5 s/10 s cadence, progress = mpv time + origin, seeks inside and outside the range, control 404/410, teardown and revoke on exit, mpv crash, launcher crash and SIGTERM, the trust prompt, malformed URLs, redeem refusals, direct play, subtitles, the mapper line, and that the token reaches no file and no argv. The fake server is written from the M1 contract in the plan, not from a server.

Package 0.5.0 review, focused regression execution, fresh package qualification
and ROG installation/physical acceptance remain pending. Earlier package
receipts apply only to their recorded renderer and source tree.
