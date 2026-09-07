# Dolby Vision delivery — why DV titles arrive as HDR10 or lower

**Status:** findings only, no code changed · **Analysed:** `main` @ `3a056dc4`
(2026-08-28 19:21 -0400) · **Written:** 2026-08-29 · **For:** fable review

Read §2 first — it is the whole answer in one page. §3–§5 are the evidence
behind it. §6–§8 are the parts that are *not* explained by the main finding
and where an actual defect most likely lives. §9 is how to confirm any of it
against the live fleet in one request. §10–§11 are the decisions that need a
ruling before anyone writes code.

Everything is cited as `file:line` against `main` @ `3a056dc4`, read with
`git show main:<path>`. Where a claim is inference rather than a code fact it
says so in the same sentence. Nothing here was executed against the live
server — no fleet access was used in this analysis.

**Worktree caveat:** Paul's checkout is on `wip/loose-work-2026-08-29` @
`ecfcdf9e`, 3 commits ahead of `main` and ~20–40 behind. Its copies of
`playback/mod.rs`, `hls.rs`, `transcode.rs` and `vodserve.rs` are older than
`main`'s and line numbers differ (`index.html` by roughly −457). Do not read
the worktree for playback code.

---

## 1. The report

2026-08-29, Paul: "I see a lot (if not all) media that is dolby vision playing
at hdr10 or lower now. Why isn't dolby vision going to all devices that can
play it? I'm watching Resident Evil on an Apple Pro Display XDR right now and
it's doing hdr10 when it's a dolby vision profile 7 movie."

Two distinct claims are packed in there, and they have different answers:

- **"P7 title arrives as HDR10"** — expected, by design, §2/§5.
- **"a lot (if not all) DV … or lower"** — partly library composition,
  partly one or more of the open defects in §6/§7.

---

## 2. Verdict — Profile 7 is unreachable by every plurx client, on purpose

No plurx client can advertise Dolby Vision Profile 7, and the server requires
an exact profile match, so every P7 source is stripped to its HDR10 base
layer. Resident Evil is doing exactly what the code says to do.

```
 file.hdr == "dolby_vision"? ───── no ──▶ deliver source grade
        │ yes
        ▼
 client's dvprofile list contains
 the file's profile?              ───── yes ─▶ preserve ─▶ DELIVERED: dolby_vision
        │ no
        ▼
 ffmpeg has dovi_rpu AND the label
 says HDR10/HLG-compatible?       ───── no ──▶ Reencode ─▶ DELIVERED: sdr
        │ yes                                              (hdr10 only if
        ▼                                                   hdr10t=1 AND P5)
     Strip ─▶ Remux ─▶ DELIVERED: hdr10        ◀── Resident Evil lands here
```

The gate is one line, computed identically in the normal and the
`Force::Original` arms:

```rust
// crates/plurx-core/src/playback/mod.rs:543  (and :692 for Force::Original)
let preserve_dolby_vision = is_dolby_vision(file) && profile.allows_dolby_vision(file);
```

`delivered_dynamic_range` is a pure readout of that decision
(`playback/mod.rs:406-433`): DV source with `preserve == false` reports
`"hdr10"` — or `"hlg"` when the label says HLG-compatible — and any transcode
reports the encoder's grade.

**Why P7 is different from P5/P8 in the real world, not just in this code.**
P7 is dual-layer disc Dolby Vision: an HDR10 base layer, a separate
enhancement layer, and the RPU. No Apple, Android or browser decoder consumes
a dual-layer stream. Every streaming device that shows DV is being fed
single-layer P5 or P8. The only way to make a P7 disc remux arrive as real DV
is to convert its RPU to single-layer P8 (`dovi_tool`-style), which plurx does
not do anywhere — see §10.

---

## 3. The delivery contract — every condition that stops DV being DV

`allows_dolby_vision` is the blanket flag **or** an exact list membership:

```rust
// crates/plurx-core/src/playback/mod.rs:184-189
self.supports_dolby_vision
    || file_profile.is_some_and(|p| self.dolby_vision_profiles.contains(&p))
```

