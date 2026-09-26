# Dolby Vision strip trial probe — a file the filter cannot read is planned around, not discovered mid-play

**Status:** ready to build after one M0 evidence step (§7) · **Executes:**
a gap found comparing plurx's Dolby Vision handling with Silo's on
2026-09-26; not in the 2026-09-20 review · **Written:** 2026-09-26 against
`main` @ `e680849f` · **Reviewed:** adversarial review 2026-09-26, verdict
*reject as written*; 16 findings folded in, and the design changed because
of three of them — ledger in §9

Companion to [DV-DELIVERY-FINDINGS.md](DV-DELIVERY-FINDINGS.md) (why a
Dolby Vision title arrives as HDR10) and
[HEVC-COLOR-CORRUPTION-RCA-AND-FIX.md](HEVC-COLOR-CORRUPTION-RCA-AND-FIX.md)
(the parameter-set census whose shape this plan copies). Those answer "why
did this DV title degrade"; this one closes a case where it does not degrade
but fails: the file carries a malformed SEI that ffmpeg's `dovi_rpu` filter
cannot decompose, the strip is applied anyway, and the viewer gets a stall
and an error on every play of that title.

The evidence is Silo's (`silo-server/internal/playback/dovi_rpu_probe.go`,
read 2026-09-26): *"The filter does not fail cleanly: it rejects every
packet, and ffmpeg keeps going, emitting a pair of errors per frame — one
observed session produced 376,316 stderr lines before the process was
killed."* Their markers were `Failed to read unit 1 (type 39)` and
`Invalid SEI message: payload_size too large`. **Type 39 is `PREFIX_SEI`,
not the RPU.** What fails is CBS decomposition of a malformed SEI inside
`dovi_rpu`'s read pass; the RPU itself may be fine. That fact, caught in
review, is what turned this plan from "Broken → transcode" into "Broken →
strip a different way" (§3).

## 1. Objective

1. Before a Dolby Vision file is planned onto the `dovi_rpu` strip, plurx
   knows whether *this file* survives that filter, and a file that does not
   is stripped by unit type instead — the path the repo already has for a
   node without `dovi_rpu` — or, if that also fails, re-encoded, with a
   reason that names the file.
2. The verdict is learned once per file per ffmpeg build, stored where a
   rescan invalidates it, replicated to every node, and read by every argv
   builder through the one constructor they already share, so a veto cannot
   be undone by a site that reads the node capability directly (there are
   about a dozen of those today, §2.2).
3. The probe is a bounded, attributable child (≤ 6 s wall, ≤ 64 KiB
   stderr, `ChildWork::realtime`), visible in Activity → Processes like every
   child since P-02 M3. An inconclusive probe never changes a plan.
4. No new decision vocabulary: `DvHandling::Strip` stays `Strip`; what
   changes is *which* strip argv a file gets.

## 2. Contract today

Re-verify line numbers at build time; they are from `e680849f`.

### 2.1 Two strip chains already exist, chosen by a node-wide bit

`crates/plurx-core/src/transcode/mod.rs:368` `hevc_copy_bsf_for_copy_retaining(hdr, have_dovi_bsf, preserve_dolby_vision, convert_dolby_vision, retain_parameter_sets)`:

```rust
} else if hdr == Some("dolby_vision") && have_dovi_bsf {
    "dovi_rpu=strip=1,filter_units=remove_types=62-63"          // :381 (retain)
} else if hdr == Some("dolby_vision") {
    "filter_units=remove_types=62-63"                            // :383
}
…
if have_dovi_bsf {
    "dovi_rpu=strip=1,filter_units=remove_types=32-34|62-63"    // :395
} else {
    // Removes the layers but not the claim. See
    // `copy_leaves_a_stale_dolby_vision_record`: the caller has to
    // delete the record from the muxer's init afterwards, or this
    // filter reintroduces the exact stutter it exists to prevent.
    "filter_units=remove_types=32-34|62-63"                      // :401
}
```

