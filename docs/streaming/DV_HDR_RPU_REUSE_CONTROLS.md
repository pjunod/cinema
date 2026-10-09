# Dolby Vision metadata reuse — bounded cache and reset controls

**Status:** open — bounded experiment and packaging independently reviewed; production
reuse remains unqualified · **Updated:** 2026-10-08

Companion to the [implementation ledger](DV_HDR_PROCESSING_STATUS.md),
[timeline controls](DV_HDR_TIMELINE_CONTROLS.md) and
[M1 contracts](DV_HDR_M1_CONTRACTS.md). This M0 follow-up distinguishes explicit
RPU mapping reuse from stale parsed side data and tests a pinned parser's cache
boundary. It does not broaden M1's fresh-metadata predicates.

## 1. Exact tested configuration

The offline public FFmpeg `dovi_rpu` filter emits full mappings on key pictures
0 and 3, and explicit mapping-ID-0 reuse on pictures 1, 2, 4 and 5. The first
mapping is identity; the second has luma `1/16 + 3x/4` with identity chroma.
Actual emitted RPUs pass pinned libdovi parsing and CRC checks. Static display
metadata compression is zero; distinct synthetic metadata tags identify which
picture supplied it.

The configuration re-read from the muxed container is **Profile 8,
compatibility ID 6, compression 1, base plus RPU, no enhancement layer**.
This names a source-supported, parser-accepted cohort. It does not certify a
standards-conforming HDR10-compatible destination or establish P8.1. The base's
observed Main10/BT.2020/PQ tags do not validate that Dolby configuration.
The original design's proposed HDR10-compatibility inference was not established
by execution and is explicitly withdrawn in the retained result documentation.

Six source-generated base pictures preserve the prior timeline experiment's
actual PTS and authored intervals through B-frame reorder. The full source
mapping changes on the second key picture, and intervening explicit reuse must
resolve to the correct mapping within its epoch. Those tags are association
markers, not a display-mapping or movie-fidelity reference.

## 2. Cache lifetime and successful video decode

| Control | Required interpretation |
|---|---|
| Seed followed by explicit reuse | Resolve the seeded mapping and current metadata within the same epoch |
| Real seek and reset | The full-mapping key at 140 ms reseeds the cache after a 200 ms seek; later pictures reuse the new mapping |
| Reuse on a cold or flushed cache | Refuse the unknown previous mapping ID; record parser status and configuration before and after failure |
| Wrong compression signaling | Refuse the parser operation even if getters still expose an older mapping |
| New epoch declared without reset | Parser success using an old cache does not satisfy epoch acceptance |
| Omitted raw RPU | Successful picture decode with stale parsed metadata does not establish explicit reuse |
| Profile 7 compression | Preserve the pinned public filter's strict refusal before output-container creation |

The compressor runs offline and closes before playback-side decoding. It has
no flush callback in this pin. Actual seek uses `dovi_split` and HEVC reset;
the private cache probe separately calls `ff_dovi_ctx_flush`. The experiment
does not infer compressor cache reset from an `av_bsf_flush` call.

HEVC picture decoding can continue after a Dolby metadata parse error.
Acceptance therefore requires the actual parser result, raw RPU association,
resolved mapping, current metadata and cache epoch. A successful decoder exit
or a non-null metadata getter is insufficient on its own.

The 140 ms picture is recorded as preroll; its presentation interval contains
the 200 ms target. The fixture's selection policy does not settle the product's
presentation-at-target behavior.

## 3. Reproduction and evidence

The [retained bundle](../../tools/dv_quality/backends/rpu-reuse/README.md)
provides source pins, dependency validation and the exact replay command.
The new scratch replay uses the verified timeline inputs and an explicitly
captured public offline Cargo registry. It builds a separate static FFmpeg
prefix with the public compression filter; earlier approved bundles are
unchanged. Source/library identities, actual muxed configuration and emitted
RPU bytes are checked separately.

Execution containers have no network, two-CPU/two-GiB limits, explicit Python
bytecode suppression and scratch-local temporary files. The inherited image
originally used live package resolution, so this is not a fully reproducible
operating-system image. Private parser symbols are experiment dependencies,
not a production API commitment.

The exact replay passed 27 checker controls and four real late-failure
controls. Ten additional safe-unpack controls and an actual extraction checked
all 398 prerequisite files. The approved source bundle has 92 ledger entries
plus its ledger; the runtime inventory covers 882 explicitly scoped entries.

| Evidence | SHA256 |
|---|---|
| Final receipt | `d5c784093128d9bf76dc8317ce794095611ffc7d74a565e52d0728d461272d41` |
| Source ledger | `715c41f793e4b803f399d4228144e2d98898631a3773be74ef1518f2bbc059d3` |
| Runtime inventory | `16e3c62cbcf7b165a1a6dd7828908cd4455173a5c0bf03aa34fc4a6aa98e5445` |

Adversarial review found that replaced inputs could previously be paired with
stale successful outputs. The corrected stage runner binds exact arguments,
consumed container/RBSP/helper bytes before and after execution, actual process
status and immediate outputs. The corrected scientific replay and substitution
controls passed. Independent review then approved the packaging-only delta;
it did not require repeating unchanged scientific execution.

Replay requires the retained prerequisite archive (82,626,560 bytes; SHA256
`d405ec20062c294d74c8e2377068cc0d6798c1f6573d26170e8d95e790326154`)
and the already available pinned Docker image. The archive is currently retained
locally outside Git; it has not been published. This is fresh-scratch replay
from retained exact prerequisites, not a clean bootstrap. The bundle README's
pending-review wording is its frozen execution snapshot; the review disposition
in this document supersedes it without changing evidence bytes.

```sh
python3 tools/dv_quality/backends/rpu-reuse/unpack_prerequisites.py \
  /path/to/plurx-dv-rpu-reuse-prerequisites.tar /tmp/NEW-prerequisites
sh tools/dv_quality/backends/rpu-reuse/replay.sh /tmp/NEW-replay \
  /tmp/NEW-prerequisites/inputs /tmp/NEW-prerequisites/registry
```

The package contains public source/fixture inputs and captured Cargo registry
files, not Docker image contents, binaries, Git metadata or credentials.
Packaging was added after the scientific replay; only extraction checks were
rerun for that delta.

## 4. Remaining acceptance

This experiment does not establish general Profile 7 reuse, multiple mapping
IDs, ID fallback, extended compression, compressed display metadata, general
seek recovery after metadata errors, container/profile conformance or device
interoperability. No GPU reconstruction, creative mapping, trims, FEL,
real-film quality, physical display performance, production fallback or
HDR10-E badge qualification is supplied by it.
