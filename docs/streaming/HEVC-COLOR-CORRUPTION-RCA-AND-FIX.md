# HEVC color corruption — why copied video turns pink and green

**Status:** root cause reproduced; containment implemented; final review and CI pending; not deployed ·
**Written:** 2026-09-25 · **Code reviewed:** `bafeb08766ce057634f3fab0850cdd9e03507a98`

Companion to [PLAYBACK.md](../PLAYBACK.md) and the earlier
[4K stutter investigation](STUTTER-4K.md). This document explains the
UNABOMBER color failure, the evidence separating it from browser rendering,
and the server changes needed to prevent it. The independent adversarial
review is recorded in
[HEVC-COLOR-CORRUPTION-REVIEW.md](HEVC-COLOR-CORRUPTION-REVIEW.md).

Read §2 before implementing §5. Preserving headers in an experimental
Matroska remux proves the cause; it does not prove that retaining them in
an `hvc1` HLS stream is a compatible production fix.

## 1. Finding — the copy path discards necessary decoder updates

Plurx's HEVC copy filter deletes VPS, SPS and PPS NAL units (types 32–34)
before packaging video for the player. It assumes the decoder configuration
in the file's initial `hvcC` remains sufficient. This movie supplies a
different in-band PPS, including different chroma quantization offsets.
Removing parameter-set updates produces pink/green patches even though video
samples are copied rather than re-encoded. The experiment isolates deletion
of types 32–34; it does not independently isolate each changed PPS field.

The inspected initial PPS declares `pps_cb_qp_offset = -6` and
`pps_cr_qp_offset = -8`; the in-band PPS at the tested seek declares `-1`
and `-2`. Both PPS records identify PPS 0 and reference SPS 0; the inspected
slice headers reference PPS 0. VPS and SPS fields compared in this inspection
did not differ. The chroma-offset differences are consistent with the color
failure, but individual PPS fields were not independently patched and tested.
The short inspection establishes a conflicting PPS at that seek; it does
not establish every configuration transition throughout the movie.

**Confidence:** high for the sampled corruption mechanism. A controlled
header-stripping experiment reproduces the defect outside both browsers,
and preserving those headers restores byte-identical decoded pixels at the
matched source frame. No production repair has been tested.

**Correction to the initial diagnosis:** comparing the original file's
software decode with Safari's screenshot implicated the playback path but
did not isolate Safari. Chrome subsequently showed a clearer failure.
Decoding Chrome's actual delivered segment reproduced the cast outside the
browser. The cause is server-side bitstream normalization; neither the
source badge nor `Encoder: copy` guarantees unchanged decoded pixels.

## 2. Evidence — source, delivered segment and controlled remux

### 2.1 What was observed on the live session

| Fact | Observed value |
|---|---|
| Title / item | UNABOMBER (2026), item `4190300654547191974` |
| File | `UNABOMBER (2026) [WEB-DL 2160p].mkv` |
| Source size / mtime epoch | `10558879147` bytes / `1790348718` seconds |
| Source video | HEVC, 3840×2160, `yuv420p10le`, BT.2020 nonconstant luminance, PQ |
| Dolby Vision record | Profile 8, level 6, RPU present, no EL, base compatibility ID 1 |
| Safari report | Dolby Vision, hardware decoder, server encoder `copy` |
| Chrome report | HDR10 with DV removed, `hvc1.2.4.L150.90`, hardware decoder |
| Server FFmpeg | `8.1.2-Jellyfin`, `/usr/lib/jellyfin-ffmpeg/ffmpeg` |
| Running image revision label | `1d68af6ebcd9322104bcea56f8322f527b2900f8` |
| Running image digest | `sha256:1ad108bec76f534728c8948d1af15d03e7a7d2d0ca26f6acb449b192a9b7c388` |

The deployment revision label differs from the checkout reviewed for the
proposal. The runtime command and experiments below are direct deployment
evidence; source references in §3 describe the inspected checkout. Recheck
both revisions when implementing. Size and mtime identify this observation,
not a cryptographic fingerprint of the entire 10.6 GB source.

The actual Chrome copy process included:

```text
-noaccurate_seek -ss 87.408
-c:v copy -tag:v hvc1
-bsf:v dovi_rpu=strip=1,filter_units=remove_types=32-34|62-63
-c:a aac -b:a 320k
-avoid_negative_ts make_zero
-movflags frag_keyframe+empty_moov+default_base_moof+delay_moov
-use_editlist 0 -f mp4 pipe:1
```

The delivered init/segment probe retained BT.2020/PQ, 10-bit HEVC and HDR10
static metadata; it did not retain a Dolby Vision configuration record.
The image from `init.mp4` plus `seg00001.m4s` reproduced the colored wall
patches with independent software decoding and SDR tone mapping.

### 2.2 The controlled experiment isolates types 32–34

Both experimental remuxes used the original file, the same seek, FFmpeg
binary, decoder and Matroska transport. Both removed Dolby Vision. Only
removal of types 32–34 differed. The matched source frame is at 88.792 s;
the seek lands on the source keyframe at 84.458 s, making decoded frame 104
the corresponding comparison frame at 24 frames/s. Rounded millisecond
timestamps differ from the exact rational frame grid.

| Input / transformation | Decoded frame MD5 | Visual result |
|---|---|---|
| Original source at 88.792 s | `77a4196afbdcd4a94e4fdbe16315a788` | Neutral wall |
| DV removed, types 32–34 preserved | `77a4196afbdcd4a94e4fdbe16315a788` | Matches source |
| DV and types 32–34 removed | `307eadd182d1194dd55b1e52a5d6fe17` | Pink/green patches |

These are **decoded raw-frame** hashes, before scaling or tone mapping:
3840×2160, 24,883,200 bytes per frame. They are not encoded-packet hashes
or screenshot comparisons. The delivered Chrome segment was visually
compared; the table's hashes belong to the controlled remux experiment.
The reference matches the preserved case exactly, while the stripped case
differs. This avoids treating different HDR display mappings as corruption.

The earlier software DV-on/DV-off stills also looked normal. That was useful
for triage but is weaker evidence than the matched raw-frame experiment.
It does not certify all Dolby Vision rendering behavior or all source frames.

### 2.3 Reproduce without changing the movie or the player

Run inside an environment with the same Jellyfin FFmpeg and read-only access
to the source. Set `movie` to that environment's path. The bounded copy runs
below write only pipes; the decoder drains them to EOF. Early experiments
stopped the decoder after one frame and caused expected upstream broken
pipes. Use the drained form below for clean exit-status verification.

```bash
set -o pipefail
ff=/usr/lib/jellyfin-ffmpeg/ffmpeg
movie='/20t/movies/UNABOMBER (2026)/UNABOMBER (2026) [WEB-DL 2160p].mkv'

# Reference: one unscaled, untone-mapped decoded source frame.
"$ff" -v error -threads 2 -ss 88.792 -i "$movie" \
  -map 0:v:0 -frames:v 1 -f framemd5 pipe:1

# A/B: the decoder reads the full bounded remux; only frame 104 is hashed.
for remove in '62-63' '32-34|62-63'; do
  "$ff" -v error -noaccurate_seek -ss 87.408 -i "$movie" \
    -map 0:v:0 -an -t 6 -c:v copy \
    -bsf:v "dovi_rpu=strip=1,filter_units=remove_types=$remove" \
    -f matroska pipe:1 |
  "$ff" -v error -threads 2 -i pipe:0 \
    -vf 'select=eq(n\,104)' -fps_mode vfr -f framemd5 pipe:1
done

# Inspect initial and in-band PPS values in the first packet at the seek.
"$ff" -hide_banner -loglevel info -noaccurate_seek -ss 87.408 \
  -i "$movie" -map 0:v:0 -frames:v 1 -c:v copy \
  -bsf:v trace_headers -f null - 2>&1 |
  rg 'pps_pic_parameter_set_id|pps_seq_parameter_set_id|slice_pic_parameter_set_id|pps_cb_qp_offset|pps_cr_qp_offset'
```

