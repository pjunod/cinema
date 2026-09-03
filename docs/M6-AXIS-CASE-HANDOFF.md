# M6's axis case — the one measurement the milestone now waits on

**Status:** ready to run · **Executes:** the case named in
[M6-CALLER-HANDOFF.md](M6-CALLER-HANDOFF.md) §3.3.3 ·
**Written:** 2026-09-03 · **Runs on:** physical Apple hardware only

Companion to
[M5.5-PREPARATION-FEASIBILITY-SPIKE.md](M5.5-PREPARATION-FEASIBILITY-SPIKE.md)
(what the original arm measured and why) and
[M5.5-SPIKE-EXECUTION-HANDOFF.md](M5.5-SPIKE-EXECUTION-HANDOFF.md) (how to run
one without producing plausible wrong numbers) — this is *the single case
neither of them ran*, and why the milestone stopped until it does.

**Standing instruction.** §2 is a measurement, not a design exercise. If a step
seems to require changing `PREPARED_AXIS`, or the server, or anything on
`main`, stop: this harness is thrown away afterwards and touches none of them.

---

## 1. Why this case exists

Shadow mode ran on the fleet on 2026-09-03 and measured two deliberate viewer
quality changes on an Apple TV:

| title | change | axis | outcome | counterfactual |
|---|---|---|---|---|
| Avatar (HDR/DV) | 2160 → 1080 | `dynamic_range` | `multiple_axes` | identical |
| Dance Flick (SDR H.264) | 1080 → 720 | `delivery_method` | `multiple_axes` | identical |

**Neither was a resolution change.** The top rung direct-plays and the lower
rungs must transcode, so the delivery method moves with the height every time;
on HDR content the delivered grade moves too. A pure `ResolutionOrBitrate`
transition does not occur on this library at all.

`PREPARED_AXIS` is `ResolutionOrBitrate`. `MultipleAxes` is ranked above the
client capability gate, which is why the counterfactual is identical to the
decision on both rows. **So M6 refuses every transition the fleet produces, and
no client release changes that.**

M5.5 proved each axis separately on Apple. Its §3 ran **same-codec, same
grade** — "a resolution/bitrate change only" — and **a codec or dynamic-range
change**, and Apple passed both 20/20 on both required devices. It never ran
their product, which the `MultipleAxes` comment already said in as many words.

The product is the only transition that happens. That is this case.

## 2. The case

**One recipe pair, twenty consecutive commits, one device class minimum.**

