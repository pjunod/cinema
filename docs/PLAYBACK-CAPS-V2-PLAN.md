# Playback capabilities v2 — highest deliverable grade, negotiated not guessed

**Status:** ready to build · **Executes:** fable's rulings of 2026-08-29 on
opus's DV-delivery findings · **Analysed:** `main` @ `4ba8bb48` ·
**Written:** 2026-08-29 · **Builder:** opus

Companion to [MEDIA-BADGES-PLAN.md](MEDIA-BADGES-PLAN.md) (what the badge
promises) and [VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) (how a
copy session is served) — this is *how the server decides what grade to put
on the wire, and how that decision stops lying at the edges.*

Read §1–§3 first: the objective, the diagnosis, and the target shape. §4 is
the contract section — every interface you will touch, copied from the code
at `4ba8bb48` and marked to re-verify. §5 is the dependency decision. §6 is
the guardrail list. §7 is the milestone sequence; build them **in order**,
each to its acceptance check, one PR per milestone, and let Paul merge.
Standing instruction: if a step seems to require changing `decide()`'s
ladder order, the badge contract in MEDIA-BADGES-PLAN §3, or the fragment
index blob format (`SEGPLAN_VERSION`), **stop and flag it** — none of those
are in scope, and each has a scar behind it.

Every `file:line` here is against `main` @ `4ba8bb48`. Read them with
`git show 4ba8bb48:<path>`; line numbers on `main` will have drifted by the
time you build. Never read Paul's worktree for playback code — his checkout
sits on whatever branch he was last testing.

---

## 1. Objective — every title plays at the highest grade the device can show

Paul's report (2026-08-29): "a lot (if not all) media that is Dolby Vision is
playing at HDR10 or lower now." The analysis (recorded in project memory and
opus's findings doc) established two things: the specific case — a Dolby
Vision **Profile 7** disc remux on a Mac — is delivered as HDR10 *by design*,
because no plurx client can decode dual-layer DV; and the "or lower" half is
real, caused by several independent paths that end in an SDR tone-map.

The objective of this plan is not "make the badge say DV". It is: **for every
(source, client, node) triple, the grade on the wire is the best grade all
three can actually honour, the decision is made once, and every demotion
carries a reason a human can read.** Concretely, when this plan is done:

- A plain HDR10 title that has to be transcoded (resolution cap, bitrate cap,
  burned subtitles, a learned limit) reaches an HDR-capable client as
  **HDR10**, not SDR. Today it is SDR unless the source is DV Profile 5.
- A Profile 5 or 8 title reaches Safari, Apple TV and DV-capable Android as
  DV over a **served fragment index**, not the live-HLS recovery path. Today
  every preserved-DV VOD session is `vod_index_pending`.
- A Profile 7 disc remux reaches those same clients as **Profile 8.1 Dolby
  Vision** through a prepared sidecar, with the badge saying so honestly
  (`DV P7 → DV P8`, enhancement layer discarded).
- The server hears one structured capabilities document per decision instead
  of a growing bag of query flags, re-derives the plan on session create
  instead of trusting an echo, and can see a browser's learned decode limits
  in its own reasons.

---

## 2. Diagnosis — the decider is right; the edges lie

`decide()` (`crates/plurx-core/src/playback/mod.rs:536-640`) is sound: a
direct → remux → transcode ladder, a reason string per demotion, and
`delivered_dynamic_range` (`:406-433`) as a pure readout of the decision.
Keep it. Every defect found lives at an edge of it:

| # | Edge | Where | Consequence |
|---|---|---|---|
| E1 | **VOD index built only for the stripped identity.** The indexer hardcodes `preserve_dolby_vision=false`; the session identity uses the request's real flag; `copy_video_args` differs, so the fingerprints differ. | `crates/plurxd/src/state.rs:1198-1209` (sole indexing caller of `from_probe`, used at `:3424`, `:3614`, `:4002`, `:4301`) vs `crates/plurxd/src/vodserve.rs:1494-1500`; args differ at `crates/plurx-core/src/transcode/mod.rs:1259-1296` | Every preserved-DV VOD session → `vod_index_pending` (`vodserve.rs:1547`) → live-HLS recovery (`crates/plurxd/src/transcode.rs:12940-12970`). A total DV outage if `playback.vod_live_recovery=0`. The admin "indexed" badge (`http/browse.rs:357-368`) lies the same way. |
| E2 | **DV facts parsed from a human label.** `dolby_vision_profile()` does `split("profile")` on `files.hdr_format`. | `playback/mod.rs:439-446`; label built at `crates/plurx-core/src/scan/probe.rs:289-325` | A record without `dv_profile`, or a codec-tag-only detection, yields the bare label `"Dolby Vision"` ⇒ profile `None` ⇒ unclaimable by **any** client. No column carries `el_present_flag`, which the P7 sidecar needs. |
| E3 | **Transcode grade is hardcoded.** `reencode_grade` returns SDR unless the source is P5-needing-RPU-render **and** the client sent `hdr10t=1`. | `playback/mod.rs:514-520`; `dolby_vision_needs_rpu_render` `:463` | Every transcode of HDR10, HDR10+, HLG, P7 or P8 is tone-mapped. This is the single largest "or lower" cause. |
| E4 | **Two deciders.** `/decision` computes `preserve_dolby_vision`; the client echoes it in the create body; create trusts the echo. | `crates/plurxd/src/http/hls.rs:562` (field), `:603` (`== Some(true)`) | Verified all three clients propagate it today (web `index.html:6786,6981`; Apple `PlayerController.swift:5074-5082`; Android `SubtitlePolicy.kt:277`) — a seam, not a live bug. It stays a seam until create re-derives. |
| E5 | **Flat, positional wire caps with presence semantics.** `dv=1` is the blanket claim only when `dvprofile` is *absent*; `hdr10t` is a bespoke flag for one rung; `maxheight` is codec-agnostic so the web sends `min(8-bit rung, Main10 rung)` and Android bolted `vmaxheight` beside it. | `crates/plurxd/src/http/stream.rs:170-215` (query), `:277-303` (`caps_profile`); web `index.html:6337-6376` | Each new capability is a new key with its own absent-means-what rule; the min-of-rungs hack costs a needless transcode on devices that decode 8-bit 4K but Main10 only at 1080p. |
| E6 | **Caps are a boot-time snapshot; learned limits live in one browser.** The web stores decode limits in `localStorage["plurx_decode_limits"]` (30-day TTL, 7-day retest) and routes matching titles to `force=transcode` on Auto. | `crates/plurxd/src/web/index.html:6684-6725`, `web/playback-policy.js:838-870, 942-956` | A stuttery session poisons every title with the same codec/size/range/bitrate-bucket identity for a month, invisibly to the server and to other nodes; combined with E3 that transcode is SDR. Apple/Android have no equivalent. |