and the file's profile is parsed out of the **scan label string**
(`hdr_format`, e.g. `"Dolby Vision · Profile 7 (HDR10-compatible)"`) at
`playback/mod.rs:439-446`; the label itself is built at
`crates/plurx-core/src/scan/probe.rs:289-325`.

| # | Condition | Site | Delivered |
|---|---|---|---|
| A | `file.hdr != "dolby_vision"` — scan never flagged it | `playback/mod.rs:359` | source grade |
| B | Client sent no runtime caps ⇒ a named profile is used, and **no built-in profile sets `supports_dolby_vision` or `dolby_vision_profiles`** | `playback/profiles.toml`, `stream.rs:277-303` | `hdr10` |
| C | Client sent `dvprofile` at all — even empty ⇒ the legacy `dv=1` blanket bit is discarded | `stream.rs:286` | `hdr10` |
| D | Source profile ∉ the client's list | `playback/mod.rs:184-189` | `hdr10`/`sdr` |
| E | Label carries no parseable "Profile N" ⇒ `dolby_vision_profile() == None` ⇒ D fires for every list client | `playback/mod.rs:439-446`, `scan/probe.rs:302-326` | `hdr10`/`sdr` |
| F | Not preserved + strippable + compatible base ⇒ `DvHandling::Strip` ⇒ `container_ok=false` ⇒ Remux | `playback/mod.rs:522-531`, `541-557` | `hdr10` |
| G | Not preserved + (not strippable **or** no compatible base) ⇒ `DvHandling::Reencode` ⇒ `video_ok=false` ⇒ Transcode | `playback/mod.rs:527-528`, `558-592` | `sdr` |
| H | `!height_ok` — `maxheight`/`vmaxheight` below source height ⇒ transcode regardless of DV | `playback/mod.rs:245-247`, `264-279` | `sdr`/`hdr10` |
| I | `!hdr_ok` — `hdr=0` and the DV claim did not match this file | `playback/mod.rs:306-313` | `sdr` |
| J | `!bitrate_ok` / `!video_ok` | `playback/mod.rs:245-247` | `sdr` |
| K | `Force::Transcode` from the quality menu — hardcodes `preserve_dolby_vision: false` | `playback/mod.rs:646-668` | `sdr` |
| L | Selected subtitle needs burn-in and the plan was already SDR | `stream.rs:775-788`, guard at `:790-797` | `sdr` |
| M | **HLS create takes `preserve_dolby_vision` from the client's query, not from the server's own decision** | `http/hls.rs:296-301` | `hdr10` |
| N | VOD prerequisites: transcode session, no index at this identity, varying parameter sets | `vodserve.rs:889-898`, `973-977`, `981-985` | refusal → client fallback |
| O | `exact_hls_context` rewrites master `CODECS` from the emitted init — a preserved DV whose init lost `dvcC`/`dvvC` re-advertises as plain `hvc1` | `http/hls.rs:2863-2932` | master says HDR10 |
| P | `copied_hls_codecs` with no stored probe JSON emits a bare `"dvh1"` with no `SUPPLEMENTAL-CODECS`, which the code's own comment says breaks asset preparation | `plurxd/src/transcode.rs:4285-4321` | AVPlayer fails → transcode |

Rows F and D together are Resident Evil. Rows E, M, N, O are the ones worth
suspecting for anything that is *not* a P7 title — see §7.

---

## 4. What each client actually advertises