So the **filterless chain is itself a strip**: NAL types 62 (RPU) and 63
(EL) are removed by type, the base layer is untouched, and what is left
behind is the DOVI side data, which movenc turns into a stale `dvcC`
(Profile 7, `el_present_flag=1`). `copy_leaves_a_stale_dolby_vision_record`
(`:438`) names that case, and the HLS copy path deletes the record from
the init it cuts: `copyseg.rs:702` `fmp4::remove_dolby_vision_record(&mut
init)`, `fragindex.rs:1316`, `fmp4.rs:2043`. The progressive `stream.mp4`
path (`http/stream.rs:3272-3318`) has **no** such repair: the moov ships
with the stale `dvcC`, which is the "Chrome refused what Safari played"
case `browse.rs`'s own comment describes.

The bit that picks the chain is `RenderCaps.dv_strippable`
(`plurx-core/src/playback/mod.rs:411`) ← `state.system.dovi_rpu`
(`http/stream.rs:792`) ← `ffmpeg::has_dovi_rpu()`, a `OnceCell` over
`ffmpeg -bsfs` (`ffmpeg.rs:2769`). Nothing about the file enters it.

```rust
// playback/mod.rs:1325
fn dv_handling(file, profile, node, target) -> DvHandling {
    if !is_dolby_vision(file) || profile.decodes_dolby_vision_as_copied(file) { None }
    else if dolby_vision_converts_to_p81(file, profile, node) { Convert }
    else if node.dv_strippable && has_compatible_dv_base(file) { Strip }
    else { Reencode(target) }
}
```

Convert precedes Strip and its chain never runs `dovi_rpu`
(`filter_units=remove_types=63`, `:377`/`:390`), so nothing here touches
Profile 7 → 8.1 conversion. Forced Original (`decide_forced`, `:1580`;
the `Force::Original` arm near `:1620`) sends any `dv != None` to `Remux`
with `preserve_dolby_vision=false`, and the builders render whichever chain
the bit selects.

### 2.2 Who reads the bit

Every argv builder reaches `have_dovi_bsf` through
`CopyVideoOptions::from_probe(source, probe_json, have_dovi_bsf, preserve)`
(`transcode/mod.rs:569`), which already reads the census memo out of
`probe_json` (`:578` `hevc_census::in_band_parameter_sets_vary`). The
callers that *supply* the bit, from `rg -n "dv_strippable\(\)|has_dovi_rpu\(\)|system\.dovi_rpu|have_dovi\b"`:

| Reader | Where | Granularity |
|---|---|---|
| Rolling / live copy HLS | `transcode/manager/start.rs:1197` `self.dv_strippable()` → `rolling_copy_video_options` `:1241` | per manager |
| Progressive `stream.mp4` | `http/stream.rs:2924` `have_dovi_bsf: state.system.dovi_rpu` → `progressive_hevc_copy_args_retaining` `:3272`, which takes the **raw bool** at `:3274` and calls `hevc_copy_bsf_for_client_retaining` `:3304`/`:3312` | per node |
| VOD copy generation | `vod/serve/create.rs:185` `has_dovi_rpu().await` → `copy_video_pipeline` `:193`; `vod/generation.rs:267` | per request |
| Fragment-index identities | `fragindex.rs:1263` `video_identities(file, probe_json, have_dovi, convert)`; `state.rs:2908` `fragment_index_requested_video_options` | per request |
| Index workers | `state.rs:7587`, `:8120`, `:8281`, `:9291`, and `ClusterFragmentIndexWorker.have_dovi` (`state.rs:1984`, consumed `:9647`) | **per pass**, applied to every file |
| Analysis candidates | `http/analysis.rs:793` | per request |
| `/decision` `vod_indexed`, admin badge | `stream.rs:2289`, `http/browse.rs:431` | per request |
| `RenderCaps` | `stream.rs:792` `render_caps(state)` (async; callers `stream.rs:2214`, `:2804`, `hls/preparation.rs:917`, `hls/create.rs:1386`) | per request |
| Live TV | plans its own command (`start.rs:217-236`), never reaches `:1197` | n/a |
| Offline package | always a transcode (`transcode/manager/plan.rs:1136`) | n/a |

The first draft counted six sites. The per-pass workers are why a per-file
answer cannot be threaded in at the call sites: it has to live where the
argv is built (§3.1).

### 2.3 What a Silo-shaped file does today