| | predecessor | successor |
|---|---|---|
| source | a file the node direct-plays at its top rung | same file |
| height | 2160 (or the source's top rung) | 1080 |
| delivery | `source` — direct play or remux | `server_selected` — transcode |
| grade | whatever the source carries | whatever the 1080 rung delivers |

The point is that **height and delivery method move together**, because that is
what a viewer's quality change does. If the source also carries HDR or Dolby
Vision the grade moves too, and that is the harder variant — run it second, and
record it separately.

**Required cohort:** the Apple TV 4K (3rd generation), because that is where
the 2160p pressure lives and where plan §5.3's one-slot question remains open.
The iPhone 17 Pro Max is a useful second data point and is not a substitute.

## 3. The delta is two lines, not a rebuild

**The corrective harness already exists and its instruments were accepted.**
The 2026-09-01 corrective pass built and validated every reader this case
needs — the contiguity runway reader, the pixel-buffer PTS assertion against
the commit boundary, the per-trial proxy byte counting under unique URLs, and
the two fields declared unanswered. None of that is rebuilt here.

What changes:

| | corrective pass (2026-09-01) | this case |
|---|---|---|
| recipe pair | same-codec/same-grade, **or** codec-or-grade | a direct-playing top rung → a 1080 transcode, so **both move together** |
| trials | 4 per device (an instrument check, and it says so) | **20 consecutive**, the spike's own proof bar |
| devices | iPhone + Apple TV | Apple TV 4K (3rd gen) required; iPhone optional |
| instruments | built and accepted | **reused unchanged** |

That is the whole ask. If the harness was deleted after the run — spike §3.1
says it is thrown away — then §4 below is the rebuild list. If it still exists,
§4 is a checklist to confirm against, not work.

Say in the report how the harness's switch differs from M6's. A runway measured
against a switch M6 will not use is a number that is wrong in a way nobody can
see later.

## 4. The seven fields — a checklist if the harness survived, a spec if it did not

The first Apple instrument set was rejected and the corrective pass repaired
it. **Those repairs are already built.** This table exists so the run can be
confirmed against them, and so the case is reproducible if the harness was
thrown away as the spike instructs. Four of the seven were not measurements the
first time, which is why each row says how rather than just what.

| field | how, specifically |
|---|---|
| runway at ack | `PlayerController.bufferedRunwaySeconds(playheadSeconds:ranges:joinTolerance:)`, **not** `loadedTimeRanges` max or sum — a successor primed at a boundary routinely holds a leading island from its initial load plus the boundary island, and taking the max over-claims. Apply the successor's own origin offset: player time is not film time. Use a **132-second fixture** so the runway can vary; the original 12,000 ms constant was the successor buffering the whole asset, and AVPlayer's full-buffer cap is 53.889 s. Report `full at ack` alongside, and it must read `false`. |
| first-frame latency | `AVPlayerItemVideoOutput` polled for `hasNewPixelBuffer`, then `copyPixelBuffer` — **and assert the copied buffer's presentation timestamp maps to film time at or beyond the commit boundary.** Report the PTS beside the latency. Without that assertion a pre-commit frame satisfies it, which is how the original returned an identical 1–4 ms for both cases. `timeControlStatus == .playing` is the clock starting, not a frame; on a delivery-method change that gap *is* the interval being measured. |
| network duplication | response-body bytes counted by the shaping proxy, under **unique device/trial/pipeline URLs**, per trial. Not derived from asset size or item access-log bytes — the original was byte-identical on two different devices, which is what gave it away. The proxy's final ledger includes bytes still in flight after the ack; the per-row snapshot is the authoritative prime-window value. |
| old-stream continuity | stalls observed in the **predecessor**. The predecessor must not suffer for the successor; this is the field that decides whether the feature is safe at all. |
| switch position error | film time skipped or repeated at the boundary, in ms. |
| peak memory | **declare unanswered.** AVFoundation decodes in `mediaserverd` and the available tooling exposes no resident memory for `mediaplaybackd` / `videocodecd`; task RSS measures the harness process and repeats the original structural error. The instrument review explicitly allowed this to stand unanswered. |
| decoder instances | **declare unanswered.** AVFoundation exposes no public hardware-decoder identity, so any number here is a logical `AVPlayer` pipeline count. Plan §5.3's one-slot question stays open, and this case does not close it. |

**Link shaping:** a *stable* contended profile passed explicitly to
`--network-profile`. Not the named `8mbps-to-1.5mbps` descent — that is a
bandwidth-cliff recovery rig for a different acceptance.

**No simulator.** A measurement of decoder allocation on a simulator is a
measurement of a Mac.

## 5. Acceptance

The spike's own, unchanged:

- **20 consecutive clean commits** with **zero predecessor stalls** and **zero
  post-commit stalls**;
- runway **varies** across trials and reads `full at ack: false` on every one;
- **every** copied pixel buffer's PTS falls at or beyond its commit boundary;
- wire counts **vary** per trial and per device;
- memory and decoder instances recorded as unanswered, with the reason.

A failed admission is evidence against the capability even when another recipe
passes. Twenty is not negotiable: the original arm's four-trial corrective pass
was an instrument check, and the residual it left — nobody has twenty
consecutive commits on the corrected fixture — is still open.

## 6. What each outcome decides

**If it passes:** `PREPARED_AXIS` widens to admit the measured combination —
resolution together with delivery method — and M6 §3.4 has something to prepare
for. The `MultipleAxes` rule stays for combinations nobody has measured; what
changes is that this specific product is no longer one of them.

**If it fails:** M6's honest scope is the one-player release-and-replace it
already has, §3.4 should not be built, and the milestone's remaining value is
the measurement itself. That is worth knowing before the executor gets a
caller, which is the entire reason shadow mode exists.

**Either way, M8 §10.2 moves with it.** Planned drain is
`PreparationExecutor` with the successor placed on a different node, so it sits
on top of §3.4 — see
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) §"M8 is not
independent of M6".

## 7. Non-goals

- **Do not change `PREPARED_AXIS` before the run.** Widening it on the strength
  of two separate single-axis proofs is exactly the reasoning shadow mode
  replaced.
- **Do not build M6's transaction into the harness.** It measures; it does not
  ship.
- **Do not reuse the `8mbps-to-1.5mbps` profile.** Different acceptance.
- **Do not accept a clock-start timestamp as a first frame.** It flatters the
  delivery-method case, which is the case this run turns on.
- **Do not run twenty trials on the short fixture.** The runway constant it
  produces looks like data.
