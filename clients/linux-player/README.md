# plurx Player (Linux)

This is the launcher half of the native Linux player in [the Linux player plan](../../docs/clients/LINUX-PLAYER-PLAN.md). Browsing stays in the plurx web app. When Settings → Playback → **Dolby Vision playback on Linux** says **plurx Player (Linux)**, a cold Play of a local Dolby Vision file in a Linux browser mints a one-time handoff code and opens `plurx-player://play?server=<origin>&code=<code>`. Other formats and clients on other operating systems keep playing in the web UI; the setting is disabled outside Linux. This launcher handles that URL: it redeems the code for a token scoped to one file, asks the server how to deliver the file, plays it in mpv, and keeps the server informed until mpv closes. It is Python 3 with the standard library only (`urllib`, `json`, a Unix socket for mpv's JSON IPC, `subprocess`), so it installs on any Ubuntu with no dependencies beyond `zenity`.

Status: milestone M4 built against a fake server and a fake mpv; the server handoff endpoints (M1) are being built in parallel, so nothing here has met a real plurx server yet. The M5 package is built: `make linux-player-deb` produces an Ubuntu 26.10 `.deb` with a private mpv and libplacebo, and mpv maps Dolby Vision with plurx's closed-form display management (the server's own `tools/dv_processing/dv_dm.h`; [plan §5](../../docs/clients/LINUX-PLAYER-PLAN.md#5-closed-form-mapping-in-mpv)). Acceptance on the ROG is M6.

## Install and run

Install the package (see [Packaging](#packaging)), or run it from a checkout with a system mpv:

```sh
clients/linux-player/plurx-player 'plurx-player://play?server=http%3A%2F%2Fmedia1%3A8096&code=CODE'
```

`man plurx-player` covers the options, the URL and the files below. `--mpv PATH` picks the mpv binary; otherwise it uses `/usr/lib/plurx-player/bin/mpv` (the private build) when installed, then `mpv` on `PATH`. `PLURX_PLAYER_MPV` does the same as `--mpv`. `--ca FILE` adds a CA for a server with a private certificate, because Python uses the system CA store rather than Chrome's; the path is remembered for that server.

The package (`debian/`, see below) installs the code under `/usr/lib/plurx-player`, the launcher as `/usr/bin/plurx-player`, and `plurx-player.desktop` with `MimeType=x-scheme-handler/plurx-player;`. dpkg's `update-desktop-database` trigger registers the scheme. There is deliberately no `xdg-mime` call in `postinst`: that runs as root and would edit root's associations, not the user's.

## mpv configuration

The launcher runs mpv with `--config-dir=$XDG_CONFIG_HOME/plurx-player/mpv` (by default `~/.config/plurx-player/mpv`), so plurx Player has its own optional mpv configuration and never reads the general `~/.config/mpv`: a stray `vo=` or `tone-mapping=` there would silently defeat the closed-form mapping. The directory does not exist by default; an `mpv.conf`, `input.conf` or script placed there applies to plurx Player only. Two Dolby Vision options belong there: `dv-brightness=absolute` maps for the panel's real luminance (Dolby's absolute rendering) instead of the default `relative`, which follows the desktop's HDR brightness setting; `dv-cm4-trims=no` / `dv-cm29-trims=no` turn off each family's creative trims. mpv's built-in key bindings and on-screen controller work without it (the launcher does not pass `--no-config`). The options the player depends on (`--vo=gpu-next --gpu-api=vulkan --hwdec=auto-safe --target-colorspace-hint=yes`, the IPC socket, `--idle=once`) are on the command line, which mpv applies over any config file. `--ytdl=no` is always passed: the player only opens plurx URLs from an allowed server, and mpv's ytdl hook would otherwise run `yt-dlp` on anything else. mpv also starts without `LD_LIBRARY_PATH`, `LD_PRELOAD` or `LD_AUDIT`, which would otherwise outrank the private build's own library path or inject code into it (see [Packaging](#packaging)).

## What it does, in order

1. **Parse and trust.** The URL must be exactly `plurx-player://play?server=…&code=…`; `server` must be an http(s) origin with no path, user information, query or fragment. Anything else is refused before any network or dialog. The origin must be in `~/.config/plurx-player/servers.json` (`$XDG_CONFIG_HOME` respected); the first time a new origin appears, a `zenity --question` names it and the player refuses unless the user allows it. Plain http is allowed only through that same question, which says the connection is not encrypted. Any web page can navigate to this scheme, so without the list a hostile page could point mpv and libavcodec at its own server.
2. **Redeem.** `POST /api/v1/playback-handoffs/redeem` with `{code, device: "plurx Player (Linux)"}` and no bearer. A 410 (expired, used, or minted by a revoked sign-in) says so in a dialog. If the answer carries the server's canonical `origin`, it must equal `server`.
3. **Decide and create.** `POST /api/v1/files/{id}/decision?force=auto[&audio=][&subtitle=]` with the caps v2 document of plan §3.2 (field names from `crates/plurx-core/src/playback/caps.rs`): HEVC Main/Main10 to 2160 with Dolby Vision profiles 5, 7 and 8 (7 because the player composes the full enhancement layer itself, so the server sends the original stream), `display {hdr, dolby_vision}` with no `max_nits`, every codec mpv decodes without passthrough, `hls_hevc_sample_entries ["hvc1","hev1"]`, `dv_transport "hls"`, no subtitle overlays. Then execute `delivery` ([API.md §7.2](../../docs/API.md)): `direct` plays the file URL; `remux` creates an HLS copy session through `sessions_url` (`copy`, `aac`, `preserve_dolby_vision` from the plan); `transcode` creates one with no `height`, because Auto is the server's choice.
4. **Play.** mpv starts with `--input-ipc-server` on a socket in a private 0700 directory under `$XDG_RUNTIME_DIR`, `--vo=gpu-next --gpu-api=vulkan --hwdec=auto-safe --target-colorspace-hint=yes` and `--idle=once`. The start position is set as mpv's `start` property, adjusted by the session's `media_origin_ms`. A text subtitle loads as a sidecar (`sub-add` of `/api/v1/files/{id}/subs/{n}`) shifted by `-media_origin_ms`, since its cues are on the source timeline. An image subtitle asks the server to burn it; if the HDR guard refuses (422), playback continues without it and the OSD says why.
5. **Keep the session alive.** A passive control exchange every 5 s: no `supported_actions`, fences from the create response (API.md §10). Progress every 10 s while playing, and on pause, after a seek, and on exit. Positions are always mpv's time plus `media_origin_ms`, added in one place. A control 404 or 410 stops mpv after showing the server's reason on the OSD for 3 s, which is how Stop in Activity reaches this client. A 401 anywhere (the user signed out of the web app) stops it too.
6. **Seek.** On mpv's `seek` event the target is compared with what the session has produced (`published_end_ms` from `/hls/{s}/status`, or mpv's own duration). Inside it, mpv seeks in the playlist. Outside it, the player pauses, deletes the session, creates one that starts at the target, loads the new playlist and takes the new origin: seeking past the end of a growing playlist would stall behind the encoder. A VOD session or a direct file never needs this.
7. **Tear down.** On exit, mpv closing, mpv crashing, SIGTERM/SIGINT/SIGHUP or a launcher exception: final progress, `DELETE /hls/{session}`, then `DELETE /playback-handoffs/self`. A second signal during teardown is ignored. SIGKILL cannot be caught; the token then expires with its 6 h lifetime or with the web sign-in.
8. **Log the mapper.** One `POST /client-log` line per playback (`event: native_mapper`) carries the Dolby Vision profile (mpv's `track-list` `dolby-vision-profile`), the mapper and the target nits (`video-target-params` `max-luma`), and the OSD shows the same. The mapper is what the private mpv logs: a `plurx mapper: closed-form CM v4.0 target=… gamut=…` line (or `plurx mapper: none (<reason>)`), or a bare `CM vX` from an unpatched libplacebo. When nothing reports it, it says `unknown` rather than guessing.

The token is in memory only. It never enters a file, a log line or mpv's command line (which `ps` shows to every user). When a bearer route must be fetched by mpv itself — the direct file or a sidecar subtitle — the header is set over the private IPC socket. HTTP redirects are never followed, so the header cannot be carried to another host. Session routes use the session id as their capability and carry no bearer.

## HDR depends on the compositor

HDR output needs a compositor with `wp_color_manager_v1` in HDR mode, such as GNOME in HDR mode. An accurate target needs one that reports the panel's real luminance, as the rog_hdr_dv nativehdr Mutter does; stock compositors may report a default instead, and the mapper line will show that target.

## Packaging

```sh
make linux-player-deb        # or: clients/linux-player/packaging/build-deb.sh [--out DIR] [--no-verify] [--no-rebuild-check]
```

This builds the Debian source package and `plurx-player_<version>_amd64.deb` in a disposable `ubuntu:26.10` container, checks that the verification really fails when a check fails, installs and checks the package in a second fresh container, then builds everything again and requires the two builds to be byte-identical. Nothing is installed on the host; it needs only Docker. The output goes to `target/linux-player-deb/` (or `PLURX_DEB_OUT`):

- `plurx-player_<version>_amd64.deb`;
- the source package: `plurx-player_<version>.dsc`, `plurx-player_<upstream>.orig.tar.gz` (the launcher and the patch series), `.orig-libplacebo.tar.gz` and `.orig-mpv.tar.gz` (upstream's tarballs, byte for byte), and `plurx-player_<version>.debian.tar.xz`;
- `SHA256SUMS` over all of those, the `.buildinfo` (the exact build environment), `build.log`, `verify.log` and `rebuild.log`.

`PLURX_DEB_JOBS` (default 8) caps the compile jobs and the container's CPUs. It is not part of CI: one run downloads sources and compiles twice, about ten minutes in all.

**What the package contains, and why.**

- `/usr/bin/plurx-player` and the launcher modules in `/usr/lib/plurx-player/plurx_player/`.
- `/usr/share/applications/plurx-player.desktop` with `MimeType=x-scheme-handler/plurx-player;`. dpkg's `update-desktop-database` trigger registers the scheme, which is why the package recommends `desktop-file-utils`. There is deliberately no `xdg-mime` call in `postinst`: it would run as root and edit root's associations, not the user's. The only maintainer scripts are the ones `dh_python3` generates to byte-compile the launcher modules and to remove every `.pyc` on removal.
- A private mpv, `/usr/lib/plurx-player/bin/mpv`, the launcher's default mpv, with a private libplacebo 7.387 linked in. Plan §5 needs them private: the closed-form Dolby Vision mapping is a patch to mpv, and FEL composition needs newer libplacebo and mpv than Ubuntu ships, and patching the distribution's copy would change every program on the machine.
  - libplacebo is linked in statically, its global symbols renamed `plurx_pl_*` and hidden, not shipped as a shared object. The system FFmpeg's `libavfilter` loads the system libplacebo (another major version) into the same process, and libplacebo's symbols are unversioned: a private `libplacebo.so` would take over every call `libavfilter` makes into libplacebo, with the wrong ABI. Renamed and hidden, each copy is reachable only from its own caller (hiding alone cannot work: the linker refuses to hide a symbol a linked library references), and there is no private shared object for `LD_LIBRARY_PATH` to swap. The launcher also starts mpv without `LD_LIBRARY_PATH`, `LD_PRELOAD` or `LD_AUDIT`, so nothing in the desktop session can redirect or inject into it.
  - The system mpv and everything else on the system never see the private libplacebo.
  - libplacebo links glslang and SPIRV-Tools statically, as Ubuntu's does, because Ubuntu ships only their static libraries. `Built-Using` names their exact versions, and their copyright files are installed as `copyright.glslang` and `copyright.spirv-tools`.
- `/usr/share/man/man1/plurx-player.1.gz`.
- `/usr/share/doc/plurx-player/private-build.txt`: the upstream versions, sha256, `git describe`, the patches applied, the statically linked glslang and SPIRV-Tools, the apt snapshot, and the corresponding source package.

`Depends` is `python3 (>= 3.10)`, `zenity`, `libdovi3 (<< 3.4~)` (the RPU parser; the pinned header's layouts are proven against 3.3) and what `dpkg-shlibdeps` finds for the private build (Vulkan, FFmpeg 8, Wayland, VAAPI and the rest). `Recommends` adds `desktop-file-utils` and a Vulkan driver (`mesa-vulkan-drivers | vulkan-icd`). The version is `<launcher>+lp<libplacebo>.mpv<mpv>-<revision>`, for example `0.3.0+lp7.387.0.mpv0.41.0+git20261026.e59d90c-1` (an mpv commit pin is `<last release>+git<date>.<short commit>`). The newest changelog entry also records the closed-form equations' hash (`dm <12 hex>`), and a test fails until it matches `tools/dv_processing`, so changed equations always come with a new version. The bundled upstream versions are visible in `apt` and `dpkg -l`. The launcher part is bumped for launcher changes and for each change to the patch series, and the revision is bumped for packaging-only changes.

**Corresponding source.** mpv is built GPL and libplacebo is LGPL, so every build also publishes the Debian source package the `.deb` was built from. The build makes the source package first, unpacks it afresh, and builds the binary from that tree; `dpkg-source -x plurx-player_<version>.dsc` then `dpkg-buildpackage -b` rebuilds the same package. The launcher, the patch series and `debian/` are its own parts; libplacebo and mpv are its `libplacebo` and `mpv` components.

**How the build works.**

1. **Environment.** `packaging/upstream.lock` pins an apt snapshot timestamp. `packaging/apt-snapshot.sh` points apt at `snapshot.ubuntu.com` at that time, so every package in the build and verification containers is the one that was current then, whenever the build runs. `build-deb.sh` mounts a cache of that snapshot's apt indexes and packages into every container (`target/linux-player-apt-cache`, or `PLURX_DEB_APT_CACHE`), and apt retries the snapshot service's 503s: once the indexes have been fetched and verified in full, later builds reuse them without contacting the mirror. One build at a time uses a cache.
2. **Sources.** The lock also pins libplacebo 7.387.0 and an mpv master commit (the first with Dolby Vision FEL composition; Ubuntu 26.10 ships 7.360.1 and 0.41.0), by URL and sha256. `packaging/fetch-sources.sh` makes the launcher orig tarball reproducibly, fetches the upstream tarballs, refuses a checksum mismatch, and checks that the changelog's upstream version ends in `+lp<libplacebo>.mpv<mpv>`. `dpkg-source -b` then makes the source package.
3. **Build.** In the unpacked source package, `dpkg-buildpackage` runs `debian/rules`, which calls `packaging/build-private.sh`:
   - each component is copied, made a git repository tagged `v<version>-plurx`, and patched with its series by `packaging/apply-series.sh`;
   - libplacebo is built with Vulkan, OpenGL, glslang, lcms2 and its built-in Dolby Vision support, as Ubuntu builds it;
   - mpv is built against it with the Wayland, X11 and DRM outputs, Vulkan, VAAPI and CUDA hwdec, the system FFmpeg 8 and Lua 5.2 (for mpv's own on-screen controller; the launcher needs only the JSON IPC). Disc, capture and other inputs a plurx stream never uses are left out.
   - The build fails unless libplacebo was built only as a static library, its symbols were renamed, and mpv needs no shared libplacebo, has no rpath, neither exports nor imports a libplacebo symbol, and does not contain the build path.
   - libplacebo's test suite and these launcher tests run at build time. The launcher tests use `packaging/run-tests`, which fails on any skip. The one skip allowed there is the caps drift check, because the source package does not carry the server's `caps.rs`; `make linux-player-check` and the package verification run it.
4. **Byte-identical rebuild.** The build always runs in `/build/plurx-player` and dates everything from the changelog, so the same inputs give the same bytes. `build-deb.sh` builds a second time and compares every file in `SHA256SUMS`.

**Patches drop in here.**

- Put each series in `packaging/patches/libplacebo/` or `packaging/patches/mpv/`, and list the files in order in that directory's `series` file.
- The build fails if `series` and the directory disagree in either direction, or if any patch does not apply exactly. A `git format-patch` file goes through `git am`, anything else through `git apply`, with no fuzz and no three-way merge.
- The libplacebo series is empty. The mpv series is one patch, the closed-form Dolby Vision mapping. It compiles against `video/out/placebo/plurx/`, which `build-private.sh` fills after applying the series: `dv_dm.h` and `dm_closed_form.h` (copied from `tools/dv_processing` by `build-in-container.sh`, so they travel in the launcher orig tarball) and the pinned libdovi header `packaging/libdovi/rpu_parser.h`. `private-build.txt` records their sha256.
- A patched build shows itself. mpv prints `libplacebo version: v7.387.0 (v7.387.0-plurx)`, read at run time from the loaded library, and `mpv v0.41.0+git20261026.e59d90c-plurx-3-g<hash>`.
- When the series changes, bump the launcher part of the version in `debian/changelog`.
- When Ubuntu moves to a new libplacebo or mpv, change `upstream.lock` and the changelog version together; `fetch-sources.sh` refuses one without the other.

**What the verification checks** (`verify.log`), in order:

1. The checksums. A self-test run, with one digest deliberately wrong, must fail at this check before the real run counts.
2. `lintian` on the `.deb` and the source package: errors fail, and warnings are listed.
3. `dpkg -i` then `apt-get -f install` succeeds.
4. The private mpv reports the `-plurx` libplacebo, needs no shared libplacebo, has no rpath, and neither exports nor imports a libplacebo symbol, and the package installs no private library. `ldd` shows no libplacebo but the system one (loaded for `libavfilter`) alone, beside the system `mpv`, and with `LD_LIBRARY_PATH` pointing at the system libraries, and the private mpv still reports its own libplacebo under that `LD_LIBRARY_PATH`.
5. The scheme is in `mimeinfo.cache`, `desktop-file-validate` passes, and no maintainer script calls `xdg-mime`.
6. The launcher resolves its mpv to the private one.
7. The system mpv uses the system libplacebo.
8. Both mpvs render a 10-bit PQ HEVC clip through `vo=gpu-next` on Vulkan (Mesa's software device under Xvfb).
9. The installed launcher's exact mpv command line plays through gpu-next without reading a hostile `~/.config/mpv/mpv.conf`, and never runs the ytdl hook on a URL mpv cannot open. Each of these has a positive control showing the same mpv does read that file without `--config-dir`, and does run the hook with `--ytdl=yes`.
10. These tests pass on the container's Python with no skip.
11. `dpkg -r` and `dpkg -P` remove everything, including the scheme registration, leaving the system mpv working.

Every check is written to fail the script; `tests/test_packaging.py` rejects the `cmd && pass` form that would not.

## Focused evidence and remaining acceptance

```sh
python3 -m unittest discover -s clients/linux-player/tests -p 'test_*.py'
make linux-player-check
```

The tests run the real launcher in-process against a fake plurx server (`http.server` in a thread) and a fake mpv (`tests/fake_mpv.py`, which speaks the JSON IPC on the socket), on a virtual clock so cadence is exact; one test runs it as a subprocess and sends SIGTERM. They cover the call sequence and which calls carry the bearer, the caps fields against `caps.rs`, the 5 s/10 s cadence, progress = mpv time + origin, seeks inside and outside the range, control 404/410, teardown and revoke on exit, mpv crash, launcher crash and SIGTERM, the trust prompt, malformed URLs, redeem refusals, direct play, subtitles, the mapper line, and that the token reaches no file and no argv. The fake server is written from the M1 contract in the plan, not from a server.

Still required: the M1 server endpoints and a run against them; whether mpv reports the seek target as `time-pos` at its `seek` event on a growing playlist (the seek re-creation depends on it); the closed-form patch series and the libplacebo mapper log line it adds (M5, rog_hdr_dv); a desktop session check that a browser actually opens the scheme through the installed package; and the plan's M6 acceptance on the ROG.
