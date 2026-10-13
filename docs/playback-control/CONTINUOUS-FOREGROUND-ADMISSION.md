# Continuous startup — foreground playback preempts background indexing

**Status:** implemented; focused regressions and adversarial review passed, physical qualification pending · **Written:** 2026-10-12

Companion to [the continuous-quality build](CONTINUOUS-QUALITY-BUILD.md):
this records the startup failure caused by omitting foreground admission.

## A background reservation outlived two foreground starts

On October 12, lab6 held a background fragment-index reservation from
20:52:18 to 21:02:18 America/New_York. Two Android starts at 20:54:41 and
20:54:44 failed before the player opened a stream. The server returned
`vod_family_capacity`, then mapped that code to an HTTP 500.

Fragment indexing reserves the software budget. Its cancellation watcher
checks the existing foreground waiter counter every 500 ms. Ordinary
encoder starts register that waiter; continuous video/audio group creation
made one `try_admit_family` call and returned immediately. Nothing told the
background owner to yield, regardless of available physical CPU.

## The complete group waits under one foreground claim

Foreground group creation now registers the existing RAII waiter and retries
atomic admission at the established 250 ms cadence for at most five seconds.
The waiter survives between attempts, preventing background reacquisition.
Cancellation drops it. Each attempt releases binding locks before awaiting;
no partial video or audio credit is held while waiting.

Speculative quality renewals remain immediate and do not preempt. An exhausted
foreground wait reaches the client as HTTP 503 `transcode_capacity_pending`
with the existing retry interval. Pool limits and incumbent reservations are
unchanged.

## Verification and remaining acceptance

The focused regressions are
`continuous_family_start_preempts_background_and_cleans_up_cancelled_wait`
and `continuous_family_capacity_is_retryable_after_bounded_admission`.
They cover the waiting signal, background reacquisition, cancelled waits,
reservation cleanup, speculative refusal and the client-visible retry.

Physical Android acceptance remains separate: start a continuous stream while
an index job owns the same node, verify that indexing yields, and verify a
first frame. This document does not claim a production deployment.
