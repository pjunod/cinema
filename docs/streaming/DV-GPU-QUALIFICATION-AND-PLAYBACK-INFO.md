# Dolby Vision GPU qualification — reconstructed delivery and skipped reasons

**Status:** draft PR #1491; named regressions executed, hardware qualification
and deployment pending · **Written:** 2026-11-09 · **Updated:** 2026-11-11 ·
**Owner:** Claude (from Codex)

Companion to [reconstructed Profile 8.1](FEL-P81-RECONSTRUCTION.md)
(the picture and quality decision), [source budgets](HDR-ENHANCEMENT-SOURCE-BUDGET.md)
(the exact output envelope), and [selection paths](DV-SELECTION-PATHS.md)
(the delivery route). This ledger tracks Intel/NVIDIA measurement support,
the AMD pending-measurement diagnosis, and the skipped-upgrade explanation.

## The incident has two distinct causes

A Safari Profile 7 FEL play on an Intel worker began ordinary Profile 8.1
conversion and requested its processed upgrade after first presentation.
The upgrade declined because the implementation restricted qualification to
radeonsi VA-API. Conversion discarded the full enhancement layer and retained
dynamic metadata, so DYN alone accurately described the delivered picture.

The same title on an AMD worker passed the hardware gate but had no current
verdict for its exact renderer, encoder, source budget, raster and cadence.
Its request was retained and its title job was queued. A legacy measurement
had rejected an older recipe; that result cannot admit or reject a different
current recipe. A pending request is not evidence that a measurement is
running or that the title will pass. Read-only checks confirmed that the queued
job matches its demand/scanner, the source is readable and current, and no
failed attempt was recorded. Follow-up identified the actual source-domain
blocker: a cancelled SDR preparation job for another file on a learner
retained expired active reader custody in the shared storage domain. The
fence therefore blocked Avatar background work without being Avatar
reader ownership. Its journal records an unmediated handoff, invalidating the
normal replay witness. An absent current reader lock is insufficient proof
that every recipient stopped. Neither journal version records an original
container or PID namespace, so a later container restart cannot retroactively
prove that lifetime closed. The unrelated durable running title job was not
the cause. Retain this fence until a validated physical-stop proof for that
exact owner is established; never release it by expiry. This repair must
recover in software without rebooting the host or restarting its services.
Retrospective settlement remains open until that proof is implemented and
verified.

The normal preparation path already brokers source input and scratch output
through HTTP, but unnecessarily passes raw descriptors as well. Removing
those descriptors and mediating only the fully brokered launch prevents that
specific invalidation. Raw descriptor, all subtitle-burn and optional content-analysis
fallbacks retain their conservative treatment. The witness does not record a
callsite, so this source-proved path cannot be claimed as the first handoff
that invalidated the historical record. This change cannot repair that record
or establish complete mediated-reader coverage by itself.

The November 8 deferred-upgrade path logged its later refusal and retained
the ordinary session, but did not carry that refusal into Playback Info.
The initial generic explanation therefore hid both causes.

## Each actual encoder must measure its own output

| Worker | Qualification path | Evidence still required |
|---|---|---|
| AMD | VA-API with the route's radeonsi policy | Current exact recipe, source and output verdict |
| Intel | Proved VA-API completed-picture encode; QSV preference maps to that existing reconstructed path | Actual Intel window measurement and serving checks |
| NVIDIA | NVENC completed-picture encode; existing renderer decode and resource accounting | Actual NVIDIA window measurement and serving checks |

Qualification and delivery must share the encode arguments, rate envelope,
output geometry and clock. Encoder/driver/device identity prevents a result
from one hardware family authorizing another. NVIDIA's default-device route
must reject ambiguous multi-GPU identity rather than guess which GPU encoded
the sample. This change does not claim NVDEC implementation.

Keep the existing quality rule: served mean error must improve by at least
10%, and served p99 error must not exceed the EL-dropped reference's p99.
A supported GPU permits measurement; it does not prove a better picture.
FEL qualification follows its own saved re-encode preference. Turning off the
independent HDR10-processing preference must not strand a DV display's request.
No stored admitted verdict may be manufactured for hardware qualification.
The separate color reproduction effort owns renderer arithmetic and independent
reference comparisons. Its unqualified equation candidate is excluded here;
changes to its renderer/header invalidate bound receipts naturally. Source
budget preservation is already included in the implementation base.

## Playback Info must explain the current generation