What is **not** a defect and stays as it is: the P7 rule. Apple advertises
`dvprofile=5,8` (`clients/apple/Sources/Caps.swift:64-71`), the web probes
`dvh1/dvhe.05.06` and `.08.07` only (`index.html:6463-6467`), Android maps
`DVHE_DTR→4, DVHE_STN→5, DVHE_ST→8` and drops `DVHE_DTB` (=7)
(`clients/android/app/src/main/java/tv/plurx/app/data/CapsPolicy.kt`).
No consumer decoder Paul owns takes dual-layer. The route to DV from a P7
disc is conversion to single-layer Profile 8.1 (§7 M5), never a wider claim.

---

## 3. Target shape — one capabilities document, one decision, one grade rule

```
 CLIENT (boot + on every /decision)               SERVER (per node)
 ┌──────────────────────────────┐                 ┌──────────────────────────────┐
 │ caps v2 JSON  (§4.1)         │   POST          │ DeviceCaps → DeviceProfile   │
 │  video[]: codec, profiles,   │──/decision────▶ │ (§4.2, one translation for   │
 │   max_height, max_bitrate,   │                 │  v2 JSON and legacy CAPS_Q)  │
 │   present[]: sdr|pq|hlg,     │                 │           │                  │
 │   dv_profiles[]              │                 │           ▼                  │
 │  audio[], containers[],      │                 │ decide(file, profile,        │
 │  transports[], display{},    │                 │        node.render_caps)     │
 │  learned_limits[] (§4.6)     │                 │  ladder unchanged            │
 └──────────────────────────────┘                 │  grade = target_grade(§4.4)  │
            │                                     │           │                  │
            │ plan (method, grade, preserve_dv,   │           ▼                  │
            │       source_variant, reasons)      │ DecisionResponse + plan      │
            ◀─────────────────────────────────────│                              │
            │                                     └──────────────────────────────┘
            │ POST /hls/sessions {caps, plan_ref, overrides}
            ▼
 ┌──────────────────────────────┐
 │ create RE-DERIVES the plan   │  mismatch ⇒ typed refusal `plan_mismatch`
 │ from caps; overrides are     │  named overrides only: compatible_hdr_base,
 │ named, each with a reason    │  force=transcode, subtitle burn
 └──────────────────────────────┘
```

Five ideas, each one milestone:

1. **Index every identity a real client can request** (M1). A DV file gets a
   preserved-DV index and, when its base layer is HDR10/HLG-compatible, a
   stripped index too. The node-local sidecar table gains a composite key.
2. **DV facts are columns, the label is derived** (M2). `files.dv_profile`,
   `dv_level`, `dv_bl_compat_id`, `dv_el_present`, `dv_rpu_present`, backfilled
   from the probe JSON already stored — no re-scan.
3. **Caps v2 + plan-by-reference** (M3). One JSON document; the server keeps
   translating legacy `CAPS_Q` for one release; create re-derives.
4. **Grade negotiation** (M4). `target_grade(source, client presentation,
   node render caps)` replaces `reencode_grade`; the HDR10 passthrough rung
   opens for non-DV HDR sources and stripped P7/P8.
5. **P7 → P8.1 prepared sidecar** (M5) using `dovi_tool` + `mkvmerge` (§5),
   scanned like any file, chosen by `decide()` as a *source variant*.

