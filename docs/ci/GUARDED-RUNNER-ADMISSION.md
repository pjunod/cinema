# Guarded runner admission — pin instance identity at the owner

**Status:** built and manually qualified on the isolated owner ·
**Written:** 2026-11-16 · **Owner:** Codex, with Astra review

Companion to the [ci-fleet supervisor plan](http://forge.lan:3000/noirr/ci-fleet):
this repository owns watchdog admission. ci-fleet owns durable requests, host
receipts, live claims, policy and verification. This change grants no automatic
eligibility and changes no labels, registrations, resource limits or placement.

## The owner contract

`plurx-runner-watchdog --describe-owner-contract` prints version 2 without
reading state or contacting Incus. A guarded call requires `--vm`,
`--expected-instance-uuid`, `--expected-instance-generation` and
`--authorization-deadline-monotonic`, `--expected-owner-sha256`,
`--guarded-command-id` and `--guarded-command-hash`. Both identities must be canonical,
nonzero UUIDs. Partial identity cannot fall back to a name-only operation.

The watchdog reads the exact instance under the fixed physical-host lock
`/var/lib/plurx-runner-watchdog/admission.lock`, then validates observed
`volatile.uuid`, `volatile.uuid.generation`, name and local member. Changing
`--state` cannot change the admission domain. The existing state lock follows
the admission lock. Allocation, real memory headroom, autostart, manual pause
and restart budget remain owner checks. Pause/maintenance and authorization
are checked again after durable attempt persistence, immediately before start.

A guarded call starts only the stopped VM. If it is already running, the
receipt says `already_running` and performs no daemon repair. Starting a VM
returns `started`; it does not prove service readiness or eligible job pickup.
An external start timeout is `unknown` with the attempt retained. Identity
conflicts are rejected; maintenance, budget and expired authorization defer.
Versioned JSON includes expected and observed resource identity, command ID,
full command hash, exact owner code hash, whether external submission began and
outcome. The host agent opens and hashes the installed script, retains its FD,
and on Linux executes that exact inode via `/proc/self/fd`. Install replaces the
script atomically with a new inode under the fixed admission lock and refuses
any maintenance marker.

Before guarded Incus submission, the watchdog fsyncs an owned maintenance marker
and a submitting record. An unknown/timeout retains both and blocks cooperating
timers/provisioning. A terminal receipt is persisted before its exact owned marker
is released. Restart can clear only a marker whose IDs/hashes match that durable
terminal record; metadata reconciliation never reruns the VM effect.

## Managed mutations share the physical lock

The sibling Ansible runner foundation takes this same lock **on the target
member** for init, CPU/memory configuration and start. Its per-instance
prerequisite applies to both full and added-capacity provisioning. Rolling
maintenance takes the target lock for stop/start and persists a separately
owned `maintenance.json` before stopping guests. The marker survives reboot
and failure; only its owning maintenance run releases it after service checks.
Watchdog recovery remains inhibited while it exists. Ordinary per-VM pause
markers remain independent.

All future managed replace, restore, rename or migration authorities must join
this exclusion contract before a resource is eligible for guarded operation.
Arbitrary concurrent root mutation is outside the guarantee; suspend guarded
eligibility during that work. A controller-local flock does not protect a
remote watchdog. Deployment must establish the actual target-host mutation
paths, lock ownership and supported Incus identity fields before qualification.

A failed maintenance run preserves its marker. Inspect and finish or explicitly
reconcile that owning operation before removing its marker under the admission
lock. Do not clear another operation's maintenance intent to free capacity.

## Evidence and remaining gates

Local focused owner suite: **41 tests pass**, including same-name replacement,
changed or absent generation, running-VM no-daemon-effect, pause during
headroom and persistence, deadline expiry, timeout uncertainty, actual fsynced
attempt state and cooperating mutation exclusion. Ansible has five offline
contract tests and syntax checks for full provisioning, added capacity and
rolling reboot. These Ansible checks remain offline; no playbook was run.
The reviewed watchdog is installed for the explicitly approved isolated qualification.

Astra closed the pause-during-persistence and two Ansible maintenance findings.
Repository paperwork passes (16/16); catalog and 104 served-script syntax checks
pass. The local commit hook stopped at Cargo, which is absent on this Mac.
No Rust source changed; whole-workspace formatting/Clippy were not run locally
and the repository promotion gate remains required. A source-only commit uses
the hook's documented single-commit bypass, without claiming those checks.
Promotion uses this companion's ordinary current-head Main promotion gate. The
installed owner has verified Incus UUID and generation fields on one isolated
6-CPU/24-GiB VM. Three capped prepared-owner prefix trials reached the native
admission guard in 2.502, 1.928 and 2.294 seconds under the unchanged 10% CPU and
100-MiB host cap. The actual guarded manual command returned `started`, was
durably acknowledged, and produced two advancing service observations. After
the collector's job-intake completeness correction, the product verified a new
fixed-workflow pickup on the exact private runner. Astra accepted this manual
qualification on 2026-11-19. The earlier successful workflow that lacked
product pickup credit remains separate historical evidence.

A full 600-second busy shadow and a fresh stopped-resource recommendation were
also accepted. The first automatic command safely deferred before admission:
its caller's 15-second offer left only 1.883 seconds for a live claim. The native
owner recorded no external submission. That separate collector correction
allows a 30-second offer while preserving the native five-second deadline and
all freshness, identity and safety-budget checks. Production registration and
all eight saved files were restored under Observe with no automatic allowlist;
the operator's restorative start is not recovery evidence.

Successful automatic recovery is a separate ci-fleet qualification, documented
in its canonical ledger. It is not an additional prerequisite for promoting
this unchanged, manually qualified native-owner component. Automatic activation
requires a current reviewed resource and live shadow/ready evidence. Successful
automatic qualification then requires an actual supervisor-originated owner
start, service recovery and eligible pickup.
Do not infer qualification from a
capability string or a passing fixture. The current upstream retired the
engineering-knowledge pilot; the older primary-tree query returned no reviewed
coverage for this file, and no retired tooling is restored by this change.
