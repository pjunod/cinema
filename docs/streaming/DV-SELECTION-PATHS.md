# Dolby Vision selection — which route, graph and encoder runs

**Status:** live · **Updated:** 2026-10-21

Companion to [Playback](../PLAYBACK.md#routing-inventory--every-fork-has-one-owner-and-one-regression-pin)
and the [processing ledger](DV_HDR_PROCESSING_STATUS.md). This is the
selection reference; the ledger holds measurements and acceptance limits.
The Intel reconstructed-encoder correction below is the candidate in
[PR #1250](http://forge.lan:3000/noirr/plurx/pulls/1250), not a deployment claim.

## Follow the decisions in order

The extra layers add constrained choices at several stages. They do not
make every source, destination, decoder, encoder and transport combination
valid. Read each stage's result before consulting the next table.

```text
source facts + client caps + request + saved settings
                         │
                         ▼
ordinary delivery: native DV / converted DV / compatible base / encode
                         │
              optional processing eligible?
                  no ────┴──── yes
                  │             │
                  │       destination + mode + admitted worker
                  │             │
                  │       decoder + reconstructed output encoder
                  │             │
                  │       first private window + output validation
                  │             │
                  ◀── decline ──┤
                  │             ▼
          ordinary delivery   processed rendition + owned report
```

The ordinary path is resolved first; optional enhancement does not create
permission to serve an otherwise incompatible stream. An explicit processed
candidate without an ordinary fallback can be refused instead of downgraded.

## Delivery and enhancement are separate decisions

| Existing delivery or request | Processing selection | Result when processing is unavailable |
|---|---|---|
| Native DV copy approved by the client's enumerated profile | Keep native delivery, including when both processing switches are enabled | Existing native copy |
| P7 with HDR10 base; client accepts P8, not P7; conversion permitted and an HLS copy producer available | Ordinary P7→8.1 copy conversion. FEL re-encode may replace this with reconstructed P8.1 | Converted copy; FEL is discarded and converted reshaping is pass-through |
| Other eligible DV delivery targeting HDR10, with HDR processing enabled | Try processed HDR10 | Existing compatible base or required conversion, according to the original plan |
| Original quality | Ordinary policy preserves video, but enabled DV processing can replace the copy with an encoded processed rendition | Original ordinary route if enhancement declines |
| Auto or a selected quality rung | Both can qualify for processing; Auto also has link admission | Selected ordinary route |
| Burned subtitles or continuous media | Current processed VOD graph declines these combinations | Ordinary policy, with a reason |
| Rolling recovery | No processed graph exists here | Retained ordinary fallback in the same incarnation, or refusal for an explicit processed candidate |
| Progressive/direct request | Does not execute the processed HLS graph. The decision service routes eligible HDR10 enhancement through HLS; a stale progressive request re-decides its available producer | Producer-compatible ordinary delivery |

The switches are independent: HDR processing and FEL re-encode default off;
P7 copy conversion defaults on. There is no separate DV Auto/Forced enum.
The quality menu's Auto/Original/rung choice and these switches are distinct
inputs. The saved switches are authoritative; readiness is advisory.

Profile 5 has no compatible HDR base to strip down to. Its ordinary
compatibility path already requires proved RPU rendering before conversion;
disabling optional enhancement does not bypass that requirement. This is
separate from optional FEL reconstruction and from a DYN-only P7 copy.

**Owners:** [ordinary DV policy](../../crates/plurx-core/src/playback/mod.rs),
[request routes](../../crates/plurxd/src/transcode/session_request.rs),
[candidate destination](../../crates/plurxd/src/transcode/manager/candidates.rs)
and [producer preparation/recovery](../../crates/plurxd/src/transcode/manager/create.rs).

Dual-layer profiles P7/P4 require an explicit numbered client claim; a
blanket DV bit is insufficient. A known single-layer profile takes
precedence over a conflicting EL flag. Unknown-profile source facts still
need their own compatibility decision.

P7 copy conversion rewrites RPU NALs after the copy muxer, without video
re-encoding or a second aligned stream. Preserved, stripped and converted
copies have distinct index identities. The delivered profile distinguishes
native P7 from converted P8 even when both badges say Dolby Vision. The
replicated conversion setting is read per request by planning and indexing;
`PLURX_DV_CONVERT` only seeds an unset setting. Recognized false spellings
(`0`, `false`, `off`, `no`, ignoring case/outer whitespace) disable it;
unreadable or unrecognized values keep its default enabled.

## Catalog eligibility permits a trial; the window proves the route

| Gate | Current rule |
|---|---|
| Native DV | Stand aside before optional processing |
| HDR10 destination | Requires HDR processing; catalog combinations are P5/base compatibility 0, P8/1, or P7/1 or 6 |
| P8.1 destination | Requires P7, conversion permission, FEL re-encode, and EL not explicitly absent |
| Catalog facts | A valid level and no explicit RPU absence are required. Profile/EL flags alone do not establish actual FEL, supported metadata or aligned pictures |
| FEL mode | Catalog P7 with EL not explicitly absent selects FEL. Execution must prove supported P7 metadata and paired BL/EL; a failed FEL trial does not silently become base mode |
| Base-RPU mode | Other supported sources use the base and RPU. It can produce HDR10, not reconstructed P8.1; its polynomial/MMR policy differs from the restricted FEL affine-polynomial/linear-DZ subset |
| Producer | Current public completed-window route is Linux finite VOD, with current planning/source authority, suitable HLS fMP4 HEVC caps and admitted preparation/graph/resources |
| Raster/cadence | Uses the selected source/rung geometry, rounded to even height. No Auto-only restriction, 720p clamp or extra speed threshold is imposed by qualification |
| Startup budget | Optional first-window qualification receives at most 8 s and half the remaining create budget. A successful verified first window does not require additional trial windows or a throughput gate |

Processed P8.1 admission uses the client's actual HLS/MP4 transport and
same-entry Main10/P8/PQ decoder claims. `dv_transport` describes an original
preserved-DV delivery requirement; a progressive claim does not prohibit HLS
when `transports` includes it. Native web readiness reads the complete finite
media timeline within its explicit 4 MiB + 72-byte budget; master/error bodies
retain 64 KiB and initialization 1 MiB. The initial URL may be scalar media.
These byte limits are client readiness bounds, not a universal duration claim.
| Completion | Source binding, fresh supported RPUs, frame coverage, actual decoder/encoder, encoded samples, timestamps/grid and destination metadata must validate before publication |

The preflight owner is
[`dv_processing_preflight`](../../crates/plurx-core/src/transcode/dv_processing.rs).
The production grant is `dv_plan_completed_window` in the same file, together
with [runtime validation](../../crates/plurxd/src/dv_runtime.rs). The older
synthetic resolver and its fixtures do not grant a public route.

## Decoder, ordinary encoder and reconstructed encoder are different choices

Ordinary family preference honors an available explicit selection; otherwise
its order is NVENC → VideoToolbox → QSV → VAAPI → Software. Availability and
usable route proofs are separate from a codec merely being compiled in.
The [decoder resolver](../../crates/plurx-core/src/transcode/decode.rs)
then considers source facts, processing needs, policy and restrictions.
Software decoding does not imply software encoding.

Ordinary HDR delivery also re-selects the encoder and output grade against
the applicable plain-PQ or Dolby-renderer proof and geometry constraints.
It is owned by `encoder_and_grade_for_with_preference` in
[the manager plan](../../crates/plurxd/src/transcode/manager/plan.rs), not by
the reconstructed-output selector below. A proof for ordinary HDR is not
blanket permission for every processed graph.

| Preference passed to the Linux completed-picture selector | Selected output | Required qualification |
|---|---|---|
| Software | Software HEVC/x265 | Explicit software is retained; completed output still must validate |
| NVENC | NVENC HEVC | Typed reconstructed contract and actual verified window; selection itself does not require the VAAPI proof |
| QSV | VAAPI HEVC when proved; otherwise Software | Host-level VAAPI HDR10 passthrough probe and a matching reconstructed output contract; actual window validation still required. PR #1250 adds this mapping, not a reconstructed QSV contract |
| VAAPI | VAAPI HEVC when proved; otherwise Software | Same host-level VAAPI proof and contract |
| VideoToolbox | Software if this enum reaches this Linux function | This fallback is not a macOS reconstruction implementation |

The software completed-picture graph explicitly bounds x265 pools/frame
threads; an unintentional fallback can therefore be expensive at 4K. This
was the [Avatar encoder-selection defect](../clients/AVATAR-WEB-PLAYBACK-STATUS.md).
The selector is
[`completed_window_encoder`](../../crates/plurxd/src/dv_runtime.rs).
`ResolvedTranscode::completed_reconstructed_output` and
`CompletedReconstructedOutputContract` in
[decode](../../crates/plurx-core/src/transcode/decode.rs) and
[encoder](../../crates/plurx-core/src/transcode/encoder.rs) respectively
permit only their explicit codec/grade/pixel contracts.

The helper can decode with VAAPI or fall back to software independently of
that output choice. Receipts record the decoder that actually ran; decoder
fallback does not change the frozen pixel-policy identity. macOS processing
has its own context-bound, proved graph selection, including SDR/HDR10 and
strict Profile 5 paths; it does not execute this Linux FEL selector. See
[the Mac implementation](MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md).

## Open proof and contract work

The argument currently named `vaapi_upload_proved` is populated from
`hdr10_passthrough_vaapi`, a host-level passthrough capability probe. It is
neither a per-render-node proof nor a direct proof of the completed graph's
system-memory P010 upload. The private window's actual execution and
fail-closed output validation remain necessary. The retained timing evidence
covers the measured single-render-node Intel configuration; it does not
qualify multi-GPU selection or every VAAPI upload path.

Additional Fable 5.1 review of PR #1250 approved the source delta and recorded
two separate follow-ups:

- Bind capability evidence to the actual execution device and surface entry
  path, then qualify multi-GPU configurations. The current host boolean
  cannot establish that association.
- Gate the pre-existing unconditional NVENC selection on
  `completed_reconstructed_output(Nvenc).is_some()`. The contract exists
  today, but a future contract removal must not reach the downstream
  `expect("verified reconstructed output encoder")`.

These are open work, not completed protections or deployment acceptance.

## Output validation and failure determine what the viewer receives

Production pixel order is normalization → supported RPU reshaping → FEL
residual when applicable → RPU color conversion → target mapping → BT.2020
NCL conversion → Main10 encoding. P8.1 also authors RPUs for the reconstructed
base. The completed constructor adds target mapping to the base graph;
creative display trims are not thereby claimed applied.

| Outcome | Delivery/report consequence |
|---|---|
| Qualification declines, times out or fails | Name the reason and retain the ordinary fallback; do not advertise the requested enhancement as completed |
| Verified HDR10 processed window | Publish its processed identity. FEL/RPU indicators depend on the operations actually reported; no DV dynamic metadata is delivered with HDR10 |
| Verified reconstructed P8.1 window | Publish reconstructed pictures and fresh/adapted RPUs; effective evidence can report FEL/RPU/DYN |
| Native or converted DV copy | Dynamic metadata can be delivered without server FEL reconstruction. A DV badge or an RPU NAL alone does not prove enhanced pixels |
| Later processed failure | Refuse the same failed episode on a fresh create rather than repeatedly paying the trial; choose an available ordinary fallback with its diagnostic |
| Missing/stale/wrong-owner effective report | Clear the processed claim; a selected preference or serialized candidate is insufficient evidence |

The process-local failure ledger is capped at 128 episodes, keyed by
playback, source version, backend, settings generation and destination. It
has no TTL; source/settings/backend identity changes are different episodes.
See [the ledger](../../crates/plurxd/src/vodencode.rs). The current report is
bound to the publication/owner generation; see
[enhancement presentation](../../crates/plurx-core/src/playback/enhancement.rs).

**How to read playback information:** source profile describes the file;
delivered profile describes the bytes; FEL/RPU/DYN describes the current
effective processing/delivery evidence. Display output remains a separate
observation. See [dynamic range](../PLAYBACK.md#dynamic-range--source-delivered-rendered).

## Keep the path inventory and tests together

The canonical IDs and behavioral pins live in
[the routing catalog](../../tests/playback/routing-decisions.toml) and
[Playback's routing inventory](../PLAYBACK.md#routing-inventory--every-fork-has-one-owner-and-one-regression-pin).
They cover ordinary decoder/HDR re-selection, Mac processing, enhanced
destination/preflight, reconstructed output, completed-window qualification
and failed-episode suppression as separate forks.

```bash
python3 -m unittest tests.validation.test_playback_routing_inventory
```

The check proves that the prose/catalog ID sets agree and their source/test
anchors still exist. It cannot discover a newly added undocumented branch,
nor prove that every hardware/metadata/transport/display combination works.
A new selection rule must update its ID, prose, source anchor and behavioral
pin together. Qualification still needs the applicable real-source and
physical-client evidence; this reference does not certify Dolby conformance
or promise unsupported combinations.