Plus M0 (measure before touching anything) and M6 (learned limits reported
inside caps v2, so the server's reasons name them).

---

## 4. Contracts — exact interfaces (re-verify each against the file at build time)

### 4.1 Caps v2 document (client → server)

Posted as the JSON body of `POST /api/v1/files/:id/decision` (new; the GET
with `CAPS_Q` stays and is translated by the same function, §4.2) and again
as `caps` in the session create body (§4.5). Every field optional; absent
means "not claimed", never "unknown-so-assume-yes" — the same rule
`hdr10t` documents today (`stream.rs:195-204`).

```json
{
  "v": 2,
  "client": { "kind": "web|ios|tvos|android", "build": "86", "ua": "..." },
  "video": [
    { "codec": "hevc",
      "profiles": ["main", "main10"],
      "max_height": 2160,
      "max_bitrate_bps": 120000000,
      "present": ["sdr", "pq", "hlg"],
      "dv_profiles": [5, 8] },
    { "codec": "h264", "max_height": 2160, "present": ["sdr"] },
    { "codec": "av1", "profiles": ["main"], "max_height": 2160, "present": ["sdr", "pq"] }
  ],
  "audio": ["aac", "ac3", "eac3", "flac", "opus"],
  "containers": ["mp4", "mov", "m4a"],
  "transports": ["progressive", "hls"],
  "dv_transport": "hls",
  "display": { "hdr": true, "dolby_vision": true, "max_nits": 1600 },
  "learned_limits": [
    { "identity": "hevc|main10|3840x2160|10|dolby_vision|Dolby Vision · Profile 7 (HDR10-compatible)|50",
      "lost": 41, "secs": 60, "rate": 41, "at_ms": 1756400000000 }
  ]
}
```

Field semantics, and what each replaces:

| Field | Meaning | Replaces |
|---|---|---|
| `video[].codec` | one entry per decodable codec; the server's per-codec ceiling replaces the single `max_height` + `vmaxheight` pair | `vcodec`, `maxheight`, `vmaxheight` |
| `video[].profiles` | codec profiles the decoder proved at `max_height` (`main`/`main10` for HEVC). A 10-bit ceiling lower than 8-bit is expressed as **two entries** for the same codec with different `max_height`, which is exactly what the web's min-of-rungs hack was working around | the `hevcTierSummary` min |
| `video[].present` | transfer functions the device can *present* for this codec: `sdr`, `pq` (HDR10/HDR10+), `hlg`. `pq` here is what `hdr10t=1` meant, generalised | `hdr`, `hdr10t` |
| `video[].dv_profiles` | DV profiles this codec's decoder takes, exhaustively; `[]` means none. **Never 7** on a consumer client (§6) — except Android devices whose MediaCodec declares `DolbyVisionProfileDvheDtb` (§7 M5 follow-on) | `dv`, `dvprofile` |
| `dv_transport` | `hls` when preserved DV must ride the copy-video HLS envelope; `progressive` when raw MP4 is fine | `dvhls` |
| `display` | facts about the attached output, separate from decode: Apple `eligibleForHDRPlayback`/`availableHDRModes`, web `(dynamic-range: high)` + a DV probe, Android `Display.HdrCapabilities` | folded into `hdr`/`dv` today |
| `learned_limits` | the web's `plurx_decode_limits` entries, verbatim (§4.6) | nothing — invisible today |

The legacy bit that **must** die with this document: `dv=1` with
`dvprofile` absent meaning "every profile" (`stream.rs:286`). In v2 there is
no blanket claim; a client that cannot enumerate profiles claims none.

### 4.2 Server translation: `DeviceCaps → DeviceProfile`

`DeviceProfile` (`playback/mod.rs:128-163`) grows, it is not replaced —
every existing test constructs it. Add:

```rust
// crates/plurx-core/src/playback/mod.rs — extend DeviceProfile
pub struct DeviceProfile {
    // ...existing fields unchanged: containers, video_codecs, audio_codecs,
    // max_height, max_bitrate, supports_hdr, supports_dolby_vision,
    // dolby_vision_profiles, video_max_heights, remux_dolby_vision,
    // supports_hdr10_transcode ...
    /// Per codec: the transfer functions the client presents. Empty map ⇒
    /// legacy caller; fall back to the boolean fields.
    pub presents: BTreeMap<String, BTreeSet<Transfer>>,   // Transfer = Sdr|Pq|Hlg
    /// Per codec, per profile: the height ceiling. Absent profile ⇒ codec ceiling.
    pub profile_max_heights: BTreeMap<(String, String), i64>,
    /// Learned client limits, keyed by decodeLimitIdentity (§4.6).
    pub learned_limits: Vec<LearnedLimit>,
}
```

One function, `DeviceProfile::from_caps_v2(&DeviceCaps)`, and the existing
`caps_profile(...)` path at `stream.rs:277-303` becomes
`DeviceCaps::from_legacy_query(&DecisionQuery)` followed by the **same**
`from_caps_v2`. That is the point: two wire shapes, one translation, one
profile. Pin it with a test that builds the same profile from
`CAPS_Q="vcodec=hevc,h264&hdr=1&dv=1&dvprofile=5,8&dvhls=1&hdr10t=1&maxheight=2160"`
and from the equivalent v2 JSON and asserts equality.

The legacy translation keeps today's semantics **exactly**, including the
blanket-claim rule at `stream.rs:286`, for one release; the deprecation is
enforced by the clients no longer sending it (M3 client half), not by the
server refusing it.

### 4.3 Structured DV columns (M2)

```sql
-- crates/plurx-core/src/store/sqlite/mod.rs migration list (append-only,
-- see the pattern at :158 "ALTER TABLE files ADD COLUMN hdr_format TEXT;")
-- and the hiqlite twin in store/hiqlite.rs — both backends, same statements.
ALTER TABLE files ADD COLUMN dv_profile        INTEGER;   -- 4,5,7,8,9,10 or NULL
ALTER TABLE files ADD COLUMN dv_level          INTEGER;
ALTER TABLE files ADD COLUMN dv_bl_compat_id   INTEGER;   -- 1/6 HDR10, 4 HLG, 2 SDR, 0 none
ALTER TABLE files ADD COLUMN dv_el_present     INTEGER;   -- 0/1
ALTER TABLE files ADD COLUMN dv_rpu_present    INTEGER;   -- 0/1
```

Source of truth is the ffprobe side data already stored as
`files` probe JSON (`store.get_file_probe_json`): the `DOVI configuration
record` entry carries `dv_profile`, `dv_level`, `rpu_present_flag`,
`el_present_flag`, `bl_present_flag`, `dv_bl_signal_compatibility_id`.
Populate the columns from the same parse `detect_hdr_format`
(`scan/probe.rs:289-325`) already does, and **backfill on boot from stored
probe JSON** — one pass over `files WHERE hdr='dolby_vision'`, no ffprobe
invocation. A file whose stored JSON has no DOVI record but whose codec tag
is `dvh1/dvhe` keeps `dv_profile NULL` and is reported in the boot log by
count (that is the §7.1 population; it needs a real re-probe, which is a
separate operator action, not a boot side-effect).

Then:

```rust
// playback/mod.rs:439 — becomes
pub fn dolby_vision_profile(file: &MediaFile) -> Option<u8> {
    file.dv_profile.map(|p| p as u8)
        .or_else(|| /* today's label parse, kept ONLY until the backfill has run once */)
}
fn has_compatible_dv_base(file: &MediaFile) -> bool {
    matches!(file.dv_bl_compat_id, Some(1 | 6 | 4))
        || /* label fallback, same lifetime */
}
```

The label `hdr_format` stays as the display string and is **derived** from
the columns at scan time from M2 on. MEL vs FEL is *not* an ffprobe fact; it
comes from the RPU and is recorded by M5 in `dv_sidecars.el_type`.

### 4.4 Grade negotiation (M4)

```rust
// crates/plurx-core/src/playback/mod.rs — replaces reencode_grade (:514-520)
pub struct RenderCaps {
    /// Boot-proven: the tonemapx passthrough graph runs on this ffmpeg
    /// (today's `has_dovi_rpu` + the tonemapx proof at transcode.rs:9134).
    pub hdr10_passthrough: bool,
    /// Boot-proven: the Profile 5 RPU renderer (transcode.rs:10059-10091).
    pub dolby_vision_p5_render: bool,
    /// Encoders that emit Main10 PQ with HDR SEI (x265 hdr10-opt, hevc_qsv
    /// main10, hevc_vaapi main10) — from the boot encoder probe.
    pub main10_encoders: BTreeSet<String>,
}

pub fn target_grade(file: &MediaFile, profile: &DeviceProfile, node: &RenderCaps)
    -> (OutputGrade, &'static str /* reason */)
{
    // 1. source grade: sdr | hdr10 | hlg | dolby_vision (from file.hdr)
    // 2. client presentation for the codec we will ENCODE (hevc): profile.presents["hevc"]
    // 3. node render capability
    // rule: the highest grade all three admit, walking down: dolby_vision is never
    //       an encode target (we never encode DV); hdr10 needs source∈{hdr10,hlg,
    //       dolby_vision-with-compatible-base, dolby_vision P5 via RPU render},
    //       client presents pq, node hdr10_passthrough && a main10 encoder;
    //       else sdr, with the reason naming WHICH of the three refused.
}
```

Reasons are fixed strings, one per refusal, because the badge and the
stats overlay show them: `"HDR kept: client presents PQ and this node
renders Main10"` · `"tone-mapped to SDR: client does not present PQ for
hevc"` · `"tone-mapped to SDR: this node's ffmpeg did not prove the HDR10
passthrough graph"` · `"tone-mapped to SDR: no Main10 encoder on this
node"`. Keep `the_grade_predicate_and_the_renderer_predicate_agree`
(`transcode.rs`, referenced at `playback/mod.rs:459-462`) green: the
daemon's renderer choice must read the same `target_grade`.

The HDR10 encode recipe for **non-DV** HDR10/HLG sources and for
**stripped** P7/P8 (base layer only): decode → (`dovi_rpu=strip=1` bsf when
DV) → scale in 10-bit → encode Main10 with `-pix_fmt p010le`
`-color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc` and the
mastering-display / content-light SEI copied from the probe
(`side_data_type: "Mastering display metadata"`, `"Content light level
metadata"`). **Do not** route these through `tonemapx` — the comment at
`crates/plurx-core/src/transcode/mod.rs:763-820` records that
`tonemapx` with `transfer=smpte2084` and the wrong pixel format aborts
ffmpeg (`vf_tonemapx.c:1475`); the P5 RPU path is the *only* tonemapx
consumer. Re-read that comment before writing the filter string, and add
the new chain to the same `pipeprobe` boot assertion so a node that cannot
run it reports `hdr10_passthrough=false` instead of aborting mid-film.

### 4.5 Session create re-derives (M3)

```rust
// crates/plurxd/src/http/hls.rs:520-590 CreateSession — add
pub caps: Option<DeviceCaps>,          // the same document as /decision
pub plan_ref: Option<String>,          // sha256 of (file_id, caps, force, selection); advisory
pub overrides: Option<CreateOverrides>, // { compatible_hdr_base: bool, force: "auto|original|transcode" }
// keep, but now ASSERTED not trusted:
pub preserve_dolby_vision: Option<bool>,
pub hdr10: Option<bool>,
```

Create runs `decide()` again from `caps` (legacy clients that send no
`caps` keep today's trust path, logged at `warn` with the client build so
the fleet can be watched for stragglers). If the client's `preserve_dolby_vision`
or `hdr10` echo disagrees with the re-derived plan **and** no named override
explains it, the create is refused with the typed error
`plan_mismatch` (reuse `ApiError::typed`, the pattern at `hls.rs:697-702`),
carrying both values. Named overrides: `compatible_hdr_base` is Apple's
existing `forceCompatibleHDRBase` retry (`PlayerController.swift:2373`) and
stays legitimate; `force` is the quality menu. Each override appends its
own reason to the session's reasons so the badge shows it.

`session_delivered_dynamic_range` (`hls.rs:664-680`) keeps reading the
built session, not the decision — MEDIA-BADGES-PLAN §3.2 — so a burn or a
forced rung still reports what it produced.

### 4.6 Learned limits (M6)

The web's entry shape today (`playback-policy.js:838-870`,
`index.html:7612-7620`): key = `decodeLimitIdentity(source)` (codec ·
video_profile · width · height · bit_depth · dynamic range · hdr_format ·
10 Mb/s bitrate bucket), value `{lost, secs, rate, label, at}`. It goes into
caps v2 verbatim. Server side: when `decide()` demotes a title because a
learned limit matched, the reason is the web's own string
(`"learned client-performance limit for <label>: lost N frames in Ss
(R/min)"`, `playback-policy.js:990`) so the badge and the admin's session
list say the same thing the browser would. The server does **not** store
them in M6 — it reports them. Storing per device is a later decision.

