# plurx Player (Linux)

This is the launcher half of the native Linux player in [the Linux player plan](../../docs/clients/LINUX-PLAYER-PLAN.md). Browsing stays in the plurx web app. When Settings → Playback → **Open titles in** says **plurx Player (Linux)**, Play mints a one-time handoff code and opens `plurx-player://play?server=<origin>&code=<code>`. This launcher handles that URL: it redeems the code for a token scoped to one file, asks the server how to deliver the file, plays it in mpv, and keeps the server informed until mpv closes. It is Python 3 with the standard library only (`urllib`, `json`, a Unix socket for mpv's JSON IPC, `subprocess`), so it installs on any Ubuntu with no dependencies beyond `zenity`.

Status: milestone M4 built against a fake server and a fake mpv; the server handoff endpoints (M1) are being built in parallel, so nothing here has met a real plurx server yet. The M5 package is built: `make linux-player-deb` produces an Ubuntu 26.10 `.deb` with a private mpv and libplacebo, but their plurx closed-form display-management patches are not written yet (the patch series is empty). Acceptance on the ROG is M6.

## Install and run

Install the package (see [Packaging](#packaging)), or run it from a checkout with a system mpv:

```sh
clients/linux-player/plurx-player 'plurx-player://play?server=http%3A%2F%2Fmedia1%3A8096&code=CODE'
```

`man plurx-player` covers the options, the URL and the files below. `--mpv PATH` picks the mpv binary; otherwise it uses `/usr/lib/plurx-player/bin/mpv` (the private build) when installed, then `mpv` on `PATH`. `PLURX_PLAYER_MPV` does the same as `--mpv`. `--ca FILE` adds a CA for a server with a private certificate, because Python uses the system CA store rather than Chrome's; the path is remembered for that server.

The package (`debian/`, see below) installs the code under `/usr/lib/plurx-player`, the launcher as `/usr/bin/plurx-player`, and `plurx-player.desktop` with `MimeType=x-scheme-handler/plurx-player;`. dpkg's `update-desktop-database` trigger registers the scheme. There is deliberately no `xdg-mime` call in `postinst`: that runs as root and would edit root's associations, not the user's.

## mpv configuration

The launcher runs mpv with `--config-dir=$XDG_CONFIG_HOME/plurx-player/mpv` (by default `~/.config/plurx-player/mpv`), so plurx Player has its own optional mpv configuration and never reads the general `~/.config/mpv`: a stray `vo=` or `tone-mapping=` there would silently defeat the closed-form mapping. The directory does not exist by default; an `mpv.conf`, `input.conf` or script placed there applies to plurx Player only. mpv's built-in key bindings and on-screen controller work without it (the launcher does not pass `--no-config`). The options the player depends on (`--vo=gpu-next --gpu-api=vulkan --hwdec=auto-safe --target-colorspace-hint=yes`, the IPC socket, `--idle=once`) are on the command line, which mpv applies over any config file. `--ytdl=no` is always passed: the player only opens plurx URLs from an allowed server, and mpv's ytdl hook would otherwise run `yt-dlp` on anything else.

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

```sh
make linux-player-deb        # or: clients/linux-player/packaging/build-deb.sh [--out DIR] [--no-verify]
```

This builds `plurx-player_<version>_amd64.deb` in a disposable `ubuntu:26.10` container (pinned by digest in `build-deb.sh`), then installs and checks it in a second fresh one. Nothing is installed on the host; it needs only Docker. The output goes to `target/linux-player-deb/` (or `PLURX_DEB_OUT`): the `.deb`, its `.buildinfo` (the exact build environment), `SHA256SUMS`, `build.log` and `verify.log`. `PLURX_DEB_JOBS` (default 8) caps the compile jobs and the container's CPUs. It is not part of CI: it downloads sources and compiles for several minutes.

**What the package contains, and why.**

- `/usr/bin/plurx-player` and the launcher modules in `/usr/lib/plurx-player/plurx_player/`.
- `/usr/share/applications/plurx-player.desktop` with `MimeType=x-scheme-handler/plurx-player;`. dpkg's `update-desktop-database` trigger registers the scheme. There is deliberately no `xdg-mime` call in `postinst`: it would run as root and edit root's associations, not the user's. The only maintainer scripts are the ones `dh_python3` generates to byte-compile the launcher modules and to remove every `.pyc` on removal.
- A private libplacebo and mpv: `/usr/lib/plurx-player/lib/libplacebo.so.360` and `/usr/lib/plurx-player/bin/mpv`, the launcher's default mpv. mpv carries the RUNPATH `$ORIGIN/../lib`, so it loads the private libplacebo and never the system one. The private directory is not in `ld.so.conf` and the package installs no shlibs file, so the system mpv and everything else linked against the system libplacebo never see the private one. Plan §5 needs this: the closed-form Dolby Vision mapping is a patch to libplacebo/mpv, and patching the distribution's copy would change every program on the machine.
- `/usr/share/man/man1/plurx-player.1.gz`.
- `/usr/share/doc/plurx-player/private-build.txt`: the upstream versions, sha256 and the patches applied.

`Depends` is `python3 (>= 3.10)`, `zenity` and what `dpkg-shlibdeps` finds for the private build (Vulkan, FFmpeg 8, Wayland, VAAPI and the rest). The package version is `<launcher>+lp<libplacebo>.mpv<mpv>`, for example `0.1.0+lp7.360.1.mpv0.41.0`: the bundled upstream versions are visible in `apt` and `dpkg -l`, and the launcher part is bumped for launcher changes and for each change to the patch series.

**How the build works.** `packaging/upstream.lock` pins libplacebo 7.360.1 and mpv 0.41.0, the versions Ubuntu 26.10 ships, by URL and sha256. In the container, `packaging/prepare-sources.sh` fetches them, refuses a checksum mismatch, unpacks each into `upstream/<name>` as a git repository tagged `v<version>-plurx`, applies the patch series, and checks that `debian/changelog`'s version ends in `+lp<libplacebo>.mpv<mpv>`. `dpkg-buildpackage` then runs `debian/rules`, which calls `packaging/build-private.sh`: libplacebo with Vulkan, OpenGL, glslang, lcms2 and its built-in Dolby Vision support, as Ubuntu builds it; then mpv against that libplacebo with the Wayland, X11 and DRM outputs, Vulkan, VAAPI and CUDA hwdec, the system FFmpeg 8 and Lua 5.2 (for mpv's own on-screen controller; the launcher needs only the JSON IPC). Disc, capture and other inputs a plurx stream never uses are left out. The script fails the build unless mpv's RUNPATH is exactly `$ORIGIN/../lib` and the staged mpv resolves the staged libplacebo. libplacebo's test suite and these launcher tests run at build time.

**Patches drop in here.** Put the series in `packaging/patches/libplacebo/` and `packaging/patches/mpv/` as `0001-...patch`, `0002-...patch`; they apply in name order. A `git format-patch` file is applied with `git am`, anything else with `patch -p1`. Both directories are empty until the closed-form display management from the rog_hdr_dv work lands. A patched build shows itself: mpv prints `libplacebo version: v7.360.1 (v7.360.1-plurx-<n>-g<hash>)`, read at run time from the loaded library, and `mpv v0.41.0-plurx-<n>-g<hash>`. When the series changes, bump the launcher part of the version in `debian/changelog`. When Ubuntu moves to a new libplacebo or mpv, change `upstream.lock` and the changelog version together; `prepare-sources.sh` refuses one without the other.

**What the verification checks** (`verify.log`): the checksum; `lintian` has no errors; `dpkg -i` then `apt-get -f install` succeeds; the private mpv reports the `-plurx` libplacebo and `ldd` resolves it from `/usr/lib/plurx-player/lib`; the scheme is in `mimeinfo.cache` and `desktop-file-validate` passes; the launcher resolves its mpv to the private one; with the system `mpv` installed beside it, the system mpv uses the system libplacebo and the private one still uses its own; both render a 10-bit PQ HEVC clip through `vo=gpu-next` on Vulkan (Mesa's software device under Xvfb); these tests pass on the container's Python; and `dpkg -r` and `dpkg -P` remove everything, including the scheme registration, leaving the system mpv working.

## Focused evidence and remaining acceptance

```sh
python3 -m unittest discover -s clients/linux-player/tests -p 'test_*.py'
make linux-player-check
```

The tests run the real launcher in-process against a fake plurx server (`http.server` in a thread) and a fake mpv (`tests/fake_mpv.py`, which speaks the JSON IPC on the socket), on a virtual clock so cadence is exact; one test runs it as a subprocess and sends SIGTERM. They cover the call sequence and which calls carry the bearer, the caps fields against `caps.rs`, the 5 s/10 s cadence, progress = mpv time + origin, seeks inside and outside the range, control 404/410, teardown and revoke on exit, mpv crash, launcher crash and SIGTERM, the trust prompt, malformed URLs, redeem refusals, direct play, subtitles, the mapper line, and that the token reaches no file and no argv. The fake server is written from the M1 contract in the plan, not from a server.

Still required: the M1 server endpoints and a run against them; whether mpv reports the seek target as `time-pos` at its `seek` event on a growing playlist (the seek re-creation depends on it); the closed-form patch series and the libplacebo mapper log line it adds (M5, rog_hdr_dv); a desktop session check that a browser actually opens the scheme through the installed package; and the plan's M6 acceptance on the ROG.
