# Desktop upgrade transactions — shutdown, installation and recovery

**Status:** open · **Updated:** 2026-10-19 · Astra-approved design; implementation in progress
on `effort/desktop-tray`; native qualification remains outstanding.

Section 6 of [the desktop tray implementation plan](DESKTOP-TRAY-IMPLEMENTATION.md),
extracted into its own file to meet the repository's 40 KB document budget.
It owns the standalone/cluster shutdown, package transaction, startup interlock
and interrupted-upgrade recovery contract. The parent plan owns all other
requirements, M0–M6 ownership, dependencies and acceptance. This is part of
Astra's approved revision 2; the [review record](DESKTOP-TRAY-REVIEW.md) records
the reviewed candidate and this mechanical split. No deployment is authorized.

## 6. Upgrade transaction — commit shutdown and recover before startup

```text
Discover → Verify → Stage complete cohort → Resolve prerequisites/authorization
    → Lock/revalidate → Prepare idle → Commit fenced shutdown → Clean exit
    → Install → Verify → Restore intended running state → Commit journal
                    │ failure / power loss
          Service-start guard → recover known stage / hold Recovery required
```

### 6.1 Standalone downtime and a generation-bound shutdown commitment

M2 distinguishes topology modes from a direct, peer-bound observation:

| Observed mode | Safe local operation |
|---|---|
| Healthy sole-voter native server | Authenticated planned local downtime with explicit interruption confirmation, local admission fence and complete idle accounting. It does not require surviving quorum while that one server is deliberately down. |
| Multi-voter member or learner | Existing rolling restart verdict/candidate rules, role and compatibility constraints remain required. A learner is not reclassified as standalone. |
| Positively identified uninitialized installation | Verified native-owner authorization may fence/stop this exact process; there is no admin account to invent. Setup has no active library workload. |
| SQLite recovery, activation in progress, unknown/stale topology | In-app safe Stop/Restart/Upgrade refuses with a specific maintenance instruction; no quorum bypass disguised as standalone. |
| Already stopped | Native identity, package/migration compatibility and authorization checks; preserve stopped state without starting a daemon to ask permission. |

An unresponsive server has an explicit administrator maintenance procedure;
it is not automatically force-killed to unlock an upgrade. Support safe
standalone operations as a first-class M2 deliverable, not a future follow-up.

Preparation accounts for all new/direct-read connections, reconnects,
mutable sessions, recording ownership and admissions in flight. Add the
missing daemon-side accounting/admission hooks rather than relying on the
current proxy-side TODO. A proxy path that cannot be accounted for is refused
for in-app upgrade. Prepare returns process nonce, topology/config generation,
operation ID, expiry and an unguessable owner capability on the verified IPC
connection. Status polling alone never authorizes installation.

`commit-shutdown` on local authenticated IPC atomically rechecks that exact
operation/process/generation, role/candidate, expiry, idle state and required
compatibility, then transfers the fence to a **non-expiring shutdown owner**.
It initiates the daemon's existing cancellation/drain/store-close path; it
never executes launchctl/systemctl. Once committed, cancellation cannot
reopen admissions. Before commitment, only the preparation owner can cancel;
expiry releases an abandoned preparation. After commitment, controller exit
cannot release the fence; the daemon completes shutdown and the durable
transaction/start guard owns recovery. New reads/recordings remain rejected
until process exit. M2 must define and test this transition inside the actual
serving fence, not only add a controller flag.

The daemon writes a final fsynced shutdown receipt with operation/process
nonce, drain result, store-close result and exit classification before normal
exit. A successful drain plus store close exits zero, so existing
`Restart=on-failure` / `KeepAlive.SuccessfulExit=false` does not relaunch it.
The worker waits for both the exact process generation to disappear and a
matching clean receipt. Missing receipt, HTTP drain timeout, failed store
close, signal death or an unexpected supervisor restart is a failed safety
check; no executable replacement follows. The start guard blocks an automatic
restart while a committed transaction is unresolved.

For a hung daemon, bound controller wait and report failure; do not enqueue a
supervisor stop that will later escalate. Keep OS default shutdown/kill
policies intact; no infinite launchd ExitTimeOut. User-requested system
shutdown or outside administrative stop can still force termination; the
worker detects the missing/unclean receipt and recovery remains conservative.
Only after a clean exit may an already-stopped job be unregistered for
replacement. Menu Stop uses a short durable transaction too, so power loss
cannot make a committed user Stop look like an unfinished Upgrade.

### 6.2 Package transaction, locking and user policy

Persist fsynced atomic journals under `/var/lib/plurx-control/transactions`
(root authority on Linux) or the user's managed macOS runtime state. Fields
include transaction/install IDs, operation kind, old/new release and package
identities, initial running and startup states, observed policy/config
revision, tray-login preference, phase, preparation/clean-exit receipt,
backup classification, retained old artifacts and recovery capsule version.
No bearer credentials. Stage/backup/capsule data live in durable owned storage,
not a temporary directory. Check signed sizes, free space and package tools
before any fence; cancellation is available until shutdown commitment.

**Linux prerequisites:** simulate the exact apt/dnf transaction before drain.
The initial implementation requires distro prerequisites already installed;
missing dependencies stop here with a documented install step. Do not first
fetch dependencies, prompt about removals or discover held packages after
stopping. Reject a solver plan that removes unrelated packages, changes
repositories, or installs an unselected cohort. Stage all selected signed
plurx packages before drain and apply without network access; dependency
trust remains with the distro. Future dependency prefetch is separate work,
not an unqualified offline-install claim.

