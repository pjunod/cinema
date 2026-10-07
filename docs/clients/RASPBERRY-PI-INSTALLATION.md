# Raspberry Pi installation — Docker by default, native by choice

**Status:** open — implementation in progress · **Written:** 2026-10-07

Companion to [the decoder implementation](RASPBERRY-PI-IMPLEMENTATION.md)
and [the live status](RASPBERRY-PI-STATUS.md). This plan closes the installation
gap: users should not assemble FFmpeg paths, device permissions, systemd
overrides and a browser themselves. The existing server and web player remain
the product. The commands below are the implementation contract until the
status records their acceptance.

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

The portable desk display cannot establish HDR/Dolby Vision HDMI acceptance.
Keep that row pending for an appropriate display chain. No installer option
may pretend that installing a decoder supplies missing display capabilities.