A refused deferred upgrade leaves active enhancements unchanged and reports
its bounded reason through the owning session's control exchange. A client
accepts that explanation only for its current generation, applies it to the
inactive enhancement rows, and clears it when the generation changes or the
report disappears. The original enhancement object remains available so a
later control response cannot retain stale fallback text.

Cover local and peer-owned playback, web, Apple and Android, stale generation
refusal, replacement/retirement, and preserved active enhancement badges.

## Qualification may finish after playback starts

Already-qualified recipes use the existing upgrade after first presentation.
The one-shot implementation loses its executable intent when qualification is
pending. The follow-up retains a bounded, typed waiter for that exact request
and checks persisted verdict readiness on accepted control exchanges. It does
not repeat measurement. A rejected verdict remains a terminal explanation.

An admitted verdict requires fresh authoritative planning from the accepted
client selection, capabilities and delivered representation. Storing the
verdict changes probe metadata and therefore the planning binding; replaying
the old intent or rewriting its digest would be incorrect. Keep source,
settings, owner/incarnation and predecessor-generation fences, preserve the
selected audio/subtitles and current playhead, and retain the pending
explanation until replacement becomes current. Release measurement resources
before publishing readiness. A process restart discards old executable
authority; this work does not promise durable waiter recovery.

## Validation and delivery

The repair started on main `3b64433bd` and is now ported to `0eb7cf6fc`,
preserving the landed shared-library and media-runtime changes. The Linux compiler
loop uses Rust 1.97.1, at most four CPUs and 8 GiB; compile the large daemon
serially because simultaneous normal/test metadata builds exceeded that cap.

The initial hardware/reporting implementation and the brokered-reader and
late-verdict follow-ups passed independent source review. The brokered-reader
checkpoint passes production Rust check and Clippy; its expanded test metadata
hit the unchanged 8 GiB cap. Fresh late-verdict checks and native execution
remain pending. The unchanged web enhancement suite passes
21 tests. Apple build 295 compiles for iOS/tvOS and its focused presentation
suite passes 15 tests. Android build 194 compiles/assembles, its enhancement
suite passes 10 tests, and lint passes. Documentation/release-counter checks
pass (38 tests); paperwork passes all 16 checks against the current base.
Native receipts and logs are retained outside compiler/cache directories.

Before those follow-ups, the Rust daemon compiled with Clippy warnings
denied, and every daemon test target type-checked. Eleven focused Core enhancement regressions pass with
`hiqlite-store`. Full daemon test codegen exceeded both the 8 GiB voter cap
and a 10 GiB learner cap, including allocator-retention tuning. The capacity
owner independently measured a 15.74 GiB peak and activated an approved
non-voting 24 GiB VM with one shared 20 GiB/four-CPU/no-swap job slot.

The task-only source-archive job queues there, verifies the common physical
20 GiB ancestor, transfers no Git state or credentials to its compiler child,
runs the focused daemon regressions and retains the actual ELF/source hashes.
Its independent heartbeat supervisor passed benign EOF, SIGTERM and lease
expiry cleanup controls. These controls do not establish worker qualification;
the real queue/job and GPU measurements remain pending. The temporary workflow,
helper and catalog entry are removed before final integration.
Exact hardware measurements and the affected main gate remain pending. No
service has been deployed or settings changed by this task.

**2026-11-11.** The nested child-container job never ran: its `/aggregate`
check fails on the heavy runner, and its path filter never fired for the new
branch. It now runs in the fast lane's own capped container on the shipping
image's bookworm base (so the ELF loads there) and is dispatched by hand. Its
first execution passed 25 of 26 named regressions; the failure was a wrong
VA-API `-pix_fmt` expectation (VA-API carries p010 in its upload filter),
corrected in the test.

The lifecycle ledger's blocking finding is fixed. `ActiveBackgroundJob::finish`
now hands enrolled custody to the completion ledger, authorized only after
claim retirement, instead of publishing the joined proof while a blocking read
can still hold its File. The handed-off patch had also never compiled: its
`#[path]` parent resolved the new module beside `background_jobs.rs`.

The stale m6-blocking custody is a dead owner's poisoned witness, not an AMD
refusal. Its anchor was poisoned 176 ms after activation, by a launch at job
start. The daemon container was recreated seven hours later, before any
explicit join. The old container's json-file logs went with it, so why the
cancelled job never joined is not recoverable. A read-only kernel census on
the owner host shows no open file description, lock, silly-rename or argv
reference for its exact source. The new
[source-open census model](../server/SOURCE-CUSTODY-RECOVERY.md#13-a-dead-owners-poisoned-witness-uses-a-host-source-open-census)
releases it once deployed and published; nothing has been released yet.
