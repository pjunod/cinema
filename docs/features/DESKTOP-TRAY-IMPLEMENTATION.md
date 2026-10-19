# Desktop tray — native server controls and GitHub release upgrades

**Status:** open · **Updated:** 2026-10-19 · implementation in progress;
Astra approved revision 2 on 2026-10-19 after R1–R8 were resolved.
Draft PR #1213 tracks promotion; the complete cohort receives one task PR
and post-PR Astra review. Native/release qualification remains outstanding.
[The qualification ledger](DESKTOP-TRAY-QUALIFICATION.md) records evidence and gaps.
This is the canonical implementation and acceptance ledger for project P31.
[The Astra review record](DESKTOP-TRAY-REVIEW.md) tracks R1–R8 and their disposition.

Companion to [the release contract](../RELEASING.md),
[macOS installation](../../deploy/MACOS.md), and
[the deployment guide](../../deploy/README.md). This plan executes the macOS
menu-bar app and Linux desktop tray, local server controls, startup policy,
and user-triggered upgrades from published GitHub release artifacts.
It does not authorize deployment, release publication, changing a real
machine's startup policy, or a fleet upgrade during implementation.

## 1. Product contract — the icon belongs to the desktop session

Paul requested a visible way to know that a natively installed server is
running, manage it, select automatic startup, and upgrade installations made
from GitHub release downloads. Linux headless servers and containers keep
working without a desktop dependency or an icon process.

| Surface | Required behavior |
|---|---|
| macOS logged-in GUI session | Native menu-bar icon, including while the local server is stopped. |
| Linux local graphical session | Tray item when the desktop supplies a compatible tray host; desktop launcher supplies a control window when it does not. |
| Linux headless, SSH-only, forwarded X11, container | No automatic tray launch; normal daemon/service commands continue working. |
| Server control | Observed state, Open plurx, View logs, Start, Stop, Restart. |
| Startup | Server automatic-start checkbox and independent desktop-companion login preference. |
| Upgrade | Current version, Check for updates, release notes, Upgrade to a named version, progress and recovery result. |

The app controls one explicitly selected local native installation. It never
chooses a remote/discovered server as a process-control target, administers
Docker, or starts another daemon because an HTTP probe timed out.
Closing the control window or choosing **Quit tray app** leaves the server
running. Stopping the server leaves the icon available for Start.

**Initial scope:** macOS 13+ on Apple Silicon and Intel; Linux systemd hosts
supported by the existing Debian/Ubuntu and Fedora/RHEL package contracts,
on x86_64 and aarch64, using X11 or Wayland. Non-systemd Linux can use the web
app and its own supervisor; automatic management is unavailable with an
explanation. Windows, unattended installation, development/nightly update
channels, remote fleet upgrades, and creating an apt/yum repository are
outside this effort. Dependency versions and deployment floors must be
pinned and proved before platform implementation.

## 2. Baseline — extend the installation paths that already exist

Inspected source baseline: `84ad750a978e9ba953e23b96702719068bb5adfc`.
The relevant packaging/control files are unchanged from the initial
conversation's `f15e3b2e71c2d3e788aee5084b04933b6895bbd9` baseline.

| Existing source | Current behavior / consequence |
|---|---|
| `deploy/com.plurx.plurxd.plist`, `deploy/install` | Per-user `com.plurx.plurxd` LaunchAgent in `gui/<uid>`. VideoToolbox needs that GUI session. The installer always bootstraps/enables/starts it, including upgrades. |
| `deploy/plurxd.service`, `deploy/install` | Dedicated `plurx` system user. Source installer enables/starts whenever the previous service was inactive; this must not override an existing operator choice on upgrade. |
| `deploy/packaging/{postinstall,preremove,postremove}` | Package upgrade uses `try-restart`, preserves disabled state and data; the tray's pre-install stop must not accidentally make a previously running server remain stopped. |
| `scripts/package-linux`, `deploy/packaging/nfpm.yaml` | Existing `.deb` and `.rpm` are server packages. Keep their headless dependency closure. |
| `.github/workflows/github-release.yml` | Public mirror workflow builds Linux packages and a universal macOS archive, checks its exact asset set, then publishes `SHA256SUMS`. Builds are read-only; only the publish job has release write permission. |
| Same workflow, macOS jobs | Currently assembles its own ad-hoc-signed archive; installer can fetch FFmpeg separately. It does not yet use the complete native-candidate layout. |
| `scripts/package-macos` | Separate richer candidate assembler: FFmpeg/parser/subtitle engine provenance, signing/entitlements and matched symbols. Ad-hoc candidates are explicitly not notarized distribution evidence. Reconcile these paths rather than adding a third. |
| `validation/release_artifact.py` | Exact source/build/target manifests and hashes exist for release binaries; these are integrity/provenance records, not a client update trust root. |
| `crates/plurxd/src/main.rs`, `http/cluster_operations.rs` | Restart preparation, expiry, admission drain and candidate checks exist. Preparation explicitly leaves direct-play/proxy verification separate. It never invokes a supervisor. |
| `http/system.rs` | Public `/api/v1/server` exposes version, build, node and instance identity. It is not authority to stop or replace a process. |