### 4.7 Fragment index identities (M1)

```sql
-- crates/plurx-core/src/store/fragindex.rs:24 — today
CREATE TABLE fragment_indexes ( file_id INTEGER PRIMARY KEY, ..., argv_fingerprint TEXT NOT NULL, ... )
-- M1: sidecar migration v7 in store/telemetry.rs (pattern at :440-480; the
-- "creating_indexes" subtlety there applies — evaluate guards BEFORE the batch)
CREATE TABLE fragment_indexes_v7 ( ...same columns..., PRIMARY KEY (file_id, argv_fingerprint) ) STRICT;
INSERT INTO fragment_indexes_v7 SELECT ... FROM fragment_indexes;
DROP TABLE fragment_indexes; ALTER TABLE fragment_indexes_v7 RENAME TO fragment_indexes;
```

`put_fragment_index` upserts `ON CONFLICT(file_id, argv_fingerprint)`;
`fragment_index(file_id, identity)` already selects by identity and stays
byte-compatible for callers; `forget_fragment_index(file_id)` drops all
rows for the file. The cluster v2 store is keyed by
`cluster_fragment_index_key(source_sha256, pipeline_sha256)`
(`store/fragment_index_cluster.rs:704-715`) and already admits several
pipelines per source — it needs the enqueue loop to emit both, nothing in
the schema.

