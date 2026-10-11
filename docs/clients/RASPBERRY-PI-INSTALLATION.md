# Raspberry Pi installation — Docker by default, native by choice

**Status:** built — software implementation and bounded Pi application
acceptance complete; merged in PR #889 · **Updated:** 2026-10-08

Companion to [the decoder implementation](RASPBERRY-PI-IMPLEMENTATION.md)
and [the live status](RASPBERRY-PI-STATUS.md). This plan closes the installation
gap: users should not assemble FFmpeg paths, device permissions, systemd
overrides and a browser themselves. The existing server and web player remain
the product. The commands below describe the implemented installation contract.
[PR #889](http://forge.lan:3000/noirr/plurx/pulls/889) holds the authoritative
final candidate, retained evidence and completed merge disposition.

Physical acceptance found that Raspberry Pi OS kernel
`6.18.50+rpt-rpi-2712` omits `CONFIG_SECURITY_LANDLOCK`. The Pi-only namespace
backend addresses that prerequisite without removing Landlock from other
systems. Docker then exposed a separate procfs mount restriction; section 6
records the narrow correction. The live status distinguishes merged source
from actual application acceptance; these commands alone are not playback
evidence.

## 1. Decision — containers are the default server deployment

1. **Docker is the default.** A Pi-specific runtime packages the same Plurx
   server with actual V4L2 Request decoding. A container shares the host kernel;
   it needs the right userspace implementation and explicit device access.
2. **Native/systemd is selectable.** It runs the existing binary and the same
   media capabilities without requiring Docker. The installer supplies the
   toolchain or selected binary, media tools, service configuration and groups.
3. **The HDMI browser runs in the desktop session.** This preserves Wayland,
   audio, sandboxing and normal login ownership. One setup command manages the
   server and browser; users do not need to coordinate them manually.
4. **Preserve the shipped media contracts.** Stock Jellyfin FFmpeg lacks the
   inspected Pi request backend. Replacing it with Pi OS FFmpeg would lose
   established audio and Dolby Vision behavior. Build a pinned Pi variant
   retaining Jellyfin's patch series and adding the maintained Pi request/SAND
   implementation. Prove AC-4, `dovi_rpu`, `tonemapx` and real Pi decoding.
5. **Build from the selected checkout initially.** This matches the existing
   clone-and-build Docker flow and binds the server to its startup-budget
   source. A first installation may take time to compile; it must not require
   manual compiler setup. Durable prebuilt release distribution can shorten
   this later without changing the installation interface.

Using an unmodified container with software server decoding was considered,
but does not complete the requested server acceleration. Mounting host FFmpeg
and shared libraries into a generic container was rejected because it couples
upgrades to an untracked host ABI. A second decoding subprocess or playback
supervisor would add ownership and recovery paths without solving packaging.

## 2. User interface — one setup command

Use 64-bit Raspberry Pi OS Trixie Desktop for HDMI Cinema or the combined role.
The browser uses the logged-in desktop session; the installer does not create
a display session or enable automatic login. Raspberry Pi OS Lite is suitable
for the server-only role.

```bash
make pi-setup                                      # server and HDMI, Docker
make pi-setup PI_SETUP_FLAGS='--server-runtime native' # systemd alternate
make pi-setup PI_SETUP_FLAGS='--role server'        # no desktop required
make pi-setup PI_SETUP_FLAGS='--role cinema --url http://media1:32400'
make pi-setup PI_SETUP_FLAGS='--dry-run'             # inspect without mutation
make pi-upgrade                                    # retain saved choices/data
make pi-status                                    # report installation facts
make pi-uninstall                                 # remove owned installation
```

The underlying command is `deploy/pi-setup` with `install`, `upgrade`,
`status` and `uninstall`. `--role` accepts `server`, `cinema` or `both`;
`--server-runtime` accepts `docker` or `native`. Defaults are `both` and
`docker` on first install. Upgrades preserve the recorded choices unless the
operator explicitly changes them. `--media` adds read-only library mounts;
`--data-dir` selects persistent server storage. `--autostart` starts Cinema
when the desktop user logs in; it does not configure automatic OS login.

Show the concrete installation plan before changes; interactive use confirms
that plan and `--yes` supports unattended installation. Run the command as
the desktop user, invoking sudo only for package and system configuration.
The browser never runs as root. `--dry-run` performs no package installation,
download, service operation, configuration write or hardware probe.

### Existing Compose installations

`make docker-up` remains the server entry point for an existing checkout and
its `deploy/.env` and Compose overrides. On a verified local ARM64 Raspberry Pi,
it prepares the same Pi runtime used by `pi-setup`, adds the actual decoder
devices and protected probe profile, and then starts the resolved Compose
project. It preserves existing data paths, media mount destinations, ports and
networks. It does not install or start the HDMI browser; use `pi-setup` for a
new combined installation.

Pi detection requires the local Linux Docker engine and the host device-tree
compatible value. A remote Docker context is not configured from the client's
hardware. `PLURX_DOCKER_GPU=manual` preserves manual GPU device selection;
the required Pi decoder and probe runtime still accompany a verified Pi
server. The read-only startup-budget check does not provision runtime assets.

Compose preparation uses a caller-owned cache under
`~/.cache/plurx/compose-pi` and a root-owned, hash-recorded profile generation
under `/var/lib/plurx-compose-pi`. It reuses an existing protected profile when
its contents match the required policy. It never overwrites an operator's
different profile, relaxes container privileges or claims the existing stack
as an installer-owned deployment.

## 3. Runtime and ownership contracts

- Provision required packages automatically. Existing Docker, browser,
  services and package installations are not automatically installer-owned.
- Download only pinned artifacts with checked-in expected hashes. Preserve
  source, configuration, licensing and binary receipts for built media tools.
- Keep the HEVC browser in an isolated versioned directory and use the
  existing `pi-player` launcher/profile ownership. Do not replace the user's
  ordinary Chromium package or disable its sandbox.
- Discover actual media/video/render devices and numeric groups. Pass only
  needed devices into Docker; use non-root execution and supplementary groups.
  Native services receive the corresponding group configuration. No privileged
  container, world-writable device or fixed `video19` assumption is permitted.
- Set the general FFmpeg, scanner FFprobe and static bound FFprobe explicitly
  in both deployments. Preserve the static parser's identity checks.
- Validate Docker's resolved startup budget before starting the service,
  using the existing validator and the exact source used to build the image.
- Detect unowned stacks, services and configuration before provisioning.
  Never overwrite an existing deployment to make an install appear successful.
- Record owned files and hashes, runtime/source identities and prior running
  state. Stage upgrades before stopping the previous runtime; restore it when
  the replacement fails readiness. An unchanged reinstall should not restart
  a working server or browser.
  Persist a transaction journal and recovery material before changing owned
  installation state, so interruption can be reconciled on the next setup run.
  Runtime ownership records retain the identities of explicitly provisioned
  artifacts; upgrades cannot adopt operator additions or refresh hashes for
  modified files. A process-held lock prevents concurrent setup from mistaking
  an active transaction for interrupted work; status and dry-run report pending
  recovery without changing the host.
- Ordinary uninstall retains databases, media, browser login profiles and
  shared prerequisites. Remove only unchanged, verified installation-owned
  files and resources. Stop only processes/services owned by this installation.

`status` distinguishes installed artifacts, a ready server, measured hardware
decoding, browser support and unverified HDMI output. Missing acceleration is
advisory and does not become a new feature gate. `/readyz` alone cannot prove
decoded pictures or actual HDR/Dolby Vision transmission.

## 4. Implementation ownership and delivery

Paul's standing instruction for this work is one batched PR, normal commits,
one final adversarial review before the fast lane, and retention of passing
test evidence. This continues the explicit exception recorded in the earlier
Pi plan; it does not create separate task PRs or a full-suite campaign.

| Owner | Files and responsibility | Acceptance |
|---|---|---|
| Setup agent, Sol 6.1 | `deploy/pi-setup`, `deploy/pi-player` autostart, their focused operations regressions | Default Docker, selectable native, ownership, upgrade recovery, uninstall, no-mutation dry run |
| Runtime agent, Sol 6.1 | Runtime provider, immutable artifact manifest, Pi media build/patch/container files, focused runtime regressions | Preserved media contracts and real container request decoding; verified isolated browser |
| Coordinator | Make targets, README/deployment docs, this plan/index/status, integration and commits | One user-facing installation flow, exact-source builds, final review, fast lane and merged PR |

The provider and installer agree on a versioned JSON receipt for selected
executables/container inputs and owned artifacts before integration. Runtime
provisioning cannot silently replace an unowned server or browser.

## 5. Verification and cleanup

Bounded physical acceptance is complete on source `a72212b5b`: automatic
1080p Main10 direct playback used `V4L2VideoDecoder` and passed tight forward
and backward seeks. Continuous playback at the default CPU pool of 3 started
the complete 720p/480p/shared-AAC family with non-root request decoding and
x264, verified 1280×720 output, resumed seeks at 10.27 s and 3.33 s, and took
25.25 s to cold-start. Daemon-selected `libplacebo_software` passed a 4K HDR10
to 1080p picture/tag comparison at 1.57× CPU speed. These bounded checks do not
establish sustained real-time playback, concurrency/soak or Dolby Vision HDMI.
The [physical receipt](http://forge.lan:3000/attachments/221bc723-202d-4737-bd7d-6aaf01f6e8c9)
and [live status](RASPBERRY-PI-STATUS.md) preserve exact results and limits.

All task Pi containers, browsers, compiler processes, native test paths and
roots were removed; user containers were preserved. UID 999 remains because
the user's discovery service uses it. Only the image alias retaining the
user's older image was preserved. Final CI must qualify the current candidate;
physical acceptance does not replace that gate.


Establish the pinned Rust compiler loop before any Rust edits. Compilation,
source patch application and syntax checks may run during development. Defer
unit and physical regression execution until final adversarial review, then
run only the required fast lane and focused installation cases. Preserve
successful methods; rerun failures or checks affected by their corrections.

Physical acceptance uses the supplied Pi in isolated task storage. Verify a
Docker installation, real Main/Main10 request decode and decoded output,
normal-user browser decoding, readiness and restart, unchanged reinstall,
upgrade failure recovery, native selection and clean uninstall. Preserve the
baseline desktop and packages; remove task-only services, containers, images,
credentials, browser profiles, media fixtures and build scratch afterward.

The first Docker installation builds from source. On the 16 GB acceptance Pi,
the server release build alone took 41m22s and its compiler reached roughly
9 GB resident memory; the companion utility and media runtime add more build
time. This is not a validated cold-build path for smaller-memory models.
A prebuilt ARM64 distribution is still needed to remove that first-install
build cost. Native `--binary` accepts an existing server binary and avoids
compiling Rust, while still provisioning its media tools and systemd service.

The portable desk display cannot establish HDR/Dolby Vision HDMI acceptance.
Keep that row pending for an appropriate display chain. No installer option
may pretend that installing a decoder supplies missing display capabilities.

## 6. Pi probe isolation — preserve Landlock elsewhere

**Decision accepted by Paul, 2026-10-07.** Keep the existing Landlock-backed
probe launcher for non-Pi systems. On a verified Linux ARM64 Raspberry Pi whose
kernel reports Landlock unsupported, use namespaces to isolate the probe.
Do not change the kernel, disable the sandbox, or add a feature switch.

Selection combines the architecture, a Raspberry Pi device-tree compatible
value, and the Landlock ABI result. A generic ARM64 host is not a Pi. Native
installations read the platform's device-tree file; the Pi container receives
that exact host file through a read-only mount at
`/run/plurx-platform/compatible`. Landlock remains preferred when available;
unexpected permission/setup errors do not silently select another backend.

Bubblewrap creates the Pi probe's user, mount, PID, IPC, UTS and network namespaces.
Expose only its trusted executable/runtime and held media input, give it
private temporary storage, and keep the server's database, credentials and
other processes outside its view. Bubblewrap creates an empty `/proc` and
read-only binds only its own task directory at `/proc/self`, resolving the
source after the namespace clone. It executes the already-open daemon
descriptor through that task's FD view. Docker refused the fresh procfs mount,
with a contemporaneous kernel `Mount too revealing` message. Locked masked
proc children are the explanation supported by
[Linux 6.18 mount source](https://github.com/torvalds/linux/blob/v6.18/fs/namespace.c#L5820);
the exact kernel call
stack has not been traced. The own-task bind keeps those masks and the
container policy intact without exposing the container's full procfs.
The bootstrap verifies its running device/inode against that descriptor
before the parser starts, avoiding a per-probe copy of the large daemon. The bootstrap executes synchronously before the
daemon runtime initializes. It installs the existing one-shot seccomp
supervisor contract and executes the sealed parser descriptor. This preserves
the parser/source identity, subsequent-execution restrictions, admission,
bounded output, deadlines and cleanup ownership of the existing launcher.
The parser runs as namespace PID 1, so no unsandboxed init peer remains visible
and the kernel kills its descendants when it exits. Bubblewrap 0.8 in the container and 0.12 on the native acceptance OS must both
be supported; descriptor inheritance and namespace-child cleanup need direct
evidence, not an assumption from command-line compatibility.

The bound task remains the parser's PID 1 after a parser fork: `/proc/self`
continues to identify that task, within the same untrusted parser domain.
Its `root` link resolves inside the probe's mount namespace. Numeric process
directories are absent; bootstrap/control descriptors close before parser
execution. The hostile-probe regression checks private-marker access through
the task's `root`, absent parent/PID-1 directories, exact source access through
FD 3, and the complete inherited FD census.

The Pi container's seccomp policy must retain its pinned Docker default rules
and add only AArch64 rules needed for the nested namespace setup. The checked-in
Moby 26.1.5 default profile, additions, hashes and license notices are installed
as root-owned, receipt-tracked files; the normal Docker image and discovery
service keep their existing policy. No privileged
mode, unconfined policy, blanket `CAP_SYS_ADMIN`, host PID namespace or Docker
socket is permitted. Native installation uses the distribution's Bubblewrap.
The installer owns any added profile/configuration through its existing
transaction and hash receipts. No additional service or watchdog is introduced.

| Owner | New scope | Required evidence |
|---|---|---|
| Probe agent, Sol 6.1 | Existing Rust probe launcher, early internal bootstrap and focused Rust regressions | Non-Pi Landlock behavior retained; verified-Pi selection; exact descriptors; filesystem/process/network isolation; bounded failure and cleanup |
| Setup agent, Sol 6.1 | Pi Bubblewrap package/image, narrow container policy and platform mount, installer ownership and focused operations regressions | Both packaged Bubblewrap versions; non-root nested operation; no broad privileges; rollback/uninstall preserve operator files |
| Coordinator | This plan/status, integration, compiler loop, review and physical qualification | Native and Docker app-level playback; applicable evidence retained; complete cleanup; CI merge authorization is separate from physical acceptance |

Compile committed source with Rust 1.97.1 on Linux before pushing. The earlier
installer review did not cover this new security scope. Using the user's
instruction to decide when unavailable, the coordinator keeps one batched PR
and reviews the newly added sandbox scope once before its tests. This is an
explicit exception to the one-review-per-PR convention, not a re-review of
the original installer findings. Focused regressions must cover
both the existing Landlock path and the Pi path, including rejected secondary
execution, attempts to reach the server's processes/files, inherited descriptor
identity, cancellation and descendant cleanup. Actual app playback must create
the protected parser identity and stream on the stock Pi kernel before this
work is considered accepted.

**Historical prerequisite failures, since resolved:** Paul authorized merging
PR #851 after its required CI gate passes while
physical verification continues. This authorization does not establish
default app playback: the original Docker procfs mount failed with `Operation
not permitted`, and native session admission separately refused an eight-unit
720p/480p/shared-AAC family against the default three-unit CPU pool. The narrow
proc-view follow-up requires review and focused qualification. Any temporary
pool increase used to measure the actual unchanged recipes is diagnostic
evidence only, restored after collection, and cannot qualify the defaults.