**How to read it:** compare the hash and byte-size columns, not the rebased
PTS columns. The first remux must match the reference; the second reproduces
the defect. Run only these short intervals for diagnosis. A full-file
correctness proof is a separate analysis operation with resource bounds.

## 3. Code path — the existing checks cannot establish safety

Re-verify line numbers at implementation time. These anchors describe the
checkout revision in the status header.

| Site | Current behavior and consequence |
|---|---|
| [`hevc_copy_bsf_for_copy`](../../crates/plurx-core/src/transcode/mod.rs), around line 325 | Deletes types 32–34 in preserved-DV, stripped-DV and ordinary HEVC copy paths. |
| [`hevc_parameter_set_promotion_required`](../../crates/plurx-core/src/transcode/mod.rs), around line 394 | Uses the special 23-byte empty `hvcC` case; a populated but conflicting configuration does not qualify. |
| [`copy_video_args`](../../crates/plurx-core/src/transcode/mod.rs), around line 1859 | Shared argument builder applies the destructive filter before downstream inspection. |
| [`PromotionInputs::from_fragment`](../../crates/plurx-core/src/fmp4.rs), around line 917 | Examines the first video sample of a fragment, not every access unit. |
| [Index inspection](../../crates/plurxd/src/fragindex.rs), around line 931 | Compares promotion inputs only at clean fragments of filtered output. Removed updates are invisible. Equality also includes HDR10 SEI, so the field is not solely a source-parameter-set proof. |
| [`promote_hevc_parameter_sets_from`](../../crates/plurx-core/src/fmp4.rs), around line 1607 | Fills missing NAL-type arrays; skips arrays already present. It cannot replace a conflicting PPS definition. |
| [Rolling init promotion](../../crates/plurxd/src/copyseg.rs), around line 733 | Runs on the first fragment only. Completeness validation does not establish continued correctness. |
| [VOD admission](../../crates/plurxd/src/vodserve.rs), around line 3007 | Rejects `parameter_sets_constant == false`, but this is based on the incomplete observation described above. |
| [Rolling recovery](../../crates/plurxd/src/transcode.rs), around line 19019 | `vod_source_unsupported` can recover through rolling copy with the same stripping. Explicit Live presentation also needs a gate. |

The old argument that stripping is harmless was supported by a
repeat-headers fixture whose definitions did not change. That proof does
not generalize to conflicting PPS definitions. The existing
`a_film_whose_clean_starts_disagree_is_not_vod_presentable` test in
[fragindex.rs](../../crates/plurxd/src/fragindex.rs) constructs differing
promotion values but does not feed an encoded changing-PPS stream through
the production analyzer.

## 4. Scope — fix decoder correctness without rewriting the library

This is server work covering HEVC copy admission, source analysis,
configuration construction, fallback and artifact identity. It must cover
preserved Dolby Vision as well as HDR10 copies because both remove headers.

Non-goals:

- Do not replace or permanently convert the user's source file. The source
  decodes correctly in the controlled test.
- Do not disable browser acceleration or change display color settings.
  The corrupted segment reproduces independently of either browser.
- Do not remove header filtering globally while continuing to label every
  output `hvc1`/`dvh1`. That changes the decoder-configuration contract and
  can reintroduce the compatibility problem documented in the stutter work.
- Do not silently transcode Dolby Vision into SDR to hide this failure.
  Any fallback must obey the existing delivered-grade and user-choice rules.
- Do not implement multi-configuration HLS as part of the first containment
  patch. Per-epoch init segments, discontinuities and client handling need a
  separate design and qualification.

## 5. Proposed fix — prove the decoder configuration before stripping

### 5.1 Observe original headers and persist an explicit proof

Introduce a source-analysis result distinct from the current
`parameter_sets_constant` boolean. Proposed names below are a contract to
implement, not existing APIs:

```text
HevcConfigurationProof
  source identity + selected video track + analyzer revision
  state: unknown | constant_verified | unsupported
  canonical configuration digest and bytes, when verified
  reason: changing_definition | incomplete | malformed | analysis_pending
```