| | **Apple** (`Caps.swift:53-78`) | **Android** (`Caps.kt:102-111`, `CapsPolicy.kt:160-165`) | **Web** (`index.html:6453-6460`, `6494-6499`) |
|---|---|---|---|
| `dv` | `1` iff `hevc && eligibleForHDRPlayback && availableHDRModes.contains(.dolbyVision)` (`:51`); else `dv=0`, key always sent | sent only when profiles non-empty **and** the display reports `HDR_TYPE_DOLBY_VISION`; otherwise key omitted | `1` iff `dvProfiles.length` (`:6454`); else `dv=0`, key always sent |
| `dvprofile` | literal **`5,8`**, or empty string — key always present (`:68-71`) | CSV from decoder constants: `DVHE_DTR→4`, `DVHE_STN→5`, `DVHE_ST→8` (`CapsPolicy.kt:109-121`) | **`5`** and/or **`8`** (`:6445-6446`), or empty — key always present |
| `dvhls` | always `1` (`:77`) | never sent | never sent |
| `hdr` | `AVPlayer.eligibleForHDRPlayback` (`:57`, `:90-92`) | any display HDR type **and** hevc-or-av1 decodable (`Caps.kt:81-82`) | `matchMedia("(dynamic-range: high)")` **and** hevc-or-av1 (`:4721-4724`, `:6430-6431`) |
| `hdr10t` | not sent | not sent | `1` iff `pq10 && hdrDisplay` (`:6460`) |
| `maxheight` | not sent | per-codec `vmaxheight` since `58fc89ea` (2026-08-21) | `min(tallest 8-bit rung, tallest Main10 rung)` since `7d2a68c3` (2026-08-17) — `index.html:6327-6356` |

**Profiles advertised, exhaustively: Apple `{5,8}` · Android `{4,5,8}` · web
`{5,8}`. No client can ever advertise Profile 7.** The exclusion is explicit
and commented in all three:

```swift
// clients/apple/Sources/Caps.swift:64-71
// Profile 5 and 8 are advertised explicitly so the server never
// mistakes support for those delivery profiles as support for
// Blu-ray Profile 7.
```

```kotlin
// clients/android/.../CapsPolicy.kt:110-118
// DVHE_DTB is profile 7: dual-layer, never claimed.
```

```js
// crates/plurxd/src/web/index.html:6442-6446
const dvCan=p=>[`dvh1.${p}`,`dvhe.${p}`].some(c=>can(...)||mse(...));
if(dvCan("05.06")) dvProfiles.push(5);
if(dvCan("08.07")) dvProfiles.push(8);
```

**There is no macOS client.** `clients/apple/project.yml:24-143` declares only
`plurx-iOS` and `plurx-tvOS` — no Catalyst, no macOS target, no `Package.swift`
anywhere in the repo; `Caps.swift:5` imports UIKit and `:26` calls
`UIDevice.current.model`. **On a Mac, Paul is necessarily in the web player**:
Chrome cannot do DV at any profile, Safari can do P5/P8 only.

---

## 5. When P7 behaviour changed, and why it feels like a regression

`807e5bbe` — *"feat(server): add native discovery and HDR negotiation"*,
2026-07-31 — replaced the web's blanket DV boolean with the profile list:

```diff
-  const dv=["dvh1.05.06","dvh1.08.07","dvhe.05.06","dvhe.08.07"].some(...)
-  ... hdr:hdr?1:0, dv:dv?1:0};
+  const dvCan=p=>...; const dvProfiles=[]; if(dvCan("05.06")) dvProfiles.push(5); ...
-const CAPS_Q=`...&dv=${PLAY_CAPS.dv}`;
+const CAPS_Q=`...&dv=${PLAY_CAPS.dv}&dvprofile=${PLAY_CAPS.dvprofile}`;
```

Before it, Safari sent a bare `dv=1`, which `stream.rs:286` reads as
`supports_dolby_vision = true` — *every* profile preserved, P7 included. After
it, the key is always present, so the blanket path is permanently dead for web
and Apple, and P7 strips.

**What that does and does not establish.** It establishes that the badge used
to read Dolby Vision on P7 titles and now reads HDR10. It does **not**
establish that Safari was ever *rendering* Dolby Vision off a dual-layer file —
the far more likely reading is that it rendered the base layer while the
delivery label claimed DV. Nobody measured it at the time, and it cannot be
measured retroactively. Treat "we lost DV on 7/31" as unproven; "we lost the DV
*label* on 7/31" is proven.