The tray and runtime libraries are new implementation. Do not describe
existing package smoke tests or a same-origin checksum as proof that the new
updater is safe, signed, or qualified.

## 3. Components — a companion, an ordinary controller and a narrow installer

```text
macOS AppKit app                 Linux SNI + control window
         │                               │
         └────── versioned JSON ──────────┘
                         │
               plurx-control (ordinary user)
                  │                 │
         launchd / systemd     release verification
                  │                 │
               plurxd       independent update worker
                                    │
                         platform installation adapter
                         (Linux authorization as needed)
```

**Code ownership:** add `crates/plurx-desktop-control` for the shared Rust
library and `plurx-control` CLI/update worker. Add `clients/server-tray/macos`
for a small Swift/AppKit app and `crates/plurx-tray` for the Linux UI.
Linux uses StatusNotifierItem/DBusMenu through `ksni`; use a small GTK4
control window for logs, authentication, confirmation and progress. GTK is
only a dependency of the separate desktop package. The macOS menu uses
`NSStatusItem`, native accessibility labels and template images; it needs
neither an embedded browser nor a Dock icon. AppKit stays on the main thread.
Rust blocking work and IPC never execute on a UI event loop.

**Controller boundary:** finite subcommands `status`, `start`, `stop`,
`restart`, `startup get|set`, `updates check|prepare|apply|status|cancel`,
and `logs`. GUI calls use argument arrays and schema-versioned bounded JSON
on stdout; logs use stderr. No command is constructed from a string returned
by HTTP, a release note, or an installation record. Long-running installation
belongs to a separate worker, which survives tray exit/replacement and exposes
an owner-only transaction receipt. GUI polling can reconnect to it.

**Privilege:** the tray never runs as root. Ordinary Linux service controls
use systemd D-Bus with the platform's interactive polkit authorization.
Do not ship passwordless grants for all units or all unit-file changes.
Package installation uses a root-owned, non-setuid helper invoked through
polkit (`pkexec`), with a closed operation set and no arbitrary executable,
unit, destination, URL, shell fragment, environment or script argument.
The stable helper is `/usr/lib/plurx-control/plurx-install-helper`; its polkit
action is `tv.plurx.install`, installed under
`/usr/share/polkit-1/actions/tv.plurx.install.policy`. It makes its own trusted
copy of inputs into root-owned staging, independently
verifies signed metadata and bytes, and takes the system installation lock
before invoking fixed absolute package-manager commands. User-writable
staging paths are never later reopened as root installation authority.
Cancellation/denial returns an actionable failure without changing state.

### 3.1 Installation identity and concurrent callers

Installers create a versioned `installation.json`, separate from the media
store, with: installation UUID, method (`macos-native`, `linux-deb`,
`linux-rpm`, `source`, `external`), absolute managed paths, service domain and
label/unit, configuration path, local origin, package architecture, release
repository identity, current tag/commit, controller protocol, update trust-key
IDs, and last successful transaction ID. No credentials are included.

Linux system records are root-owned under `/etc/plurx`; per-user macOS
records live under `~/Library/Application Support/plurx`. Use restrictive
permissions, atomic writes and schema validation. The record describes
installation routing, not unrestricted filesystem or supervisor authority:
adapters enforce known units, roots, ownership and package names independently.
The Linux server package owns the non-GUI controller, helper, service-launch
guard and boot recovery unit; the desktop package owns only GUI assets and
depends on a compatible controller. Server-only recovery never requires GTK
or a logged-in user. Startup preferences are read from the supervisor/registration, not guessed
from installation version or a replicated server setting.