```rust
// crates/plurxd/src/state.rs:1198 — becomes
async fn fragment_index_video_identities(store, file, have_dovi)
    -> Result<Vec<plurx_core::transcode::CopyVideoOptions>, StoreError>
{
    // non-DV: [from_probe(.., preserve=false)]
    // DV:     [from_probe(.., preserve=true)]  +  [from_probe(.., preserve=false)] when
    //         has_compatible_dv_base(file)   (a P5 has no strippable base; its
    //         non-preserved route is a transcode, which is not a copy identity)
}
```

All four callers (`:3424`, `:3614`, `:4002`, `:4301`) iterate the vector;
`INDEX_MAX_PER_PASS` (`state.rs:930-937`) counts **identities**, not
files, so a DV-heavy library does not double its pass time silently. The
browse badge (`browse.rs:352-370`) reports `indexed` only when **every**
identity for the file is present, `partial` when some are, else `pending`.

### 4.8 P7 → P8.1 sidecar (M5)

```sql
CREATE TABLE dv_sidecars (
    file_id        INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    path           TEXT NOT NULL,          -- under <runtime cache>/dv-sidecars/<file_id>.mkv
    source_size    INTEGER NOT NULL,       -- invalidation by mismatch, as fragment_indexes
    source_mtime   INTEGER NOT NULL,
    el_type        TEXT NOT NULL,          -- 'mel' | 'fel'  (from dovi_tool info on the RPU)
    bytes          INTEGER NOT NULL,
    built_at_ms    INTEGER NOT NULL,
    last_used_ms   INTEGER NOT NULL        -- LRU for the size cap
) STRICT;
```

Build recipe (the standard dovi_tool workflow, verified against its README
2026-08-29):

