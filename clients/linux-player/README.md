# plurx Player (Linux)

This is the launcher half of the native Linux player in [the Linux player plan](../../docs/clients/LINUX-PLAYER-PLAN.md). Browsing stays in the plurx web app. When Settings → Playback → **Open titles in** says **plurx Player (Linux)**, Play mints a one-time handoff code and opens `plurx-player://play?server=<origin>&code=<code>`. This launcher handles that URL: it redeems the code for a token scoped to one file, asks the server how to deliver the file, plays it in mpv, and keeps the server informed until mpv closes. It is Python 3 with the standard library only (`urllib`, `json`, a Unix socket for mpv's JSON IPC, `subprocess`), so it installs on any Ubuntu with no dependencies beyond `zenity`.

Status: milestone M4 built against a fake server and a fake mpv; the server handoff endpoints (M1) are being built in parallel, so nothing here has met a real plurx server yet. The private mpv/libplacebo build with plurx's closed-form display management and the finished `.deb` are M5. Acceptance on the ROG is M6.

## Install and run

Until M5 ships a package, run it from a checkout with a system mpv:

```sh
clients/linux-player/plurx-player 'plurx-player://play?server=http%3A%2F%2Fmedia1%3A8096&code=CODE'
```

`--mpv PATH` picks the mpv binary; otherwise it uses `/usr/lib/plurx-player/bin/mpv` (the private build) when installed, then `mpv` on `PATH`. `PLURX_PLAYER_MPV` does the same as `--mpv`. `--ca FILE` adds a CA for a server with a private certificate, because Python uses the system CA store rather than Chrome's; the path is remembered for that server.

The package (`debian/`, see below) installs the code under `/usr/lib/plurx-player`, the launcher as `/usr/bin/plurx-player`, and `plurx-player.desktop` with `MimeType=x-scheme-handler/plurx-player;`. dpkg's `update-desktop-database` trigger registers the scheme. There is deliberately no `xdg-mime` call in `postinst`: that runs as root and would edit root's associations, not the user's.

## What it does, in order

1. **Parse and trust.** The URL must be exactly `plurx-player://play?server=…&code=…`; `server` must be an http(s) origin with no path, user information, query or fragment. Anything else is refused before any network or dialog. The origin must be in `~/.config/plurx-player/servers.json` (`$XDG_CONFIG_HOME` respected); the first time a new origin appears, a `zenity --question` names it and the player refuses unless the user allows it. Plain http is allowed only through that same question, which says the connection is not encrypted. Any web page can navigate to this scheme, so without the list a hostile page could point mpv and libavcodec at its own server.
2. **Redeem.** `POST /api/v1/playback-handoffs/redeem` with `{code, device: "plurx Player (Linux)"}` and no bearer. A 410 (expired, used, or minted by a revoked sign-in) says so in a dialog. If the answer carries the server's canonical `origin`, it must equal `server`.
3. **Decide and create.** `POST /api/v1/files/{id}/decision?force=auto[&audio=][&subtitle=]` with the caps v2 document of plan §3.2 (field names from `crates/plurx-core/src/playback/caps.rs`): HEVC Main/Main10 to 2160 with Dolby Vision profiles 5 and 8, `display {hdr, dolby_vision}` with no `max_nits`, every codec mpv decodes without passthrough, `hls_hevc_sample_entries ["hvc1","hev1"]`, `dv_transport "hls"`, no subtitle overlays. Then execute `delivery` ([API.md §7.2](../../docs/API.md)): `direct` plays the file URL; `remux` creates an HLS copy session through `sessions_url` (`copy`, `aac`, `preserve_dolby_vision` from the plan); `transcode` creates one with no `height`, because Auto is the server's choice.
4. **Play.** mpv starts with `--input-ipc-server` on a socket in a private 0700 directory under `$XDG_RUNTIME_DIR`, `--vo=gpu-next --gpu-api=vulkan --hwdec=auto-safe --target-colorspace-hint=yes` and `--idle=once`. The start position is set as mpv's `start` property, adjusted by the session's `media_origin_ms`. A text subtitle loads as a sidecar (`sub-add` of `/api/v1/files/{id}/subs/{n}`) shifted by `-media_origin_ms`, since its cues are on the source timeline. An image subtitle asks the server to burn it; if the HDR guard refuses (422), playback continues without it and the OSD says why.
5. **Keep the session alive.** A passive control exchange every 5 s: no `supported_actions`, fences from the create response (API.md §10). Progress every 10 s while playing, and on pause, after a seek, and on exit. Positions are always mpv's time plus `media_origin_ms`, added in one place. A control 404 or 410 stops mpv after showing the server's reason on the OSD for 3 s, which is how Stop in Activity reaches this client. A 401 anywhere (the user signed out of the web app) stops it too.
6. **Seek.** On mpv's `seek` event the target is compared with what the session has produced (`published_end_ms` from `/hls/{s}/status`, or mpv's own duration). Inside it, mpv seeks in the playlist. Outside it, the player pauses, deletes the session, creates one that starts at the target, loads the new playlist and takes the new origin: seeking past the end of a growing playlist would stall behind the encoder. A VOD session or a direct file never needs this.
7. **Tear down.** On exit, mpv closing, mpv crashing, SIGTERM/SIGINT/SIGHUP or a launcher exception: final progress, `DELETE /hls/{session}`, then `DELETE /playback-handoffs/self`. A second signal during teardown is ignored. SIGKILL cannot be caught; the token then expires with its 6 h lifetime or with the web sign-in.
8. **Log the mapper.** One `POST /client-log` line per playback (`event: native_mapper`) carries the Dolby Vision profile (mpv's `track-list` `dolby-vision-profile`), the mapper and the target nits (`video-target-params` `max-luma`), and the OSD shows the same. The mapper is what libplacebo logs: a `plurx mapper: <name>` line, or a bare `CM vX` from libplacebo. When nothing reports it, it says `unknown` rather than guessing.

The token is in memory only. It never enters a file, a log line or mpv's command line (which `ps` shows to every user). When a bearer route must be fetched by mpv itself — the direct file or a sidecar subtitle — the header is set over the private IPC socket. HTTP redirects are never followed, so the header cannot be carried to another host. Session routes use the session id as their capability and carry no bearer.

## HDR depends on the compositor

HDR output needs a compositor with `wp_color_manager_v1` in HDR mode, such as GNOME in HDR mode. An accurate target needs one that reports the panel's real luminance, as the rog_hdr_dv nativehdr Mutter does; stock compositors may report a default instead, and the mapper line will show that target.

## Packaging

`debian/` is a skeleton: `dpkg-buildpackage -us -uc -b` from this folder builds an `Architecture: all` package that depends on `python3` and `zenity`, recommends the system `mpv` and runs these tests at build time. The private mpv and libplacebo are placeholders marked `M5` in `debian/rules`, `debian/install` and `debian/control`; when they land the package becomes architecture-specific and installs them under `/usr/lib/plurx-player`, leaving the distribution's mpv and libplacebo untouched.

## Focused evidence and remaining acceptance

```sh
python3 -m unittest discover -s clients/linux-player/tests -p 'test_*.py'
make linux-player-check
```

The tests run the real launcher in-process against a fake plurx server (`http.server` in a thread) and a fake mpv (`tests/fake_mpv.py`, which speaks the JSON IPC on the socket), on a virtual clock so cadence is exact; one test runs it as a subprocess and sends SIGTERM. They cover the call sequence and which calls carry the bearer, the caps fields against `caps.rs`, the 5 s/10 s cadence, progress = mpv time + origin, seeks inside and outside the range, control 404/410, teardown and revoke on exit, mpv crash, launcher crash and SIGTERM, the trust prompt, malformed URLs, redeem refusals, direct play, subtitles, the mapper line, and that the token reaches no file and no argv. The fake server is written from the M1 contract in the plan, not from a server.

Still required: the M1 server endpoints and a run against them; whether mpv reports the seek target as `time-pos` at its `seek` event on a growing playlist (the seek re-creation depends on it); the libplacebo mapper log line (M5); the `.deb` install, scheme registration and uninstall on Ubuntu 26.10 (M5); and the plan's M6 acceptance on the ROG.