**Rolling copy.** `dovi_rpu` rejects every packet; production copy pipes run
`-loglevel error` with no `-xerror` (`copy_input_args`, `transcode/mod.rs:2160`),
so ffmpeg keeps going and produces no media. `PROGRESS_STALL` (10 s,
`transcode.rs:501`) or the 30 s `PREPUBLICATION_COPY_STARTUP_BUDGET`
(`playback_control.rs:80`) fires as `ProgressDeadline` / `StartupDeadline`;
neither is permanent (`is_permanent()`, `:4603`), so the control channel
answers `ControlAction::RetryResource { after_ms: NEXT_EXCHANGE_MS }`
(`:2044-2056`) and the client retries into the same verdict. Viewer:
10–30 s of spinner, the error surface, repeat.

**Progressive `stream.mp4`.** `remux()` (`http/stream.rs:3347`) answers
`200 video/mp4` right after spawn; stdout yields `Ok(0)`, the client gets an
empty body; stderr is only logged (`consume_remux_stderr`, `:3327`) — one
pair of lines per frame is the 376k-line log flood.

**VOD index build.** For HEVC the index pass is `-xerror -bsf:v
trace_headers,<chain>` (`transcode/mod.rs:2317-2327`). `trace_headers`
decomposes every unit, so on a malformed-SEI file **every** identity fails —
preserve and convert too — with `[trace_headers @ 0x…] Failed to read unit`,
not a `dovi_rpu` line. The outcome is `IndexFailed(IndexProcessFailed,
"index pipe exited {status}")` (`fragindex.rs:1919-1921`), not retried
(`content_analysis.rs:280`), "attention" disposition; `stderr_tail` is
4 KiB / 16 lines (`content_analysis.rs:15-16`), mostly header trace. Such a
file has no VOD today and this plan does not change that (§5).

Nothing anywhere classifies bitstream-filter stderr today.

### 2.4 The per-file memo pattern to copy

`crates/plurxd/src/hevc_census.rs` measures whether a file's in-band
parameter sets vary, on the first copy of that file, after the decision
(`stream.rs:2280-2285`): `probe_json_for_copy(store, file)` (`:60`) returns
early on `!needs_census` — which requires `probe_json.is_some()`
(`transcode/hevc_census.rs:144`) — or `recently_failed`; per-file
`IN_FLIGHT` async mutex; in-memory `FAILED` keyed `(id, size, mtime)` with
`RETRY_AFTER_FAILURE = 600 s`; budgets `SAMPLE_WALL_TIME = 5 s`,
`CENSUS_BUDGET = 6 s`, `SAMPLE_MAX_BYTES = 64 MiB`; each sample is
`plurx_core::process::bounded::output(ffmpeg_bin(), &args, wall, max_bytes,
ChildWork::realtime("HEVC parameter-set census"))` with `-map 0:V:0`
(`:186-188`, capital V so an attached cover picture is skipped); the record
is grafted into replicated `probe_json` under `PROBE_KEY =
"plurx_hevc_parameter_sets"` / `REVISION` (`:39-45`) through

```rust
// crates/plurx-core/src/store/mod.rs:3118
async fn merge_file_probe_hevc_parameter_sets(&self, file_id: i64, size: i64, mtime: i64, census_json: &str) -> Result<bool, StoreError>;
```

fenced on `(size, mtime)` and `probe_json IS NOT NULL` (`sqlite/media.rs:1889`,
`hiqlite_media.rs:3420`, `json_set`, last writer wins). No migration.

`bounded::output` (`plurx-core/src/process/bounded.rs:87`) returns
`Output { status, stdout, stderr }` with stderr kept up to the cap even on
non-zero exit, SIGKILLs the process group on timeout (`cfg(unix)`;
`kill_on_drop` elsewhere), and goes through `spawn_job_owned` →
`priority::register` (`process/mod.rs:45`), so it shows in Activity →
Processes. Do **not** use `ffmpeg::bounded_command_output_with_limits`
(`ffmpeg.rs:2407`): it discards stderr on non-zero exit (`:2438`), which is
the one thing this probe reads.

Verified during review with the container's ffmpeg 6.1 on a deliberately
corrupted HEVC MKV: `-f null -` with `-c:v copy` does run the bsf chain;
`-xerror` turns the first filter error into a non-zero exit (183/234
observed) while without it ffmpeg exits 0 after the flood; a missing file is
`No such file` + exit 254 with no filter line; a truncated file is `File
ended prematurely` + exit 0. The AVClass prefix is `[<name> @ 0x…]`, never
a bare `[<name>]`.

## 3. Design

