# HDR enhancement quality — preserve the benefit in the delivered picture

**Status:** open; measurement repair integrated into the effort, production quality work in
progress · **Written:** 2026-11-03 · **Owner:** Codex, with Astra consultation

Companion to [the processing ledger](DV_HDR_PROCESSING_STATUS.md),
[display management](DISPLAY-TARGETED-DV-BUILD.md), and
[the reconstructed P8.1 measurements](FEL-P81-RECONSTRUCTION.md). This is the
current repair ledger: which losses have been isolated, what changes the
picture, and what evidence remains before the enhancement work is complete.
Historical receipts keep their original source and tool attribution.

## 1. Objective — make the enhancements work

The owner's instruction of 2026-11-03 is to make RPU, DYN and FEL useful
without making the delivered picture worse. Withholding an enhancement or
loosening a quality threshold does not achieve that objective. Existing
fallbacks remain availability protection while the actual losses are fixed.

Evaluate the picture after reconstruction, display mapping, colour export,
encoding, metadata authoring and decoding. Shader agreement alone cannot
establish final-delivery quality. Preserve native delivery where the client
can compose the original layers without re-encoding.

The owner further clarified that enhancements must not impose a lower
bitrate than the source. The existing 20 Mb/s Original replacement and the
initial 60 Mb/s repair ceiling both violate that intent for higher-rate
sources. The 60 Mb/s implementation was not published and is superseded.
An equal bitrate budget is necessary here; it does not make a lossy
re-encode equivalent to preserving the original compressed video.

## 2. Measurement repair — an inverse conversion added error

The old qualifier and standalone proof expanded limited-range BT.2020 YUV
with `scale` into RGB48, then interpreted those samples as full-range PQ.
Independent H.273 constant-patch controls on the worker's system FFmpeg
5.1.9 and supported Jellyfin FFmpeg 8.1.3 measured up to **203 RGB16 codes**
of conversion error. The validated `zscale` inverse differs by at most
**one code** in the initial controls. A 16-bit 4:4:4 round trip also exposes
the error: its mid-brightness mean Delta E ITP falls from 1.144 to 0.012
when only the inverse conversion is corrected in the diagnostic.

The shared [filter](../../tools/dv_quality/bt2020_pq_rgb48.filter) fixes
matrix, limited/full range, BT.2020 primaries, PQ transfer, left chroma
location and bicubic reconstruction. Its planar RGB intermediate is
required before packing RGB48. The helper's already-RGB output is unchanged.
`zscale` is an existing every-node requirement in
[the support matrix](../../scripts/ffmpeg-support-matrix.json).

The [qualifier](../../crates/plurxd/src/source_probe/fel_qualification.rs)
and [standalone proof](../../tools/dv_processing/controls/fel_composition_proof.py)
use the same filter. Qualification identity v3 invalidates v2 verdicts;
the storage schema, encoder recipe and admission thresholds are unchanged.
Missing conversion support fails measurement rather than selecting the old
biased inverse. The native regression requires an explicitly selected executable:

```bash
PLURX_FFMPEG=/usr/lib/jellyfin-ffmpeg/ffmpeg make dv-rgb-conversion-check
```

It runs in the full CI Rust job after provisioning the pinned media tool.
General operations preflight runs only the sharing/structural control and
does not gain a native-tool dependency. This repairs the measurement; it does not change encoded
pixels or qualify an enhancement by itself.

Historical swscale-derived RGB scores, including the title verdicts in the
P8.1 ledger, need fresh measurement before reuse as current quality evidence.
Keep their raw outcomes and their non-colour evidence, including timing,
source identities and byte-level observations.

## 3. Separate representation loss, codec loss and the real fallback

The first diagnostic acquires four identical presented frames three ways:
FEL RGB48 reference, original P7 with NLQ disabled, and FEL YUV420P10 export.
Every acquisition must agree on source PTS, duration, RPU hash and decoded
base-layer hash. Four-frame runs diagnose startup; they do not qualify a
production window or title.

- An x265 lossless control reproduces the encoder-input YUV frame hashes.
  Its RGB error equals the unencoded export's: a useful representation
  ceiling, not a usable-bitrate recipe.
- Four chroma phase combinations and reference-defined luminance bands
  expose local errors that a full-frame mean or one fixed lattice can hide.
  Do not derive the diagnostic crop from an intensity threshold.
