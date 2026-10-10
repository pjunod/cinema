# Installing plurx on macOS

One Mac, one command, a server that starts at login and restarts if it
crashes. This page is the whole macOS path from a fresh machine; the
reference material it rests on is the launchd section of the
[deployment guide](README.md#run-as-a-service--launchd-macos).

plurx runs on macOS as a **LaunchAgent in your login session**, not a
boot-time daemon. That is deliberate: VideoToolbox hardware transcoding needs
a logged-in GUI session. The consequences — it starts when you log in, and
stops when the Mac sleeps — are covered under [Keeping it up](#keeping-it-up).

## 1. Prerequisites

Each of these is a one-time step. Skip any you already have.

```sh
# Apple's compiler and git (the build compiles a little C)
xcode-select --install

# Homebrew — the installer puts plurxd in its prefix (no sudo on Apple Silicon)
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# Rust. rustup reads rust-toolchain.toml and fetches the pinned version itself
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

You do not install ffmpeg yourself. The installer fetches the exact build
plurx needs — see [the media engine](#the-media-engine-jellyfin-ffmpeg-813-1).

## 2. Install

From a checkout of this repository:

```sh
make install-macos
```

That one command:

1. builds `plurxd` from this checkout (`cargo build --locked --release`) —
   the first build takes several minutes;
2. downloads **Jellyfin FFmpeg 8.1.3-1** for your Mac's architecture,
   verifies its SHA-256 against the pinned value, and installs it next to the
   binary (below);
3. installs `plurxd` into the Homebrew prefix — `/opt/homebrew/bin` on Apple
   Silicon (no sudo), `/usr/local/bin` on Intel;
4. renders `~/Library/LaunchAgents/com.plurx.plurxd.plist` from
   [`com.plurx.plurxd.plist`](com.plurx.plurxd.plist) with your home directory,
   the `plurxd` path, and the Jellyfin `ffmpeg`/`ffprobe` paths;
5. loads and starts the agent, waits for `/readyz`, and prints the version the
   server reports.

It is not finished until the server answers. If it times out, it names the log
to read.

To see every command without running any of them:

```sh
make install-macos INSTALL_FLAGS=--dry-run
```

### Installing a prebuilt binary instead

If you already have a `plurxd` for macOS (a release build, or one built on
another Mac), skip Rust entirely:

```sh
make install-macos INSTALL_FLAGS='--binary ~/Downloads/plurxd'
```

`--prefix <dir>` puts the binary somewhere other than the Homebrew prefix; the
agent and the media engine follow it.

### The media engine: Jellyfin FFmpeg 8.1.3-1

plurx on macOS runs **Jellyfin FFmpeg 8.1.3-1**, the official portable build
from [jellyfin/jellyfin-ffmpeg](https://github.com/jellyfin/jellyfin-ffmpeg/releases/tag/v8.1.3-1),
and nothing else. That build carries what the macOS video path is built and
qualified against: VideoToolbox encode/decode and `scale_vt`, the
`tonemap_videotoolbox` and `tonemapx` tone mappers, the AC-4 decoder, and the
Dolby Vision bitstream filters. **Homebrew's `ffmpeg` is a different build that
lacks several of these; the installer never installs or uses it**, even if it
is on your `PATH`.

| | |
|---|---|
| Release | `v8.1.3-1` — the same pin as [`scripts/build-macos-video-ffmpeg`](../scripts/build-macos-video-ffmpeg) |
| Apple Silicon archive | `jellyfin-ffmpeg_8.1.3-1_portable_macarm64-gpl.tar.xz`, SHA-256 `22445d7299742749ad2eeb9ce87963d50def0357e45b3e6c7b69987c8365dbf6` |
| Intel archive | `jellyfin-ffmpeg_8.1.3-1_portable_mac64-gpl.tar.xz`, SHA-256 `cb2b5c154d49a6b6bfe29fa6c16510e85dcbf60b805764121a167b093d4bb6fb` |
| Installed to | `<prefix>/libexec/plurx/jellyfin-ffmpeg-8.1.3-1/` — `/opt/homebrew/libexec/plurx/jellyfin-ffmpeg-8.1.3-1/` on Apple Silicon; deliberately not on `PATH` |
| Agent settings | `PLURX_FFMPEG` and `PLURX_FFPROBE` point at the two files there |

An archive whose checksum differs stops the install before anything is
replaced. A rerun finds the pinned build already in place and does not
download it again.

**No internet on the Mac?** Download the archive for your architecture from the
release page above on any machine, copy it over, and pass it in — it is still
checksum-verified:

```sh
make install-macos INSTALL_FLAGS='--ffmpeg-archive ~/Downloads/jellyfin-ffmpeg_8.1.3-1_portable_macarm64-gpl.tar.xz'
```

**Upgrading from an older install** that pointed the agent at Homebrew's
ffmpeg: `make install-macos` repoints `PLURX_FFMPEG`/`PLURX_FFPROBE` at the
Jellyfin build and says so. Any engine that already reports itself as Jellyfin
FFmpeg is left as you set it, as is everything else in your plist.

To confirm what the agent is running:

```sh
/usr/libexec/PlistBuddy -c 'Print :EnvironmentVariables:PLURX_FFMPEG' ~/Library/LaunchAgents/com.plurx.plurxd.plist
"$(/usr/libexec/PlistBuddy -c 'Print :EnvironmentVariables:PLURX_FFMPEG' ~/Library/LaunchAgents/com.plurx.plurxd.plist)" -version | head -1
# ffmpeg version 8.1.3-Jellyfin ...
```

## 3. First run

When the agent starts, macOS asks once whether `plurxd` may **accept incoming
network connections**. Allow it, or other devices on your network cannot reach
the server.

Then:

1. Open <http://localhost:32400> (or `http://<this-mac>:32400` from another
   device) and create the first administrator account.
2. **Settings → Libraries → Add & scan.** Use the real path on this Mac, for
   example `/Users/you/Movies` or `/Volumes/Media/Movies`. There is no
   container path translation on a native install.
3. Optionally add a TMDB key under **Settings → Metadata** for posters and
   metadata.

The server listens on `32400`, and on `8096` for Jellyfin-compatible clients.

### Media folders macOS protects

macOS privacy controls cover `~/Desktop`, `~/Documents`, `~/Downloads`,
external drives and network volumes. A background agent has no window to show
the permission prompt from, so the first sign is a library that scans zero
files with permission errors (see
[reading library scan status](../docs/OPERATIONS.md#reading-library-scan-status)).

Fix it once in **System Settings → Privacy & Security → Full Disk Access**:
press **+**, press **⌘⇧G**, enter the path the installer printed
(`/opt/homebrew/bin/plurxd` on Apple Silicon), add it, then restart the agent:

```sh
launchctl kickstart -k gui/$(id -u)/com.plurx.plurxd
```

The grant is tied to that exact binary. An upgrade replaces it, so check this
entry again after upgrading if scans start failing. A library under `~/Movies`
or a plain folder elsewhere on the internal disk does not need it.

## 4. Day to day

| Task | Command |
|---|---|
| Is it running? | `launchctl print gui/$(id -u)/com.plurx.plurxd \| grep -E 'state\|pid'` |
| Follow the log | `tail -f ~/Library/Logs/plurxd.log` |
| Health | `curl -fsS http://127.0.0.1:32400/readyz` |
| Restart | `launchctl kickstart -k gui/$(id -u)/com.plurx.plurxd` |
| Stop until next login | `launchctl bootout gui/$(id -u)/com.plurx.plurxd` |
| Upgrade | `git pull && make install-macos` |
| Uninstall | `make uninstall` |

**Upgrading** stops the agent, replaces the binary, and starts it again. Your
plist is never overwritten, so edits you made to it survive.

**Uninstalling** removes the agent, the binary and its Jellyfin FFmpeg, and
keeps your data.

Data — accounts, keys, library metadata, watch state — lives in
`~/Library/Application Support/plurx`. Back that folder up; nothing else is
needed to restore a server.

### Changing settings in the agent

Server environment lives in the plist's `EnvironmentVariables`. Edit it with
`plutil`, then restart:

```sh
PLIST=~/Library/LaunchAgents/com.plurx.plurxd.plist
plutil -replace EnvironmentVariables.PLURX_BIND -string 127.0.0.1:32400 "$PLIST"   # this Mac only
launchctl bootout gui/$(id -u)/com.plurx.plurxd
launchctl bootstrap gui/$(id -u) "$PLIST"
```

A plist change needs the `bootout`/`bootstrap` pair; `kickstart` alone restarts
the process with the environment launchd already loaded. Every `PLURX_*`
variable is listed in [Operations](../docs/OPERATIONS.md).

## Keeping it up

A LaunchAgent runs only while you are logged in, and only while the Mac is
awake. For a Mac that is mainly a server:

- **Sleep.** In **System Settings → Energy** (or **Battery → Options** on a
  laptop), turn on *Prevent automatic sleeping when the display is off*. A
  sleeping Mac answers nothing.
- **Reboots.** After a restart the server comes back when you log in. With
  FileVault on, macOS cannot log in automatically, so someone has to log in
  once after each reboot.
- **Running with nobody logged in** needs a system LaunchDaemon. This
  repository does not ship one, and a daemon loses VideoToolbox, so
  transcoding falls back to software. The
  [deployment guide](README.md#run-as-a-service--launchd-macos) explains the
  trade-off.

## Limits on macOS

- **Permanent Dolby Vision conversion must stay Off.** The agent runs as your
  own login account, which shares an identity with every other app you run;
  that breaks the dedicated-account boundary the conversion worker requires.
  On-the-fly playback is unaffected. Use the Linux container or systemd
  install if you need permanent conversion.
- **Docker Desktop is not a substitute.** A Linux container cannot use
  VideoToolbox; on a Mac, the native install above is the hardware-accelerated
  path.

## When it does not start

| Symptom | Look at |
|---|---|
| `make install-macos` says `is not the pinned Jellyfin FFmpeg` | The download was cut short or the file is a different build. Rerun, or fetch the exact archive named in [the media engine](#the-media-engine-jellyfin-ffmpeg-813-1) and pass `--ffmpeg-archive`. |
| `make install-macos` says `cargo is not on PATH` | Run `source "$HOME/.cargo/env"` (or open a new terminal) after installing Rust, or pass `--binary`. |
| `make install-macos` times out waiting for `/readyz` | `tail -100 ~/Library/Logs/plurxd.log`. A port already in use on `32400` is the usual cause. |
| Agent is loaded but `state` is not `running` | `launchctl print gui/$(id -u)/com.plurx.plurxd` shows `last exit code`; the log shows why. |
| Works on this Mac, not from other devices | The incoming-connections prompt was denied. Allow `plurxd` in **System Settings → Network → Firewall → Options**. |
| A library scans zero files | [Media folders macOS protects](#media-folders-macos-protects). |
| Everything plays in software | The agent must run in a logged-in GUI session. Check that you used `make install-macos`, not a LaunchDaemon. |