### 3.1 Three-way availability, decided per file, applied in `from_probe`

```
 system.dovi_rpu ──┐
                   ▼
   dv_strip::stored(file, probe_json) ──▶ DvStripChain
        none / Filter ──▶ Filter     : dovi_rpu=strip=1,filter_units=…      (today's)
        UnitsOnly      ──▶ UnitsOnly : filter_units=remove_types=…|62-63    (today's filterless)
        Broken         ──▶ None      : no strip; Reencode
```

`CopyVideoOptions::from_probe` (`transcode/mod.rs:569`) is the one place
every argv builder already passes through with the file's `probe_json` in
hand. It gains one line: `have_dovi_bsf = have_dovi_bsf &&
dv_strip::stored(source, probe_json) != Some(Verdict::UnitsOnly)`. That
single downgrade makes rolling copy, VOD create, index identities,
`vod_indexed`, the badge, analysis and all four index workers render the
filterless chain for a `UnitsOnly` file, synchronously, with no new
plumbing — and `copy_leaves_a_stale_dolby_vision_record` then answers true
for it, so copyseg and fragindex delete the record exactly as they do on a
filterless node today. The strip identity's `argv_fingerprint` changes for
such a file; an existing index under the `dovi_rpu` fingerprint is
orphaned and a cluster request naming it hits the existing "no longer
produced for this source" refusal (`state.rs:2919-2925`) — correct, and
M3 counts those rows.

Residual raw-bool sites, switched by hand:

- `progressive_hevc_copy_args_retaining` (`stream.rs:3274`) and its two
  `hevc_copy_bsf_for_client_retaining` calls take the resolved chain, and
  — because progressive has no init repair (§2.1) — a `UnitsOnly` file sets
  `requires_hls` in `remux_requires_hls` (`stream.rs:678-684`; it has no
  `probe_json` today, the caller at `:2299` does, so add the parameter).
  The clause is guarded on `caps.transports` containing `"hls"` (otherwise
  the existing 409 fires) and applies even when `caps` is `None`, which
  today short-circuits to `false`. `requires_hls` is advisory: it is
  computed in `/decision` and honoured by the web player
  (`decode-tiers.js:887`); the `stream.mp4` handler (`stream.rs:2840-2924`)
  never re-checks it. So the handler also **refuses** a `UnitsOnly` DV
  progressive remux with the same 409 `hevc_copy_requires_hls` already
  uses — an old web build, a native progressive client or a hand-typed URL
  gets a typed refusal, never a stale `dvcC`. Cheaper and safer than
  teaching the progressive pump to rewrite a moov in flight.
- `RenderCaps.dv_strippable: bool` becomes `dv_strip: DvStripAvailability
  { Filter, UnitsOnly, None }` with `dv_strippable() -> bool` returning
  `!= None`, so every existing test keeps its meaning. `render_caps(state)`
  (already async, `stream.rs:792`) takes `&MediaFile` and `probe_json`; its
  four callers all have both.
- `dv_handling`'s Strip arm stays `node.dv_strippable() && has_compatible_dv_base(file)`.
  Its reason text gains two branches, chosen from `dv_strip`:
  `UnitsOnly` → *"Dolby Vision metadata removed by unit type for this
  device; ffmpeg's dovi_rpu filter cannot read this file's SEI; compatible
  HDR base kept"*; `None` on a node that has the filter → *"this file's
  Dolby Vision layers cannot be removed by either strip; re-encoding from
  the compatible HDR base"*.

### 3.2 The probe — two chains, one child each, same budget

```bash
# chain 1, the filter
ffmpeg -hide_banner -nostdin -v error -xerror \
  -ss <t> -t 2 -i <file> -map 0:V:0 -c:v copy \
  -bsf:v dovi_rpu=strip=1,filter_units=remove_types=62-63 -f null -
# chain 2, only if chain 1 is Broken
ffmpeg … -bsf:v filter_units=remove_types=62-63 -f null -
```

- `<t>` = `hevc_census::sample_points(file)[1]` (the 10 % point;
  `SAMPLE_FRACTIONS[0]` is 0.0, `hevc_census.rs:52`) falling back to `[0]`
  when the duration is unknown. One point; Silo's caveat stands and is not
  solved here (§5). On Windows the source is opened through the held path
  the progressive route uses (`windows_source_path`, `stream.rs:2905`).
