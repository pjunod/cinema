# Source custody recovery — settle abandoned readers with physical proof

**Status:** open · reboot-free macOS/Linux witnesses qualified; legacy fleet acceptance pending · **Updated:** 2026-11-11

Executes the first independently useful recovery portion of
[P42's RCA and proposed fix](JOB-QUEUE-ROOT-CAUSE-AND-FIX.md). That document
owns the larger scheduling and queue-management project. This is the bounded
implementation ledger and recovery procedure for expired physical custody.
Do not use it to infer that a dead lease proves a dead reader.

## 1. This first PR restores a safe recovery path

**Branch:** `codex/source-custody-recovery` · **Owner:** Codex · **Review:**
Astra implementation review cleared after corrections · **PR:** #1378.

This is an independent corrective PR to `main`, so a usable incident repair
does not wait for the remaining queue redesign. The remaining M1 containment
work and M2–M4 stay on the proposed effort workflow. This is not the
independent-file-ownership exception: those later tasks still share files.
The main-bound PR must pass the affected-surface gate and the full unit run
requested by the maintainer before merging.

**Current repair:** new daemon-mediated readers retain an operating-system file
lock for their full physical lifetime. After that Cinema process exits, the
next daemon can settle the exact abandoned owner on the same kernel without
rebooting the server. The protocol applies to native macOS, native Linux and
containers. It does not depend on Docker process identities or Linux cgroups.

The initial v1 Linux reboot verifier remains a compatibility reader for proof
of a reboot that already occurred. A host reboot is not the operational repair
for a Cinema reader. Legacy records without sufficient physical evidence need
a separate verified Cinema-only stop procedure; that migration is still open.
Do not manufacture identity for an old record or treat lease expiry as proof.

Qualification of the initial repair also found scan stack exhaustion and
canonical managed-schema v4 bootstrap misclassification. Those landed repairs
preserve directly awaited cancellation/join ownership and identity checks.

### 1.1 New readers carry a kernel lifetime witness

[Reader witnesses](../../crates/plurxd/src/source_custody/recovery/reader.rs)
use the separate `source-custody-recovery/v2/` directory. Before activation,
the daemon creates and exclusively locks a private local file. Its evidence
binds the immutable custody token, node, activation nonce, kernel instance,
and the anchor's device, inode, change time and nonce. The authoritative activation must
acknowledge the exact new nonce before a separate durable marker arms it.
Rejected activation cannot arm a legacy owner's witness.

The daemon retains the lock even if a caller abandons its future. Only an
acknowledged physical join removes it normally. Recovery acquires that exact
lock after process exit, verifies the same kernel and untouched anchor, and
holds the lock through the existing typed Store settlement and release.
Cancellation, PID reuse, a copied receipt, replaced anchor, missing marker,
foreign kernel or a failed release cannot authorize another owner's release.
Store errors retain the evidence for retry. Original change-time equality also
rejects old bytes restored in place. Raw launch is refused unless durable
invalidation advances that kernel timestamp, including coarse filesystems.

Every production child launch is observed before exec. Only narrowly audited
children that receive broker URLs and no original media descriptor or pathname
retain daemon-only eligibility. Unknown launches and raw descriptor handoffs
permanently invalidate the held anchor before launch. The broker's original
source stays inside the daemon; surviving children cannot keep reading after
its death. This conservative rule may leave an owner held when an unaudited
child ran concurrently; it must never assume a raw reader stopped.

| Runtime | Lock domain identity | Supported anchor filesystems |
|---|---|---|
| Native macOS | Kernel `kern.bootsessionuuid` | APFS, HFS |
| Native Linux | Actual procfs kernel boot UUID | ext, XFS, btrfs, tmpfs, overlay |
| Container on Linux | Actual procfs UUID, including a provisioned host procfs bind | Same local Linux filesystems |

Unknown or remote filesystem lock semantics remain held. The v2 path runs
before the legacy Linux identity lookup, so native macOS needs no Linux host
identity mounts. A v2 record is never retrofitted onto an already active reader. Admission and
replay share a 4,096-owner / 16,384-file bound, reserving each pending arm marker
before creating an anchor. Sidecars do not consume the logical owner limit.

**Acceptance:** seven native macOS and eleven native Linux cases pass across
retained executions and a failed-only fixture retry, covering both custody
families and all independent background owner types, cancellation, a surviving
blocking reader thread, process exit, rejected activation, descriptor export,
command reuse, foreign/corrupt anchors, and failed Store release retry. Native
macOS/Linux workspace all-target Clippy passes. The installed legacy migration
remains pending; these observations are not fleet acceptance.

### 1.2 The finite legacy DV migration uses host-admin stop proof

The Linux-only compatibility consumer reads the fixed, root-controlled
`/run/plurx-physical-stop/legacy-dv-title-v1.json` path. The worker cannot create,
modify or remove it. The proof covers one audited historical v1 activation by
its exact file digest, immutable claim and custody identity, expired authority,
and the authoritative DV title job's scanner digest. It does not add general
permission to release old records or infer an old PID from a newer container.

The daemon and both native reader classes were verified in the old artifact
closure. A complete initial-host PID namespace census must open and inspect
actual executable descriptors, including each live thread, in two matching
passes. It retains those descriptors, rejects unreadable/zombie/unknown/changing
inventories, and requires zero matching readers. Cinema's data-directory lock
fences new startup during the census; the lock alone is not stop evidence.
The [host helper](../../scripts/source-custody-reader-stop) performs no service
stops, signals, reboots or database writes. Its default `--check` mode publishes
nothing; `--publish` performs its own fresh census under the startup fence.

The authoritative consumer checks the exact legacy activation and current job
again, rejects any v2 witness, and uses the existing typed Store settlement and
release. Failure keeps the journal. After acknowledged exact absence it removes
only that worker journal; host-admin proof remains read-only until its operator
removes it. Containers need an explicit read-only bind of the host proof
folder; ordinary v2 recovery needs no such mount.

**Acceptance:** fifteen new helper controls pass on native macOS, and the real
Linux executable-handle case passes. Both Linux Store refusal/release cases
pass. Helper publication and fleet recovery are pending. No service has
been stopped and no old reservation has been released by this continuation.

### 1.2a A live owner settles through its completion ledger

On Linux every job custody has an in-memory completion ledger. It counts
InputState holders and declared raw children from the moment the job starts.
An enrolled brokered preparation also hands it the original File Arcs.
`ActiveBackgroundJob::finish` retires the claim and then authorizes the
ledger; an unenrolled job keeps the explicit join. Dropping a job without
`finish` never settles.

Once every holder has retired, the ledger publishes the joined proof:

- **at once** if the witness saw no unknown launch;
- **after an unknown launch** (any unmediated daemon child, including
  another owner's), only once every live daemon descendant started at or
  after that launch's boot tick has exited. Those are the only processes that
  could have received or reopened the source. The daemon is PID 1 (or a
  subreaper), so orphans stay in its tree; otherwise it refuses with a warning;
- **on a backend without witnesses** (non-replicated Store), treating the
  ledger's own start as the escape tick.

A dropped, unjoined raw declaration vetoes live proof, as does a capability
the ledger cannot track. Both log a warning; neither falls back to the
legacy join. At enrollment the owner writes the opened source's
`dev ino` beside its witness (`.source`). A later dead-owner census must
count exactly that inode.

### 1.3 A dead owner's poisoned witness uses a host source-open census

Any unmediated child launch poisons every armed anchor (`!` nonce, new
ctime): the anchor lock can no longer stand for every holder of the source.
The live owner still settles normally through its explicit join. If the owner
process dies first, its custody has no live owner and no replayable witness,
and its expired row holds the shared storage fence. The first such record was
a cancelled SDR `transcode_prepare` on a learner (custody `4170fe8e…`,
activated 2026-11-09T19:51:33.759Z); its anchor was poisoned 176 ms after
activation, so by a launch at job start. It blocked AMD title measurement
for another title in the same storage domain.

Model `source_open_census_v1` proves closure from the source capability
instead of from the reader's executable history. The
[host helper](../../scripts/source-custody-source-census) runs as root on the
owner host. Twice, at least three seconds apart, it reads the kernel's
`i_readcount`/`i_writecount` for the exact source inode, through `O_PATH`
handles on every superblock that reaches the file (any mount namespace). It
also records locks on the anchor inode, NFS silly-renamed `.nfs*` entries
beside the source, and tasks outside its own ancestry naming the source in
argv. The kernel counts include inherited, mmapped and SCM_RIGHTS in-flight
descriptors. Any nonzero or unstable value refuses. `--publish` writes
`/run/plurx-physical-stop/source-census-v1/<custody>.json`, root-owned and
read-only to the worker.

The daemon (Linux, every scheduler tick) trusts only the counts. It requires:

- the poisoned v2 witness for that custody, from this kernel instance and node;
- the exact job owner token, with a lease that ended before the first pass
  **and before this daemon process started**. A token whose claim predates
  the process was issued to an earlier process, and the data-directory
  daemon lock this process holds excludes that process still running;
- the witness's anchor inode, with the ctime the census saw, unlocked by any
  other open file description (the daemon takes its own non-blocking lock and
  holds it through settlement);
- the job payload's exact source: Store file row, payload size/mtime, and the
  daemon's own `stat` of that path matching inode, size and mtime on a
  superblock the census counted;
- a census at most fifteen minutes old.

When the owner recorded its opened source (`.source`), the census and the
daemon both require that exact inode. A path re-copied with identical size and
mtime therefore refuses. Superblocks are found by the file's NFS handle
(`open_by_handle_at`), not by mount-source strings. Records without a
`.source` (the 2026-11-09 one predates it) fall back to the Store's size/mtime
plus the daemon's own stat.

It then uses the ordinary typed settlement and clears only its own witness.

**When the owner's job row is gone.** Retention can delete a settled job while
its custody is still charged. That row is the custody's only exact source
binding, so the census alone cannot release it. The helper, running in the
host's PID namespace, records an `owner_teardown` view in each pass:

- the PID namespace of the `plurxd run` that is PID 1 of its namespace;
- processes in that namespace that started at or before the owner's lease
  expiry;
- any other `plurxd run` on the host;
- the following, counted only for processes outside that namespace:
  - anything held (a descriptor, cwd, root, executable or mapping) on media
    storage. Media storage means every network or FUSE superblock in any mount
    namespace on the host, the census's superblock, and any `--library-root`
    on local disk.
  - FFmpeg tools or plurx helpers. An idle survivor between opens holds
    nothing, so it is caught here instead.
  - processes that cannot be read. These are never skipped.

The helper reads zombie leaders through their live tasks. It names every
holder on stderr so a refusal can be acted on.

The daemon accepts the view only when all of these hold:

- it is PID 1 of that namespace;
- both passes were taken after this daemon started;
- every count is zero in both passes;
- the scanned set covers every network or FUSE superblock in its own mount
  table and the superblock of every library root it serves;
- the owner's recorded source inode, if any, is the census inode.

The owner died before this daemon started, so its survivors are outside this
namespace. None of them is reading any media file, whichever file the owner
read. A hung network mount can stall the helper's scan; interrupting it
publishes nothing.
The operator removes the proof file after release. The model never repairs a
nonce, rewrites a witness or treats lease expiry as stop evidence. A future
unmediated launch still poisons live witnesses; reducing those launches
(brokered producer handoff, declared raw retirement) is the root-cause work.

```bash
# On the owner host, as root. Check first; publishes nothing.
sudo scripts/source-custody-source-census --custody <uuid> --source <host path>
sudo scripts/source-custody-source-census --custody <uuid> --source <host path> --publish
# After the daemon logs the release:
sudo rm /run/plurx-physical-stop/source-census-v1/<uuid>.json
```

The container needs the existing read-only bind of `/run/plurx-physical-stop`.

A witness from an earlier boot of the same machine is censused too. Legacy
reboot recovery needs the host identity binds, and a host without them writes
its v1 record with no identity, which a reboot can never verify. The helper
then reads the machine's own journal (`journalctl --list-boots`). It requires
the witness's boot to be listed there and to have ended before the current
boot began, so no process of that kernel survives. It records that boot as
`prior_boot` in the proof. The daemon accepts a witness from another kernel
only when the proof names exactly that boot. The census itself must still have
run on the daemon's current kernel, and every other check is unchanged. A
cloned data directory on another machine cannot claim boots from this
machine's journal. The daemon cannot read the host journal: it trusts the root
helper's `prior_boot`, as it already trusts the helper's open counts. The
helper needs persistent journald (`/var/log/journal`); without it, earlier
boots are not listed and it refuses. A later boot can mount the same export
under another device number, so the owner's source binds by the file
server's inode on the superblock the daemon sees now. With the owner's job row
gone the daemon has no path to check that inode against, and refuses.

### 1.4 A daemon restart proves its old namespace's readers gone

A plurxd restart during any read left that owner's witness poisoned by some
earlier unmediated launch. Its expired custody then held the shared storage
fence, and every source-reading job in the cluster, live work included, was
refused until the next host reboot. On 2026-11-12 one daemon restart fenced
the cluster this way within a minute.

Each witness now records its daemon's PID namespace and whether the daemon was
that namespace's init (PID 1). Replay releases a poisoned witness only when
all of these hold:

- the witness's daemon was the init of its PID namespace;
- the replaying daemon is the init of a different PID namespace;
- the anchor lock is free.

When a namespace's init exits, the kernel kills every process left in the
namespace, and the init's exit completes only once they have all been reaped.
A process cannot leave its PID namespace, so every child the old daemon ever
launched, mediated or not, died with it. Only the old daemon held the anchor
lock, and close-on-exec keeps that descriptor from its children. The container
runtime starts the replacement daemon in a new namespace only after the old
init has exited.

The proof depends on that runtime ordering. The anchor lock is released when
the old init's descriptors close, slightly before the kernel finishes killing
the rest of its namespace, and a reader blocked on a hung hard NFS mount can
take minutes to die. `docker restart` and Compose recreation report the old
container stopped only after its whole namespace is reaped, so the new daemon
cannot exist sooner. Never run two daemons on one data directory at once (a
second container, or a rolling update sharing the volume): that breaks the
ordering the proof relies on.

A same-namespace replay, a daemon that is not an init, and evidence written
before this field existed prove nothing and keep their reboot or census path.
An older daemon refuses the new evidence field and so skips the record, the
safe direction during a rolling upgrade. Each release logs `source custody
recovered after its daemon's PID namespace was torn down`.

## 2. The journal never impersonates an activation or completed join

[Recovery replay](../../crates/plurxd/src/source_custody/recovery.rs) uses
`data_dir/source-custody-recovery/v1/`, separate from the old activation/join
receipt directory. Version 1 is a strict JSON object:

```json
{
  "version": 1,
  "machine_id": "11111111111111111111111111111111",
  "boot_id": "11111111-1111-4111-8111-111111111111",
  "node_id": "owner-node",
  "records": [
    {
      "kind": "background",
      "token": {
        "custody_id": "22222222-2222-4222-8222-222222222222",
        "owner": {
          "kind": "analysis",
          "request_id": "33333333-3333-4333-8333-333333333333",
          "node_id": "owner-node",
          "fence": 1
        }
      }
    }
  ]
}
```

The sample identities are fabricated. Foreground records carry the existing
`ForegroundSourceToken`; background records carry
`BackgroundSourceCustodyToken`. Both are defined in
[the Store resource contract](../../crates/plurx-core/src/store/background_jobs_resources.rs).
The immutable owner and custody identity select the existing exact settlement
operations; no old demand lease is renewed. `Companion` retains its foreground
owner and does not gain a second physical receipt.

**Start barrier:** active evidence is written and fsynced before activation
can authorize input. Successful activation clears only the old pre-read
intent, retaining the new evidence. Normal physical join still follows the
existing exact-owner Store path. A duplicate activation cannot rewrite the
earlier recorded boot identity. Missing platform identity is represented by
empty fields and cannot authorize replay; it does not disable normal reads.

Each recovery context belongs to the exact Store allocation, with its own
directory and authoritative-backend flag. Fallback SQLite and unrelated Stores
cannot inherit the replicated Store's journal or consume its evidence. A
confirmed activation abort removes modern evidence only after an authoritative
exact-owner absence lookup; clearing a successful activation's old intent
retains modern evidence. Creating each journal directory syncs the child and
its parent before progressing; failure prevents activation from exposing input.

**Replay conditions:** version 1 · valid machine/boot identities · same actual
machine · different kernel boot UUID · runtime node matches every record ·
bounded regular private evidence. Unknown, corrupt, foreign, same-boot, or
unsupported evidence remains held. There are at most 128 owners per envelope,
1 MiB per file, and 4,096 journal entries; replay rotates 128-file batches so
an unchanged-boot record cannot hide later recoverable evidence.

**Authority:** startup configures replay only after selecting the replicated
Store, before source producers start. An unreplicated activation-recovery
SQLite backend cannot consume these records. Repeated Store errors retain
evidence for retry. Every exact owner must acknowledge settlement and release
before the file is removed and its directory fsynced. A crash after partial
success is safe to replay; an absent old row does not release a replacement.

The protocol relies on the same trusted private data-directory boundary as
the existing joined receipts. It has no public proof-upload or force-unlock
endpoint. It does not accept an operator-entered “reader stopped” boolean.

## 3. Legacy v1 proof verifies an already completed host reboot

This compatibility helper is not the reboot-free recovery procedure. Do not
reboot a shared server to clear Cinema custody. The following describes the
legacy proof format for a host reboot that occurred independently.

The [helper](../../scripts/source-custody-recovery) runs on the Linux owner
host, not in an arbitrary container or a different machine holding a copy of
the database. It never writes SQLite, stops services, or reboots anything.
Use the active database and node identity from the installed server; the
standalone historical database is not the replicated state machine.

For a container, first make the real Linux host's machine and boot identities
readable to its service UID through these read-only binds in the local Compose override:

```yaml
services:
  plurxd:
    volumes:
      - /etc/machine-id:/run/plurx-host-machine-id:ro
      - /proc/sys/kernel/random/boot_id:/run/plurx-host-boot-id:ro
```

The [override example](../../deploy/docker-compose.override.example.yml)
shows their placement. Both files must exist on the actual host and be readable.
Every Linux daemon requires both explicitly provisioned host identities;
absence of container markers never permits an image-local machine identity or
namespace-local boot identity fallback. Some runtimes replace the namespace's
`/proc` boot UUID; [systemd-nspawn](https://github.com/systemd/systemd/blob/main/src/nspawn/nspawn.c)
is one example. A container restart must never impersonate a host reboot.
The [native systemd unit](../../deploy/plurxd.service) supplies the same
read-only binds from `/etc/machine-id` and `/proc/sys/kernel/random/boot_id`
to the two `/run/plurx-host-*` paths.
Its optional source prefix preserves ordinary startup if identity is missing;
automatic recovery remains unavailable. Systemd's bind is private to the
service, as specified in [systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html#BindPaths=).
Custom service managers must explicitly provide both equivalent actual-host
read-only files. The daemon never uses its namespace's `/etc` or `/proc` as a
fallback. There is no CLI identity override.

The helper reads `/etc/machine-id` and `/proc/sys/kernel/random/boot_id` in the actual owning host's context. Its
challenge does not provision daemon identity: the daemon independently checks
it against the explicit host bind. Running the helper in an arbitrary image or
namespace is outside this operating contract and cannot supply recovery authority.

```bash
# On the actual owning Linux host; substitute its node ID and active paths.
scripts/source-custody-recovery prepare \
  --data-dir /srv/plurx \
  --database /srv/plurx/hiqlite/state_machine/db/plurx.db \
  --node-id owner-node
```

**How to read it:** the helper returns a challenge UUID and owner count without
printing tokens. It snapshots only unresolved exact custody for that node,
including rows whose jobs no longer exist, from a bounded read-only WAL-aware
transaction. No owners, excessive owners, invalid identity, unsafe paths, or
bad ownership/modes refuse preparation. Challenges live under
`source-custody-recovery/challenges/`, outside caches, and remain auditable.
Private directories/files inherit the data-directory UID/GID when root
creates them, so the container's service UID can read the published proof.

**Compatibility boundary:** the helper can validate a changed host boot only
when the original challenge was captured before that independently occurring
reboot. Preparing a challenge now cannot prove an old unrecorded process exit.
Use the v2 witness for newly activated mediated readers; preserve unresolved
legacy custody until its separate Cinema-only physical proof is available.

After the same host has actually rebooted, use the returned challenge UUID:

```bash
# Verify proof without changing any files or the database.
scripts/source-custody-recovery validate \
  --data-dir /srv/plurx --node-id owner-node --challenge CHALLENGE_UUID

# Publish the exact verified challenge; the daemon independently verifies it.
scripts/source-custody-recovery apply \
  --data-dir /srv/plurx --node-id owner-node --challenge CHALLENGE_UUID
```

`apply` is idempotent for the same proof and refuses conflicting content. It
publishes into the separate v1 namespace. The daemon then marks and releases
the exact custody through Store; it never edits a replica behind Raft. A
wrong node/machine, unchanged boot, unknown version, symlink/hardlink, or
nonprivate file refuses release and keeps the original challenge.

Confirm actual library requests acquire execution fences and publish new
media. An empty blocker list, smaller queue, or lower cancellation count alone
does not prove successful scans. Preserve the proof and observations. If the
daemon still lacks the host identity bind, retain evidence and correct that
prerequisite; do not weaken the check to make the screen green.

## 4. Upgrade and rollback preserve evidence

Upgrade the replay-capable daemon before applying v1 legacy challenges. Older
binaries ignore the separate namespace and cannot confuse its active records
with old activation-abort or joined receipts. Drain/join/release active owners
before downgrade. If that is impossible, preserve the recovery-capable binary
and all journals; reverting binaries alone does not recover abandoned work.

A daemon older than §1.4 cannot parse a witness that records its namespace:
it logs `invalid reader evidence retained` and skips that record, which then
needs a host reboot or the §1.3 census. Downgrade only after those owners have
settled.

Do not delete an unresolved record to downgrade. A missing identity source is
a recovery limitation, not permission to manufacture proof. New same-kernel v2 witnesses cover native macOS/Linux and containers. Legacy
reader migration and unaudited raw-descriptor containment remain M1 work;
neither receives an invented successful join or retrospective witness.

## 5. Validation and review ledger

| Date | Evidence | Scope / remaining work |
|---|---|---|
| 2026-11-04 | Rust 1.97.1 baseline and initial modified `cargo check --locked -p plurxd --all-targets` passed locally. | Compiler loop established before Rust edits; final tests and exact-candidate checks still required. |
| 2026-11-04 | Helper: 20 Python cases, 19 passed and one root-only ownership case skipped on the non-root Mac host. Activity shipped-painter regressions passed. | Root Linux execution remains required for the actual ownership case; runtime recovery remains unqualified. |
| 2026-11-04 | Focused Rust source-custody tests: 11 passed locally on Rust 1.97.1, including the accepted manual scan claim after exact recovery. | Linux same-boot survivor test and real-root helper case are required in the full CI run; a real host reboot remains separate fleet acceptance. |
| 2026-11-04 | Astra reviewed PR #1378, corrections and final fixture ownership; no remaining actionable findings. | Two production P2 findings corrected: abort cleanup after exact authoritative absence, and durable directory creation. A follow-up fixture P2 was corrected with immediate child cleanup ownership and bounded process-group supervision. Exact-Store scoping and the ownership census were reviewed. |
| 2026-11-04 | Final local recovery cases: 17 passed on Rust 1.97.1. Operations: 1,623 passed (three environment skips). Validation: 659 passed (one environment skip). Activity regressions passed. | Linux-only process cases, root helper ownership and full unit CI remain required on the final PR revision. No live recovery or reboot qualification is claimed. |
| 2026-11-04 | Final Rust 1.97.1 workspace/all-target Clippy passed with `-D warnings`; normal commit hooks passed. | PR #1378 owns the immutable CI and landing receipts. The full unit run and main gate must be green before landing; merge alone is not fleet recovery acceptance. |
| 2026-11-04 | CI preflight caught formal correspondence drift before units started. The audited source-pin refresh passed Astra's independent review, all three mapping checks, 35 formal Python tests and the three pinned TLC smoke runs locally. | Models, properties, bounds, fairness and toolchain are unchanged. Physical reboot proof and filesystem/Store recovery waits remain explicit runtime obligations outside modeled guarantees. Full CI must rerun on the updated revision. |
| 2026-11-04 | Astra closed both identity P1s after reviewing the correction and refreshed pins. All 20 local recovery tests, 100 operations contracts, three mapping checks, 35 formal Python tests, three pinned TLC smoke runs and workspace/all-target Clippy passed. | Removed image-local machine and namespace-local boot identity fallbacks universally; native service provisions both explicit host binds. Exact-candidate full CI and Linux-only cases remain required before merge. |
| 2026-11-04 | Full CI found baseline Apple/Android `media_waiting` table drift from the shared fixture. The two native source rows now include intent settlement and supersession; focused iOS parity/contract replay and ten Android surface tests passed locally. | [Native correction and release counters](../apple-builds/1378-native-media-wait-retirement.md) preserve fixture expectations. Fresh full CI, UI seed-startup investigation and runner termination checks remain; no red gate is waived. |
| 2026-11-04 | Native correction cleared Astra review; complete local iOS suite passed 1,264 tests, Android JVM suite passed 1,251 tests plus lint. | Apple build 289 and Android versionCode 189; full exact-candidate platform CI remains required. |
| 2026-11-04 | UI seeding reproduced a fatal stack overflow on an ordinary ~2 MiB daemon worker. Boxing the producer before task-local/admission scopes reduced carried state from 16,104 to 64 bytes. The budget regression failed before and passed after; all 13 worker lifecycle tests passed, and an isolated normal-stack daemon completed an empty-library scan with zero errors. | Native crash/prolog receipts are retained outside compiler caches. This does not identify the original fleet crash or qualify real-media/full UI capture; those remain separate evidence. Cancellation, source scope and join/drop order are unchanged. |
| 2026-11-04 | A real one-voter repeat-bootstrap regression failed with malformed JSON before accepting canonical v4 in both managed-schema branches, then passed repeated bootstrap, wrong-identity refusal and reopen. All ten managed-schema unit tests passed with `hiqlite-store`. | Exact DDL/marker classification, legacy guards, identity checks and schema SQL remain unchanged. Astra cleared the runtime and source correspondence corrections; final Clippy and three TLC smoke runs passed. |
| 2026-11-04 | Debian snapshot download failures blocked the full unit job before its tests and both package jobs. The exact pinned package subsequently returned HTTP 200 over both transports. | No snapshot date, package version or integrity check was changed. Retry the required CI; the earlier failures do not supply qualification credit. |

| 2026-11-08 | Five new real-process native macOS witness regressions passed once; affected all-target Clippy passed. | New v2 daemon-mediated recovery only. Linux execution, raw-reader migration and production acceptance remain open. |

| 2026-11-08 | New in-place anchor restore regression passed on native macOS; the original five passes were retained. | Kernel change-time correction is independently exercised. Bounded inventory, FEL transport and finite legacy migration await Linux execution. |

| 2026-11-08 | Seven macOS reader cases, eleven composed Linux recovery/FEL cases, fifteen helper controls and the Linux executable-handle case passed. Both native workspace Clippy checks pass. | The first Linux batch retained ten passes; only its failed fixture was corrected and retried. Production acceptance remains open. The gate's upstream formal JAR timeout is addressed with the existing private cache and unchanged toolchain pins. |

| 2026-11-11 | `source_open_census_v1` consumer and helper. Live check on the learner against custody `4170fe8e…`: two passes, readers/writers 0, no anchor lock, no silly-rename or argv reference. Positive control with one read-only descriptor held: readers 1, argv 1, refused. | Rust regression awaits the task CI run. Nothing published or released yet. |