**Lock protocol:** the app coordination lock protects its journal and native
operations, not arbitrary apt/dnf or external supervisor commands. Package
scripts never block acquiring that lock. All shipped installers and Linux
`preinstall`/scriptlet equivalents detect a trusted active transaction before
file mutation. They join only the journal's exact package plan and verified
root-worker process lineage; an environment transaction ID alone is not
authority. A conflicting outside package transaction refuses early, before
unpacking, instead of waiting on the app. During an authorized transaction,
scriptlets suppress automatic restart and report completion to the worker;
they do not recursively invoke recovery or apt/dnf. Ordinary package-manager
upgrades outside a tray transaction preserve current package behavior.

The worker invokes apt/dnf with nonblocking package-lock acquisition: never
wait holding a committed fence for a package-manager lock. Lock contention
before install aborts the attempt and restores the prior verified running
state; packages have not changed. Capture package inventory before and after
invocation. A manager that has begun installing is never killed for an app
timeout: keep observing its process/journal or report recovery required.
Detect changed package/config generations before proceeding, and test an
external package transaction already holding its lock and entering a
scriptlet. User Start/Stop/Restart mutations join the same coordination lock;
package recovery never relies on GUI availability.

**Policy preservation:** upgrades do not enable/disable server or tray
startup at all. Package scripts and macOS promotion retain those settings.
Re-read effective policy/configuration at restoration, preserve externally
changed values and report a conflict instead of overwriting them with the
initial snapshot. Unexpected external unit jobs/PID/config/package changes
hold restoration for inspection. No app can infer an outside command that
leaves no observable state change; the supported upgrade procedure requires
no concurrent external maintenance, and detection is conservative rather
than a claim that root administrators are locked out.

**Installation:** Linux keeps package ownership through apt/dnf. The Linux
server package owns stable helper/guard paths; a verified immutable copy of
the active worker and its required runtime closure is retained in the
transaction capsule before package replacement. macOS uses its per-user
layout in [the managed layout](DESKTOP-TRAY-IMPLEMENTATION.md#31-installation-identity-and-concurrent-callers) with journaled app/control/runtime promotion. Preserve custom
config/data/tool paths, startup and tray-login choices. Readiness must include
expected build and node identity for an initially running server. An initially
stopped server stays stopped; verify binary/signatures/config without startup.
Only after those checks and journal commit may the new tray reconnect and
report Complete.

### 6.3 Recovery runs before the daemon, even with the tray disabled

Every tray-capable native install installs a non-GUI start interlock before
its first update. Linux uses a root-owned recovery unit ordered before
`plurxd.service` at boot plus a mandatory pre-start guard for manual/restart
activation. The guard can inspect/hold startup with narrowly scoped root
execution; the daemon still runs as `plurx`. macOS's LaunchAgent always uses
the stable service-launch guard, which reconciles the journal before exec.
These paths run with tray login off and without a desktop. The recovery
engine/capsule is retained outside package/version paths being replaced and
has an explicit journal-version compatibility contract.

| Last durable phase | Recovery action before allowing server startup |
|---|---|
| Prepared/downloaded, no shutdown commitment | Verify no mutation; cancel any still-owned expiring preparation and discard/resume staged work. Preserve the prior installation. |
| Shutdown committed, no clean receipt | Hold startup; inspect exact process outcome and record failed/uncertain stop. Require explicit recovery when clean completion cannot be established. Never install. |
| Clean stop, install not begun | Validate old closure and restore intended prior state; respect a completed deliberate Stop as stopped. |
| Installing or partly configured Linux packages | Block normal daemon activation, inspect exact package-manager/process status and inventory. Finish only the authorized exact package plan through an explicit recovery invocation when safe; otherwise require documented package repair. A pre-start hook never runs nested dpkg/dnf. |
| macOS app/control/pointer partly promoted | Under lock, validate old/new closures and journal phases, complete promotion or revert only when the data-compatibility class permits. Never exec a mixed closure. |
| Candidate installed, readiness pending/failed | Verify expected identity and migration class; retry a bounded health check, perform compatible binary/package recovery or hold Recovery required. |
| Committed success or completed Stop | Allow the committed service policy; retain rollback artifacts until retention/compatibility checks permit cleanup. |

Boot recovery is a bounded reconciliation, not unattended consent to a new
upgrade. It only completes/repairs the exact previously approved transaction
or fails closed with logs and a repair command; it never fetches a new release.
Recovery can be resumed from the GUI or CLI; absent authority, unavailable
package tools or unknown journal schema leaves the server held, not launched
against uncertain state. Test reboot/logoff with the tray disabled at each
phase, including active-worker/helper replacement.

**Backup/rollback:** classify candidate migrations before stop. Backups use
the existing consistent portable-backup facility where applicable, after a
write fence and with credentials stored separately as its contract requires;
never label a live database-file copy as a restorable backup. Record exact
schema/version, data/config/credential scope and verification. For clustered
storage use cluster-coordinated backup/restore only; a single voter snapshot
is not an automatic fleet rollback. Configuration-only and binary-compatible
releases may recover retained artifacts without restoring data. If the new
binary could have written incompatible data, keep the failed server stopped
and provide its version-specific recovery procedure. Linux binary rollback
is a package-manager transaction, not manual file replacement. Failed package
scripts are not atomic. Never automatically restore old database/Raft state.