Deployment: `git merge-base --is-ancestor 807e5bbe 97a10ee` → yes, and
`docs/STATUS.html:311` records nuc4 on `v0.2.7-502-g97a10ee`. The web player
ships inside `plurxd`, so there is no client-side lag — this is live.

**Stale doc:** `docs/streaming/MEDIA-BADGES-PLAN.md` (~line 340) still says "DV P7
HDR10-compatible + DV-capable client → direct play, delivered `dolby_vision`".
The allow-list has made that unreachable since 2026-07-31. Fix the doc in
whatever PR settles §10.

---

## 6. "or lower" — the paths that end at SDR rather than HDR10

`reencode_grade` (`playback/mod.rs:514-520`) returns `OutputGrade::Sdr` unless
`hdr10t=1` **and** the source is a P5 needing RPU render
(`dolby_vision_needs_rpu_render`, `:463`). So every re-encode of a DV title is
SDR unless both hold. Four ways to land there:

**6.1 `has_dovi_rpu()` false — turns every strip into a full SDR transcode.**

```rust
// crates/plurxd/src/ffmpeg.rs:445-467, memoized at boot in a OnceCell (:481-486)
Ok(list) => declares_bsf(list, "dovi_rpu"),
Err(e)   => { tracing::warn!(...); false }   // a probe FAILURE is indistinguishable
```

`declares_bsf` is an exact whole-line match (`ffmpeg.rs:404-406`), so different
indentation in `-bsfs` output, or a `bounded_command_output` timeout, answers
false. The Dockerfile installs jellyfin-ffmpeg7 **unpinned**, so a rebuild can
drop the filter silently. Consumers: `state.rs:501` (server-wide
`dv_strippable=false`), `stream.rs:587-589` (`Strip` becomes `Reencode` ⇒ SDR),
`core/transcode/mod.rs:270-286` (drops `dovi_rpu=strip=1`, leaving DOVI side
data so the muxer writes a `dvcC` over a stripped stream — the Safari
software-decode bug in `docs/streaming/STUTTER-4K.md`), and `fragindex.rs:258-265`
(`have_dovi` is inside the argv fingerprint ⇒ **every stored fragment index
invalidated at once**).

**It cannot turn a preserved DV delivery into HDR10** — `dv_strippable` is not
read anywhere in the `preserve_dolby_vision` computation. It explains "or
lower", not "DV → HDR10".

**6.2 Safari and `hdr10t`.** `index.html:6386-6389` passes
`video.transferFunction:"pq"` to `mediaCapabilities.decodingInfo`.
`transferFunction` is a Chrome-origin `VideoConfiguration` member; if Safari
rejects or ignores it, `pq10` is false ⇒ `hdr10t=0` (`:6460`) ⇒
`supports_hdr10_transcode=false` (`stream.rs:296`) ⇒ every DV re-encode is
tone-mapped to SDR. **Inference, unverified** — one console line settles it.

**6.3 The `maxheight` cap, new on 2026-08-17.** Before `7d2a68c3` the web sent
no `maxheight` at all, so `height_ok` was vacuously true. It now sends
`min(tallest 8-bit rung, tallest Main10 rung)` (`index.html:6327-6356`). If
`hvc1.2.4.L153.B0` (4K Main10) fails `decodingInfo` while the 8-bit 4K rung
passes, `maxheight` comes out **1080**, and `playback/mod.rs:245-247` then
transcodes *every 4K title*, DV or not. That is "HDR10 or lower, library-wide,
on a machine that used to direct-play". **Inference with a one-line check**
(§9). `58fc89ea` (2026-08-21) added the same mechanism per-codec on Android
(`vmaxheight`, ceiling at `playback/mod.rs:264-272`).

**6.4 `Force::Transcode`** from the quality menu hardcodes
`preserve_dolby_vision: false` and grade `Sdr` (`playback/mod.rs:646-668`).
Worth ruling out before chasing anything else if the session was started from
the quality menu.

---

## 7. Real defect candidates that are *not* the P7 rule

Ranked by how well each explains "a lot (if not all)", with the fact/inference
line drawn explicitly.