- `-map 0:V:0`, capital, as the census does; with `0:v:0` a cover-art MKV
  yields a `mjpeg … not supported` line → Inconclusive → re-probe every
  600 s forever.
- Both chains together under **one 6 s wall budget**, run concurrently with
  the census (§3.4) — a DV file that will strip is a DV file that will
  remux, so the census is needed anyway and the start pays one budget, not
  two.
- `classify(status, stderr) -> Verdict`, a pure function over the whole
  buffer, not per line:

| exit | stderr matches `^\[dovi_rpu @ 0x[0-9a-f]+\] Failed to read unit` | Verdict |
|---|---|---|
| 0 | — | `Filter` |
| ≠ 0 | yes | `Broken` for chain 1 → run chain 2 |
| ≠ 0 | no | `Inconclusive` (I/O error, missing file, decoder the copy path never uses; a `[filter_units @` failure during chain 1 also lands here, and chain 2 is what settles it) |
| timeout | — | `Inconclusive` |

  Chain 2: exit 0 → `UnitsOnly`; non-zero with `^\[filter_units @` →
  `Broken`; otherwise `Inconclusive`. **The regexes are constants tested
  against lines captured from the real binary in M0, not against Silo's
  prose** — the first draft's bare `[dovi_rpu]` would never have matched
  and the feature would have silently done nothing.
- `Filter`, `UnitsOnly`, `Broken` are recorded; `Inconclusive` goes to
  the in-memory `FAILED` map for `RETRY_AFTER_FAILURE` and the plan is
  unchanged — "we did not find out" must not move a strip that most Profile
  7 sources need.

### 3.3 Where the verdict lives

