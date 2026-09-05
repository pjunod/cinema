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

Host-specific bits (media mounts, GPU, and shared Docker networks) live in an
untracked override file, so pulling updates never conflicts with local edits:

```sh
cd deploy
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
      - /mnt/qnap/media:/media:ro
      - /mnt/qnap/media/dv-conversion:/media-dv-conversion:rw
```

Add `/media-dv-conversion` as its own library in Settings. Do not overlap a
writable child with a library configured through the read-only parent path;
one container path should name each opted-in file. Confirm the effective uid
with `docker compose exec plurxd id`, prove that account can create, hard-link,
sync, rename, and remove a sibling test file on the real mount, then delete the
test artifacts before enabling conversion.

When `PLURX_SERVER_NAME` is still the default `plurx`, the companion advertises
the Docker host name plus its LAN address, so a picker says
`m6 · 192.168.1.20` instead of showing another anonymous `plurx` row. Set a
custom `PLURX_SERVER_NAME` when a room or role name is clearer; the custom name
replaces the host name while the address remains visible. Cluster nodes are
named the same way — every node of one logical server reports the same
`PLURX_SERVER_NAME`, and it is the host name and the address that tell them
apart. A node with neither falls back to the first twelve characters of its
node id, which is the only case where a picker shows a UUID.

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
cd deploy
printf '%s\n' \
  'PLURX_IMAGE=192.168.4.7:3000/noirr/plurxd:main' >> .env
docker compose pull plurxd   # downloads without touching the running voter
docker compose up -d         # swaps this voter only after the pull completes
curl -fsS http://127.0.0.1:32400/readyz
```

On a replicated cluster, run the final two commands on one voter at a time and
require `/readyz` to return 200 before advancing. A pull is harmless to the
running process; the `up -d` is the quorum boundary.

The post-merge Forgejo job runs `scripts/registry-push` after the complete
`main` validation fan-out. It publishes the moving `main` tag only after the
immutable `sha-<12hex>` image passes its registry-side identity and smoke
checks. Versioned releases keep the separate `latest` alias. Use the immutable
tag for rollback:

```bash
PLURX_IMAGE=192.168.4.7:3000/noirr/plurxd:sha-<old-sha> \
  docker compose up -d
```

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
plurxd run          # serves :32400; config via ./plurx.toml or PLURX_* env
```

Install `ffmpeg`/`ffprobe` (or point `PLURX_FFMPEG`/`PLURX_FFPROBE` at a build
such as jellyfin-ffmpeg for the best hardware/tone-mapping support). To keep it
running across reboots, install it as a service — **systemd** on Linux or
**launchd** on macOS, both below.

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

## Run as a service — systemd (Linux)

Keeps plurxd running across reboots and restarts it if it crashes. The unit
([`plurxd.service`](plurxd.service)) runs as a dedicated unprivileged `plurx`
user and is sandboxed (`ProtectSystem=strict`, `NoNewPrivileges`), writing only
to its data dir.

```sh
# 1. Install the binary + a service user + its data dir
sudo install -m755 plurxd /usr/local/bin/plurxd
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

## Run as a service — launchd (macOS)

Runs plurxd as a **LaunchAgent** in your login session — start-at-login, restart
on crash. A user agent rather than a boot-time system daemon on purpose:
VideoToolbox hardware transcoding needs a logged-in GUI session, which a daemon
doesn't have. The template is [`com.plurx.plurxd.plist`](com.plurx.plurxd.plist);
launchd doesn't expand `~`, so the install fills in absolute paths for you.

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

The image and Compose default to a five-minute startup health grace: enough for
the default 120-second snapshot deadline and the three 45-second startup phases,
without hiding a permanently broken build for an hour. If `.env` raises
`PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS`, also set
`PLURX_HEALTH_START_PERIOD` to at least that timeout plus 135 seconds. The
maximum supported pair is shown in `.env.example`.

Run `make docker-up` from the repository root for every Compose rollout. Its
read-only `docker-startup-budget-check` prerequisite resolves shell variables,
`deploy/.env`, Compose defaults and overrides, and a readable bind-mounted
production TOML before `docker compose up` can replace a container. If a named
volume or another opaque mount hides the production config, the check assumes
the source maximum unless the resolved
`PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS` is explicit. This conservative
fallback prevents a custom 600-second TOML deadline from accidentally shipping
with the five-minute health grace. An explicit command-line `--config` takes
precedence over `PLURX_CONFIG`, matching the server. A direct `docker compose
up` bypasses the preflight.