- Independent 4:4:4/4:2:0, 16/10-bit exports distinguish colour-conversion,
  quantization and subsampling losses. A filter swap alone has not removed
  the representation floor.
- Original P7 with NLQ disabled is a distinct ablation from actual `To81`
  fallback. Compare real converted output, joining its decoded BL hashes to
  the original reference frames before scoring. On diagnostic sample A,
  mode 2, mode 5 and the NLQ-off ablation produced identical measured RGB;
  that result does not generalize to nonidentity mappings.

The early system-FFmpeg encodes remain diagnostics for 5.1.9. Later encoder
comparisons explicitly select `/usr/lib/jellyfin-ffmpeg/ffmpeg` 8.1.3;
source-render, encoder and measurement identities are recorded separately.
The legacy and supported executables are not interchangeable evidence.

## 4. Production repair — choose from measured final-delivery results

The supported-encoder screen now covers 48 consecutive sample-B pictures.
An initial software screen omitted input colour tags and is invalidated;
its retained output is not evidence for a preset decision. The corrected
60 Mb/s, 90 Mb/s VBV comparison gives mid-brightness mean/p99 Delta E ITP
of 3.041/9.335 for VAAPI, 2.623/7.566 for x265 medium and 2.516/7.703
for x265 slow. Medium takes 42.8 seconds and slow 121.3 seconds for a
2.002-second window. This supports investigating prepared production;
it does not establish live feasibility or held-out acceptance.

A separate HDR10-E window uses the same exact source identities, a
1,000-nit target, actual RPU L5 active area and four chroma phases.
Its current Original path inherits the ordinary 20 Mb/s 4K rung after
processing turns source copy into an encode. Raising only the bitrate
to 60 Mb/s, while retaining VAAPI policy 19, improves all measured bands:

| Reference band | 20 Mb/s mean / p99 | 60 Mb/s mean / p99 |
|---|---|---|
| Dark | 3.466 / 9.840 | 2.646 / 7.751 |
| Mid | 4.440 / 13.873 | 3.198 / 10.145 |
| Bright | 3.156 / 11.903 | 2.454 / 8.139 |

These compare decoded delivery to the helper's target output, so they
isolate export/encode preservation rather than independent Dolby mapping
accuracy. The policy-0 screen does not improve every dark metric, so it is
not part of the production change. The 60 Mb/s result diagnoses the old
20 Mb/s loss; it is not the accepted replacement policy. Independent
mapping comparison and weaker-source coverage remain required.

Four full 4K frames from that same window (indices 0, 16, 32 and 47) have
now been independently mapped with Dolby's reference tool at the same
1,000-nit/P3 target. Lossless transported masters match their original
RGB48 hashes; all 48 pre-map/target source PTS, duration, RPU and BL bindings
agree. The actual resolved target is checked in the tool log. This validates
mapping conditional on the supplied FEL master, not FEL reconstruction.
The XML bridge omits L5; scoring uses the original L5 interior and retains
full-frame spatial context. Four phase lattices give 122,880 samples:

| Output compared to independent mapping | Active-picture mean / p99 Delta E ITP |
|---|---|
| Unencoded enhanced helper | 0.0079 / 0.0190 |
| Unencoded no-FEL ablation | 3.268 / 6.489 |
| Decoded 20 Mb/s output | 4.045 / 12.345 |
| Decoded 60 Mb/s output | 3.221 / 10.228 |

The rate repair recovers the mean advantage on this strong-FEL sample,
but its tails still lose to the unencoded no-FEL ablation. The two rates
produce identical sampled startup-IDR pixels; fresh-window encoder startup
is a further repair target, consistent with the earlier slice-QP evidence
in the P8.1 ledger. These results do not complete enhancement acceptance.
Only five samples occupy the below-0.01-nit band, so this run cannot qualify
black preservation. Weaker-FEL scenes, other targets and held-out content
remain open.

The revised implementation preserves the source video budget for Original
and Remux replacement, without a fixed ceiling. Prefer authoritative combined
video rate; where that is unavailable, the container average is a conservative
video budget, explicitly including audio and other stream overhead. A selected
BL-only stream rate must not discard the separate EL budget. Round fractional
kb/s upward. Explicit encode rungs retain the user's selected rate.

