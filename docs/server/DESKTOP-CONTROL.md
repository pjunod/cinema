# Desktop controls — run and maintain one local server

**Status:** open · **Updated:** 2026-10-19 · implementation and native qualification in progress.

Companion to [the desktop implementation ledger](../features/DESKTOP-TRAY-IMPLEMENTATION.md)
and [upgrade recovery](../features/DESKTOP-UPGRADE-TRANSACTION.md). This document
owns everyday tray operation and the local controller. [The deployment guide](../../deploy/README.md)
owns source installation; [macOS installation](../../deploy/MACOS.md) owns its
platform prerequisites. This implementation does not publish a release or change
an existing machine's startup settings during development.

## Install the companion with the native server

The macOS release archive contains the complete runtime, controller helpers and
`Plurx Server.app`. Extract the versioned archive and run its installer as the
account that will run the server. The stable app location is
`~/Applications/Plurx Server.app`; the server remains a per-user LaunchAgent.
macOS 13 or newer is required. A GUI login session is required for the menu-bar
app and VideoToolbox service operation.

From a source checkout, `make install-macos` builds the matching native
controller and Swift companion as well as the daemon, prepares the complete
runtime before changing the service, and opens the installed app in the GUI
login. Building or copying `plurxd` alone does not install a menu-bar app. To
reopen the installed companion, run `open "$HOME/Applications/Plurx Server.app"`.
Choose **Show tray app at login** in its menu for future logins. The explicit
`--server-only` installer option retains the legacy daemon-only installation
for operators who deliberately do not want desktop controls.

Local source runtimes attest their actual native architecture and exact source
commit. They can later select a signed Universal release; they never make a
thin or unsigned release artifact eligible. Source installation needs Rust and
Swift even with `--binary`; release archives need neither.

On systemd Linux, install the server package and the matching `plurx-desktop`
package from the same release. The server package owns the daemon, controller,
service and narrow authorization helper. The desktop package owns the tray,
GTK4 dependency, icons, desktop launcher and autostart entry. A headless server
needs only the server package.

For a Linux GUI host running the standard local Compose deployment,
`make install-docker` installs a separate host tray. An existing deployment can
add it with `python3 scripts/install-compose-tray --checkout "$PWD" --update-mode source` from its
checkout; use `--update-mode published` for a qualified image deployment.
Refresh retains that explicit choice. It controls only the pinned local engine and two selected containers,
opens the observed published loopback port and reads bounded container logs.
It never installs a native host daemon or GUI dependencies in the image.
Root and headless installs print a later-install command. Containers recreated
by `make docker-up` require an explicit same-deployment tray refresh. Custom Pi
setup ownership and remote Docker engines are not supported by this adapter.

If the host reboots or an operation worker dies before completion, the private
operation journal holds the controls. Inspect the selected containers and run
that companion's controller with its exact installation path:

```sh
/path/to/plurx-compose-control --installation /path/to/installation.json updates status
/path/to/plurx-compose-control --installation /path/to/installation.json resume --transaction UUID --confirm-interruption
```

This command also reconciles interrupted startup-policy changes without
interruption confirmation when no stop/restart is pending. Resume refuses an active worker or different transaction/deployment and retains
the original restart policies from the interrupted operation. It never clears
the journal merely to enable new controls.

Container startup On selects `always`; Off selects `no`; the choice persists in
`deploy/.env` for recreation. Existing `unless-stopped` remains until changed,
and manually stopped containers under that policy stay stopped across engine
restarts. Docker must itself start at boot. Stop/Restart send TERM and wait for
actual container exit, without Docker's eventual forced kill. Long operations
continue in a private host worker after the UI closes; an uncertain outcome
holds controls for local recovery. Check for updates explains the existing
source/image deployment command; native release archives cannot update a
container. No image signature verification is claimed.

Local Linux source replacement retains a protected candidate receipt and original
running/startup policy before downtime. The native controller holds its launch
interlock across package replacement and exact identity/readiness verification;
retry reconciles that saved candidate. A partial package result stays held for
recovery. macOS validates migration conflicts before stopping the old agent and
retains a private identity/policy receipt for verified legacy recovery.

A previous source install is not adopted merely because its HTTP port responds.
The controller requires a native installation record with exact paths, owner,
service and release identity. The macOS installer preserves source configuration
and data and refuses an unauthorized replacement of protected legacy files.
Do not run the tray as root or change ownership of a legacy system directory to
make an upgrade pass.

## Read the observed status before choosing a control

| State | Meaning and available action |
|---|---|
| Running | The selected native service and local daemon are observed running. Open the web interface, read logs or request a safe stop/restart. |
| Starting | The supervisor is starting the selected service. Wait for its observed state. |
| Stopped | The supervisor positively reports the service stopped. Start is available. |
| Unhealthy | The selected process or service is present but its health check failed. Read logs and the specific error. |
| Unknown | Identity, authorization or observation could not be established. This is not permission to start a second server. |

**Open plurx** opens the selected installation's validated local web address.
**View logs** reads a bounded local log view: the per-user log on macOS or the
`plurxd.service` journal on Linux. Linux log access uses the scoped administrator
authorization helper; no global journal-group membership is needed. Neither action discovers or administers a
remote server.

**Start**, **Stop** and **Restart** operate on the exact native service. Stop and
Restart ask for interruption confirmation. A running daemon first fences new
admissions, proves its active workload and cluster constraints permit the
operation, drains HTTP bodies and recording finalization, and closes its store.
The controller requires the exact old process to disappear and a matching clean
shutdown receipt. An unresponsive server, failed drain, stale identity or failed
store close produces a specific error; the app does not force-kill the process.
For a multi-voter cluster, rolling restart/candidate rules still apply. A healthy
sole-voter installation can accept explicit planned local downtime.

