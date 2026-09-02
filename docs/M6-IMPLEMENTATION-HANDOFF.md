# M6 — prepared recipe handoff, with the numbers it was waiting for

**Status:** ready to build · **Executes:** §3 of
[REMAINING-ROADMAP-HANDOFF.md](REMAINING-ROADMAP-HANDOFF.md), which is §5.1–5.4
of [PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md) ·
**Written:** 2026-09-02 · **Baseline:** `main` at `5a0ab78`

Companion to [REMAINING-ROADMAP-HANDOFF.md](REMAINING-ROADMAP-HANDOFF.md)
(what the milestone is) — this is *what M5.5 measured and what M6 must do
about it*, written now because the results are fresh and three of the
roadmap's open questions are answerable from them.

Section references: *plan §n* is the protocol plan, *roadmap §n* is the
remaining-roadmap handoff, a bare §n is this document.

**Standing instruction.** §2 records answers that were measured rather than
reasoned. If a step seems to require re-deriving one of them, stop and say
which — the whole reason M5.5 ran is that three capability literals had been
written by assumption.

---

## 1. Read the capability. Do not re-derive it.

This is the single most important instruction in the document, and it is the
one an implementation is most likely to get wrong by being sensible.

`dual_player_preparation` is a frozen field in the v1 capability set
(`playback_control.rs`, `DynamicCapabilities`). M5.5 measured it on hardware:

| platform | measured | evidence |
|---|---|---|
| Apple | **`true`** | iPhone 17 Pro Max and Apple TV 4K, 20/20 both cases, zero stalls, plus a corrective instrument pass |
| web | `false` | Safari codec/HDR 13/20 |
| Android | `false` platform-wide | phones pass both; the tunneled Google TV fails same-codec 0/3 |

**The three client literals still say `false`.** Correcting Apple's is a
coordinated client release and it is M6's to make — but until that ships, a
server that re-derives the answer from the wire sees three `false`s and
concludes no platform can prepare, regardless of what the hardware said.

So: M6 reads the field. When Apple's literal flips to `true`, M6 prepares on
Apple and does not on the others, with no server change.

## 2. Three of roadmap §8's open questions are now answered

**Q3 — does a retry bound belong on the client at all? Yes, and it is
load-bearing.** Measured on nuc4: `resolve_action` never reads the client's
observation, so a healthy session answers `none` to a client reporting a broken
stream — thirteen exchanges spanning a session kill, every one `none`. And M5b
made `endedTries` the bound on the very `retry_resource` meant to replace it
(`endedTries <= CONTROL_DEFER_LIMIT`), because `retry_resource { after_ms }`
bounds the *rate* of reconnection and nothing in the protocol bounds the
*count*. This is why M5c and M5h were struck. **M6 must not assume the client
budgets are going away.**

**Q4 — is a bounded admission overcommit proven safe on any hardware?
Partly, and the exception is the interesting half.** Two live pipelines
coexisted on both Apple devices and both Android phones, 20/20, zero
predecessor stalls. But:

- **The tunneled Google TV passed codec/HDR 20/20 and failed same-codec 0/3** —
  the *harder* case works and the easier one does not. Two successor-prime
  timeouts and one `ERROR_CODE_AUDIO_TRACK_WRITE_FAILED` read as two
  *identical* tunneled pipelines colliding rather than a capacity ceiling.
  **Same-codec is M6's first axis** (roadmap §3.3), so on that device the
  common case is the failing one.
- **Apple's hardware slot count is unanswered.** AVFoundation exposes no public
  decoder identity, so the measured `2` is a logical `AVPlayer` count. Two
  logical pipelines is not two hardware slots, and plan §5.3's one-slot problem
  is therefore still open on the platform with the most 2160p pressure.

So §5.3's overcommit branch has evidence on phones, a counter-example on a TV,
and no evidence at all on Apple hardware slots. Build the fallback ladder as
though the one-slot path is ordinary, not exceptional.

**Q5 — what interruption bound is acceptable for
`buffered_break_before_make`? Start from 2,246 ms.**

| platform | fallback first-frame interruption |
|---|---|
| web / Safari | 271–2,246 ms (mean 1,121) |
| Android / Google TV | 353–766 ms (mean 471) |

Worst observed is **2,246 ms**, from the platform most likely to set the bound.
Apple's fallback was not measured, because Apple passed dual preparation and
the fallback was not exercised — so if Apple ever needs the break-before-make
path, its bound is unmeasured.

**Q1 and Q2 remain open** — how long a client may wait for an action before
falling back, and whether `terminal` ends playback or offers a *Try again*.
Neither is answerable from hardware; both are product decisions.

