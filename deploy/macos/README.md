# noirr cinema for macOS

This archive contains a universal `plurxd` server (Apple Silicon and Intel),
the installer, the launchd agent template it renders, and
`plurx-source-parser.wasm` — the sandboxed FFprobe build plurxd reads decoder
facts through, which the installer puts beside the pinned FFprobe. The
`subtitle-engine/` directory carries the pinned whisper.cpp 1.9.2 CPU
recognizer, its verified build manifest, speech diagnostic and MIT notices.

Install it as a login agent that starts with your session and restarts if it
crashes, from this directory:

```sh
./install
```

The installer puts `plurxd` in the Homebrew prefix (`/usr/local/bin` without
Homebrew), downloads and checksum-verifies the pinned Jellyfin FFmpeg build it
runs against, renders `~/Library/LaunchAgents/com.plurx.plurxd.plist`, starts
the agent and waits until the server answers. Then open
<http://localhost:32400> and create the first administrator.

The installer verifies and stages the complete subtitle engine before stopping
the agent, then installs it beside `plurxd` as `subtitle-engine/`. A failed
update restores the prior engine directory. Models are installed explicitly
through server settings and remain in the server data directory across
upgrades; the archive downloads no model during installation.

`./install --dry-run` prints every step without running any of them, and
`./install --uninstall` removes the agent and the binary while keeping your
library data in `~/Library/Application Support/plurx`.

The binary is ad-hoc signed, not notarized. The installer clears the download
quarantine from the copy it installs, as it does for FFmpeg.

The full macOS guide — offline installs, protected media folders, sleep and
upgrades — is `deploy/MACOS.md` in the source distribution.
