# noirr cinema for macOS

This archive contains a universal `plurxd` server (Apple Silicon and Intel),
the installer, and the launchd agent template it renders.

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

`./install --dry-run` prints every step without running any of them, and
`./install --uninstall` removes the agent and the binary while keeping your
library data in `~/Library/Application Support/plurx`.

The binary is ad-hoc signed, not notarized. The installer clears the download
quarantine from the copy it installs, as it does for FFmpeg.

The full macOS guide — offline installs, protected media folders, sleep and
upgrades — is `deploy/MACOS.md` in the source distribution.
