# Automatic quality — independent Opus review

**Reviewer:** claude-opus-5-5 (Cowork) · **Written:** 2026-09-30
· **Reviewed:** `DISPLAY-AWARE-AUTO-QUALITY-PLAN.md`, `-REVIEW.md` and
`-OPUS-HANDOFF.md` (untracked in the working tree), checked against source at
`ceb7dd8cc`

**Revision and drift.** `ceb7dd8cc` sits on `codex/cluster-worker-activity`,
9 commits ahead of the last *locally fetched* `origin/main` (`28964229c`). Those
9 commits include #650, "improve admitted throughput", which touches admission.
I did not fetch current Forgejo `main`, so drift against it is unverified. I
read the source but did not compile, test or benchmark anything, and I made no
physical measurements. Line numbers are at `ceb7dd8cc`.

## Verdict: revise first, narrowly

The direction is right. Original-first, fitting the picture rather than the
panel, route-aware candidates and a single owner are the correct shape, and
R1–R5 were real findings with real fixes. But four P1s change the milestone
structure and the wire plan. Fixing them in the document costs an hour.
Finding them in M1 or M4 would cost a rebuild. None of them needs new research,
only a rewrite of §4.1, §7 and §8.2 plus some sharper M0 items.

## P1 findings

### P1-1 · "Roll out server first" does not survive this cluster's strict relays (§4.1, §4.2)

**Source.** Every hop that would carry the new fields rejects unknown fields:

- `ControlRequestV1` and `ControlRelayRequest` (`playback_control.rs` ~157, 2168)
- `QualitySelection` (~474) and core `DesiredQuality` (`desired.rs:44`)
- `MediaIntentEnvelope` (`intent.rs:115`)
- `RemoteStartRequest` (`media_sessions.rs:1181`)
- `ControlResponseV1`, which the *relaying* node re-parses in
  `validated_control_relay_response` (`media_sessions.rs:2895`)
- `RetainedTerminalResponse` (`http/hls/control.rs:127`), whose stored JSON is
  replayed after a rollback

The repository has already paid for this lesson.
`STREAMING-RELIABILITY-HANDOFF.md` §4 step 2 withdrew
`ControlResponseV1.accepted_acknowledgements` for exactly this reason. It
concludes that any negotiation field needs **three releases**: relax
`deny_unknown_fields`, deploy that fleet-wide, then add the field.

**Failing scenario.** The rolling deploy runs `serial: 1`.

1. nuc4 is upgraded and owns the tablet's session. Its start response
   advertises support.
2. The tablet sends `candidate_id` (or `presentation_target` in
   `DynamicCapabilities`).
3. The exchange enters through the not-yet-upgraded nynuc. Its
   `ControlRequestV1` parse refuses the field, and the result is
   `503 control_unavailable`, which the reporter retries.

The same break happens on three other paths:

- A new ingress places a remote start carrying the new `SessionRequest`
  fields on an old worker. `RemoteStartRequest` refuses it.
- A retained terminal response containing a new `EffectiveSelection` field is
  replayed after a rollback.

The plan's matrix of old/new client against old/new server has no row for a
**mixed-version cluster**.

**Smallest correction.** In §4.1:

- State the release sequence as three steps: (a) tolerate the new optional
  fields on every nested type, and ship only that, fleet-wide; (b) have the
  server advertise support; (c) have clients send the new fields.
- Put the advertisement where the relay does not re-parse it. The handoff doc
  names the start response, which the owner mints.
- Make "the whole fleet runs step (a)" a precondition of (b). No single
  node's build should decide it.
- Add M1 fixtures for an old ingress relaying to a new owner, a new ingress
  relaying to an old owner, a new-to-old remote start, and replay of a
  retained terminal response after rollback.

Carrying `presentation_target` on `DisplayCaps` at create time is additive
today, because `caps.rs` has no `deny_unknown_fields`. So the initial-selection
half can ship one release earlier than the runtime half. Say so explicitly.

### P1-2 · One effort branch couples a week of value to a month-long blocked dependency (§2.1, §7)