```bash
# 1. base layer + RPU as Annex B, enhancement layer dropped (NAL 63)
ffmpeg -nostdin -i "$SRC" -map 0:v:0 -c:v copy \
  -bsf:v hevc_mp4toannexb,filter_units=remove_types=63 -f hevc BL_RPU.hevc
# 2. rewrite every RPU to Profile 8.1 (mode 2). --discard is belt-and-braces
#    if step 1 ever runs without filter_units.
dovi_tool -m 2 convert --discard -i BL_RPU.hevc -o BL_RPU.p81.hevc
# 3. MEL/FEL for the ledger and the badge: `info` reads an RPU binary, not
#    HEVC, so extract first. The summary names the source profile with its
#    EL type ("Profile: 7 (MEL)" / "(FEL)") — verify the exact string on the
#    pinned version and parse defensively.
dovi_tool extract-rpu -i BL_RPU.hevc -o RPU.bin
dovi_tool info -i RPU.bin --summary
# 4. remux video-only; mkvmerge parses the RPU NALs in a raw Annex B stream
#    and writes the Profile 8 Dolby Vision configuration block — the step
#    ffmpeg cannot do. Pin MKVToolNix >= 68: earlier builds could miss the
#    RPU when the first access unit overran the 1 MiB probe buffer
#    (mkvtoolnix issue #3363, fixed 2022-07).
mkvmerge -o "$SIDECAR" BL_RPU.p81.hevc
# 5. probe the sidecar with the ordinary scanner: expect dv_profile=8,
#    dv_bl_compat_id=1|6, dv_el_present=0
```