The analyzer must see original parameter-set NAL units **before** a filter
or muxer removes them. Seed its effective decoder state from source `hvcC`,
then consume every access unit in decode order. Parse parameter-set IDs and
references; distinguish repeated identical definitions from a changed
definition with the same ID. Require one supported decoder/sample-entry
configuration across **all reachable active sets**: immutable definitions
under different IDs can still switch resolution, bit depth, profile, chroma
format or color signaling. Merely placing all arrays in `hvcC` does not prove
that one immutable sample entry describes those switches. First containment
should conservatively reject active multi-ID/configuration switches unless
the specific case has a tested compatibility proof.

Missing references, parsing limits, truncated
input, cancellation and partial scans do not produce a verified result.

The first implementation should conservatively reject conflicting
definitions rather than guess whether a changed field is visually harmless.
Do not append all versions of the same PPS ID to an immutable `hvcC` and
call that a repair. Track HDR10 SEI separately from parameter-set stability.
Bound NAL sizes, definition counts, memory, execution time and cancellation;
read a source snapshot and verify its identity again before committing proof.
The chosen numeric limits must be specified and tested during implementation.

**Initial containment:** any initial-versus-in-band definition conflict is
unsupported for immutable-init copy, including this title. An optimization
that replaces stale container definitions with one proven effective constant
configuration may follow, but only after a full presentation proof, including
startup and seek semantics. This proposal does not assume that one sampled
PPS can safely replace the configuration for the whole movie.

### 5.2 Apply the same decision to every serving entry point

```text
exact source/track + current-revision proof available?
        │ no → unknown → queue bounded analysis
        │                 → allowed verified encode OR explicit pending/refusal
        ▼ yes
constant, one supported configuration, canonical hvcC covers all active sets?
        │ no → unsupported → allowed verified encode OR explicit refusal
        ▼ yes
build/validate canonical init → strip redundant in-band headers → copy
```

`constant_verified` permits destructive stripping only when the emitted
decoder configuration contains the verified definitions. An unknown source
must not be admitted because an old index defaults to `true`, because the
client selected Original/Live, or because VOD failed. The guard belongs in
shared server admission and is rechecked before producer creation/artifact
attachment. Clients cannot override it with capability claims.

Use a distinct typed reason for unsafe/unproved HEVC copy so generic
`vod_source_unsupported` handling cannot route it back to rolling copy.
Cover VOD, rolling HLS, progressive remux, prepared replacements, seek/restart,
cached rendition attachment and automatic recovery. Audit direct-original
delivery separately: unchanged file delivery is safe only for a client that
actually accepts that container and its in-band configuration semantics.

### 5.3 Fallback preserves policy and accurately reports the result

An allowed encode must read the **original** source so its decoder receives
all parameter-set updates. It must use an independently qualified graph and
report the actual codec, resolution and dynamic range. If the current policy
does not authorize the resulting grade, return a typed unsupported/pending
result with the existing user-directed alternative. Do not loop between
VOD and rolling or keep serving corrupted frames after refusal.

This containment can increase first-play waiting and encoding load. Use the
existing durable analysis queue and resource admission rather than adding an
unbounded request-time scan. Record unknown versus unsupported distinctly so
the UI can distinguish pending analysis from a known incompatible copy.
Acceptance must measure cold-start delay and resource use; the fix is not
qualified merely because it refuses everything.

### 5.4 Invalidate old proofs and media artifacts together

Add an explicit HEVC analysis/normalization revision to legacy and clustered
index identities and all derived rendition/prepared-artifact identities.
Do not rely solely on the FFmpeg argv hash: a Rust analyzer or init rewrite
can change correctness without changing argv.

Relevant identity sites are
[fragindex.rs](../../crates/plurxd/src/fragindex.rs) around line 1200,
[fragment_index_cluster.rs](../../crates/plurxd/src/fragment_index_cluster.rs)
around line 848, and
[vodserve.rs](../../crates/plurxd/src/vodserve.rs) around line 7728.
The existing post-mux revision is specific to DV conversion; it is not a
general HEVC correctness revision. Re-evaluate
[`SEGPLAN_VERSION`](../../crates/plurx-core/src/segplan.rs) and storage migration
semantics if the persisted structure changes.

