# Enhanced playback preparation — supply, observation and resume repairs

**Status:** open · **Updated:** 2026-11-03 · **Owner:** Codex ·
**Branch:** `codex/m6-hdr10e-preparation`

Companion to [playback](../PLAYBACK.md) (delivery and progress),
[the DV helpers](DV_PROCESSING_TOOLS.md) (window validation), and
[the cache budget](PLAYBACK-CACHE-BUDGET.md) (physical capacity).
This ledger distinguishes three faults reported during one 4K P7 playback
investigation. It does not declare physical acceptance from unit tests.

## Evidence identifies three different failures

The deployed source inspected was `7ea74370b`; the repair starts from
`bd213c819`. DV execution and generation matched the deployed source.
Private incident captures remain outside the repository; recorded summaries
contain numeric timings rather than account or session identifiers.

| Observation | Established conclusion | Limit |
|---|---|---|
| Chrome on lab6: 37 captured two-second media requests; median first byte 1,997.9 ms, median body transfer 57 ms; 27 requests exceeded two seconds. Same browser and processing through media1: 14 requests, medians 1,510.3/46.3 ms; none exceeded two seconds. | The dominant observed wait preceded media delivery; the client repeatedly exhausted its runway. | Different movie windows and cache conditions prevent assigning the entire difference to one stage. |
| Local installed renderer/VA-API encoder diagnostic: two seconds of 4K FEL/RPU video took about 1.2 seconds. The complete daemon window harness retained raw-RPU, encoded/header, plan and receipt checks. Its validation stages took tens of milliseconds. | The renderer-only diagnostic omitted production work. Removing validation is neither justified nor necessary. | Unoptimized test diagnostics and scene variation prevent treating these figures as a sustained production rate. |
| Safari first stopped when its native audio track ran dry; video decoding remained healthy. After reconnect, native logs reported about 24 displayed frames/s and an advancing timebase while the application declared a persistent stall and eventually stopped playback. | Initial audio starvation and subsequent false application recovery are distinct. | The logs do not identify the JavaScript callback, clock or ownership divergence. |
| A partial rewatch retained its saved position and `watched=true`; the UI required `!watched` to resume it. | Historical watched state incorrectly suppressed a valid current position. | No zero-position database overwrite was established. |

The later failed-seek trace captured the outgoing attachment's origin once.
Its zero origin was not evidence that the replacement used zero: the actual
attachment calculation mapped a 4,319.584-second film target to a 4,318.792-second
origin plus 0.792-second element position. No speculative origin patch is made.

## Preparation keeps every publication check

Encoder input discovery, AAC preroll, priming, offsets, filters and duration
remain unchanged. A NUT-input discovery experiment showed less than 1% median
gain in a balanced twelve-run comparison; that change was dropped.

After both children have completed and been reaped, optional cache inventory
runs beside existing verification. It uses the same registered global cache
budget. Busy operations, an unknown ceiling or an occupied inventory slot
cause warming to skip. Optional waiting is bounded to 250 ms; warming never
grants capacity or becomes a DV-validation refusal.

Publication still performs its ordinary reservation. A warmed inventory can
include a temporary probe copy removed before publication. If that sample
would cause capacity or free-space pressure, reservation forces a current
inventory before eviction or refusal, then recomputes device/allocation and
retains dirty-revision reconciliation and the existing total deadline.

The following remain required before publication:

- Held source identity, source clock, raw RPU association and FEL evidence.
- Helper completion, exact encoder success and owned-job retirement.
- Picture count, raster, cadence, sample timing and encoded payload checks.
- The independent held-output header/audio probe.
- Completed-plan validation, processing receipt and source/backend fences.
- Ordinary disk reservation, free-space floor and publication ownership.

Debug timings use one entry identity. `finalize.*` timings are nested within
`finalize_and_backend`; do not sum both. Request/admission and publication
waiting remain separate. Physical controls can select up to 32 explicit
entries; an explicit out-of-range request fails instead of disappearing.
Cache comparisons require complete ABBA blocks of the same entry and report
actual completed warming separately from permission to warm. A read-only
control measured the real Rust inventory at 91–94 ms for 25.84 GB allocated;
this measures inventory, not complete file or HTTP publication.

One same-entry ABBA control retained every check and measured preparation plus
reservation at 1,568/1,494 ms without overlap and 1,475/1,449 ms with overlap
(medians 1,531/1,462 ms). The two warmed scans completed; reservation then took
0 ms instead of 94–96 ms. All samples retained 48 FEL and receipt frames.
Renderer time also varied (1,317–1,386 ms), and expensive test-only luma
diagnostics separated samples. This proves a bounded inventory improvement,
not sustained production throughput, HTTP delivery or smooth playback.
Transferred executables and generated media were automatically removed.

## Native observation remains a root-cause investigation

A proposed seek-completion renewal could discard the only queued paused
landing callback and was removed. A narrower once-per-attachment active renewal
passed focused tests and Astra's code review, but the owner rejected it as a
symptom workaround without an identified incident cause. It is also removed.

No observer-retry or seek-lifecycle change remains in this candidate. The
historical native player was mapped to the visible current element; its
continued display was not a hidden prepared stream. The callback, sampled
JavaScript clock and ownership state at the divergence were not captured.
Native decoding statistics alone do not establish accepted application receipts.

Five isolated Safari cases using generated SDR HEVC/AAC HLS reproduced no
stuck observer through load, reload, seek/pause or ownership rejection. These
are negative evidence only: native registration could be deferred by readiness,
reload totals included predecessor callbacks, and the fixture did not reproduce
the movie's growing Dolby Vision/E-AC-3 stream. The fixture server, test tab and
generated media were removed immediately after the run; compact receipts remain.

A separate source counterexample loses observation when a full replacement
play fails while preserving a started predecessor: the open invalidates its
attachment, an outstanding callback retires, and refusal restores `PLAYER`
without coherent attachment/observer ownership. Ordinary native recovery and
the recorded cold close/reopen and later seek do not take that restoration
path. It remains a separate defect, not an explanation or patch for this stall.

## A rewatch position is independent of watched history

Local and Shared launches use the same resume calculation. A valid position
above three seconds and below 95% of known duration resumes even when watched
history remains true. Completed/unstarted positions retain start-over behavior;
unknown-duration watched records retain conservative behavior. No saved watch
state or watched flag is rewritten by this repair.

## Review and acceptance

Astra independently reviewed causal claims and changes. Its rejected
seek-completion renewal could discard the only queued paused landing callback;
that design was removed. The active-observer workaround was subsequently
rejected by the owner despite passing its narrow code review. Astra also required consistent timing identities,
rejection of invalid explicit physical samples, bounded optional cache work,
pressure reconciliation for removed probe files, and a timeout/cancellation
test for inventory ownership.

Focused resume regressions have passed, including Shared launches (22 tests).
The cache suite passed 27 tests; its read-only physical timing control is
explicitly ignored by ordinary runs. The candidate physical ABBA harness
passed with all validation intact. The rejected observer patch's tests do not
establish incident closure. No candidate deployment or sustained smooth-playback
acceptance is recorded yet; further production timing and a matching Safari
JavaScript capture remain necessary.