Choosing **Quit tray app** leaves the server running. Stopping the server leaves
the tray available so it can be started again.

## Choose server startup and tray startup independently

| Preference | macOS | Linux |
|---|---|---|
| Start server automatically | Per-user LaunchAgent at GUI login; a menu-bar service is not a pre-login root daemon. | System service enablement at boot, independent of whether a GUI is present. |
| Show tray app at login | Native login-item registration for the stable app. macOS may require approval in System Settings. The menu reflects the OS readback. | XDG autostart override for the current desktop user. The daemon's boot policy is unchanged. |

Changing automatic startup does not imply Start or Stop now. Existing disabled
and stopped installations retain those choices across ordinary installation and
upgrade. A failed authorization or OS registration leaves the menu showing the
observed policy and explains the failure.

On Linux the tray launches automatically only in a local, active, user-owned
X11 or Wayland logind session with a real local seat, usable display, session bus
and a registered StatusNotifier host. It rejects root, SSH-only, forwarded-only,
headless and container sessions. When a desktop has no compatible indicator
host, automatic launch times out quietly; the application launcher opens the
control window with an explanation. Installing a desktop extension is optional
and is never done by the installer.

The user override is `tv.plurx.Server-autostart.desktop` under
`$XDG_CONFIG_HOME/autostart`, or `~/.config/autostart` when XDG_CONFIG_HOME is
unset. It has the same basename as the system entry, so `Hidden=true` actually
overrides that entry. Relative configuration paths cannot become a controller
write destination.

## Check and apply a named GitHub release

The tray shows the installed version and can check the stable release feed for
`pjunod/cinema`. A candidate must include a manifest signed by a compiled-in
Ed25519 public root, exact source/version/architecture metadata, and a complete
compatible artifact cohort. A server installed with its desktop package upgrades
both together. A headless server does not acquire the desktop package.

Choose **Check for updates**, read the version and release notes, then choose the
explicit named upgrade. Preparation verifies and stages all files before requesting
downtime. Linux prerequisite resolution and authorization happen before shutdown.
macOS staging verifies the complete app, helper and media-engine closure.
Distribution packaging also verifies Developer ID team identity, hardened runtime
and notarization requirements for a distributable candidate. An ad-hoc developer build is not distribution proof.

The durable update worker continues if the tray closes. Progress distinguishes
download/staging, waiting for authorization, safe shutdown, installation,
verification and recovery. Cancellation is available before shutdown commitment.
After commitment, the start interlock and durable journal retain ownership until
the installation is resolved; restarting the tray cannot abandon that journal.

The updater restores the pre-upgrade running/stopped choice and startup policy
only after verifying the installed cohort. An interrupted or failed transaction
is inspected before a service launch. Recovery either completes a known verified
stage or holds the service with **Recovery required** and a specific next action.
It never guesses a database/Raft rollback by swapping an executable.

**Release provisioning:** the checked-in production trust-root list starts empty.
Update checks and production manifest publication fail closed until the release
owner provisions the approved public roots and protected matching signer. macOS
distribution also requires its protected Developer ID/notary lane. Fixture keys,
ad-hoc apps and local qualification overlays are not production update roots.
The implementation ledger records their remaining release-readiness evidence.

## Use the same controller from a terminal

```bash
plurx-control status                         # Versioned JSON observation for the selected native installation.
plurx-control logs --lines 100               # Bounded local log view.
plurx-control start                          # Start only the recorded native service.
plurx-control stop --confirm-interruption    # Request a clean, identity-bound local stop.
plurx-control restart --confirm-interruption # Apply the same safe shutdown before restarting.
plurx-control startup get                    # Read server and tray startup policy.
plurx-control startup set server on          # Enable server automatic startup; do not start it now.
plurx-control startup set tray off           # Disable this user's tray login entry.
plurx-control updates check                  # Verify the stable signed release feed.
plurx-control updates status                 # Read durable progress or recovery state.
```

The command examples use the Linux PATH entry. On macOS invoke
`~/Applications/Plurx Server.app/Contents/MacOS/plurx-control` with the full
path quoted.

If an install reports an unresolved transaction, read `updates status` and run
explicit recovery with the transaction ID from the error:

```bash
"$HOME/Library/Application Support/plurx/runtime/control/plurx-recovery" --transaction <UUID>
```

A macOS upgrade interrupted at `restoring` can finish after its exact promoted
runtime, app and controllers, installed commit, original node, readiness and
unchanged startup/configuration policy pass verification. Recovery preserves a
running daemon or an originally stopped installation. A mismatch leaves the
transaction unresolved for explicit repair. Source-install failures retain their
cause in the journal; failures before shutdown commitment cancel the unchanged
preparation so a retry can proceed.

The CLI emits a bounded versioned JSON envelope, with a machine-readable error
code on failure; stderr carries diagnostics. A successful HTTP response alone
cannot authorize a process stop or upgrade. The controller uses a local Unix
connection with kernel peer identity, a fresh process nonce and exact operation
ownership; it does not save an admin token for unrelated requests.

Linux system mutations pass through the fixed installed helper and explicit
administrator authorization. Status and sanitized progress remain readable
without repeated authorization prompts. Private transaction journals remain
protected. An externally managed installation or unsupported supervisor reports
that limitation instead of attempting another supervisor's commands.