**7.1 Profile-less labels (fact in code, unmeasured in the library).**
`scan/probe.rs:302-326` emits a bare `"Dolby Vision"` with **no profile
number** when the DOVI record lacks `dv_profile`, or when only the codec tag
identified the stream. `dolby_vision_profile()` then returns `None`, condition
E fires, and the file can never be claimed by **any** client — P5 and P8
included. If a meaningful share of the library scanned that way, this alone
reproduces the whole report. Measured by one SQL query (§9).

**7.2 HLS create trusts the client, not the server (fact).**

```rust
// crates/plurxd/src/http/hls.rs:296-301
preserve_dolby_vision: self.preserve_dolby_vision == Some(true),
```

The server's own `decide()` result is not consulted at create time — the
client must re-assert the flag on the create, and on every seek or reopen that
starts a new session. Any client path that drops the field silently downgrades
to HDR10 with no reason string. Whether a live client path drops it is
**unverified**; the seam is real either way and is cheap to close by making
create re-derive rather than trust.

**7.3 The VOD index is only ever built for the stripped identity (fact in
code; user impact inferred).** The background indexer hardcodes the flag:

```rust
// crates/plurxd/src/state.rs:1197-1209
CopyVideoOptions::from_probe(file, probe_json.as_deref(), have_dovi,
                             /* preserve_dolby_vision */ false)
```

but the index identity is the **argv fingerprint**
(`fragindex.rs:258-265` → `segplan::argv_fingerprint(copy_video_args(...))`),
and `copy_video_args` differs between preserve true/false for every DV file
(`-strict unofficial` and the `-bsf:v` string,
`core/transcode/mod.rs:1245-1300`). So a `preserve_dolby_vision: true` VOD
session's identity is **never indexed** ⇒ permanent `vod_index_pending`
(`vodserve.rs:973-977`) for every DV title on every DV-capable client. The
admin "indexed/pending" badge computes the same wrong way
(`http/browse.rs:356-368`).

What probably keeps playback alive: `live-hls-recovery` is a default feature
(`crates/plurxd/Cargo.toml:10`) and `live_hls_recovery_enabled()` is true
unless the setting is literally `"0"` (`transcode.rs:10100-10114`); the live
copy segmenter *does* honour `preserve_dolby_vision`
(`transcode.rs:10144-10156`, `12305-12311`). **If `playback.vod_live_recovery=0`
is set anywhere on the fleet, this becomes a total DV outage.** Web fallback
policy on refusal: Chrome → `progressive_remux`; Safari/native-HLS → `"fail"`
→ error path → transcode (`web/playback-policy.js:556-566`,
`index.html:6960-6985`).

Compounding it, `9280c696` (2026-08-28, ~24h before the report) changed
`copy_video_args` for any HEVC whose probe reports `extradata_size == 23` — the
common MKV/WEB-DL minimal-`hvcC` shape — to
`[dovi_rpu=strip=1,]hevc_mp4toannexb,extract_extradata[,filter_units=remove_types=62-63]`
(`core/transcode/mod.rs:1272-1290`). That invalidates **every stored fragment
index** for those files, and re-indexing runs at `INDEX_MAX_PER_PASS = 4` files
per pass on a 15-minute cadence (`state.rs:930-937`, `VOD_INDEX_MINS`
default 15). The timing coincidence with the report is worth taking seriously.

**7.4 `hevc_mp4toannexb,extract_extradata` may strip `dvcC` from preserved DV
(unverified inference).** The same `9280c696` chain now runs on preserved-DV
streams too. If ffmpeg's MOV muxer no longer emits `dvcC`/`dvvC` afterwards,
`exact_hls_context` (`hls.rs:2863-2932`) reads the init, finds no DV record,
and re-advertises the variant as plain `hvc1` — a silent DV→HDR10 downgrade
with no reason string anywhere. One `xxd` on a live init segment settles it
(§9).

