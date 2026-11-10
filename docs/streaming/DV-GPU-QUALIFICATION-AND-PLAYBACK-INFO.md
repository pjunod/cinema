# Dolby Vision GPU qualification — reconstructed delivery and skipped reasons

**Status:** implemented locally; Rust regressions, hardware qualification and
deployment pending · **Written:** 2026-11-09 · **Owner:** Codex

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
failed attempt was recorded. A preparation job was marked running at the
observation, and physical foreground admissions deliberately defer background
title measurement. That durable job status alone does not prove which worker
held capacity or explain the complete queue delay.

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

## Validation and delivery

The repair started on main `3b64433bd` and is now ported to `0eb7cf6fc`,
preserving the landed shared-library and media-runtime changes. The Linux compiler
loop uses Rust 1.97.1, at most four CPUs and 8 GiB; compile the large daemon
serially because simultaneous normal/test metadata builds exceeded that cap.

Source review found no actionable defects. The web enhancement suite passes
21 tests. Apple build 295 compiles for iOS/tvOS and its focused presentation
suite passes 15 tests. Android build 194 compiles/assembles, its enhancement
suite passes 10 tests, and lint passes. Documentation/release-counter checks
pass (38 tests); paperwork passes all 16 checks against the current base.
Native receipts and logs are retained outside compiler/cache directories.

The Rust daemon compiles with Clippy warnings denied, and every daemon test
target type-checks. Eleven focused Core enhancement regressions pass with
`hiqlite-store`. Full daemon test codegen exceeded both the 8 GiB voter cap
and a 10 GiB learner cap, including allocator-retention tuning. The capacity
owner independently measured a 15.74 GiB peak and activated an approved
non-voting 24 GiB VM with one shared 20 GiB/four-CPU/no-swap job slot.

The task-only source-archive job queues there, verifies the common physical
20 GiB ancestor, transfers no Git state or credentials to its compiler child,
runs eleven fixed daemon regressions and retains the actual ELF/source hashes.
Its independent heartbeat supervisor passed benign EOF, SIGTERM and lease
expiry cleanup controls. These controls do not establish worker qualification;
the real queue/job and GPU measurements remain pending. The temporary workflow,
helper and catalog entry are removed before final integration.
Exact hardware measurements and the affected main gate remain pending. No
service has been deployed or settings changed by this task.