**Source.**

- On the work board, A-05 is **unclaimed** and gated on A-04's D3 matrix.
  That board row and the GPT build status (line 342) say D3 is still open for
  HDR and every native platform.
- F-1's own card says: "No native client reads or shows `playback_auto_abr`."
- AGENTS.md "Large efforts": nothing reaches `main` until the effort is
  complete, frozen and promoted.

**Failing scenario.** The plan runs M0 to M6 in order on
`effort/display-aware-auto-quality`. Consider what M1–M3 alone would deliver:
the presentation target, the 1440 candidate, a player menu that shows 1440,
and an initial Auto start at 1440 on the tablet. That is exactly Paul's ask,
and it involves no runtime controller. Under the plan it cannot merge until:

- M4 (the Android controller, which needs A-05 M0–M3 plus a shaped trace)
- M5 (Apple, which is blocked on a fresh per-transfer sample that may not
  exist)
- M6 (the physical matrix)

Meanwhile `main` moves several times a day, as the board notes show, and the
effort accumulates integration debt on the busiest files in the repo:
`playback_control.rs`, `http/hls/*` and `transcode/manager/*`.

**Smallest correction.** Split the plan into two deliverables.

- **Effort A: display-aware start and honest 1440.** M0, M1 (create-time
  target plus candidate builder; no control extension yet), M2, and the
  initial-selection and menu parts of M3. Web and Apple get only the
  create-time target and a menu refresh. Its physical acceptance is the
  tablet's initial-selection rows plus prepared *manual* switches.
- **Effort B: runtime display-aware Auto.** The control extension,
  `candidate_id`, copy↔encode Auto, upgrade/return-to-original, and the
  reducers. Deliver it as additional milestones *inside A-05*, claimed on the
  A-05 board row, rather than as a parallel effort that "coordinates" with it.

This is also what §2.1's "do not run two efforts against the same controller
files" actually requires.

### P1-3 · The tablet's common case — a 4K HDR source that must be encoded — cannot reach 1440 under the current proofs (§3.3, §3.4, §8.2)

**Source.** `capability_height_for_encoder` (`ladder.rs:27`) returns:

- `AUTO_SOFTWARE_HEIGHT` = **720** for software
- `AUTO_HARDWARE_PROBED_HEIGHT` = **1080** for "other hardware HDR graphs",
  which is every tone-map-to-SDR route except QSV plus the Profile-5 reshape
- `MAX_HEIGHT` only for known-SDR sources

`hdr10_rung_fits` admits only 1080 and 2160-QSV. The admission comment
records a CPU tone-map chain measured at 0.71×.

**Failing scenario.** Most 4K titles are HDR10 or Dolby Vision. The tablet
needs an encode for a burned PGS subtitle, a Profile 7 source, or a TrueHD-only
source with video conversion. What happens next depends on the panel:

- **SDR panel:** the route is tone-map → SDR H.264, capped at 1080 by the
  current proof. M2 adds a 1440 bitrate, but nothing in M2 raises the cap. The
  tablet keeps landing at 1080 for exactly the content Paul cares about. The
  §8.2 row "Required SDR transcode … starts at eligible 1440" still passes,
  because it can be run on an SDR *source*.
- **HDR panel:** 1440 HDR10 is unproved. The plan handles this case (prefer
  2160-QSV, else 1080) but never says which panel the tablet actually has.

**Smallest correction.**

- M0 records the tablet's panel HDR support, HDR decode profiles and codec
  levels.
- M2 adds two explicit rows:
  - **tone-mapped SDR 1440** from HDR10 and from DV, per production encoder,
    measured at realtime margin under the burn graph
  - **SDR-source 1440**
- `capability_height_for_encoder` becomes per-route candidate admission
  rather than a single ceiling. §3.3 already says this in general terms, so
  name the function.
- §8.2 splits the "required transcode" row by source grade. The Android
  acceptance bar names which grade it covers.