Old `parameter_sets_constant = true` is unknown under the new contract.
Rebuild indexes, byte counts and init hashes for the new normalization;
reject stale artifacts even when an old worker advertises them. Rollout must
prevent old workers from serving newly guarded sessions through a fallback.
Plan draining/restarting affected sessions explicitly rather than altering
an init segment already attached to a live player.

## 6. Implementation sequence and acceptance gates

This is a proposed sequence, not authorization to claim a fix has shipped.
Follow [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md). Establish the
pinned Rust 1.97.1 compile loop before any Rust edits. If implemented as
multiple reviewable tasks, integrate on an effort branch; no disjoint-file
exception is proposed because analyzer, producer and identity work overlap.

| Step | Work | Required acceptance |
|---|---|---|
| 1 | Create a redistributable encoded HEVC fixture with changed same-ID PPS chroma offsets; include stable and late-change variants. | Current destructive copy fails decoded-frame equality; source decode succeeds. Fixture passes through real parser/filter paths, not only hand-built structs. |
| 2 | Add pre-filter analysis and versioned proof persistence. | Whole-file successful analysis distinguishes stable, conflicting, malformed, cancelled, truncated and unknown inputs; changes inside non-first samples are observed. |
| 3 | Gate every copy route and close fallback loops. | Unknown/unsafe source cannot reach the stripping producer through VOD, Live, progressive, Original, recovery, prepared switch or old cache attachment. |
| 4 | Validate canonical init construction and allowed fallback from original input. | Stable copied fixtures are pixel-identical before/after; changing fixtures are refused or correctly encoded with accurate delivery labels. Missing-array promotion still works. |
| 5 | Migrate identity and deploy with worker fencing. | Old proofs/artifacts are rejected across legacy and cluster paths; mixed-version workers cannot bypass the guard. |
| 6 | Qualify live devices and this private source. | Safari DV and Chrome HDR10 show correct colors on continuous playback, initial resume and seeks before/at/after changes; no stutter regression on stable 4K HEVC. |

The regression matrix must include ordinary HEVC, DV Profile 8 preserved and
stripped, the existing Profile 5 empty-configuration path, and Profile 7
conversion. Keep parameter-set and DV handling independent. Test multiple
parameter-set IDs and same-ID reuse, distinct-ID SPS geometry/bit-depth
switches, updates away from clean boundaries,
unknown or stale source identity, and changes late enough to escape a short
startup probe. Include full original-vs-copy decoded-frame comparisons for
small synthetic fixtures. For an encode fallback, use a suitable lossy-output
color/fidelity assertion rather than demanding byte-identical frames.

Focused Rust test names and commands must be recorded once the fixtures and
implementation exist. Storage regressions covering replicated state require
`make unit-core` or `--features hiqlite-store`. Run the required compiler,
Clippy, formatting and affected-surface checks before pushing; CI is not the
compiler loop. The documentation-only change is checked with:

```bash
python3 -m unittest discover -s tests/operations -p test_docs_index.py
```

## 7. Completion means correct pixels and an honest delivery contract

The root cause is established for the sampled scene. The fix remains open
until pre-strip evidence, all-path admission, artifact invalidation and
device qualification pass together. Header preservation in a Matroska
experiment is not browser acceptance. A green unit test asserting a flag is
not pixel correctness. A refusal that immediately becomes unsafe rolling
copy is not containment.


## 8. Implemented containment and review amendments (2026-09-25)

The implementation is on `codex/hevc-color-correctness`, based on
`3c89ad2ee` in an independent clone. This section narrows the original proposal to the safe delivery
contract implemented here; the earlier sections retain the diagnosis and
future qualification requirements.

- Background indexing runs `trace_headers` before every HEVC copy filter.
  It streams stderr with an 8 KiB line limit and 64 KiB per parameter-set
  limit; no whole trace is retained. Existing index wall-time and process
  limits still apply. Child success, complete stderr and full media coverage
  are prerequisites for a persisted proof.
