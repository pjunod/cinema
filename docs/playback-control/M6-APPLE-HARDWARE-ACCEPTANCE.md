# M6 Apple hardware acceptance — the two numbers source cannot supply

**Status:** open · **Owner:** an operator with the devices, or an agent with
physical access · **Written:** 2026-09-08

Everything else in the Apple half of M6 is discharged by
`make apple-test` on a simulator. These two are not, and cannot be: the whole
capability is a hardware claim, and a simulator's decode path is not the
device's.

Hand the section below to whichever agent has the devices. It is written to be
pasted whole — it states what to do, what a pass looks like, and what to write
down, without assuming the reader has read anything else.

---

## §1 — The prompt

> You are running the hardware acceptance for the Apple prepared quality
> handoff (plurx M6). You need an iPhone (or iPad) **and** an Apple TV 4K, both
> running a build of `clients/apple` at or after Apple build 119, pointed at a
> plurx server built from the same commit.
>
> **Read this first, because it decides whether the run means anything.** A
> *prepared handoff* is the server staging a whole second session for a quality
> change and the client building a second, muted `AVPlayer` on it before
> switching. The server today **stages but does not prime**:
> `stage_prepared_successor` is documented "stage only", and a `GET` of the
> staged successor's playlist answers `503 media_owner_transition` until the
> pointer moves. **So on an unmodified server the expected result of §1.1 is the
> fallback, not the handoff** — and the fallback is exactly what §1.2 needs you
> to measure. Do not report the fallback as a failure; report it as the number.
>
> ### 1.1 One directed replacement, on real hardware
>
> 1. Turn on **Settings → Developer → Advertise playback control protocol v1**
>    on the server. Nothing else on that page needs changing; the card below it
>    is advisory and gates nothing.
> 2. Start a film on the device and let it play for at least 30 seconds, so the
>    session has a delivered-rate measurement window behind it. **Leave it
>    playing** — do not pause. A paused viewer's change deliberately takes the
>    in-place path, so a paused run tests nothing.
> 3. Capture the client's control request bodies for the session — a proxy, or
>    the server's own request log. **`observed_download_bps` must be non-null in
>    them.** If it is null, stop: the throughput floor refuses on a missing
>    input and the run proves nothing about the handoff.
> 4. Change the quality (Quality → a different rung).
> 5. Record, from the captured exchanges:
>    - whether an `action` of type `prepare` was ever sent to this client. If
>      not, record the server's `fallback_reason` counter movement instead —
>      `client_cannot_prepare`, `axis_not_proven`, `multiple_axes`,
>      `throughput_unreported`, `throughput_insufficient` — and say which.
>    - every `acknowledgement` the client sent, in order, with its `state`.
>    - whether `committed` was reached, and its `first_frame_unix_ms`.
> 6. Whatever happened, record what the **viewer** saw: did the film keep its
>    position, its pause state, its audio track, its subtitle selection and its
>    grade across the change? That is the exit criterion
>    (`DECODER_SELECTION_AND_RECOVERY_PLAN.md` §12.7) and it must hold on the
>    fallback path too.
> 7. Repeat on the other device class.
>
> ### 1.2 The fallback interruption, measured
>
> This is the number Apple has never had: how long the viewer's picture is
> interrupted when a prepared handoff gives up and the in-place change takes
> over. Web/Safari measured 271–2,246 ms (mean 1,121) and Android/Google TV
> 353–766 ms (mean 471); Apple is blank, because Apple passed dual preparation
> and never exercised the fallback.
>
> Measure it the way those were measured — wall clock from the last frame of
> the predecessor to the first frame of the replacement, from an external
> capture (a 120 fps screen recording of the device output is enough; count
> frames). Do **five** changes per device class and report every value, not a
> mean alone.
>
> The client also records its own view of the same quantity in
> `PlayerController.preparedFallbackInterruptionMs`, which is the wall clock
> from the switch to giving up on the first frame. It is **not** the same
> measurement — it cannot see the reopen that follows — so report both and say
> which is which.
>
> ### 1.3 What to write down
>
> A table per device: device model, OS version, app build, presentation
> (VOD or live), whether a `prepare` arrived, the acknowledgement sequence, the
> commit's `first_frame_unix_ms` if there was one, the five interruption
> measurements, and a yes/no on each of position, pause state, audio track,
> subtitle selection and grade being preserved.
>
> Then say plainly which of these three the run supports:
> **(a)** the handoff works on this hardware; **(b)** the handoff never fires
> because the server does not prime, and the fallback costs *N* ms; **(c)**
> something else, described.
>
> Do not change `Caps.swift`'s `dualPlayerPreparation`. If your measurement
> disagrees with the table in the contract's §C14, that is a finding to file,
> not a literal to edit — it is a frozen v1 field whose value authorises the
> server to prime a second pipeline on a viewer's device, and three literals
> were already written by assumption once.

---

## §2 — Where the results go

`docs/playback-control/PLAYBACK-CONTROL-STATUS.md`, beside the M5.5 numbers, and
the interruption value into the same table that carries web's and Android's. A
result that stays in a chat log is a result nobody can act on six weeks later.

## §3 — What these two do *not* close

- **Twenty consecutive commits on a realistic runway.** M5.5's 20/20 ran on a
  fixture short enough to buffer completely. Nobody has this number and this
  acceptance does not produce it.
- **Apple decoder memory across repeated trials.** AVFoundation decodes in
  `mediaserverd` and the tooling exposes no resident memory for
  `mediaplaybackd` or `videocodecd`, so the one measurement that would catch
  accumulation cannot currently be taken at all.
- **Behaviour under an active throughput drop.** The whole spike ran on a
  steady shaped 80 Mbit/s link.