**7.5 Ruled out.** `scan/probe.rs` DV detection is untouched since 2026-07-22 —
not a re-scan regression unless ffprobe itself changed under it. The Android
`dv=1 & hdr=0` coupling bug is **fixed** on main at the server seam
(`6e98c2fb`, 2026-08-17, `playback/mod.rs:289-307`, regression pinned at
`:1367-1413`) and the Android client no longer couples them
(`CapsPolicy.kt:160-165` gates on the DV-specific display bit). The **Apple**
client still has the equivalent coupling — `let supportsDolbyVision = hevc &&
displayHDR && dolbyVision` (`Caps.swift:51`) — and because it zeroes `dv`
before the wire, the server-side fix at `6e98c2fb` cannot rescue it. Not
Paul's client in this report, but it is a live defect for Apple TV.

---

## 8. What this analysis does **not** establish

- Nothing was run against the live fleet. Every "this would cause X" in §6
  and §7 is a code path, not an observation.
- The library's actual profile mix is unmeasured. "Most disc remuxes are P7"
  is a general fact about disc remuxes, not a measurement of Paul's library —
  §9.1 measures it.
- Whether Safari ever truly rendered DV from a P7 file before 2026-07-31 is
  unknowable now (§5).
- Whether `dvcC` survives the new bsf chain (§7.4) is untested.
- Whether Safari answers `decodingInfo` with `transferFunction` (§6.2) is
  untested. The repo's only "evidence" about Safari's DV answers is a test
  *assumption* — `tests/playback/web-policy.test.js:2763-2764` says "Safari
  answers yes to both profiles" and then feeds a stub. Nobody recorded a real
  Safari answer.

---

## 9. The decisive checks

**9.1 Library census — sizes the whole problem, one query.**

```sql
-- how much of the library is P7 (working as designed) vs P5/P8 (should be lit)
SELECT hdr_format, COUNT(*) FROM files WHERE hdr='dolby_vision' GROUP BY 1;

-- §7.1: files no client can ever claim, because the label has no profile
SELECT id, hdr_format FROM files
 WHERE hdr='dolby_vision' AND hdr_format NOT LIKE '%Profile %';
```

**9.2 One request separates four candidates.**

```bash
GET /api/v1/files/<id>/decision?<the client's own CAPS_Q>
```

Read four fields: `preserve_dolby_vision`, `delivered_dynamic_range`,
`vod_indexed` (`stream.rs:530`, `1140-1145` — computed with the real
`preserve_dolby_vision`), and `reasons`. The reason string
`"Dolby Vision metadata removed for this device; compatible HDR base kept"`
(`playback/mod.rs:553-556`) is the P7/allow-list path. `vod_indexed:false` on
every DV title while the browse view says "indexed" confirms §7.3.

**9.3 Browser console, on the player page.**

```js
PLAY_CAPS   // .dv, .dvprofile, .hdr, .hdr10t, .hdrDisplay, .maxheight
```

`maxheight === 1080` on a 4K-capable Mac confirms §6.3. `hdr10t === 0`
confirms §6.2.

**9.4 Fleet capability.**

```bash
curl -s http://<node>/api/v1/system | jq .dovi_rpu   # §6.1
ffmpeg -hide_banner -bsfs | grep -x '  *dovi_rpu'    # on the node itself
grep -i 'vod_index_pending\|temporary live-HLS recovery' <plurxd log>   # §7.3
```

**9.5 Init segment box dump — settles §7.4.**

```bash
curl -s <session>/init.mp4 | xxd | grep -i 'dvcc\|dvvc'   # absent ⇒ confirmed
```

---

## 10. The options on the table for P7

**Option A — leave delivery alone, make the badge unambiguous.** Show
`DV P7 → HDR10` with the reason, so it reads as a property of the format
rather than a fault. Cost: near zero; the `delivered_dynamic_range` contract
already carries it (`docs/streaming/MEDIA-BADGES-PLAN.md`). Buys nothing in picture
quality.

**Option B — convert P7 to single-layer P8 (`dovi_tool`-style RPU
conversion).** The only option that actually delivers DV from disc remuxes to
Apple TV and Safari. Shape of the work, none of it verified against plurx's
current transcode seam:

- Extract the RPU, convert it (the `dovi_tool -m 3`-equivalent mode), drop the
  enhancement layer, remux the base layer with a P8 `dvcC` and the converted
  RPU. MEL sources are near-lossless this way; **FEL sources lose the
  enhancement layer's detail** — the honest framing is "P8 with the FEL
  discarded", not "lossless P7".
- New external dependency, or an in-tree RPU implementation. `dovi_tool` is
  Rust and MIT-licensed, so vendoring is at least conceivable — Paul's standing
  constraint of no new external deps (`plurx-perf2-plan`) makes this a decision
  for him, not an implementation detail.
- Where it runs matters: on the copy/remux path it is a per-session bitstream
  transform, which is exactly where §7.3's identity fingerprint and §7.4's
  `dvcC` survival question already live. Both have to be settled first or the
  conversion will land on top of a broken index keyspace.
- It also needs the clients to keep advertising `{5,8}` — the file becomes P8
  on the wire, so no client change is required, which is the one genuinely
  clean part of this.

**Option C — let a client claim P7 when it genuinely decodes it.** Nothing
consumer-side does, so this is only interesting if plurx ever targets a
hardware player that does. Not worth building today; noted so it is not
re-proposed as if it were free.

**Option D — treat `dv=1` with an empty/absent `dvprofile` as the blanket
claim again** (undo the `stream.rs:286` semantics). This is what the pre-7/31
behaviour was. It would restore the DV *label* on P7 titles without restoring
DV, i.e. it re-introduces a lie. Listed only to be explicitly rejected.

---

## 11. What fable is being asked to rule on

1. Is the P7 finding (§2/§4/§5) accepted as the explanation for the Resident
   Evil case, or is there a reading of `allows_dolby_vision` that this analysis
   missed?
2. §7.3 — is the "VOD index never built for the preserved identity" reading
   correct, and if so is the fix (a) index both identities, (b) make the
   identity ignore the DV-affecting argv, or (c) make the indexer take the
   session's real flag? Each has a different blast radius on the existing
   index keyspace.
3. §7.2 — should `hls.rs:296-301` re-derive `preserve_dolby_vision` from the
   server's own decision rather than trusting the query parameter? That is a
   wire-contract change for all three clients.
4. Option B: build, defer, or reject on the external-dependency constraint —
   and if build, does the FEL-discard trade get surfaced in the UI or hidden?
5. Ordering: §6/§7 diagnosis before any P7 work, or in parallel? The argument
   for serial is that §7.3 and §7.4 sit on exactly the seam Option B would
   modify.

---

## 12. Non-goals for whoever executes this

- **Do not "fix" P7 by widening a client's advertised profile list.** No
  consumer decoder takes dual-layer; a client claiming P7 would get a stream
  it cannot play, and the failure mode is a black screen, not a downgrade.
- **Do not change strip/tonemap behaviour while chasing the badge.** The
  reporter/decider split in `docs/streaming/MEDIA-BADGES-PLAN.md` §9 exists for a reason.
- **Do not touch `exact_hls_context` on a guess.** It has already caused one
  AVPlayer `-12927` outage by rewriting a correct `dvh1.05.06` master CODECS
  into an HEVC-shaped identifier; any change there needs a master-playlist
  assertion test, of which there are currently none.
- **Do not re-scan the library to "fix" §7.1 labels** before checking whether
  the current ffprobe emits `dv_profile` for those files — a re-scan that
  produces the same bare label costs hours and changes nothing.

---

## 13. Provenance

Analysed read-only on Paul's Mac against `main` @ `3a056dc4`, via
`git show main:<path>` — never the worktree (see the caveat under the status
header). Two independent agents traced the server path and the client path
separately and agreed on the P7 finding; where they disagreed on ranking, the
disagreement is preserved in §6.1 (`has_dovi_rpu` explains "or lower", not
"DV → HDR10"). No commits, no branches, no writes to the repo. No live server
or fleet access was used.