- The initial supported contract is one complete source-extradata VPS/SPS/PPS
  configuration with identical repeated definitions and resolved slice
  references. Missing arrays, multiple configurations, changed definitions,
  parser failures, and absent or obsolete proofs refuse HEVC copy by default. The proof
  lives in the existing promotion JSON as optional metadata, attached after
  fragment comparison, and names the selected stream and source object.
- HEVC copy is admitted only through VOD's existing held-source fence. The
  recipe carries the exact proved object version, which also enters the
  rendition identity. Original header changes cannot escape into rolling
  fallback: rolling HEVC copy, including explicit Live and retries, refuses.
  Progressive HEVC copy likewise returns a typed refusal before registering
  playback. These routes need equivalent descriptor/proof binding before
  they can be considered verified. An unrestricted Developer override can
  enable them now, with these limits shown as advisory information. H.264
  behavior is unchanged.
- `hevc_configuration_unverified` and `hevc_configuration_unsupported` are
  distinct from the old VOD fallback codes. No automatic HDR/DV downgrade or
  encode was added. For this affected film, the expected outcome is a clear
  refusal of unsafe copy; a separately selected compatible encode remains
  a separate delivery choice.
- Legacy index fingerprints, cluster pipeline digests, and resulting VOD
  rendition keys change. A source object replacement also changes the VOD
  rendition key. Media-worker protocol 7 excludes older placement/takeover
  workers. Rollout must drain old ingress and sessions; a protocol bump cannot
  remotely stop an old node's public progressive endpoint.

The implementation review specifically rejected relying on a start-time
pathname stat for rolling/progressive retries. Refusing those routes is the
bounded first repair; copying arbitrary changing-configuration HEVC remains
outside this release's contract. No deployment or browser qualification is
claimed by this source change.

### 8.1 Review corrections and source equivalence

Local proofs carry the issuing node and exact filesystem object version.
Cluster reuse requires a full-source SHA-256 attestation in the distinct
`hevc-full-v1` regime; the older sampled digest cannot establish equivalent
HEVC bytes. Portable blobs clear local object/node fields and verify the full
digest before rebinding to the receiving node. Full-hash memos survive sampled
subtitle observations of the same object; both SQLite and Hiqlite enforce this.
A changed object version creates a new preparation generation even when size
and scanner mtime stayed the same. Source freshness is checked before publication
and by the existing held-source producer fence.

Full attestation reads the entire file once per changed object, in bounded
256 KiB buffers. The existing ten-minute analysis budget and preemption still
apply. Large/slow sources can remain unverified; no unlimited foreground scan
was added. `trace_headers` parsing stops on refusal while stderr continues to
drain. Missing stderr completion is a transient build failure, not a completed
proofless index. Stable repeated headers, changed PPS, parser failures, opaque
DV RPU, actual pixel equality, portable proof identity and source memo behavior
have regressions in the candidate.

### 8.2 Operator enable and autonomous decision

The latest operator instruction requests an enable control with advisory
requirements, never a readiness gate. Settings → Developer → Enable HEVC copy
therefore exposes **Enable unverified HEVC copy**, backed by
`playback.hevc_unverified_copy`. Saving `true` is unconditional: it permits
unverified/changing-configuration VOD copy and the progressive/rolling routes.
It does not make their output correct. Requirements report FFmpeg header tracing,
background indexing, per-title configuration, source holding and fleet state;
unknown observations stay unknown and never disable the control.

**Decision made without waiting for an answer:** retain verified-only behavior
by default and expose the unrestricted override, rather than silently continuing
known corrupt copy. The override applies on new admission, without restart;
existing sessions are not revoked. It is not a bypass for unrelated authentication,
resource limits or established transport contracts.

### 8.3 Delivery workflow and remaining qualification

Batch the implementation and documentation into one main-bound PR. Final
adversarial review occurs once the candidate is ready, findings are addressed,
and only then does the configured fast lane run. Earlier targeted test passes
predate the workflow change and do not qualify the final tree. Compiler, lint
and syntax checks continue before commits. Physical Safari/DV and Chrome/HDR10
qualification and rolling deployment remain explicitly outstanding; a green
PR does not certify those devices or deploy the fleet.
