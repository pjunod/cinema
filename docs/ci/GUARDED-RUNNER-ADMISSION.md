# Guarded runner admission — pin instance identity at the owner

**Status:** open — Astra-reviewed source; promotion, deployment and live qualification pending ·
**Written:** 2026-11-16 · **Owner:** Codex, with Astra review

Companion to the [ci-fleet supervisor plan](http://forge.lan:3000/noirr/ci-fleet):
this repository owns watchdog admission. ci-fleet owns durable requests, host
receipts, live claims, policy and verification. This change grants no automatic
eligibility and changes no labels, registrations, resource limits or placement.

## The owner contract

`plurx-runner-watchdog --describe-owner-contract` prints version 2 without
reading state or contacting Incus. A guarded call requires `--vm`,
`--expected-instance-uuid`, `--expected-instance-generation` and
`--authorization-deadline-monotonic`. Both identities must be canonical,
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
Versioned JSON includes expected and observed resource identity plus outcome.

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

Local focused owner suite: **36 tests pass**, including same-name replacement,
changed or absent generation, running-VM no-daemon-effect, pause during
headroom and persistence, deadline expiry, timeout uncertainty, actual fsynced
attempt state and cooperating mutation exclusion. Ansible has five offline
contract tests and syntax checks for full provisioning, added capacity and
rolling reboot. These are source checks; no playbook or watchdog ran on a host.

Astra closed the pause-during-persistence and two Ansible maintenance findings.
Repository paperwork passes (16/16); catalog and 104 served-script syntax checks
pass. The local commit hook stopped at Cargo, which is absent on this Mac.
No Rust source changed; whole-workspace formatting/Clippy were not run locally
and the repository promotion gate remains required. A source-only commit uses
the hook's documented single-commit bypass, without claiming those checks.
Repository promotion remains open. Install the reviewed owner
and all participating mutation paths, verify the installed Incus identity
fields, then qualify one explicitly authorized non-production VM through the
complete ci-fleet claim/receipt path. Do not infer qualification from a
capability string or a passing fixture. The current upstream retired the
engineering-knowledge pilot; the older primary-tree query returned no reviewed
coverage for this file, and no retired tooling is restored by this change.