`probe_json` graft under `PROBE_KEY = "plurx_dv_strip"`, keyed **by ffmpeg
build digest** so a mixed-build fleet never ping-pongs (two nodes
overwriting one record and each treating the other's as absent):

```json
{"plurx_dv_strip":{"revision":1,"by_engine":{"<digest>":{"verdict":"units_only","sample_seconds":412.0,"marker":"Failed to read unit 1 (type 39)","probed_at_ms":1790400000000}}}}
```

The digest is `crate::ffmpeg::fragment_index_engine_digest()`
(`ffmpeg.rs:2059`) — already computed once per daemon over the executable
and its dependency reports, already the fragment-index cache key. Not
`decode_facts::build_digest` (`decode_facts.rs:868-877`), which hashes the
whole binary behind held-file checks and is heavier than this needs.

Store: one generic method rather than a second census-shaped one,

```rust
async fn merge_file_probe_json(&self, file_id: i64, size: i64, mtime: i64, key: &str, record_json: &str) -> Result<bool, StoreError>;
```

with the census's exact fence (`(size, mtime)`, `probe_json IS NOT NULL`),
implemented beside `sqlite/media.rs:1889` and `hiqlite_media.rs:3420`;
`merge_file_probe_hevc_parameter_sets` becomes a call to it; one
`store_contract.rs` case. `needs_probe` requires `probe_json.is_some()` like
`needs_census`, or a never-probed file pays the budget on every start.

For the Developer row: `list_files_with_probe_verdict(key, verdict, limit)`
on both backends (the only JSON extracts today are `$.chapters`,
`sqlite/media.rs:2153`, `hiqlite_media.rs:3395`), `LIMIT 5`, result cached
in the readiness builder for 60 s.

### 3.4 When the probe runs

1. **At decision time, once per file, only when the plan would strip.**
   The handler builds the `DeviceProfile` first, then asks a new pure
   `playback::would_strip(file, profile, node_has_filter) -> bool` (the
   `dv_handling` predicate up to the Strip arm: DV, not decoded as copied,
   not convertible, compatible base). Only then does `dv_strip::ensure(store,
   file, probe_json)` run — concurrently with `probe_json_for_copy`, under
   one 6 s budget — before `decide`. Gating on the profile matters: without
   it every DV-capable client and every convertible Profile 7 title would
   pay the probe for a strip that never happens.
2. **Not in the index build.** The first draft wanted the index pass's
   `-xerror` failure "for free"; §2.3 shows that pass fails on
   `trace_headers`, not `dovi_rpu`, and cannot tell "strip broken" from
   "file un-indexable". No new `IndexFailureCode` (it is a stored wire
   vocabulary, `content_analysis.rs:151-178`; `encode_bounded` rejects
   unknown codes).
3. **Not at scan.** Probing every DV file in the library at up to 6 s
   each is hours of ffmpeg over NFS for a question most of them will never
   be asked. Decision-time is where the answer is needed, once.

Budget honesty: the web start has one absolute 20 s lifetime covering
`/decision` + create + first segment (`web/player/measurements.js:71-100`);
Apple's is 180 s (`PlurxAPI.swift:72`). Probe and census sharing one 6 s
budget keeps the first DV start inside the web's 20 s with the same margin
it has today. HLS create's `render_caps` (`create.rs:1386`) runs before
`OWNER_ASSIGNMENT_DEADLINE` (3 s) is armed, so that deadline is unaffected.

### 3.5 Attribution

- The probe children appear in Activity → Processes as "Dolby Vision strip
  probe" while they run (§2.4).
- Developer tab (`http/developer.rs`, builder `readiness` at `:107`):
  switchless item `dolby_vision_strip_probe` (`enabled: None, setting:
  None`, the `source_probe_comparison` precedent at `:622-625`) with
  requirements `ffmpeg_dovi_rpu` (Met when `system.dovi_rpu`; evidence the
  engine digest), `verdicts` (evidence: counts per verdict since boot) and
  `units_only_files` (evidence: up to five `file_id: title` from §3.3's
  query). Gates nothing.
- `/metrics`: `plurx_dv_strip_probe_total{verdict}`.
- `/decision` carries the per-file reason text (§3.1). No new serialized
  field (`Decision` has prose reasons, `playback/mod.rs:707`).

## 4. Decisions taken on Paul's behalf — his to overturn

1. **Broken means "strip by unit type", not "transcode".** The filterless
   chain plus record removal is an existing, tested strip; throwing a 4K
   HDR10 remux away for a 1080p SDR transcode because one filter cannot
   parse one SEI is the wrong trade. Trade-off: this rests on the
   hypothesis that `filter_units` (which does not decompose units) passes
   where `dovi_rpu` (which does) fails. **M0 tests the hypothesis on a real
   file before M2 is built**; if it is false the fallback is `Reencode` and
   §3.1's `UnitsOnly` arm is simply never recorded.
2. **Downgrade in `from_probe`, not at the call sites.** §2.2's per-pass
   workers make call-site threading impossible without restructuring them;
   `from_probe` already has the memo in hand.
3. **`UnitsOnly` files require HLS on the web.** Progressive has no init
   repair; routing beats building one.
4. **Inline at decision time, gated on `would_strip`, sharing the
   census's 6 s.** Decision 3 of the first draft ("inline, not at scan")
   survives; the gating and the shared budget are review findings.
5. **Fail open.** An inconclusive probe changes nothing.

## 5. Non-goals

- **Mid-stream detection.** A file whose SEI is fine at the 10 % point and
  malformed later still fails mid-play. The fix is a bounded classifier on
  the producer's stderr ("N filter errors with no output in T seconds →
  permanent `Unsupported`"), which belongs to the producer-decision work in
  [playback-control/](../playback-control/). This plan makes the case
  rare, not impossible.
- **Making such files indexable.** The index pass's `trace_headers` fails
  on the same SEI (§2.3). A `UnitsOnly` file plays via rolling copy and
  never gets a VOD index until that pass stops decomposing what it does
  not need. Separate change; named here so nobody reads "no index" as this
  plan's bug.
- **Capping `consume_remux_stderr`.** The log flood is real and a two-line
  fix; separate PR.
- **Fixing the SEI or the RPU.** No `dovi_tool`, no rewrite.
- **Forced Original on a `Broken` file.** `from_probe` downgrades only for
  `UnitsOnly`; a file both chains reject, under `Force::Original`, still
  renders the `dovi_rpu` chain and stalls as today. Both chains fail on it
  anyway, so there is no argv that helps; the honest fix is for the Original
  menu to show the per-file reason and offer the transcode, which is a UI
  change outside this plan. Recorded so "planned around" is not read as
  "cannot fail".
- **Touching Convert, Live TV, or offline packages** (§2.1, §2.2).
- **A reason-code vocabulary on `Decision`.**

## 6. Tests

- `classify`: one test per regex against **captured** stderr from M0 (the
  `dovi_rpu` line from ffmpeg 7.1, the `filter_units` line, the mux-thread
  `Error applying bitstream filters` line), plus negatives: `No such file
  or directory` → Inconclusive; `File ended prematurely` + exit 0 → Filter
  (a truncated file that parsed is fine); a `[trace_headers @` line →
  Inconclusive (it is not our filter).
- `from_probe`: a `UnitsOnly` memo yields the filterless chain and
  `copy_leaves_a_stale_dolby_vision_record == true`; a `Filter` memo, a
  `Broken` memo and no memo yield today's argv byte-for-byte (a `Broken`
  memo is a decision fact, not an argv fact). A memo under a different
  engine digest is absent.