## 3. The capability is too coarse to carry the finding

Every platform came back **recipe- and device-dependent**:

- web: same-codec passes, codec/HDR does not;
- Android: both pass on phones, and on the TV the *harder* case passes;
- Apple: both pass on both devices.

A bare boolean cannot say *yes for this recipe on this device*. A correct
`false` on Android throws away two phones that passed both cases and a TV that
passed the harder one.

**This is a protocol decision, not a client one.** The field is frozen in v1,
so a narrower capability — keyed by recipe axis, and possibly by device class —
is a change M6 must make deliberately. Doing nothing is a defensible choice
with a cost: Android never prepares, on any device, despite two thirds of its
fleet being able to.

Whatever M6 decides, decide it explicitly and write the reason down. The
placeholder `false` that started all this was not a decision; it was an absence
of one.

## 4. A correction to roadmap §6's gate

§6 gates every milestone on:

```
plurx_playback_control_vocabulary_total{complete="true",platform="…"}
```

reading non-zero, and records it as zero on 2026-08-31.

**That gate has since been satisfied** — sixteen complete web exchanges on nuc3
on 2026-09-01, and forty-plus more on nuc4 the same night.

**But it can never be a durable gate, and the doc should stop treating it as
one.** These are in-memory counters: every deploy resets them. They read zero
on nuc3 and nuc4 again right now, purely because the fleet redeployed. A gate
whose evidence is destroyed by shipping is a gate that will keep re-blocking
work that is not blocked. Read it as "has this build ever completed an
exchange", captured at the time, not as a live precondition.

## 5. Build order

Roadmap §3.3's axis order stands, and M5.5 sharpens why:
**resolution/bitrate first** — same codec and grade, the only axis plan §5.2
expects to be transparent, and the axis both Apple devices passed 20/20.

Within that, the three phases worth building first are roadmap §3.1's, because
they carry the properties that are testable without hardware:

1. **Commit durability** — one CAS from the expected predecessor to the staged
   incarnation. A lost CAS **aborts the staged generation and never reaps a
   newer player generation**. #726's store half already implements this
   (`commit_media_session_preparation` returns `Ok(None)` when the pointer
   moved); M6 is its first caller.
2. **Abort** — tears down only the successor; the current stream stays
   authoritative and playable.
3. **Disconnect does not imply commit** — after the preparation lease expires
   the actor aborts the successor and keeps a healthy current generation.

`prepare_replacement`, `commit_replacement` and `abort_replacement` are the
three actions carrying an `action_id`, and unlike `hold` / `retry_resource` /
`terminal` they **replay exactly rather than recompute**. `resolve_action`
already ranks a decided action above every advisory one, so the ranking needs
no change — only the decided actions need to start existing.

## 6. Non-goals

- **Do not delete the client recovery budgets.** §2's Q3. M5c and M5h are
  struck; the budgets bound the server's own `retry_resource` as well as the
  no-answer case.
- **Do not call any path with a measured interruption "seamless."** Plan §5.2
  draws this line and M5.5 gave it numbers. A visible break reported as
  seamless is a support cost later: the viewer files a bug against a feature
  whose own copy says it cannot happen.
- **Do not describe a finite retained buffer as a running predecessor**
  (roadmap §3.2).
- **Do not change `dual_player_preparation` server-side alone.** It is a
  coordinated client release gated by
  [M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md](M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md)
  §5.

## 7. Two residuals to close while building

Neither blocks the start; both should be closed before a behaviour change
reaches viewers.

**Nobody has twenty consecutive commits on a realistic runway.** M5.5's 20/20
ran on a short fixture that buffered completely; the corrected instruments ran
on a 132-second fixture for four trials per device. The two halves were
measured under different conditions.

**Apple memory is unanswered at both fixture sizes.** AVFoundation decodes in
`mediaserverd`, and the available tooling exposes no resident memory for
`mediaplaybackd` / `videocodecd`. Memory is the field that would catch
accumulation across twenty trials — so the one measurement that would close the
first residual is the one that cannot currently be taken. Say so in M6's own
acceptance rather than inheriting it silently.

## 8. What M5.5 does not settle, and M6 must not assume

**Behaviour under an active throughput drop.** The whole spike ran on a steady
shaped 80 Mbit/s link with the transition held beyond the run. Dual preparation
doubles network demand, and the case where that hurts is a constrained link —
which is exactly the case not tested. Apple's codec/HDR prime moved
1,592–2,418 Mbit of successor traffic per trial. M6 should treat a prepared
handoff on a contended link as unproven and gate it on the client's own
observed throughput rather than on the capability alone.