If tone-map at 1440 fails its realtime proof on the fleet's GPUs, Paul needs to
know **before** M3. That is the case his request was about.

### P1-4 · Upgrades and return-to-original have no attainable evidence source yet, and the plan's recovery path can strand a whole film (§4.3, §8.3)

**Source.** The paced encoded-VOD producer (`vodencode.rs`; the reader
frontier drives production) runs at about 1.0× by design. §4.3 requires a
measured production margin of at least 1.15×. `predictedSpeed(null)` is
tightened, so unknown speed no longer counts as allowed. §8.3 already concedes
that paced producers and unknown speed "can permanently strand reduced
quality".

**Failing scenario.**

1. At minute 5 of a 2-hour 4K film, a microwave takes out the Wi-Fi for 20 s.
2. Auto correctly drops copy → 1440.
3. For the rest of the film, the producer is paced, so its measured margin
   never exceeds about 1.0×. The passive transfer estimate comes only from
   1440 segments, and §4.3 correctly says those cannot prove an 80 Mb/s
   original.
4. The viewer watches 115 minutes at 1440 and never reaches the original.

That is the dominant user-visible outcome of the whole feature, and today it is
a TODO in M0.

**Smallest correction.** Add two cheap, deterministic mechanisms to §4.3 now.
Leave the clever evidence path as the M0 experiment.

1. **Re-plan at viewer-initiated discontinuities.** On a user seek, a resume
   after a pause of at least N s, or the next episode, re-run the *cold-start*
   selection: original-first, and unknown network is not evidence against it.
   Pass the current fresh transfer estimate as a veto only. The player
   interrupts at these points anyway, so a route change costs no extra
   interruption. Add the fixture: after one transient cliff, a seek returns to
   copy.
2. **Define producer headroom for paced producers** as the share of wall time
   the producer spent blocked on pacing, or as the ffmpeg `speed=` observed
   during its unpaced catch-up burst. Throughput divided by realtime is the
   wrong measure for a paced producer. Pick one in M0 and pin it with a
   fixture that shows a paced-but-idle producer counts as having headroom.

Without (1), P1-4 is a product regression compared with plain copy for anyone
with a flaky link.

## P2 findings