- Decision: with `UnitsOnly`, `dv_handling` is `Strip` with the unit-type
  reason and `remux_requires_hls` is true for the web caps; with `Broken`
  on a filter node, `Reencode` with the per-file reason; `decide_forced(
  Original)` remuxes with the filterless chain for `UnitsOnly`. Every
  existing DV test unchanged
  (`dolby_vision_is_not_handed_to_a_browser_that_cannot_decode_it`,
  `forced_original_strips_dolby_vision_rather_than_handing_over_the_raw_file`,
  `a_strip_overtaken_by_a_demotion_stops_claiming_the_base_was_kept`,
  `a_dolby_vision_plan_reports_the_grade_it_actually_delivers`).
- Probe runner takes the binary path as an argument (no `PLURX_FFMPEG`
  env mutation — `ffmpeg_bin()` re-reads it per call (`ffmpeg.rs:73`) but
  parallel tests share the process environment). Shims: exits 1 printing
  the captured `dovi_rpu` line → chain 2 runs → exit 0 → `UnitsOnly`
  recorded; both shims fail → `Broken`; shim sleeps past the wall →
  `Inconclusive`, nothing recorded, one `FAILED` entry.
- Store contract: `merge_file_probe_json` fenced on `(size, mtime)`,
  refuses `probe_json IS NULL`, round-trips on both stores; the census
  method delegating to it leaves `test_hevc_census_*` green.
- There is no real DV fixture in the tree (`testfixtures::with_dolby_vision_rpus`
  appends an RPU after the muxer where no bsf sees it; `scan::write_fake_video`
  is `b"not really video"`), so the shims are the unit; the real bytes are
  M0 and M3.

## 7. Milestones

### M0 — one afternoon of evidence, before any code

On a fleet node with jellyfin-ffmpeg7 (has `dovi_rpu`), with the GPT
session that has the media:

1. Make a malformed-SEI Profile 7 sample: take 30 s of a real P7 title and
   corrupt one prefix-SEI payload size (`dovi_tool` or a hex edit at a NAL
   type 39 offset), or use the first title whose rolling copy ever logged
   `Failed to read unit`.
2. Run chain 1 and chain 2 from §3.2 on it, `-v error -xerror`, and capture
   stderr and exit codes verbatim into `tests/fixtures/dv_strip/`. Run both
   on a healthy P7 title too.
3. Play the chain-2 output through the copy segmenter path (a rolling copy
   with `have_dovi_bsf=false` forced) in Chrome and Safari; confirm the init
   has no `dvcC` and the picture is HDR10.
- Acceptance: the four captures exist; the answer to Decision 1's
  hypothesis is written at the top of this section (holds / does not). If
  it does not hold, §3.1's `UnitsOnly` collapses to `Broken → Reencode`
  and M1/M2 shrink accordingly.

### M1 — classifier, memo, chain resolution (core + store)

`plurx-core/src/transcode/dv_strip.rs` (`PROBE_KEY`, `REVISION`, `Verdict`,
`classify`, `stored`, `needs_probe`, `chain_for`), the `from_probe`
downgrade, `DvStripAvailability` on `RenderCaps` with the two reason
branches, `would_strip`, `merge_file_probe_json` on both backends with the
contract case and the census delegating to it. Acceptance: `make unit` for
core and the store contract green; §6's core tests.

### M2 — the probe and the residual sites (plurxd)