Freeze the target, peak, buffer, HEVC tier/level and authored DV level before
recipe identity, admission and resource accounting. Peak and buffer policy
may adapt to real constraints while preserving target; an actual delivery
budget below target is a specific conflict, not permission to lower quality
silently. Raster and profile alone do not prove a client's maximum level.
Derive codec level from rate and buffer as well as raster/cadence, and verify
the resulting hvcC/VPS/SPS and DV configuration. The encoded stream, advertised
CODECS, held-root reuse and measured recipe must agree.

Different effective video envelopes need their own measured qualification
variants. Persist bounded typed pending requests and source-bound completed
variants in the existing title worker, preserving other variants and requests
that arrive during measurement. Client identity and audio choices must not
multiply identical video measurements. The old capped implementation's six
focused passes and compiler/review receipts remain historical evidence only;
they do not qualify the revised source. The revised candidate passes independent
Astra review and ten focused core cases covering envelope arithmetic, encoder
arguments, full hvcC/VPS/SPS/HRD inspection and authored DV signalling. The
exact-variant storage regression executes on SQLite memory, SQLite file and
an actual three-voter Hiqlite cluster, including Busy/preemption reclamation,
late requests, source CAS and distinct-title family counts. Initial SQL-binding
and malformed-fixture failures remain retained beside their corrected results.

The revised native author also passes its reordered-packet control with actual
three-frame synthetic FEL reconstruction and a legal Main10 High envelope.
It verifies exact timing/RPU association, unchanged encoded base and decoded
YUV, authored DV level and refusal cases. The shipping FFmpeg 8 fixture uses
passthrough timestamps without the contradictory output `-r` accepted by older
FFmpeg; the short NUT's estimated cadence is not source-clock authority.
The exact staged native source passes all-target compilation and Clippy; all
ten requested Linux daemon regressions execute successfully with zero ignored,
covering Original rate/cost replay, reduced-raster execution, Busy/preemption,
foreign-backend pending work and actual encoder binding. Integration remains
pending.
None of these controls establishes final movie-picture acceptance.

The weaker-FEL sample A now has a matched 48-picture HDR10-E window at
1,000 nits, using its actual title minimum 0.0001 nits and peak 1,000 nits.
Original RPU L5 defines the active area. Against its unencoded enhanced target:

| Output | Dark mean / p99 | Mid mean / p99 | Bright mean / p99 |
|---|---|---|---|
| Unencoded no-FEL ablation | 1.501 / 4.029 | 1.478 / 4.399 | 1.121 / 2.919 |
| Unencoded 4:2:0 export | 0.927 / 5.539 | 1.368 / 8.742 | 1.425 / 9.558 |
| Decoded 20 Mb/s | 3.551 / 11.985 | 4.625 / 16.412 | 4.612 / 17.726 |
| Decoded 60 Mb/s | 2.959 / 9.752 | 3.767 / 13.136 | 3.637 / 13.922 |

Higher bitrate helps this source too, but cannot recover losses already
introduced by colour export. Relative to this point-sampled helper reference,
even the unencoded export loses bright mean and all three tails to the
no-FEL ablation. That ablation does not establish the actual native-client
fallback. The packaged Linux player composes original BL+EL locally using
bilinear plane sampling; the server helper uses point sampling with a tie
bias. Compare original and processed video with the same actual consumer
before attributing a delivered regression to FEL. Do not smooth the reference
and rename it an independent golden output.

Matched source-budget screens preserve targets of 83,515 kb/s for sample A
and 90,893 kb/s for sample B, conservatively derived from container averages.
For sample B, a 90,893/130,000/160,000 target/peak/buffer envelope reduces
mid-band mean/p99 to 2.713/8.384, compared with 4.440/13.873 at 20 Mb/s.
Its HEVC Main10 High level is 5.1; the unconstrained preferred peak/buffer
requires 5.2. Both retain the same target, with small measured differences
between bands. No HRD is present in these diagnostic streams; command-line
buffer settings are not evidence of signalled HRD.

An independent identity/no-residual/no-mapping GPU control now isolates
generation loss before encoding: luma round-trips exactly, while the current
export changes chroma even when no enhancement is applied. Matched-consumer
scoring, actual native-player capture, nonzero-residual controls and authored
P8.1 colour-basis checks remain open. Private representation prototypes have
not established a robust replacement and are not production changes.

A direct horizontal sample with vertical averaging exactly recovers the
identity fixture's original YUV codes, under all three matched consumer
kernels. It fails the next test on real FEL pictures. Exporting the same
frozen RGB16 master through both variants first reproduces the current
production export byte-for-byte, then shows slightly worse means and tails
with the direct variant on weak/strong pre-DM and post-DM frame-zero controls,
using both bilinear and bicubic consumers. The direct variant is rejected;
no full-window encode or production shader change follows that result.