| # | Plan § · source | Scenario | Smallest correction |
|---|---|---|---|
| P2-1 | §3.3 "prior evaluation" · `auto_height_from_prior` (`ladder.rs:231`) | Both branches look up `LADDER_HEIGHTS` / `ladder()`: the starved-rung branch looks for the "rung below" there, and the sustained-kbps branch searches there too. So a starvation at 2160 lands at **1080**, skipping 1440, and a sustained 14 Mb/s (which would fit 1440 at a 12 Mb/s peak) also returns 1080. `worst_rung_height` also records supply *and* network stalls together. | Name this function in M2's scope and make it walk the route's candidate list. Add fixtures for a starve at 2160 → 1440 and for 14 Mb/s sustained → 1440. Record the prior only for `link`-class causes (A-05's classification), so encoder pressure cannot write a network verdict. |
| P2-2 | §3.3 bitrate · `bitrate_for_height` (`ladder.rs:3`) | The band covering 1080–2159 already gives a 3840×1600 scope "source rung" transcode (a burn under Original) only 8 Mb/s. Moving a band edge to introduce 12 Mb/s at 1440 silently changes every non-standard height in 1440–2159, which is most 4K scope films. Keying on height also ignores fps: 60 fps at 12 Mb/s is a different quality point from 24 fps. | Key the rate on output pixel rate (w×h×fps from the resolved candidate), not height, or state the band edges and the scope effect explicitly. Include the scope source rung in the §8.2 bitrate comparison. |
| P2-3 | §3.1/§3.2 "smallest covering" | A 2000×1200 panel (a common 11" tablet class) fits 16:9 at 2000×1125. The 1080 candidate misses by 45 px (4%), so Auto picks 1440 at 1.78× the pixel rate and 1.5× the bitrate. The §8 comparison also compares each rung *at its own bitrate*, which mixes resolution and rate. | Add a coverage tolerance (for example, accept a rung covering ≥ 0.9 of the fitted height) as an explicit fixture constant, and put it in the decision list for Paul. Make the §8.2 quality comparison equal-wire-cost: 1080 vs 1440 at the same kb/s, and 1440 vs 2160 at the same kb/s. That answers "is 1440 the best picture per bit", which is the real question. |
| P2-4 | §3.1 · pretranscode (`construct.rs:806` `pretranscode_target_height_for`; `recipe.rs`) | The speculative producer fills the content-addressed cache at **720** (Auto/software) or the source rung (explicit hardware). An Auto choice of 1440 never hits those entries. A title already pretranscoded at 2160 SDR, which is free to serve, gets re-encoded live at 1440 instead. Nothing in the plan mentions pretranscode. | Treat a complete cached recipe as a candidate with zero encode cost in the candidate builder. When it passes the network and decode checks, a cached 2160 beats a live 1440. State that pretranscode target policy is unchanged (a non-goal) or bring it into M2. |
| P2-5 | §8.2 thresholds vs A-04 D3 | The plan allows 250 ms presentation gaps. A-04's D3 oracle for the same controller is 100 ms with zero hitches, which Firefox has been failing on at 116–166 ms. Two acceptance bars for one reducer let a native trace "pass" here and fail D3. | Adopt the D3 oracles and harness verbatim for every cliff and recovery row. Add only the display-specific rows (1440 start, copy retention, geometry). |
| P2-6 | §5 Developer switch | Display-aware *initial* selection is server-side and applies to any client that sends a target. `playback_auto_abr` governs a controller no native client has. Under Paul's 2026-09-28 lifecycle rule, a behavior change that is not fully tested belongs on a Developer card of its own until the tablet acceptance passes, then graduates. The plan leaves open which switch covers M1–M3. | Give Effort A its own Developer card ("Fit Auto to the display") with advisory readiness rows, never blocking. Name its graduation condition. Leave `playback_auto_abr` to A-05. |
| P2-7 | §4.2 · `DesiredQuality`/`QualitySelection` derive `Copy` | A `String` `candidate_id` removes `Copy` from both enums, which ripples through every by-value match (for example `candidate_request`'s `match (&current.kind, selection.quality)`). | Use a fixed-width id (`[u8; 16]`, hex on the wire) so both enums stay `Copy`, or carry the id beside the selection in the intent envelope. Decide this in M0 with the schema. |
| P2-8 | §4.2 successor admission · `TranscodeResourceEstimate` is slot-count | A voluntary upgrade or return-to-original preparation takes a *second* hardware slot while the incumbent still holds one. Admission cannot tell 1440 from 2160 apart and has no notion of "voluntary". This is the Live TV "all slots are busy" failure class. | State that voluntary preparations are admitted only as speculative work (`Encoding.speculative`, `vodencode.rs:38`) and never preempt foreground or Live TV. Emergency step-downs keep today's priority. Add an admission fixture where Live TV plus one viewer plus a voluntary upgrade refuses the upgrade and keeps the incumbent. |
| P2-9 | §5 / M5 web · `playerPixelHeight` (`playback-policy.js:263`) | The web cap is element height × DPR. A portrait or narrow window (1080×2340 backing) caps at 2340 and permits 2160, although the fitted 16:9 picture is about 608 px tall. | Make M5 replace the height cap with the §3.2 fit function, with a fixture for a portrait window and a 2160 source picking at most 720. |
| P2-10 | §2 baseline | The baseline `ceb7dd8cc` is a feature branch (see the note under the title), not `main`. | Rebase the census to current Forgejo `main` at M0. The plan already says so; also record the SHA. |

## The internal review's dispositions

- **R2, R3, R4 and R5 close** as written. I checked R5's claims against the
  fixture: five `web_current` disagreements (link refusal, decode block,
  stall-verdict hold, cooldown boundary, and the six-switch budget), which
  matches §2.1 "four questions" plus the separately-unresolved budget.
- **R1 closes semantically but not operationally.** The candidate identity is
  right, but its rollout reproduces the withdrawn `accepted_acknowledgements`
  design (P1-1).
- **The retained question on scope proportionality** ("Opus should assess
  whether the effort needs an explicit product split") gets a yes: P1-2.

## Missing deterministic regressions

- **Mixed-version cluster (P1-1):** old ingress to new owner and new ingress
  to old owner for control, new to old for remote start, and retained terminal
  replay after rollback.
- **Tone-mapped SDR admission (P1-3):** an HDR10 source with a burned PGS
  subtitle on an SDR display with a QSV or VAAPI worker admits 1440 only when
  that route's proof exists, and otherwise 1080 with reason
  `capability_limit`.
- **Re-plan after a discontinuity (P1-4):** a transient cliff, then a user
  seek, returns to copy; unknown network does not veto copy at the re-plan.
- **Paced headroom (P1-4):** a paced producer that is idle on pacing counts as
  having headroom; a producer with unknown speed and no pacing data does not.
- **Prior (P2-1):** a starve at 2160 gives 1440; 14 Mb/s sustained gives 1440;
  a starve caused by the encoder writes no network verdict.
- **Rate (P2-2):** the scope source-rung rate is unchanged, or changed
  deliberately, and pinned; the 60 fps rate differs from the 24 fps rate.
- **Coverage tolerance (P2-3):** a 2000×1200 panel picks 1080 under the
  chosen constant.
- **Cache-aware candidates (P2-4):** a complete 2160 cached recipe beats a live
  1440 when the link fits.
- **Speculative admission (P2-8):** a voluntary upgrade is refused under full
  hardware slots; the incumbent is untouched.
- **Web fit (P2-9):** a portrait window with a 2160 source picks at most 720.

## Missing physical evidence

- **Tablet identity, recorded in M0:** model, panel HDR support, decoder
  profiles and levels, and whether two decoders can run at 4K + 1440 together.
- **Tone-map at 1440, per production GPU:** HDR10 → SDR and DV → SDR, with and
  without a PGS burn, measuring the realtime margin while one other session is
  admitted.
- **Equal-wire-cost picture comparison at 2400×1350:** 1080 vs 1440 at the
  same kb/s, and 1440 vs 2160 at the same kb/s, on grain, motion, animation and
  scope content.
- **Flaky-link case:** a 20 s cliff in minute 5 of a long title, followed by a
  seek. The route returns to copy.

## Decisions that are Paul's

1. **Split into Effort A plus A-05 milestones** (recommended, P1-2), or keep one
   effort and accept that nothing lands until native controllers and D3 are
   done.
2. **Coverage tolerance.** A rung may fall this far short of the fitted picture
   before Auto steps up: none (plan as written) or about 10% (recommended,
   P2-3).
3. **Return-to-original aggressiveness.** Re-plan only at viewer
   discontinuities (recommended baseline, zero extra interruptions), or also
   allow mid-play voluntary returns once an evidence path exists. The latter
   costs one prepared or fallback switch each time.
4. **Tone-map at 1440 fails its realtime proof on some GPUs.** Either
   advertise 1440 only on nodes that pass (heterogeneous per worker), or keep
   HDR-source encodes at 1080 fleet-wide.

## Strengths worth keeping

- Original-first, with "a smaller display alone must never force transcoding"
  and "unknown network is not evidence against original."
- The fit formula with backing pixels, keeping coded geometry separate from
  display geometry, and "unknown rectangle never enables a route."
- Keeping the capability view (what can be served) separate from the policy
  view (what Auto picks), so a Manual 1440 is never hidden by temporary
  pressure.
- The candidate identity as a *requested route, never an authorization token*,
  re-resolved server-side, with old digests byte-identical when it is absent.
- Refusing N/A rows for Apple cliff parity; missing measurements recorded as
  unknown.
- Leaving `LADDER_HEIGHTS` alone to protect offline and pre-route APIs.
- The honest note that §8.2's thresholds are proposals, not observations.