Audio, subtitles and chapters are **not** copied into the sidecar; the
sidecar is a *video variant* of the same `files` row. At decision time,
`decide()` receives the source with its video facts swapped for the
sidecar's (`container=mkv`, `dv_profile=8`, `dv_el_present=0`) when the
client's `dv_profiles` contains 8 and the sidecar is current; the copy
session's ffmpeg gets `-i sidecar.mkv -i original.mkv -map 0:v -map 1:a…`.
`delivered_dynamic_range` returns `dolby_vision`; the badge reads
`DV P7 → DV P8` (new "converted" state in MEDIA-BADGES-PLAN §4, dimmed
source half like today's `→ HDR10`, with the tooltip "Profile 7 enhancement
layer discarded; MEL: near-lossless / FEL: EL detail lost").

Policy knobs (settings keys, `store/mod.rs:369-380` style):
`playback.dv_sidecar` (`off|lazy|eager`, default `lazy` — build on the
first DV-capable request for a P7 title, serve HDR10 base meanwhile with the
reason `"Dolby Vision Profile 8 variant is being prepared"`),
`playback.dv_sidecar_cap_gb` (LRU, default 200). A sidecar is roughly the
source size minus the EL (≈ 85–95 % of a disc remux) — Paul decides the cap.

---

## 5. Dependencies — what to add, and what each buys

Paul (2026-08-29): "I don't necessarily have a no external deps rule. If
there's something I can use to make this better, we should discuss it."
The options, with the recommendation marked:

| Dependency | Form | Licence | Buys | Cost / risk |
|---|---|---|---|---|
| **`dovi_tool`** (quietvoid) — **recommended, M5** | static binary in the Docker image (GitHub release asset, pinned version + sha256; or `cargo install dovi_tool --locked` in the builder stage) | MIT | the only battle-tested P7→P8.1 RPU converter; `info` gives MEL/FEL; the de-facto tool every DV library workflow uses | +~10 MB image; a boot probe (`dovi_tool --version`) so its absence degrades to "sidecars off", never a crash |
| **`mkvtoolnix`** (`mkvmerge`) — **recommended, M5** | `apt-get install mkvtoolnix` | GPL-2 (separate process, fine) | writes the Profile 8 DV configuration block from a raw HEVC stream, which ffmpeg's muxers cannot (verified 2026-08-29: `dovi_rpu` bsf has `strip`/`compression` only and copies the RPU profile header verbatim; `ff_dovi_configure_ext` refuses profiles 4/7) | +~30 MB image; pin ≥ 68 (raw-HEVC RPU detection landed after the MP4-only support in v57; the large-first-frame miss was fixed 2022-07) |
| `dolby_vision` crate (quietvoid) | Cargo dependency | MIT | in-process RPU parse/convert — the road to **streaming** P7→P8.1 with no sidecar disk cost | needs plurx to also write the `dvcC` box and the master `CODECS`; touches the copy seam that M1 is fixing; do it **after** M5 proves clients play the converted output |
| `libplacebo` (already optional in jellyfin-ffmpeg) | none new | LGPL | higher-quality tone-mapping on Vulkan GPUs; `ToneMap::Libplacebo` exists at `transcode.rs:3596` | unchanged; not on this plan's path |
| `mediainfo` | apt | BSD-2 | prettier DV strings | nothing ffprobe + dovi_tool don't give; **not recommended** |

Decision Paul is being asked to confirm (§9 Q1): add `dovi_tool` +
`mkvmerge` to the image now; take the `dolby_vision` crate as the M5
follow-on if sidecar disk cost turns out to matter more than the seam risk.

---

## 6. Non-goals and guardrails

- **Do not widen any client's `dv_profiles` to include 7.** No decoder Paul
  owns takes dual-layer; the failure is a black screen, not a downgrade. The
  one exception is an Android device whose MediaCodec explicitly declares
  `DolbyVisionProfileDvheDtb` (Shield-class boxes); gate on that constant and
  nothing else, and only after M5 exists so the server has a P8 fallback.
- **Do not change `decide()`'s ladder order or its reason strings** other
  than adding the new ones named in §4. Tests at `playback/mod.rs:1100-1500`
  pin the existing ones and the badge plan cites them.
- **Do not bump `SEGPLAN_VERSION` or change the packed row format** for M1.
  The composite key is a table change, not a blob change; a version bump
  would invalidate every index on the fleet for nothing.
- **Do not touch `exact_hls_context` (`hls.rs:2863-2932`) without a
  master-playlist assertion test.** It has already caused one AVPlayer
  `-12927` outage by rewriting `dvh1.05.06` into an HEVC-shaped identifier.
  M5's `DV P7 → DV P8` variant goes through it unchanged because the sidecar
  is a real P8 source.
- **Do not route non-DV HDR10 through `tonemapx`** (§4.4). It is the P5
  renderer and aborts on the wrong input.
- **Do not re-scan the library to fix profile-less labels.** M2 backfills
  from stored probe JSON; the residue that has no DOVI record needs a real
  re-probe, reported as a count for Paul to act on.
- **Do not make the sidecar per-session.** Once per file, LRU-capped, or
  the streaming-crate follow-on — never "convert on every play".
- **Do not merge to `main`.** One PR per milestone, CI green, Paul merges.
- **Do not hand-edit build-claim surfaces.** Client changes in M3/M6 bump
  via `make apple-build-bump`; Android `versionCode` + its README status line
  by hand together (project memory: five-surface contract).

---

## 7. Milestones — build in order, each to its acceptance check

### M0 — Measure before touching anything (no code)

Run against the live fleet (nuc4 is on `v0.2.7-502-g97a10ee` or later) and
paste the results into a `docs/PLAYBACK-CAPS-V2-M0.md` so every later
milestone argues from numbers:

```sql
-- library census: how much is P7 (designed) vs P5/P8 (should be lit)
SELECT hdr_format, COUNT(*) FROM files WHERE hdr='dolby_vision' GROUP BY 1;
-- unclaimable-by-anyone (E2 / opus §7.1)
SELECT COUNT(*) FROM files WHERE hdr='dolby_vision' AND hdr_format NOT LIKE '%Profile %';
```

```bash
# one request separates the P7 rule, the maxheight transcode, the index hole
curl -s "http://<node>/api/v1/files/<resident-evil-id>/decision?<CAPS_Q from the browser>" \
  -H "Authorization: Bearer $TOKEN" | jq '{preserve_dolby_vision,delivered_dynamic_range,vod_indexed,reasons}'
curl -s http://<node>/api/v1/system -H "Authorization: Bearer $TOKEN" | jq .dovi_rpu
grep -c 'temporary live-HLS recovery' <plurxd log>        # E1 in the wild
```

Browser console on the player page (Safari **and** Chrome, on the XDR):
`PLAY_CAPS` (`.dv .dvprofile .hdr .hdr10t .maxheight`) and
`localStorage.getItem("plurx_decode_limits")`.

**Acceptance:** the M0 doc exists with all five numbers and the browser
used for the original report named. If `maxheight === 1080` or a
`plurx_decode_limits` entry matches Resident Evil, say so at the top — it
changes what Paul was actually seeing.

### M1 — Index every reachable identity (server, E1)

Schema per §4.7; `fragment_index_video_identities`; four call sites;
browse badge tri-state; `/decision`'s `vod_indexed` (`stream.rs:1141-1145`)
already uses the real flag and stays. Cluster v2: enqueue one job per
identity. Add a `regressions.d` row (`validation/regressions.d/*.toml`, see
`validation/history.py`) pointing at the new test, since the commit subject
will match `ISSUE_RE`.

**Acceptance:** on a node with a P8 (HDR10-compatible) title, after one
index pass, `SELECT COUNT(*) FROM fragment_indexes WHERE file_id=<id>`
returns 2; `GET /decision` with Safari's caps returns
`vod_indexed:true` and a create returns a VOD session with **no**
"temporary live-HLS recovery" log line. A P5 title returns 1 row. A non-DV
title returns 1. `make unit` green; `make check` at the PR gate.

### M2 — DV facts as columns, label derived (server)

Migrations per §4.3 on both backends; scanner writes columns; boot backfill
from stored probe JSON with a summary log line (`dv backfill: N files
updated, M with no DOVI record`); `dolby_vision_profile` /
`has_compatible_dv_base` read columns first. `SourceSummary`
(`stream.rs:381-399`) gains `dv_profile`, `dv_el_present` so clients and the
stats overlay can show them.

**Acceptance:** the M0 census re-run shows the `NOT LIKE '%Profile %'`
count reduced to exactly the "no DOVI record" residue; a unit test proves a
row with `dv_profile=8` and a garbage label is claimable by a `{5,8}`
client; `hdr_format` for a freshly scanned P7 reads
`Dolby Vision · Profile 7 (HDR10-compatible)` byte-for-byte as today.

### M3 — Caps v2 + plan-by-reference (server + all three clients)

Server first (PR 3a): `DeviceCaps`, `from_caps_v2`, legacy translation,
`POST /decision`, create re-derivation with `plan_mismatch`, the equality
test in §4.2. Clients second (PR 3b–3d, one per client): build the v2
document from the same probes that build `CAPS_Q` today (web
`buildPlayCaps` `index.html:6440-6481`; Apple `Caps.swift:20-78`; Android
`CapsPolicy.kt`), POST it, send it again on create, stop sending the
legacy `dv=1`-without-`dvprofile` shape. Web keeps `CAPS_Q` generation for
the progressive `play_url` only.

**Acceptance:** server: `curl -X POST /decision` with the §4.1 example
returns the same `decision` as the GET with the equivalent `CAPS_Q`;
a create whose body says `preserve_dolby_vision:true` for a client whose
caps have `dv_profiles:[]` is refused `plan_mismatch`; Apple's
`compatible_hdr_base` override is accepted and its reason appears in the
session's `reasons`. Clients: each build's `/decision` request in the
server log carries `caps.v=2`; `tests/playback/web-policy.test.js` has a
case for the two-entry HEVC ladder replacing the min-of-rungs.

### M4 — Grade negotiation and the HDR10 rung (server, E3)

`RenderCaps` from the boot probes; `target_grade` per §4.4 replaces
`reencode_grade`; the non-DV HDR10 encode chain with its `pipeprobe`
assertion; `hdr10` create-body field honoured through the same function;
`the_grade_predicate_and_the_renderer_predicate_agree` updated. Measure the
new chain on nuc4 at 1080p and 2160p (QSV) the way D6 measured the P5
chain (`docs/PERF2-IMPLEMENTATION-HANDOFF.md` §D6) and record the numbers
in the M0 doc.

**Acceptance:** a plain HDR10 title forced to `force=transcode` on Safari
with `present:["pq"]` returns `delivered_dynamic_range:"hdr10"` and the
session's init/first segment probe shows `color_transfer=smpte2084`,
`pix_fmt=yuv420p10le` and mastering-display SEI; the same request from
Chrome on an SDR display returns `sdr` with the reason naming the client;
the same request on a node with `hdr10_passthrough=false` returns `sdr`
with the reason naming the node. Stripped P7 on an HDR-presenting Chrome
transcodes to **hdr10**.

### M5 — Profile 7 → Profile 8.1 sidecar (server + image)

Image: `dovi_tool` + `mkvmerge` pinned; boot probe. Table, recipe, policy
knobs, LRU per §4.8. `decide()` source-variant swap. Badge state
`DV P7 → DV P8` in the web, Apple and Android badge code (MEDIA-BADGES-PLAN
§4 table gains the row; fix the stale P7 row at ~line 340 in the same
commit). Settings UI: the three knobs under Playback.

**Acceptance:** for Resident Evil (P7) with `playback.dv_sidecar=lazy`: the
first Safari request returns `hdr10` with the "being prepared" reason; after
the build, `dv_sidecars` has the row with `el_type` set, ffprobe on the
sidecar reports `dv_profile=8, el_present_flag=0`, and the next Safari
request returns `delivered_dynamic_range:"dolby_vision"` over a VOD session
(M1 indexed the sidecar's preserved identity). Apple TV and a DV-capable
Android phone play it with the DV badge lit on the device (device check —
§8). Chrome on the same title still gets `hdr10` with the strip reason.

### M6 — Learned limits reported (web + server, E6)

Web includes `learned_limits` in caps v2; server matches by identity in
`decide()` and emits the web's own reason string; the player's diagnostics
row (`#dlrow`, `index.html:11129`) and the admin session list show it; the
learned-limit transcode now lands on M4's HDR10 rung when the client
presents PQ.

**Acceptance:** seed `plurx_decode_limits` in a browser with an entry
matching a 4K DV title; `/decision` from that browser returns
`method:"transcode"` with the learned-limit reason **from the server**, and
`delivered_dynamic_range:"hdr10"` on an HDR display. Clearing the entry
returns the title to remux.

---

## 8. Rollout — fleet, devices, status page

Server milestones ship through the ansible media playbook to every node
(project instruction), in milestone order; M1 and M2 carry sidecar/database
migrations, so deploy one node, watch the boot backfill and index-pass log
lines, then the rest. Client milestones (M3b–d, M5 badge, M6) go through the
`mobile_release` role to every reachable Apple and Android device (the floor
is the named roster, not a count — project memory).

Device verification that cannot be done from a shell — DV badge lit on Apple
TV / Android for the M5 sidecar, the `plan_mismatch` refusal never firing on
a real seek/reopen in M3 — is a checklist in the PR description for Paul to
run or delegate; write it as exact taps and expected on-screen text.

`docs/STATUS.html` gets one line per merged milestone with the node version
it landed in; build-number claims only through `make apple-build-bump`.

---

## 9. Open decisions for Paul

1. **Dependencies (§5):** add `dovi_tool` + `mkvmerge` to the image now?
   Recommended yes.
2. **Sidecar disk policy (§4.8):** `lazy` with a 200 GB LRU cap as the
   default, or `eager` for the whole P7 population once M0's census says how
   big that is?
3. **FEL sources:** convert them too (the base + RPU is still real DV, just
   without the EL detail) and label it, or skip FEL and serve HDR10?
   Recommended convert-and-label; the alternative is what happens today.
4. **`plan_mismatch` strictness (§4.5):** refuse, or warn-and-re-derive for
   one release while the three clients roll out? Recommended refuse only
   for clients that sent `caps.v=2`; legacy creates keep the trust path.
5. **Android Profile 7 (§6):** allow it when MediaCodec declares
   `DolbyVisionProfileDvheDtb`? Only matters if a Shield-class box joins the
   fleet.

---

## 10. Provenance

Findings by opus at `main` @ `3a056dc4` (2026-08-29); re-verified by fable
at `4ba8bb48` in an independent clone, with two corrections that shaped
this plan — ffmpeg cannot convert P7→P8 (the `dovi_rpu` bsf and
`ff_dovi_configure_ext` were read on FFmpeg master 2026-08-29), and
`fragment_indexes` is keyed by `file_id` alone, so "index both identities"
is a migration. The web learned-decode-limit path (E6) was found in the
re-verification. Nothing here was executed against the fleet; M0 exists so
that the first thing built is the measurement.
