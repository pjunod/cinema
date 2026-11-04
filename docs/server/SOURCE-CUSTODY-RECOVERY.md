# Source custody recovery — settle abandoned readers with physical proof

**Status:** open · initial recovery implemented; fleet acceptance pending · **Written:** 2026-11-04

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

**Implemented scope:** foreground and independent background readers retain
versioned exact-owner evidence before activation. Normal settlement removes it
after the authoritative acknowledgement. Startup and recurring upkeep can
settle those exact owners after a verified reboot of the same Linux host.
A host-side helper captures legacy orphan identities before a controlled
reboot and publishes evidence afterwards. Activity names expired reader
reservations even when their executable job has been pruned.

**Limits:** an ordinary daemon/container restart, expired lease, new PID,
acquired data lock, changed namespace, or unavailable peer does not authorize
release. Same-boot automatic recovery is deliberately absent. Linux host
identity must be available; native macOS/Windows retain the journal but do not
automatically infer reboot proof. No production recovery or reboot has run as
part of implementation. The complete M1 physical-containment acceptance is
still open, rather than being silently claimed from this narrower fix.

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

## 3. Prepare legacy evidence before the owner host reboots

The [helper](../../scripts/source-custody-recovery) runs on the Linux owner
host, not in an arbitrary container or a different machine holding a copy of
the database. It never writes SQLite, stops services, or reboots anything.
Use the active database and node identity from the installed server; the
standalone historical database is not the replicated state machine.

For a container, first make the real Linux host's machine identity readable
to its service UID through this read-only bind in the local Compose override:

```yaml
services:
  plurxd:
    volumes:
      - /etc/machine-id:/run/plurx-host-machine-id:ro
```

The [override example](../../deploy/docker-compose.override.example.yml)
shows its placement. The file must exist on the host and be readable. A known
container without that bind does not substitute an image-specific identity.
Native Linux uses `/etc/machine-id`; Linux kernel boot identity comes from
`/proc/sys/kernel/random/boot_id`. There is no CLI identity override.

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

**The intervening reboot is an operational action:** coordinate quorum,
viewers, recordings, and other workloads before rebooting the owning host.
This document and helper do not authorize an unrelated host reboot. Do not
replace that evidence step with `docker restart` or a delay.

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

Do not delete an unresolved record to downgrade. A missing identity source is
a recovery limitation, not permission to manufacture proof. Same-boot
container/process containment and non-Linux reboot verifiers remain later
M1 work, with real descendant-stop evidence required before enabling them.

## 5. Validation and review ledger

| Date | Evidence | Scope / remaining work |
|---|---|---|
| 2026-11-04 | Rust 1.97.1 baseline and initial modified `cargo check --locked -p plurxd --all-targets` passed locally. | Compiler loop established before Rust edits; final tests and exact-candidate checks still required. |
| 2026-11-04 | Helper: 20 Python cases, 19 passed and one root-only ownership case skipped on the non-root Mac host. Activity shipped-painter regressions passed. | Root Linux execution remains required for the actual ownership case; runtime recovery remains unqualified. |
| 2026-11-04 | Focused Rust source-custody tests: 11 passed locally on Rust 1.97.1, including the accepted manual scan claim after exact recovery. | Linux same-boot survivor test and real-root helper case are required in the full CI run; a real host reboot remains separate fleet acceptance. |
| 2026-11-04 | Astra reviewed PR #1378, corrections and final fixture ownership; no remaining actionable findings. | Two production P2 findings corrected: abort cleanup after exact authoritative absence, and durable directory creation. A follow-up fixture P2 was corrected with immediate child cleanup ownership and bounded process-group supervision. Exact-Store scoping and the ownership census were reviewed. |
| 2026-11-04 | Final local recovery cases: 17 passed on Rust 1.97.1. Operations: 1,623 passed (three environment skips). Validation: 659 passed (one environment skip). Activity regressions passed. | Linux-only process cases, root helper ownership and full unit CI remain required on the final PR revision. No live recovery or reboot qualification is claimed. |
| 2026-11-04 | Final Rust 1.97.1 workspace/all-target Clippy passed with `-D warnings`; normal commit hooks passed. | PR #1378 owns the immutable CI and landing receipts. The full unit run and main gate must be green before landing; merge alone is not fleet recovery acceptance. |