`plurxd/src/dv_strip.rs` (`ensure`, `IN_FLIGHT`, `FAILED`, the two bounded
children under one budget, concurrent with the census), the `/decision`
and create handlers gated on `would_strip`, `render_caps` taking the file,
the progressive raw-bool sites and `requires_hls`, the Developer item,
`list_files_with_probe_verdict`, `/metrics`. Acceptance: fast lane green;
the shim tests; "Dolby Vision strip probe" visible in Activity → Processes
during a first DV start (screenshot in the PR); a second start of the same
title runs no probe (log shows one).

### M3 — the fleet

With the M0 sample copied into a library and a healthy P7 title:
the healthy title records `Filter` and plays exactly as before; the sample
records `UnitsOnly`, the first start goes straight to a working HDR10 copy
with the unit-type reason in the overlay and no 10 s stall, on Chrome
(HLS, because `requires_hls`), Safari and Android; the Developer row lists
it; the count of orphaned strip-identity index rows is recorded. If the
library holds no such file and M0's sample is the only one, say so — that
is a result.

## 8. Open questions for Paul

1. Whether a `UnitsOnly` verdict should surface on the item-page badge
   (today it follows `has_dovi_rpu()` directly, `browse.rs:431`; after M2 it
   follows `from_probe` and reads as a filterless node would).
2. Whether the index pass should stop prefixing `trace_headers` to
   identities that do not need it (§5, second bullet) — that would give
   `UnitsOnly` files a VOD index and is a separate small plan.

## 9. Review log

Adversarial review 2026-09-26 (Claude, ran ffmpeg 6.1 in the container
against a corrupted HEVC MKV to check the marker format and `-xerror`
behaviour; verdict *reject as written*). Dispositions:

| # | Finding | Disposition |
|---|---|---|
| 1 | `[dovi_rpu]` marker never matches; AVClass prefix is `[dovi_rpu @ 0x…]`; per-line "and" clause wrong | **Accepted.** Regex over the whole buffer, constants tested against M0 captures, §3.2 |
| 2 | Filterless Original is not "RPU left in band, no dvcC": `filter_units` removes 62/63 and leaves a stale `dvcC`; copyseg/fragindex repair it, progressive does not | **Accepted.** §2.1 rewritten; `requires_hls` for `UnitsOnly` on the web (Decision 3) |
| 3 | Type 39 is SEI, not RPU; `filter_units` likely passes where `dovi_rpu` fails → a Broken file can be stripped, not transcoded; index arm keys on `trace_headers` | **Accepted, decisive.** Design pivot to three-way availability (§3.1); index arm dropped (§3.4); M0 tests the hypothesis first |
| 4 | Site inventory undercounted; per-pass workers; `fragment_index_cluster.rs:951` mis-cited | **Accepted.** §2.2 rebuilt from `rg` |
| 5 | Put the downgrade in `CopyVideoOptions::from_probe` | **Accepted.** §3.1 |
| 6 | `render_caps` has no profile; probe would fire for DV-capable and convertible cases | **Accepted.** `would_strip` gate, §3.4 |
| 7 | Probe + census budgets additive → 12 s vs web's 20 s | **Accepted.** One shared 6 s budget, concurrent |
| 8 | `sample_points[0]` is 0; `-map 0:v:0` catches cover art | **Accepted.** `[1]`, `0:V:0` |
| 9 | Use `fragment_index_engine_digest()`; key the record by digest to avoid fleet ping-pong | **Accepted.** §3.3 |
| 10 | `needs_probe` must require `probe_json.is_some()` | **Accepted.** §3.3 |
| 11 | New `IndexFailureCode` is a wire change; `stderr_tail` too small | **Accepted.** Index arm removed |
| 12 | Identity churn / orphaned index rows | **Accepted.** §3.1, M3 counts them |
| 13 | Broken-files query needs a Store method, `LIMIT`, cache | **Accepted.** §3.3 |
| 14 | No env mutation in tests | **Accepted.** Runner takes the binary path |
| 15 | Citation errors | **Accepted.** Corrected |
| 16 | Convert never runs `dovi_rpu`; Live TV separate; Windows held path | **Accepted.** §2.1, §2.2, §3.2 |

Round 2 (same reviewer): *approve with changes* — `requires_hls` is advisory, so the progressive handler must refuse a `UnitsOnly` DV remux (§3.1); forced Original on `Broken` still stalls and must be said (§5); `remux_requires_hls` needs the `probe_json` parameter and the `caps` guard (§3.1); `decide_forced` citation; chain-1 `filter_units` failure routing (§3.2). All applied.