The installed baseline offline P8.1 author refuses the strong-source window's
unsupported creative/complex metadata. That refusal is retained, with source
metadata intact; it provides no authored colour-basis or P8.1 acceptance
evidence for that source.

An isolated Weston 16 headless output now supplies a measured 1,000-nit
P3-D65 target, 0.005-nit black and 203-nit reference white to the packaged
Linux player. A copied original P7 clip preserves BL+EL+RPU without encoding;
all 48 reference BL hashes join uniquely after clock rebasing. Tagged 16-bit
PQ captures confirm actual FEL residual shader execution and the intended
mapping target. They remain provisional: seek-preroll decode warnings require
verification of the actual paired EL and RPU, not BL hashes alone. A private
instrumented build is being prepared; the installed player is unchanged.

Further startup controls are retained as negative or limited evidence:

- VAAPI initial-QP hints 24, 20 and 16 reach libva successfully but produce
  identical decoded pixels for all 48 pictures. Mesa 26.0.8 ignores that
  misc-parameter field; it is not a production fix.
- Repeating the same 48 input pictures in one 96-picture encoder preserves
  the first GOP exactly. The second IDR improves from QP 24 to 17, while
  mid-band mean/p99 changes only 3.198/10.145 to 3.177/10.117. Retaining
  rate-control state alone does not close the quality gap.
- Pre-encoding at unchanged rate improves some means but worsens measured
  tails; explicit safe VBV occupancy produces the same decoded pixels.
  Neither diagnostic is adopted as a production recipe.

1. **Finish the full-window comparison.** Measure 48 consecutive pictures,
   actual encoded segment peaks, time and per-frame/band errors. Compare
   actual conversion fallback and composed output at the same presentation
   times. Add at least a weak-FEL and a strong-FEL source before choosing a
   production recipe.
2. **Preserve the recovered gain.** If a usable-bitrate software recipe wins
   but cannot meet live deadlines, integrate it with the existing owned
   background producer and prepared rendition cache. The current software
   completed-output contract exists, but prepared production explicitly
   disables DV processing and retained publication refuses ephemeral
   processing. Removing those guards alone is not a valid implementation.
3. **Fix any remaining representation loss.** Test a concrete export change
   against the same decoder model, colour controls and real source frames.
   Higher bitrate cannot recover information lost before the encoder.
4. **Run HDR10-E through final decoding too.** Compare RPU reconstruction,
   FEL and DYN/creative trims separately at a common target against the
   independent reference. Carry colour, black/highlight and metadata
   observations through the encoded container. Native P8/P7 and server
   HDR10-E are separate acceptance paths.

No new scheduler, gratuitous Developer switch, empirical per-title colour
correction, arbitrary bitrate/QP increase or altered pass threshold is
part of this repair. A slow quality producer must use existing admission,
source fences, cancellation/reap, storage accounting and resumable ownership.

## 5. Completion requires delivered-picture evidence

| Milestone | Required evidence | State |
|---|---|---|
| M0 trustworthy conversion | Independent H.273 10/16-bit and 4:4:4/4:2:0 controls; unchanged RGB branch; old verdict invalidation; independent diff review and focused checks | Integrated into the effort by #1367; exact-head effort gate 6403 passed, identical landing tree and five regression fields verified |
| M1 loss isolation | Exact frame bindings, representation/codec split, true fallback, full 48-frame weak/strong FEL comparisons on supported tools | Both source windows measured; native DV basis, additional targets and held-out coverage open |
| M2 production improvement | A measured recipe/export change delivered through supported ownership, admission and metadata paths | Source-aware HDR10-E Original implemented and focused checks/review passed; representation and codec repairs open |
| M3 enhancement acceptance | Held-out scenes and final decoded outputs: dark detail, neutral/saturated colour, highlights, grain and temporal stability; CM4/CM29, P7/P8 and both destinations; then physical client checks | Open |

Use mean and tail error, per-frame and luminance-band results together.
Exact pixel equality is a control for lossless transport, not a promise for
12-bit reconstruction exported to 10-bit 4:2:0. Material regression limits
must be justified by precision and visible defects; never choose a tolerance
merely to pass the current encoder.
