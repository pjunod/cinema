# Deploying plurx

plurx is a single static binary (`plurxd`) plus an embedded web app. Pick the
path that matches your setup.

This document deploys the server. Native Android and Apple delivery runs from
the macOS Ansible controller because that host owns adb pairing, Xcode signing,
and App Store Connect authentication; see
[`docs/PUBLISHING.md`](../docs/PUBLISHING.md#ansible-owns-the-repeatable-mobile-deploy).

For Apple, `scripts/ship --apple` means test, archive, and upload to
TestFlight. There is no direct-install step for the fleet: after the upload is
accepted, each iPhone, iPad, and Apple TV installs that build through
TestFlight. “Deploy to all Apple devices” therefore includes that explicit
on-device install/update step.

## Docker / Compose (recommended for homelabs)

One command from the repository root brings the stack up for the first time:

```sh
make install-docker
```

It writes `deploy/.env` (with your uid/gid) and
`deploy/docker-compose.override.yml` from their examples when they are
missing, creates the data directory named in `.env`, runs `make docker-up`,
and then waits for `/readyz` and prints the version the server reports.
Host-specific bits (media mounts, GPU, and shared Docker networks) live in
that untracked override file, so pulling updates never conflicts with local
edits — put your mounts and GPU there and run `make docker-up` again; that is
the deploy from then on. By hand, the same first run is:

```sh
cd deploy
cp .env.example .env                   # PUID/PGID, ports, the data directory
cp docker-compose.override.example.yml docker-compose.override.yml
$EDITOR docker-compose.override.yml   # your media mounts (host:container:ro), your GPU
cd .. && make docker-up                  # builds from source; stamps the commit into the build
```

Open `http://<host>:32400` and create your admin account. If Plex still owns
TCP 32400, set `PLURX_HTTP_PORT` in `.env` and use that port instead.
Library paths in
the web UI are the *container-side* paths (e.g. `/media/movies`). For
hardware transcode, uncomment the GPU block in your override (Intel/AMD via
`/dev/dri`, NVIDIA via the container toolkit). If another service (a
still-running Plex) owns UDP 32414, set `PLURX_GDM_PORT` in `.env`
(see `.env.example`).

The base Compose stack keeps `plurxd` on ordinary Docker networking and
publishes TCP `PLURX_HTTP_PORT` (32400 by default) plus UDP 32414. Your
override may therefore attach it to an external network such as `media`. A
separate `plurx-discovery` companion uses host networking only for Bonjour
`_plurx._tcp`; it reads the server identity through the selected published
port and advertises that port with the host's LAN address. That split keeps
automatic iPhone, iPad, Apple TV, and Android discovery without moving the
media server off the networks its peer services use.

When `PLURX_SERVER_NAME` is still the default `plurx`, the companion advertises
the Docker host name plus its LAN address, so a picker says
`lab6 · 10.42.1.20` instead of showing another anonymous `plurx` row. Set a
custom `PLURX_SERVER_NAME` when a room or role name is clearer; the custom name
replaces the host name while the address remains visible. Cluster nodes are
named the same way — every node of one logical server reports the same
`PLURX_SERVER_NAME`, and it is the host name and the address that tell them
apart. A node with neither falls back to the first twelve characters of its
node id, which is the only case where a picker shows a UUID.

### Writable media is a narrow, explicit opt-in

Every shipped media mount remains read-only. Keep permanent Dolby Vision
Profile 7 → 8.1 conversion **Off** for a library unless all of the following are
true:

- A tested backup exists outside the writable library. **Keep the Profile 7
  original** remains the safer policy, but a sibling original is not a backup
  of the filesystem that contains it.
- plurxd runs as a dedicated Unix uid that no downloader, organizer, shell job,
  or other application uses. The private cleanup anchor isolates other users;
  POSIX cannot isolate a second process deliberately given the daemon's uid.
- The exact library path is writable and traversable by that uid. Other
  libraries and the surrounding media root remain read-only.
- Source, hidden sibling workspace, recovery hard link, witness, and cleanup
  anchor live on one mounted filesystem that supports hard links, atomic rename,
  Unix ownership/modes, and durable file and directory sync. Object-store/FUSE
  adapters that only approximate those operations do not satisfy the contract.
- Every voter that may lease conversion work sees that same mounted filesystem
  with the same write semantics. If one voter cannot, keep conversion Off.

The full publication, recovery-guard, witness, and tombstone lifecycle is in
[`docs/OPERATIONS.md`](../docs/OPERATIONS.md#permanent-dolby-vision-profile-7--81-conversion).
Silence is not proof of a mount: the worker deliberately refuses cleanup when
the source-parent witness is absent.

For Compose, set `PUID` and `PGID` to a dedicated host service account, grant
that account access to only the opted-in library, and add a separate writable
bind rather than changing the broad `/media` bind. The default uid `1000` is
commonly the interactive login user and does **not** satisfy this boundary for
permanent conversion.

```yaml
services:
  plurxd:
    volumes:
      - /mnt/nas/media:/media:ro
      - /mnt/nas/media/dv-conversion:/media-dv-conversion:rw
```

Add `/media-dv-conversion` as its own library in Settings. Do not overlap a
writable child with a library configured through the read-only parent path;
one container path should name each opted-in file. Confirm the effective uid
with `docker compose exec plurxd id`, prove that account can create, hard-link,
sync, rename, and remove a sibling test file on the real mount, then delete the
test artifacts before enabling conversion.

### Recording needs a writable DVR root, and no shipped mount is a good one

Every *media* mount this stack ships is read-only, which is the right default.
The one writable bind it does ship is the data volume,
`${PLURX_DATA:-/srv/plurx}:/var/lib/plurx`, and nothing in the code stops you
pointing `dvr.root` inside it — on a single-node install that will record
today. Do not. That volume holds the database, and it is node-local: captures
would compete with the database for space, a full disk there takes the server
down rather than just the DVR, and no other node can serve what the owner
wrote.

So in practice recording needs a bind the shipped Compose file does not create
for you. Add it in `docker-compose.override.yml`:

```yaml
services:
  plurxd:
    volumes:
      - /mnt/shared/plurx-dvr:/dvr:rw
```

Then set `dvr.root` in Settings to the **container** path (`/dvr` above).

Three requirements, and the last two are the ones that bite:

- **Writable** by the uid the container runs as (`PUID`:`PGID`). Create the host
  directory owned by that account before the first start.
- **The same underlying filesystem on every node.** The owner node writes the
  capture and any node may serve it back through the ordinary VOD path, so a
  path that exists on one node only produces recordings the rest of the fleet
  cannot play. In a cluster this means a real shared filesystem — the same
  requirement the shared cache has, for the same reason.
- **Free space above the floor**, which defaults to **50 GB**
  (`dvr.free_floor_gb`). This one is silent and absolute: below the floor every
  scheduled row goes to `Conflict` with "disk below the free-space floor" and
  *nothing records at all*, on a root that is present and perfectly writable.
  A small test share is the easiest way to be defeated by this.

Do not satisfy this by making the `/media` bind writable. Keep the DVR root a
separate path outside the read-only media root, exactly as permanent Dolby
Vision conversion keeps its opted-in library separate above — and note that the
owner creates a `Recordings` library pointed at the DVR root the first time it
sweeps, so a root nested inside an existing library path gives you two
libraries over the same files.

Settings → Developer lists what recording needs and whether each part is met.
**It is advisory and gates nothing** — it exists so a failure has somewhere to
point. Read the whole card before blaming the mount, because the row that is
red is often not this one:

- *The tuner owner can write to the DVR root* — a real probe, but only on the
  owner. On any other node it reports `Unobservable`.
- *Every node can read the DVR root* — always `Unobservable` in a cluster: a
  node sees its own filesystem and no peer's. `Unobservable` is not `Met`;
  check it yourself with `ls` on each node. (On a single-node install it says
  so instead — the node that records is the node that serves.)
- *The DVR root has room above the floor* — the 50 GB default above.
- *The guide reaches far enough to schedule from* — `Unmet` on a free
  HDHomeRun tier, which records what is on but gives a series rule nothing to
  schedule. A subscription tier or an XMLTV source is what changes it, and no
  amount of fixing the mount will.

Recording itself runs only on the Live TV owner node, so `ps` on any other node
shows nothing even when everything is working.

Do not add `network_mode: host` to `plurxd`. Compose forbids one service from
declaring both host networking and `networks`, so doing that recreates the
configuration error the companion is designed to avoid. If the Docker host
cannot provide host networking, the server still works at
`http://<host>:<PLURX_HTTP_PORT>`, but native clients must use manual entry.

### Pull a prebuilt fleet image

The Compose service still builds from the checked-out Dockerfile by default.
Set `PLURX_IMAGE` only when one builder publishes an image that every server
should pull:

```bash
git fetch origin
git switch --detach origin/main # the checkout must match the published image
cd deploy
printf '%s\n' \
  'PLURX_IMAGE=forge.lan:3000/noirr/plurxd:main' >> .env
cd ..
make docker-image-up         # pulls, proves the budget, then uses --no-build
curl -fsS http://127.0.0.1:32400/readyz
```

On a replicated cluster, run `make docker-image-up` and the readiness probe on
one voter at a time, and require `/readyz` to return 200 before advancing. The
target pulls while the existing voter runs, freezes the pulled image ID, and
requires its nonempty `org.opencontainers.image.revision` OCI label to match
the clean checkout before any checkout-owned budget proof runs. It also
requires the server and discovery companion to resolve to that same frozen ID.
Only then does it derive and prove one exact startup period and cross the quorum
boundary with `compose up --no-build --pull never`.

The post-merge Forgejo job runs `scripts/registry-push` after the complete
`main` validation fan-out. It publishes the moving `main` tag only after the
immutable `sha-<12hex>` image passes its registry-side identity and smoke
checks. Versioned releases keep the separate `latest` alias. Use the immutable
tag for rollback:

```bash
git fetch origin
git switch --detach <old-40-character-sha>
sed -i.bak \
  's|^PLURX_IMAGE=.*|PLURX_IMAGE=forge.lan:3000/noirr/plurxd:sha-<first-12-characters>|' \
  deploy/.env
make docker-image-up
curl -fsS http://127.0.0.1:32400/readyz
```

The 40-character checkout SHA and the image's `sha-` tag must name the same
commit. A mismatch is refused before the startup-budget checker or replacement
runs; use the exact revision printed by the target rather than guessing which
commit a moving tag contains.

If the registry is unavailable, the tracked `build:` block remains the local
fallback. Build the checked-out revision with `make docker-up`; it preserves
the build stamp and does not require the registry to answer. The noirr fleet's
registry operations and recovery runbook live in
[`docs/OPERATIONS.md`](../docs/OPERATIONS.md#the-fleet-registry--build-once-pull-everywhere).

### Project name

The compose file pins `name: plurx`. Left to itself, Compose names the project
after the directory holding the file — which here would be `deploy`, a name a
great many other self-hosted stacks also use. Two stacks that resolve to the
same project name *are* the same project as far as Compose is concerned:
`docker compose ps` in one lists the other's containers, and `up
--remove-orphans` or `down` in one **deletes** the other's, on the reasoning
that they aren't in the compose file it just read. The container disappears
outright rather than exiting, so there is no exit code and no log left to read.

Don't remove the `name:` line, and if you run several stacks from directories
called `deploy`, give each of them one too.

### The data directory

The authoritative database, identity, secrets, and compatibility cache layout
live in one directory, bind-mounted from the host:

```yaml
- ${PLURX_DATA:-/srv/plurx}:/var/lib/plurx
```

Create it before the first run, owned by the uid the container runs as:

```sh
sudo install -d -o $(id -u) -g $(id -g) /srv/plurx
```

To isolate heavy I/O, retain that state mount and add two host-specific
override mounts:

```yaml
services:
  plurxd:
    environment:
      PLURX_CACHE_DIR: /var/cache/plurx
      PLURX_TRANSCODE_DIR: /var/tmp/plurx-transcode
    volumes:
      - /srv/plurx-cache:/var/cache/plurx
      - /srv/plurx-scratch:/var/tmp/plurx-transcode
```

The same block is commented out in `docker-compose.override.example.yml`;
uncomment it rather than retyping it.

Create both with the daemon uid/gid. Cache survives restarts. Scratch must be
empty on first start and must not be a path under `PLURX_DATA`; plurx claims it
with `.plurx-transcode-scratch`, then removes only its verified children at
later starts and fails startup if cleanup is incomplete. The scratch mount must
be owned by the container uid — another uid refuses startup, while a
container-uid-owned mount that is group/world-writable is repaired to `0700`
with a warning. Do not bind the same host directory at a persistent path and
the scratch path. If `PLURX_SHARED_CACHE_DIR` is configured, keep that mount
separate from all three paths too; a missing shared mount retains node-local
fallback and is not created by storage preflight. Neither local path is searched
for a database.

Setting `PLURX_CACHE_DIR` on an existing install moves nothing — the daemon
warns while the legacy trees still hold bytes and starts anyway. Copy
`<data>/artwork`, `<data>/cache/transcode`, `<data>/cache/subs`, and
`<data>/cache/renditions` to the same child names below `<cache>`. Stop the
daemon and delete `<data>/cache/runtime`; that ffmpeg state is regenerable and
the next start creates `<cache>/runtime` as needed. `cp -a <data>/cache
<cache>` is wrong: it nests every child below `<cache>/cache`, where nothing
reads it. Roll back by stopping one non-leader, copying persistent bytes to the
legacy data-root children, discarding the replacement runtime cache, removing
these settings/mounts, and proving `/readyz` before moving the next voter.
Delete the two keys before installing an older image: `[storage]` rejects
unknown fields, so an older binary fails to parse the config rather than
ignoring them.

**Why a bind mount and not a named volume.** A named volume lives at a path
Docker chose, and pointing a container at one that does not exist yet is not an
error — Docker creates an empty one, plurxd initialises a fresh database in it,
and the first-run setup screen appears. That is indistinguishable from losing
your library, and it happens for undramatic reasons: renaming the deployment
directory, recreating the stack in a way that drops the volume, a `down -v`
typed in the wrong terminal. A path you can `ls` cannot go missing quietly.

**Moving from the old named volume.** Revisions before this used
`plurx-data:/var/lib/plurx` (and older ones `<project>_plurx-data`, usually
`deploy_plurx-data`). Copy it across once, with the stack down so you are not
copying a live database:

```sh
docker compose down
docker volume ls | grep plurx-data                  # confirm the source name
sudo install -d -o $(id -u) -g $(id -g) /srv/plurx
docker run --rm -v plurx-data:/from -v /srv/plurx:/to \
    alpine sh -c 'cd /from && cp -a . /to'
sudo chown -R $(id -u):$(id -g) /srv/plurx
docker compose up -d
```

Keep the old volume until you have confirmed your users and libraries are
intact, then `docker volume rm plurx-data`.

**Checking what is actually mounted.** One command, worth running whenever
something that should have persisted did not:

```sh
docker inspect -f '{{range .Mounts}}{{.Type}} {{.Source}} -> {{.Destination}}{{"\n"}}{{end}}' plurxd
```

Nothing mapping to `/var/lib/plurx` means the next rebuild loses everything.

## Bare metal

```sh
# Linux amd64/arm64, macOS, Windows — one binary; ffmpeg is the base runtime dependency.
make install-binary # builds plurxd from this checkout and puts it on PATH (no service)
plurxd run          # serves :32400; config via ./plurx.toml or PLURX_* env
```

`make install-binary` also installs `ffmpeg`/`ffprobe` with the platform's
package manager when they are missing (or point `PLURX_FFMPEG`/`PLURX_FFPROBE`
at a build such as jellyfin-ffmpeg for the best hardware/tone-mapping
support). To keep it running across reboots, install it as a service instead —
`make install` picks the native Windows service, **systemd** on Linux, or
**launchd** on macOS, each described below. Every target takes
`INSTALL_FLAGS`: `--binary <path>` installs a prebuilt `plurxd` instead of
building, `--prefix <dir>` moves the binary, `--dry-run` prints the plan, and
`make uninstall` removes the service while keeping data and configuration.

Permanent Dolby Vision Profile 7 → 8.1 conversion additionally needs
`dovi_tool` and `mkvmerge` 68 or newer. The Docker image includes pinned builds;
bare-metal installs may put them on `PATH` or set `PLURX_DOVI_TOOL` and
`PLURX_MKVMERGE`. Missing tools disable that admin action with a reason and do
not prevent the server from starting. Run each configured executable with
`--version` as the service account before enabling conversion; an interactive
shell's `PATH` does not prove that systemd or launchd can find it.

### Hardware transcode & recent Intel GPUs

The Docker image defaults to **jellyfin-ffmpeg**, which bundles a current Intel
media driver + libva + oneVPL. This matters for newer silicon: an Arc / Meteor
Lake / **Arrow Lake** iGPU (on the kernel `xe` driver) is years newer than the
VA driver Debian ships, so the distro ffmpeg fails VAAPI init with an I/O error
while jellyfin-ffmpeg drives it fine. Pass the GPU through and add the render
group in your compose override:

```yaml
    devices:
      - /dev/dri:/dev/dri
    group_add:
      - "992"          # `stat -c '%g' /dev/dri/renderD128` on the host
```

On Intel **Arc**-class GPUs, QuickSync (oneVPL) is usually more reliable than
VA-API — set `PLURX_HWACCEL: "qsv"` in the override to prefer it. Startup
validation test-encodes each path and Settings → Logs shows why any hardware
probe was rejected.

## Run as a service — Windows

One command from an elevated PowerShell in the repository root:

```powershell
powershell -ExecutionPolicy Bypass -File deploy\install.ps1                                   # builds plurxd.exe from this checkout
powershell -ExecutionPolicy Bypass -File deploy\install.ps1 -Binary .\plurxd-windows-x86_64.zip  # or installs the release archive
```

(From an elevated Git Bash or MSYS2 shell with GNU make, `make install` runs
the same script; the script elevates itself when it is not already.) It copies
`plurxd.exe` and `plurx.example.toml` to `C:\Program Files\plurx`, writes
`C:\ProgramData\plurx\plurx.toml` from the example if there is none, installs
ffmpeg with winget when none is found and puts `ffmpeg.exe`/`ffprobe.exe`
beside `plurxd.exe` (the service runs as LocalSystem, which does not share
your `PATH`; machine-level `PLURX_FFMPEG`/`PLURX_FFPROBE` are honoured
instead), registers the automatic LocalSystem service through
`plurxd.exe service install`, opens TCP 32400 and UDP 32414 in Windows
Firewall, then waits for `/readyz` and prints the version the server reports.
Run it again to upgrade: the service is stopped, the binary replaced, and the
service started. `-Uninstall` removes the service and the firewall rules and
keeps the install directory, the config, and the data. `-DryRun` prints the
plan. Edit `plurx.toml` afterwards for your libraries: use absolute Windows
paths for data, cache, transcode scratch, media libraries, and external tools.
The managed data and cache roots must be on NTFS or ReFS; a read-only library
may be on another filesystem or an SMB share.

By hand, the same install is: download `plurxd-windows-x86_64.zip`, expand it
to a stable directory such as `C:\Program Files\plurx`, and copy
`plurx.example.toml` to `C:\ProgramData\plurx\plurx.toml`.

Install a current jellyfin-ffmpeg Windows build. Put `ffmpeg.exe` and
`ffprobe.exe` beside `plurxd.exe`, or set the machine-level
`PLURX_FFMPEG`/`PLURX_FFPROBE` variables to absolute paths. The sibling files
win over `PATH`; explicit environment variables win over both.

From an elevated PowerShell window:

```powershell
New-Item -ItemType Directory -Force C:\ProgramData\plurx | Out-Null
Copy-Item .\plurx.example.toml C:\ProgramData\plurx\plurx.toml
notepad C:\ProgramData\plurx\plurx.toml

.\plurxd.exe service install --config C:\ProgramData\plurx\plurx.toml
Get-Service plurxd

netsh advfirewall firewall add rule name="plurx HTTP" dir=in action=allow protocol=TCP localport=32400
netsh advfirewall firewall add rule name="plurx GDM discovery" dir=in action=allow protocol=UDP localport=32414
```

The service starts automatically as LocalSystem. When `storage.data_dir` is
left at the relative default, service mode deliberately relocates it to
`%ProgramData%\plurx\data`; explicit paths are preserved. Open
`http://<host>:32400` after `Get-Service plurxd` reports `Running`.

UDP 32414 is only discovery: omitting that rule makes automatic server picking
fail silently, while a direct URL can still work. TCP 32400 is the web and
media API. Cluster voters additionally need their explicitly configured Raft
and peer API ports; do not expose any of these ports to the public internet.

For an upgrade, stop the service, replace `plurxd.exe`, and start it again.
Uninstall removes the SCM registration after a bounded stop; it leaves data,
configuration, and the firewall rules for the operator to retain or remove.

```powershell
Stop-Service plurxd
Copy-Item .\plurxd.exe 'C:\Program Files\plurx\plurxd.exe' -Force
Start-Service plurxd

& 'C:\Program Files\plurx\plurxd.exe' service uninstall
netsh advfirewall firewall delete rule name="plurx HTTP"
netsh advfirewall firewall delete rule name="plurx GDM discovery"
```

## Run as a service — systemd (Linux)

Keeps plurxd running across reboots and restarts it if it crashes. The unit
([`plurxd.service`](plurxd.service)) runs as a dedicated unprivileged `plurx`
user and is sandboxed (`ProtectSystem=strict`, `NoNewPrivileges`), writing only
to its data dir. One command from the repository root:

```sh
make install          # or: make install-linux
```

It builds `plurxd` from this checkout (`--binary` in `INSTALL_FLAGS` installs a
prebuilt one instead), installs ffmpeg with apt/dnf/pacman/zypper/apk when it
is missing, creates the `plurx` user and `/var/lib/plurx`, installs the binary
and the unit, enables and starts the service, then waits for `/readyz` and
prints the version the server reports. sudo is used for exactly the steps
that need root; the build never runs as root. Run it again to upgrade: a
running service is stopped before its binary is replaced and started after,
and an existing unit is yours — the installer never overwrites it, so the
`SupplementaryGroups`, `ProtectHome`, and `PLURX_FFMPEG` edits below survive.
A `--prefix` other than `/usr/local` lands as a drop-in
(`plurxd.service.d/10-prefix.conf`) rather than an edited unit; a prefix under
`/home` is refused because the unit's `ProtectHome=true` would hide it.
`make uninstall` disables the service and removes the unit, drop-in, and
binary, and keeps `/var/lib/plurx` and the `plurx` user. By hand, the same
install is:

```sh
# 1. Install both Linux binaries + a service user + its data dir
sudo install -m755 plurxd /usr/local/bin/plurxd
sudo install -m755 plurx-optical-helper /usr/local/bin/plurx-optical-helper
sudo useradd --system --home /var/lib/plurx --shell /usr/sbin/nologin plurx
sudo install -d -o plurx -g plurx /var/lib/plurx

# 2. Install the unit and start it (edit paths/env in the file first if needed)
sudo cp deploy/plurxd.service /etc/systemd/system/plurxd.service
sudo systemctl daemon-reload
sudo systemctl enable --now plurxd

# 3. Watch it come up
systemctl status plurxd
journalctl -u plurxd -f            # live logs; Ctrl-C to stop tailing
```

Open `http://<host>:32400` and create your admin account. To update later,
replace the binary and `sudo systemctl restart plurxd`.

- **Hardware transcode:** uncomment `SupplementaryGroups=render` in the unit
  (match `stat -c '%G' /dev/dri/renderD128`, usually `render`) so the `plurx`
  user can reach the GPU. The software x264 path works without it.
- **Media under `/home`:** the unit sets `ProtectHome=true`, which *hides*
  `/home` from the service — if your library lives there the scan finds nothing.
  Set `ProtectHome=read-only` before adding the library; an additional
  `ReadOnlyPaths=/path/to/media` can narrow the documented path but cannot make
  a path hidden by `ProtectHome=true` visible.
- **ffmpeg:** uncomment the `PLURX_FFMPEG` line to use a jellyfin-ffmpeg build
  for the best hardware/tone-mapping support.

The unit's `ProtectSystem=strict` and `ReadWritePaths=/var/lib/plurx` keep media
read-only by default. To opt one non-home library into permanent conversion,
first satisfy the writable-media contract above, then add an exact drop-in:

```bash
sudo systemctl edit plurxd
```

Enter this exact-path exception in the editor:

```ini
[Service]
ReadWritePaths=/mnt/media/dv-conversion
Environment=PLURX_DOVI_TOOL=/absolute/path/to/dovi_tool
Environment=PLURX_MKVMERGE=/absolute/path/to/mkvmerge
```

Then reload, restart, and probe through the service account:

```bash
sudo systemctl daemon-reload
sudo systemctl restart plurxd
sudo -u plurx /absolute/path/to/dovi_tool --version
sudo -u plurx /absolute/path/to/mkvmerge --version
```

The underlying directory still needs write/traverse permission for the
dedicated `plurx` account; the systemd allow-list does not grant filesystem
permissions. If the library is below `/home`, also change `ProtectHome` to
`read-only` in the drop-in so the exact `ReadWritePaths` exception can be seen.
Do not replace the exact path with `/mnt` or another broad media root.

### Optical drives on Linux

Optical hosting is an explicit runtime opt-in. A source install now places
both `plurxd` and `plurx-optical-helper` in the selected binary directory;
the Linux container and tagged release artifact carry the same pair. The
helper is inert until a drive is declared in `plurx.toml` and the switch is
saved under Settings > Developer. Readiness rows beside that switch are
advisory: they explain what will fail, but never rewrite or veto the saved
choice.

Use a stable block-device name and an operator-managed read-only mount:

```toml
[optical]
helper_path = "plurx-optical-helper"
poll_interval_secs = 5

[[optical.drives]]
id = "media-room"
label = "Media room drive"
device_path = "/dev/disk/by-id/replace-with-actual-optical-drive"
mount_path = "/mnt/optical/media-room"
```

The helper does not mount media. Configure the host to mount DVD UDF/ISO9660
or Blu-ray UDF media at that exact path with `ro,nosuid,nodev,noexec`; an
inspection refuses a writable mount or a navigation tree containing symlinks.
The service account needs traverse/read access to the mount and open/eject
access to the device. On a typical distribution that means adding the drive's
group (often `cdrom`) to `SupplementaryGroups` in a systemd drop-in:

```ini
[Service]
SupplementaryGroups=cdrom
ReadOnlyPaths=/mnt/optical/media-room
```

Confirm the configured probe binary really carries the two required inputs.
Do not trust the command's exit status by itself; some FFmpeg builds exit zero
while printing an unknown-input diagnostic.

```sh
ffprobe -hide_banner -h demuxer=dvdvideo 2>&1 | grep -E 'dvdvideo|title'
ffprobe -hide_banner -h protocol=bluray 2>&1 | grep -E 'bluray'
sudo -u plurx plurx-optical-helper presence \
  --device /dev/disk/by-id/replace-with-actual-optical-drive \
  --mount /mnt/optical/media-room
```

For Compose, map the stable host device to `/dev/optical`, bind the mounted
filesystem read-only, and add the host device's numeric group with
`group_add`. The complete commented shape is in
[`docker-compose.override.example.yml`](docker-compose.override.example.yml).
Set the TOML paths to the container-side paths. Do not grant broad privileged
mode or mount `/dev` wholesale.

Eject is generation- and session-checked by the API before the helper receives
an ioctl request. Disabling optical stops observation and fences the active
insertion; it does not need every advisory readiness row to be green.

## Run as a service — launchd (macOS)

Runs plurxd as a **LaunchAgent** in your login session — start-at-login, restart
on crash. A user agent rather than a boot-time system daemon on purpose:
VideoToolbox hardware transcoding needs a logged-in GUI session, which a daemon
doesn't have. The template is [`com.plurx.plurxd.plist`](com.plurx.plurxd.plist);
launchd doesn't expand `~`, so the install fills in absolute paths for you.
One command from the repository root:

```sh
make install          # or: make install-macos
```

It builds `plurxd` from this checkout, installs ffmpeg with Homebrew when it
is missing, puts the binary in the Homebrew prefix (no sudo on Apple Silicon;
`--prefix` in `INSTALL_FLAGS` chooses another), renders the agent plist with
your home directory and the real `plurxd`/`ffmpeg`/`ffprobe` paths, bootstraps
and starts it, then waits for `/readyz` and prints the version the server
reports. Run it again to upgrade: the agent is booted out before its binary
is replaced, and an existing plist is yours — the installer never overwrites
it, so `plutil -replace` edits survive. `make uninstall` boots the agent out
and removes the plist and binary, and keeps
`~/Library/Application Support/plurx`. `make uninstall-docker` is the Compose
counterpart (`docker compose down`; `.env`, the override, and data stay).

**This shipped LaunchAgent does not support permanent on-disk Dolby Vision
conversion. Keep that feature Off.** It runs as your interactive login uid, so
desktop apps, downloaders, organizers, and shell jobs share the identity that
owns the private cleanup anchor. That violates the destructive worker's
dedicated-account boundary even when the anchor is mode `0700`. The trade-off
is explicit: this user-session recipe keeps VideoToolbox available; the safer
dedicated-account service needed for permanent conversion loses that guarantee.
Use the Linux container or Linux systemd recipe above for the supported
conversion path. A site-specific macOS LaunchDaemon can use a dedicated
account, but this repository does not ship one because its media ACLs and
user-session hardware policy cannot be inferred safely.

By hand, the same install is:

```sh
# 1. Install the binary + ffmpeg (Homebrew satisfies the runtime dep)
brew install ffmpeg
sudo install -m755 plurxd /usr/local/bin/plurxd     # /opt/homebrew/bin on Apple Silicon

# 2. Fill in your username + real binary paths, drop the agent into place
mkdir -p ~/Library/LaunchAgents "$HOME/Library/Application Support/plurx"
sed -e "s|YOUR_USERNAME|$USER|g" \
    -e "s|/usr/local/bin/plurxd|$(command -v plurxd)|" \
    -e "s|/usr/local/bin/ffmpeg|$(command -v ffmpeg)|" \
    -e "s|/usr/local/bin/ffprobe|$(command -v ffprobe)|" \
    deploy/com.plurx.plurxd.plist > ~/Library/LaunchAgents/com.plurx.plurxd.plist

# 3. Load, enable, and start it (modern launchctl)
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.plurx.plurxd.plist
launchctl enable   gui/$(id -u)/com.plurx.plurxd
launchctl kickstart -k gui/$(id -u)/com.plurx.plurxd

# 4. Check it, then open the app
launchctl print gui/$(id -u)/com.plurx.plurxd | grep -E 'state|pid'
tail -f ~/Library/Logs/plurxd.log
open http://localhost:32400
```

The plist contains absolute placeholders for `PLURX_DOVI_TOOL` and
`PLURX_MKVMERGE` so Settings can report deterministic probe failures instead of
depending on launchd's restricted `PATH`. They do **not** make conversion safe
in this LaunchAgent. If you installed the tools for read-only inspection or a
later move to a dedicated service, replace the placeholders only after proving
the operator-provided paths:

```bash
DOVI_TOOL=/absolute/path/to/dovi_tool       # operator-installed executable
MKVMERGE=/absolute/path/to/mkvmerge         # operator-installed executable
test -x "$DOVI_TOOL" && "$DOVI_TOOL" --version
test -x "$MKVMERGE" && "$MKVMERGE" --version
plutil -replace EnvironmentVariables.PLURX_DOVI_TOOL \
  -string "$DOVI_TOOL" ~/Library/LaunchAgents/com.plurx.plurxd.plist
plutil -replace EnvironmentVariables.PLURX_MKVMERGE \
  -string "$MKVMERGE" ~/Library/LaunchAgents/com.plurx.plurxd.plist
```

macOS prompts once to allow incoming connections (needed for other devices to
reach `:32400`). To update later, replace the binary and re-run the `kickstart`
line. To stop and remove it:

```sh
launchctl bootout gui/$(id -u)/com.plurx.plurxd
rm ~/Library/LaunchAgents/com.plurx.plurxd.plist
```

For a headless Mac that must run **with no one logged in**, a site-specific
system **LaunchDaemon** can run under a non-root account and load under
`system/` instead of `gui/$(id -u)`. This is not a conversion recipe and the
LaunchAgent plist cannot merely be copied into place: a correct daemon needs a
dedicated `UserName`, account-owned data and log paths, explicit tool paths,
and media ACLs for that site. None is safe to infer in a tracked template. Use
the supported Linux container or Linux systemd path for permanent conversion.
The headless trade-off remains real: without a GUI session, VideoToolbox is not
available and hardware transcoding falls back to software x264.

## Unraid

Add [`unraid-plurx.xml`](unraid-plurx.xml) as a user template (or via the
Docker "Add Container" screen), set your media and appdata paths, and
optionally pass through `/dev/dri` for QuickSync/VA-API. The template uses
host networking because Bonjour multicast does not leave a Docker bridge.
That is what makes `_plurx._tcp` visible to iPhone, iPad, and Apple TV; a
bridge deployment can still use manual server entry, but cannot provide
automatic native discovery.

The template keeps Media access mode **Read Only**. Permanent conversion must
stay Off in that default. To opt in, create a second path mapping for only the
conversion library and change that mapping—not the broad Media mapping—to
**Read/Write**. Use `docker exec plurxd id` to identify the image's numeric uid,
grant only that uid access to the selected host path, and do not reuse it for
downloaders or organizers. Confirm the Unraid share really provides the hard
link, rename, sync, and same-filesystem semantics in the writable-media contract
above; a share or remote mount that cannot prove them is read-only for this
feature. Keep an independently restorable backup before enabling the library.

Host networking also means the ports are real host ports. Stop any process
already using TCP 32400, or change `PLURX_BIND`. If Plex still owns UDP 32414,
change `PLURX_GDM_PORT`; this disables GDM discovery on its fixed standard port
but does not affect Bonjour.

## TrueNAS SCALE / Kubernetes

Use the Docker image with stable per-voter storage for `/var/lib/plurx` and a
read-only mount for media. The Service/Ingress routing pattern is in
[`cluster-routing/`](cluster-routing/); it deliberately does not pretend that
the workload, Raft storage, GPU scheduling, or media mounts are stateless.
Follow the cluster bootstrap and rolling-drain rules in
[`docs/OPERATIONS.md`](../docs/OPERATIONS.md#cluster-ingress-drain-and-recovery).

Permanent conversion remains Off for that read-only deployment. An opt-in
workload must use a dedicated non-root `runAsUser`/`runAsGroup` that no other
workload shares, mount only the selected library with `readOnly: false`, and
grant that identity write/traverse access at the storage backend. Every voter
eligible to lease work must mount the same path and filesystem; a node-local
volume, object-store adapter, or CSI driver without proven hard-link, atomic
rename, Unix mode/owner, and durable-sync behavior is unsupported. Keep other
libraries read-only, take and test a backup outside the writable PV, and leave
conversion Off until a real pod proves the filesystem contract above.

## Ports

| Port | Proto | Purpose |
|---|---|---|
| 32400 | TCP | HTTP API + web app (and the Plex-compat façade) |
| 32414 | UDP | GDM discovery so Plex/Kodi clients find the server on the LAN (host port movable via `PLURX_GDM_PORT`, but discovery only works on 32414) |
| 5353 | UDP multicast | Bonjour `_plurx._tcp` discovery for native clients; the Compose companion owns this on the host network |

## Observability

`GET /healthz` is liveness. `GET /readyz` is serving readiness, and the image's
Docker health check uses it so a voter without usable quorum is not marked
healthy. `GET /metrics` includes process, playback, cached cluster membership,
quorum, leader, commit, and apply-lag state without performing a Store read on
scrape. The same image also contains the read-only `plurx-cluster-check
inspect-wal` stopped-node tool; the safe preservation and interpretation
runbook is in
[`docs/OPERATIONS.md`](../docs/OPERATIONS.md#inspecting-a-stopped-voter-without-changing-it).

The image and Compose default to a 25-minute startup health grace: enough for
the default 1,200-second snapshot transfer stage, 120-second final install, and
three 45-second startup phases. The independent 30-second non-final chunk
watchdog does not extend that formula. A deployment that raises either
`cluster.snapshot_transfer_timeout_secs` or
`cluster.install_snapshot_timeout_secs` — in `.env`, or in a production TOML
this repository never sees — needs a longer grace, and does not have to
maintain one: leave `PLURX_HEALTH_START_PERIOD` unset and either supported
rollout target derives it from both resolved stage timeouts plus 135 seconds.
Use `make docker-up` for a local source build or `make docker-image-up` for a
prebuilt fleet image. Set the variable only to choose a grace deliberately.
Any resolved grace other than the tracked 25-minute default counts as chosen —
wherever you wrote it — and is used exactly as Compose resolved it, refused by
name rather than overruled if it cannot cover both stages. The fleet rollout
pair is shown in `.env.example`.

Run one of those targets from the repository root for every Compose rollout.
After an optional image pull, it derives the grace only if the deployment has
not chosen one, proves the budget it is about to apply — shell variables,
`deploy/.env`, Compose defaults and overrides, and a readable bind-mounted
production TOML all at their usual precedence — and only then does `docker
compose up` replace a container, with the same period that was proved. If a
named volume or another opaque mount hides the production config, the check
assumes the source maximum for each snapshot budget unless all three
`PLURX_CLUSTER_SNAPSHOT_CHUNK_TIMEOUT_SECS`,
`PLURX_CLUSTER_SNAPSHOT_TRANSFER_TIMEOUT_SECS` and
`PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS` are explicit. This conservative
fallback prevents custom TOML deadlines from accidentally shipping with a
shorter health grace. An explicit command-line `--config` takes
precedence over `PLURX_CONFIG`, matching the server. A direct `docker compose
up` bypasses the preflight.