Migrate existing native installs by positively identifying their unit/plist,
executable, configuration and data paths. Ambiguous, custom or conflicting
installations are shown as externally managed; never take them over silently.
A source build is not offered an automatic release downgrade merely because
its semver equals a tag. It may explicitly adopt a verified release through
the installer, preserving its paths. Earlier releases without a tray/controller
need one manual upgrade to the first tray-capable release.

One UI instance per user/session; one controlled mutation/upgrade per
installation across users, tray, CLI and the managed installer paths. Linux
uses a root-owned coordination lock; macOS uses an owner-only lock. Busy
callers observe the active operation. Package-manager locks have their own
protocol in [the package transaction contract](DESKTOP-UPGRADE-TRANSACTION.md#62-package-transaction-locking-and-user-policy); ordinary external `systemctl`/`launchctl` commands do not
participate in the app lock. Detect observable conflicts and report them;
never claim to serialize arbitrary concurrent administrator actions.

**macOS managed layout:** the tray-capable installer adopts a per-user layout:
`~/Applications/Plurx Server.app` is the stable login-item bundle location;
`~/Library/Application Support/plurx/runtime/versions/<release-id>/` holds
immutable complete runtimes; `runtime/current` is an atomically replaced
relative symlink within that managed root. Stable, separately versioned
controller/service-launch/recovery entry points live under `runtime/control/`.
The daemon plist points to service-launch, which resolves a verified current
runtime only after recovery checks. App/controller replacement uses a
journaled two-phase install under the same transaction; neither is claimed
to be atomic together with the runtime pointer. OS login-item registration
always names the stable app location and is revalidated after replacement.
Active workers run from a retained immutable transaction capsule, not a path
about to be replaced. Retain required capsule/control protocol versions until
the transaction is committed and a later recovery compatibility check permits
cleanup.

For legacy user-owned prefixes, the release's prebuilt `./install` imports
the existing plist/data/config and repoints only its managed daemon/default
tool paths after a confirmed stop. Root-owned `/usr/local` or other protected
legacy paths are never overwritten, chowned or deleted by the per-user
updater. A one-time migration creates the per-user runtime and updates the
user-owned LaunchAgent; it leaves old protected binaries inert and offers
explicit administrator-authorized manual cleanup instructions. If a
system-domain job or protected registration also exists, refuse automatic
adoption and require its explicit authorized removal in the bootstrap
installer before switching. No in-app root macOS helper is introduced. The
installer inventories conflicts and proves there is one service owner before
migration; it preserves operator-defined external tools and all user data.
A denied required authorization leaves the legacy installation usable and
unchanged. First installation/migration and routine upgrade need no checkout,
Homebrew or compiler.

### 3.2 Bind controller traffic to the actual daemon

M0/M2 add an HTTP-over-Unix-socket control transport to plurxd, using the same
login/session and administrator middleware as the existing HTTP handlers.
Only a fixed local route subset is exposed: public process identity, existing
auth handlers, restart preparation/status/cancel/commit and necessary local
readiness/cluster observations. This is a new documented local IPC surface,
not an unauthenticated network stop/update route. It carries control requests
and saved credentials on the **same peer-verified connection**; do not send
them to a separately probed loopback HTTP port. Disable HTTP redirects and
proxy environment discovery for all controller traffic.

Linux creates `/run/plurx/desktop-control.sock` in a root-created directory
owned by `plurx`, not writable by desktop users. Socket connect permission
allows ordinary users to perform the minimal public handshake; stateful
operations require server-admin authentication. Kernel peer credentials must
match systemd's current MainPID, service UID and invocation identity; bind
PID/start generation and connection lifetime, then revalidate before a
mutation. Permission-restricted process inspection is performed by the
narrow installed helper, not assumed available to the GUI. When polkit
already authorizes that helper to manage this exact unit, its verified root
peer can request the same fenced operations without storing a server token.
This permits safe first-run local control while preserving cluster gates.

macOS creates an owner-only socket below the user's managed runtime state.
Use `getpeereid` and `LOCAL_PEERPID` plus launchd's current service process and
process-start generation. The authenticated local owning UID has the native
service-control authority it already possesses; server-admin sessions remain
available. A different installation/socket owner, stale PID/generation or
unexpected peer fails closed. The daemon handshake includes a fresh
per-process nonce, node/instance identity, runtime build, configured browser
origin and supervisor binding. Public identity strings alone are not proof.
A connected Unix socket cannot silently switch peers after process exit;
reconnect requires a new full handshake before reusing saved auth.

Use existing admin login on this transport when native OS authorization is
not in use. Store refresh/session material in Keychain or Secret Service;
use memory-only sessions if a keyring is absent. Never persist credentials
in metadata, argv, release requests or logs. First-run setup opens the web
interface. If the daemon is stopped, Start uses the positively identified
supervisor and needs no HTTP response. For a legacy daemon without the new
transport, status/browser/logs remain available and the manual bootstrap
installer handles adoption; the tray cannot invent a weaker token-bearing
loopback fallback.

The browser still opens the configured local web origin, mapping wildcard
binds to matching loopback and preserving port/base path. Custom proxy/public
URLs can be browser destinations but are never lifecycle-control targets.
Readiness and version from public HTTP are advisory until the local peer is
bound. Test lookalike HTTP servers replaying node IDs, port reuse, PID reuse,
restricted cross-user inspection, token expiry and reconnect races.

## 4. Menu and observed states — never equate a process with readiness

```text
plurx — Running / Starting / Stopped / Unhealthy / Unknown
Version 0.x.y (build)
Open plurx…
View logs…
────────────────────────
Start / Stop… / Restart…
☑ Start server at login           [macOS]
☑ Start server at boot            [Linux system service]
☑ Show tray app at login
────────────────────────
Check for updates…
Upgrade to 0.x.z…                 [only verified compatible release]
────────────────────────
Quit tray app
```

A check mark is committed only after observing the effective result. Display
an authorization-required, masked, policy-managed, pending-approval or unknown
state as such. Disabling startup changes future startup, not current running
state. Start/Stop change current state without changing the startup checkbox.
Restart preserves both startup and prior configuration. A separate update
state shows Checking, Available, Downloading, Verifying, Waiting for idle,
Installing, Restarting, Complete, Failed or Recovery required.

Poll service state and bounded identity/readiness probes every five seconds
while the menu/window is visible and every thirty seconds otherwise. Use a
two-second network timeout, no overlapping polls, and refresh immediately on
an operation completion. Unknown/unreachable is distinct from stopped; a
starting server is never an invitation to spawn a second one. Use shape/text
as well as color. Log view shows the actual LaunchAgent file or bounded
`journalctl -u plurxd.service` output, with search/copy and visible permission
errors, without enabling persistence or exposing unrelated journal entries.

### 4.1 macOS startup — retain GUI-session hardware access

Label server policy **Start server at login**. A boot-time LaunchDaemon would
change VideoToolbox behavior and is outside scope. Preserve the existing
`com.plurx.plurxd` GUI-session service and operator environment/tool paths.

Store a managed canonical plist outside launchd autoload directories.
Automatic startup registers a regular copy under `~/Library/LaunchAgents`;
turning it off removes only that managed login registration, retaining the
canonical definition. An already loaded job remains running. Manual Start
bootstraps the canonical definition into the current `gui/<uid>` domain when
needed, so it remains available with automatic startup off. Stop uses the committed local shutdown protocol in [the shutdown contract](DESKTOP-UPGRADE-TRANSACTION.md#61-standalone-downtime-and-a-generation-bound-shutdown-commitment); bootout only
unregisters an already-exited job. It leaves login policy alone. Restart
starts it again only after a proved clean exit. A deliberate stop must not
trigger KeepAlive to restart it. Neither `bootout`, `kickstart -k` nor
`systemctl stop` is treated as a graceful-only operation: supervisors can
escalate to SIGKILL.

Do not implement Off by only changing `RunAtLoad`: the current
`KeepAlive.SuccessfulExit=false` implies initial demand. Validate the chosen
registration/removal sequence on each supported macOS version, including
logout/login and external `launchctl disable` state. An externally disabled
job is reported distinctly; ordinary Start/migration does not undo it. An
explicit Enable action is required before Start can proceed. Import existing plists
without losing custom keys. A changed/conflicting registration requires an
explicit migration, not deletion of an unrelated file.

The menu app's separate **Show tray app at login** uses `SMAppService.mainApp`
in its `.app` bundle. Report OS approval/disabled status accurately; never
re-enable it every launch or restart the server just to register the UI.

### 4.2 Linux startup — system boot and desktop login are independent

The server checkbox queries systemd unit-file state and uses enable/disable
without `--now`. Follow Start job completion and then inspect actual
state/readiness; healthy Stop/Restart use [the shutdown contract](DESKTOP-UPGRADE-TRANSACTION.md#61-standalone-downtime-and-a-generation-bound-shutdown-commitment) rather than assuming a systemd
stop job proves graceful drain. Masked/static/unavailable units are explanatory
states, not booleans to overwrite. A desktop user's logout never stops the
system daemon. Changes to boot policy require the existing platform
authorization; the checkbox survives an authentication cancellation.

Install the UI in a separate `plurx-desktop` `.deb`/`.rpm`, with desktop entry,
icons and XDG autostart entry. Installing the server package alone introduces
no GTK, desktop bus, polkit GUI agent or UI process dependency. Desktop
installation enables the user's tray-login preference through its setup flow;
it does not change server boot policy. Respect user `Hidden=true` overrides.

Automatic launch verifies logind session owner/UID, `Remote=false`,
`Active=true`, Type `x11` or `wayland`, and a local seat where logind exposes
one, plus usable session bus/display. Reject root, SSH/forwarded-only and
container contexts; missing/ambiguous session proof means no automatic launch. Do not infer GUI eligibility from a plugged-in monitor
or `DISPLAY` alone. Require and observe a registered StatusNotifier host;
wait at most thirty seconds at login for the panel, then exit quietly if
unsupported. A tray host appearing after that exit requires the desktop
launcher or the next login; the UI does not leave a polling daemon behind.
A manually opened desktop launcher still shows the control
window and explains missing tray-host support. A GNOME session without the
appropriate indicator support is not claimed to have an icon; do not install
shell extensions or modify desktop configuration behind the user's back.
Handle tray-host disappearance/reappearance without affecting the daemon.

## 5. GitHub Releases — complete, authenticated installation artifacts

### 5.1 Published asset set and compatibility metadata

Extend `.github/workflows/github-release.yml`; preserve its public-mirror
conditions, read-only builds, immutable tag/commit checks, and publish-only
write authority. Final filenames and asset-set validation change together.

| Platform | Release artifacts |
|---|---|
| macOS | Existing universal archive name retained; add `Plurx Server.app`, controller/worker, installer and complete matching FFmpeg/FFprobe, WASM parser, subtitle engine, licenses and provenance. Preserve daemon JIT entitlements and each architecture's acceptance. |
| Linux amd64 / arm64 | Existing `plurxd` server `.deb`/`.rpm` gains the non-GUI controller/worker/helper/guard/recovery; matching `plurx-desktop` packages own GUI assets only. Server-only headless upgrades remain possible. |
| All | Existing `SHA256SUMS`, plus signed `plurx-update-v1.json` and detached signature. Existing Windows artifacts remain unaffected and outside tray work. |

Bring macOS release assembly onto the verified `scripts/package-macos`
closure. No first-run or upgrade build, git checkout, cargo, Homebrew, private
repository credentials, or unpinned network tool fetch is allowed. The macOS
archive is self-contained and installs offline after download. Linux retains
its declared distro runtime prerequisites; [the package transaction contract](DESKTOP-UPGRADE-TRANSACTION.md#62-package-transaction-locking-and-user-policy) requires these to be already
installed before drain in the initial implementation. Dependency prefetch is deferred. User media/models are
not bundled or re-downloaded. macOS minimum OS, Linux glibc/distro floors,
CPU architecture, package method and desktop protocol are explicit constraints.

The signed manifest includes: schema, repository, tag, semver, monotonically
increasing release sequence, source commit/tree, packaging commit, creation
and expiry times, supported upgrade-from versions, data/migration and cluster
compatibility IDs, minimum controller version, rollback classification,
release-notes URL, and the complete artifact set (asset name, platform,
architecture, format, package/version identity, byte size, SHA-256).
Select a cohort by the installed package method: server-only Linux selects
only the server/controller package; an installed desktop selects the matching
server/controller plus desktop package. Adding desktop is a separate explicit
installation. macOS selects its complete native runtime/app set. Missing or
incompatible members of that cohort make the candidate unavailable, never a
partial upgrade. Compare signed application semver plus exact distro package
epoch/version/release and architecture; do not compare an RPM/deb revision as
application semver or override held/pinned packages.
Backfilled old tags distinguish source and packaging commits and cannot
silently replace a published manifest/asset identity.

### 5.2 Trust and discovery

Use an Ed25519 detached signature over the exact manifest bytes, verified
against public update keys embedded in the controller/helper. Pin the signed
repository identity and stable release channel. `SHA256SUMS` and GitHub's
asset digest are additional integrity checks; neither establishes independent
publisher trust. Private signing keys exist only in the protected release
signing job, never runners executing untrusted PR code, release downloads or
installed clients. Support key rotation with an overlapping pinned key set
shipped in a trusted release; revocation/recovery is documented before launch.
Ad-hoc macOS code signing remains separate from update-manifest authentication.

Use unauthenticated GitHub Releases REST discovery for the configured public
mirror, then verify the named manifest/signature and artifact identities.
Do not embed a repository token. Ignore drafts/prereleases in stable, compare
semver numerically, and reject lower release sequences, incompatible upgrade
paths, changed bytes for a previously seen release, and unsupported schemas.
Persist the highest accepted sequence. Expired metadata or a clock too far
outside signed validity means manual attention, not a signature bypass.
A legacy checksum-only release can be installed manually but is never an
automatic in-app upgrade candidate.

Bound API/manifest size (1 MiB manifest, at most 128 artifacts), download size
by signed size plus a fixed product ceiling, redirects (five), timeouts and
free-space requirements. HTTPS only; allow GitHub release asset CDN redirects
through a maintained host policy, with no credentials/cookies forwarded.
Restrict release-notes/browser URLs to trusted HTTPS origins. Archive
extraction rejects traversal, absolute entries, links, special files and
duplicate members. Verify the extracted expected closure before execution.

Default automatic **checks** are once daily with jitter; expose their own
preference and send no telemetry. Cache ETags, honor rate limits/Retry-After
and back off offline. A manual check reports unavailable/rate-limited without
losing server controls. **Installation is always user-triggered** and names
the target version, restart interruption and migration/rollback implications.
Do not infer user consent from a background check.

## 6. Upgrade transaction — the detailed shutdown and recovery contract

[Desktop upgrade transactions](DESKTOP-UPGRADE-TRANSACTION.md) owns this
section's full implementation contract. Its three parts remain mandatory:

| Contract | Detailed specification |
|---|---|
| Standalone/cluster planned downtime, peer-bound shutdown commitment, clean exit receipt | [§6.1](DESKTOP-UPGRADE-TRANSACTION.md#61-standalone-downtime-and-a-generation-bound-shutdown-commitment) |
| Selected package cohort, preinstalled prerequisites, lock order, state preservation | [§6.2](DESKTOP-UPGRADE-TRANSACTION.md#62-package-transaction-locking-and-user-policy) |
| Mandatory pre-start guard, non-GUI recovery, journal stages and migration-aware rollback | [§6.3](DESKTOP-UPGRADE-TRANSACTION.md#63-recovery-runs-before-the-daemon-even-with-the-tray-disabled) |

The split changes document placement only; it does not defer these mechanisms
or weaken the M2/M5 dependencies and acceptance below.

## 7. Milestones — one integrated effort with reviewable tasks

Integrate on `effort/desktop-tray`; each task branches from current effort and
returns to it. Files overlap, so the independent-main exception does not
apply. Table ownership identifies the writer for each task; integration owns
shared manifests, release workflow, documentation and acceptance ledger when
parallel branches would otherwise conflict. Task gates do not qualify a release.

M1–M5 integrate as one cohort task PR into the effort, preserving their owned
commits and focused regressions. This supersedes partial component PRs; complete
review, the native transaction batch and the effort gate still precede its merge.

| Task | Owns / delivers | Depends on | Acceptance before task merge |
|---|---|---|---|
| M0 · identity, installation and transaction contracts | Shared record/JSON/IPC/auth types, topology modes, fixed privilege paths, macOS migration/layout, journal/start-guard/lock protocol, package cohorts, signed schema/key/bridge procedure and fixtures. | Plan review | Static/schema and compile evidence; platform feasibility recorded. No native upgrade acceptance claimed. |
| M1 · controller and service adapters | Shared controller/CLI, effective startup readback, policy/config generations, locks/journal interface, keyring and helper invocation adapters; scriptlet/installer conflict protocol. | M0 | Compile/static checks plus focused adapter/migration regressions required before push; upgrade integration acceptance belongs to M5/M6. Live disruptive controls remain unavailable until M2 integration. |
| M2 · process identity and safe shutdown | Daemon local IPC/peer binding, standalone-vs-cluster preparation, direct-read/recording accounting, atomic commit-to-shutdown and final receipt; controller integration in a coordinated task branch. | M0, then M1 controller integration | One-voter success, unknown/multi-voter refusal, nonce/peer reuse, expiring-fence handoff and clean/unclean drain regressions. No desktop screenshot substitutes for these. |
| M3 · macOS app and migration | Swift app, stable bundle/login registration, canonical plist migration, native logs and per-user runtime adapter. | M1 + M2 for lifecycle integration | Build/static/UI contracts; focused migration/startup regressions. Actual Intel/ARM login/transcode/upgrade acceptance is retained for M6. |
| M4 · Linux desktop app | ksni/GTK UI, logind session detection, desktop/autostart/icons, helper/auth frontend; server package keeps non-GUI helper/recovery ownership. | M1 + M2 for lifecycle integration | Compile/static/UI contracts and focused session/authorization regressions; integrated privilege/multi-user/package behavior is M5/M6. |
| M5 · artifacts, worker and boot recovery | Offline macOS closure; selected Linux package cohorts; signed publication; immutable active worker/capsule and pre-start recovery; repair scriptlet state behavior. Integration writer owns shared packaging/workflow paths. | M0–M4 | Independent complete-change review/fixes, then focused native transaction/package batch: real old-artifact → new-artifact and recovery, including no-tray boot, lock conflicts and helper replacement. |
| M6 · final qualification and promotion | Docs/ledger, evidence inventory, independent final integrated review, affected batch and current-main promotion. | M0–M5 | Required native/session/release matrix on final tree. Reuse unchanged valid evidence; rerun only failed/invalidated checks. Main promotion gate and qualification receipt. |

Do not add a Developer switch to the daemon for tray infrastructure. Tray
visibility, startup and update-check preferences are useful permanent local
controls in the companion. Any temporary advisory Developer card needs named
remaining evidence and graduates under the repository lifecycle rule.

## 8. Verification — package behavior and real sessions are acceptance

**Local loop:** before any Rust edit establish the pinned 1.97.1 compiler,
then check, clippy `-D warnings` and rustfmt on the exact intended
branch/base. Compile/static inspection can run during development; complete
the independent whole-change review and fixes before the affected unit batch.
The user-provided repository AGENTS.md still requires each task
branch's smallest meaningful focused regression before push/merge; record it
once and reuse unchanged evidence rather than replacing that rule with the
HDR-specific workflow's test deferral. Quick documentation/paperwork checks
are also allowed before review. Integration/native acceptance is not claimed
from mocks or a task's compile checks. Use `scripts/quiet` for builds/suites.
New desktop crates
have their own focused checks; headless `plurxd` build/test must retain its
desktop-free closure. Apple native app tests do not count as tray app tests.

**Automated coverage:** controller state transitions/locks/journal recovery;
install migration/ownership; signed-manifest rejection and archive limits;
version/architecture/package selection; supervisor authorization/readback;
expiry and identity races; streaming/recording drain; package scripts and
preserved stopped/disabled state; install failure before/after each durable
stage; UI menu enabled states, accessibility and error mapping. Test behavior,
not a duplicate of production implementation. Include negative controls that
fail if signature, drain, identity or stopped-state checks are removed.

**Native matrix:** test macOS ARM64 and x86_64 with real login sessions and
hardware transcode after upgrade; Debian/Ubuntu and Fedora/RHEL package
install/upgrade/remove paths; representative KDE Wayland, X11 desktop and
GNOME with/without tray-host support; polkit unavailable/denied/approved;
headless and container server-package installs. VM session/reboot tests use
owned disposable hosts, never modify a developer's real boot/login policy.
A screenshot proves appearance, not drain, identity or crash recovery.

**Release matrix:** start from the actual published old archive/package,
with no checkout/compiler and a fresh user profile. Cover running+enabled,
running+disabled, stopped+enabled and stopped+disabled, custom config/port/tool
paths, old tray protocol, server-only Linux, partial/tampered download,
offline/rate-limited GitHub, package lock, low disk, privilege cancellation,
tray exit during installation, power loss at journal boundaries, health-check
failure with rollback-compatible and migration-incompatible releases, and a
multi-voter safe/unsafe/expired candidate. Never run destructive restore tests
against production libraries or clusters. Retain old/new asset hashes and
exact source/package build identities with each receipt.

**Review-driven required cases:** retain each case as a named regression or
native receipt rather than treating the general matrix as implicit coverage.

| Contract | Required false/control cases |
|---|---|
| Standalone vs cluster | One-voter planned downtime succeeds; unsafe multi-voter, learner misclassification, recovery/unknown topology fail. Uninitialized owner-authorized Stop has no invented account. |
| macOS adoption | User-owned prefix, protected `/usr/local`, custom plist/tool paths, denied required migration authorization, conflicting system-domain job, external disabled job and stable login registration after rollback. |
| Shutdown | Deliberately non-terminating daemon and HTTP/store-drain timeout on both OSes never produce an installation-authorizing clean receipt. Outside forced kill remains distinguishable. |
| Fence handoff | Expiry/cancel immediately after the final prepare response, delayed controller, new direct-read/recording admissions, controller death after commitment; no committed fence reopens. |
| Recovery/concurrency | Reboot with tray off at each journal phase, external apt/dnf lock + scriptlet, active helper replacement, partly configured package, changed startup policy after preflight and unsupported journal version. |
| Identity | Lookalike loopback identity replay, PID/start-generation reuse, socket reconnect and restricted Linux cross-user inspection never receive saved auth before proof. |
| Cohorts/prerequisites | Fresh Linux missing dependencies/offline fails before drain; server-only install/update adds no GTK; held packages/epoch revisions and unrelated-removal solver plans are refused. |

**Repository gates:** update package/workflow contract tests, document routes
if M2 adds any, add new docs to their subject index and keep this ledger/P31
current. Every behavior fix uses `fix(`/`perf(` and names focused
`Regression-Test:` entries in PR and landing message. Run `make paperwork`
before pushing; manually dispatch a passing Effort development gate for each
task merge. Freeze tasks, integrate current main, then qualify and pass Main
promotion gate before effort promotion. Any candidate change invalidates
only the evidence whose source/runtime inputs changed; do not claim prior-base
compilation or a package assembly receipt as final runtime qualification.

### 8.1 Launch prerequisites and retained boundaries

Before offering Upgrade to users, the release owner must provision the public
update verification keys/signing job and exercise recovery/key rotation.
No signing secret is invented or committed by this plan. Confirm the public
mirror identity already used by GitHub packaging, not the private Forgejo URL.
Developer ID/notarization requires available distribution credentials; until
provided and proved, retain truthful ad-hoc/unnotarized installation wording
and Gatekeeper instructions rather than claiming seamless first launch.

A controller that skips an overlapping trust-key or minimum-controller
bridge release uses the verified manual bootstrap installer; do not claim
every old controller can jump directly to every later release.

A manually installed old release is the bootstrap path; an installed tray
must never accept unsigned new update metadata to make that migration easy.
Releases that cannot prove direct-play/recording drain or cluster-version
compatibility retain a manual maintenance path instead of an unsafe upgrade
button. User-triggered upgrade is required; background installation remains
outside scope.

## 9. Execution ledger and primary references

| Date | Record |
|---|---|
| 2026-10-19 | Requirements captured: desktop-only native icon, status/web/logs/start/stop/restart, independent startup policy, GitHub release-artifact installation and user-triggered upgrades. Draft reviewed by Astra in `t_9ed58371` (changes required: R1–R8). Revision 2 defines standalone downtime, peer-bound local IPC, committed graceful shutdown, legacy macOS migration, boot recovery/locks, package cohorts and corrected task dependencies. Astra recheck `t_ec1a09fb` approved revision 2 with no blocking findings; two summary phrases were clarified as permitted by that review. No implementation or runtime acceptance claimed. |

Primary platform references, checked 2026-10-19:

- [Apple SMAppService.mainApp](https://developer.apple.com/documentation/servicemanagement/smappservice/mainapp) — app login registration.
- [Apple launchd plist reference](https://github.com/apple-oss-distributions/launchd/blob/main/man/launchd.plist.5) — KeepAlive/RunAtLoad semantics; confirm against the installed OS manual in M3.
- [StatusNotifierWatcher](https://specifications.freedesktop.org/status-notifier-item/latest/status-notifier-watcher.html) — registered tray-host capability.
- [Desktop autostart specification](https://wiki.freedesktop.org/www/Specifications/autostart-spec/) — login-scoped application startup.
- [systemd D-Bus manager](https://www.freedesktop.org/software/systemd/man/257/org.freedesktop.systemd1.html) — separate unit runtime and unit-file policy operations/authorization; use the oldest supported interface subset.
- [GitHub release API](https://docs.github.com/en/rest/releases/releases) — releases and asset metadata. Public API digests supplement the signed update contract.
