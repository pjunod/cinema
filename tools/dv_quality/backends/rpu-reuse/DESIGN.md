# Bounded explicit RPU reuse controls

**Status:** proposed — source inspection complete; no new fixture generation, build or execution yet. Adversarial design review gates the experiment. Approved predecessor bundles remain unchanged.

## Supported candidate and validity boundary

The candidate is HEVC Profile 8 with HDR10-compatible base, limited metadata compression (`dv_md_compression=1`), mapping ID 0, disabled EL residual and full VDR sequence information. Pinned FFmpeg's public `dovi_rpu` bitstream filter generates compression syntax, enables reuse on non-key packets, and sends full metadata on keys. Its limited encoder path permits mapping reuse at ID 0. This is source-supported implementation syntax, not a Dolby conformance certificate or a claim of device interoperability.

The strict pinned Profile 7 path rejects compression for profiles below 8 and rejects explicit `use_prev_vdr_rpu=1` without compression. This experiment will not relax those guards, forge a Profile 7 compression claim, or qualify P7 reuse. Extended compression, multiple mapping IDs, missing VDR sequence information and the parser's fallback from an unknown requested ID to ID 0 remain excluded.

Fresh full RPUs will be authored through pinned libdovi's public Profile 8 serializer. It supports the reuse flag/previous ID and CRC validation but does not resolve a reused mapping by itself. The preferred compressor is FFmpeg's public BSF, not hand-edited encoded bits. Each source RPU has a distinct synthetic `source_min_pq` value so static DM comparison changes; the target cohort has mapping reuse but `dm_compression=0`, retaining independently parseable full DM. This assumption must be checked in actual emitted headers. If the compressor still emits DM compression or libdovi rejects emitted syntax, stop and report the unsupported generator path instead of weakening validation.

Profile 8 mapping A is identity; B is the previously bounded luma affine curve `1/16 + 3x/4`, with identity chroma. IDs stay zero. Distinct tables prevent an unchanged identity cache from masquerading as proof that the requested mapping was resolved. Other matrix/trim semantics are not under test.

## Generator and build prerequisites

Use new scratch only, no repository edits or host/fleet libraries. Verify the actual pinned Rust 1.97.1 compiler and the existing resolved-lock libdovi loop before editing the standalone generator. All source and generated fixtures retain exact hashes and CRC round trips.

FFmpeg remains revision `bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa`, source archive SHA256 `fb1931fd4eb29297ee1c1017a24f800c4d8fbea35b4f2aaeb28308a48a9149b4`. The new isolated minimal build adds `dovi_rpu` BSF (and its selected CBS/encoder dependencies) to the reviewed decoder/filter configuration. No upstream source patch or compliance relaxation is planned. FFmpeg's ordinary encoder configuration explicitly disables compression; therefore the experiment uses the compression BSF rather than claiming an x265 encoder option does it.

libdovi remains `83e1fdad6dcd5995556235946e7c5c0f9010d5a1`, archive SHA256 `57d13f03d04a7a1b3d7dcb139b976dc3748c745c1720d4b6e44b61e5ef67b69b`, with the existing reviewed workspace patch/resolved Cargo lock. Source-generated six-picture 64 × 64 Main10 base inputs and actual VFR/keyframe timing come from the reviewed timeline controls. No EL is present in this Profile 8 cohort; it cannot prove FEL reconstruction.

The replay keeps validated image/dependency identities, new-scratch refusal, disabled bytecode, scratch-local temporary directories, no-network containers, 2 CPU / 2 GiB caps, and actual process status capture. Building/fetching follows the existing source-only dependency workflow; offline execution is mandatory. Static-prefix reuse versus fresh build is recorded honestly.

## Proposed executed controls

| Control | Actual action | Required evidence |
| --- | --- | --- |
| Syntax/config gate | Generate full A/B RPUs, insert into the existing timestamped base, run `dovi_rpu=compression=limited`, mux its output configuration | Re-read actual serialized configuration from the muxed container before decoder creation: profile 8/compression 1/HDR10 compatibility/BL+RPU/no EL; actual key flags; libdovi CRC/header round trips; full headers at keys, explicit ID-0 reuse on non-keys; DM compression zero |
| Same-epoch reuse | Decode the compressed six-picture stream, including B-frame reorder | Native base hashes and actual rational timing; raw RPU for every picture; full/reuse flag and ID from that actual RPU; resolved semantic A mapping for pictures 0–2 and B for 3–5; fresh per-picture DM tag |
| Real seek/reset success | Send two real reordered packets through runtime `dovi_split=bl_rpu`, actual demuxer seek to 200 ms, flush that splitter and the HEVC decoder, read from keyframe 140 ms | Ordered boundary/reset evidence, keyframe full B seed after reset, B reuse resolution thereafter; preroll versus selected frames as in the timeline slice; preserved stored authored intervals |
| Same-context parser proof | Feed actual generated seed and reuse payloads to a small pinned metadata-cache probe | Strict parse/CRC return values, configuration, resolved semantic table digest and cache provenance; actual payload digests; no inference from side-data presence |
| Cold/flush cache failure | Feed the exact valid reuse payload before any seed, and after `ff_dovi_ctx_flush` | Exact negative parser code and `Unknown previous RPU ID: 0` diagnostic; no resolved mapping accepted; sample actual configuration immediately after flush and before the expected failing parse, then again after failure; clear distinction from picture decode status |
| Wrong compression signal | Feed the valid reuse payload with compression set to none under strict parser checks | Exact clean refusal for uncompressed explicit reuse; never count crashes or unrelated errors as this negative |
| No-reset observation | Keep old A cache across a deliberately declared new epoch and feed A reuse | Parser may successfully resolve A; record that old-cache result as expected diagnostic behavior, while the epoch acceptance gate refuses the missing reset. This is not proof that same-epoch reuse itself is invalid |
| Omitted RPU contrast | Remove a non-key RPU after a valid seed in a separate encoded negative | Native decoding may succeed and metadata may persist; raw RPU is absent. Refuse explicit-reuse acceptance even if the resolved mapping happens to match |

The compressor runs only offline before muxing, then is closed. The real seek path uses the reviewed `dovi_split=bl_rpu` extraction filter plus HEVC decoder. Calling `av_bsf_flush` on the splitter is not claimed to clear Dolby mapping/DM state; the decoder flush and private `ff_dovi_ctx_flush` are the cache-reset boundaries. Pinned `dovi_rpu` has no flush callback, so a compressor reused across epochs would require explicit close/recreation and is outside this slice.

The metadata-cache probe is a private pinned-source test boundary, not a production API commitment or a substitute for the real HEVC seek control. It must use decoded RBSP payloads through the pinned NAL parser or documented unescaped writer output. `AV_FRAME_DATA_DOVI_RPU_BUFFER` preserves escaped bytes; those must not be passed directly to the private parser as decoded data.

## Acceptance and failure receipts

Each accepted decoded frame binds container, packet/display association, actual PTS/duration, native base, actual raw RPU, parsed full/reuse header, prior full mapping seed and resolved semantic mapping/DM fields. Semantic mapping comparison reduces public numeric fields/rationals; it does not hash struct padding. Expected A/B records come from the authored coefficients independently of decoder output. A raw RPU plus a cached mapping is accepted only when explicit reuse was parsed and the referenced seed is valid in the same epoch.

Ordered typed/finite evidence checks must reject late or duplicated reset/seed events, missing seed dependencies, altered reuse IDs/tables, overflow values, booleans in integer fields, missing raw RPU, wrong process status/diagnostic, and crash after complete artifact writes. Controls use exact expected stages/reasons; a decoder status zero alone is insufficient.

Pinned HEVC deliberately ignores metadata parser failure after clearing the raw RPU and logging a warning. Therefore parser failure may coexist with successful picture decoding. Record both layers of evidence rather than promising a fatal decoder exit. A compression-guard failure can leave earlier cached mapping/metadata present; record the actual getter/cache state but never classify a negative parser return as success. Unknown-cache failure can clear configuration through context unreference; recovery after that error is outside this first slice, and failures use separate contexts unless explicitly recorded.

Generated syntax/CRC acceptance and the second parser's resolution establish bounded implementation behavior only. No public Dolby-authoring oracle, certified container/profile compliance, general reuse validity, real film quality, mapper/trim parity, physical display result, production cache adapter, fallback or badge qualification is claimed. An unsupported generator/config path is a reported M0 gap, not permission to invent compressed syntax.

## Primary source anchors

- [FFmpeg compression BSF](https://github.com/FFmpeg/FFmpeg/blob/bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa/libavcodec/bsf/dovi_rpu.c#L68): non-key compression, full key metadata and profile guards.
- [FFmpeg limited mapping reuse](https://github.com/FFmpeg/FFmpeg/blob/bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa/libavcodec/dovi_rpuenc.c#L700): ID-0-only limited reuse; static DM comparison/generation.
- [FFmpeg strict parser](https://github.com/FFmpeg/FFmpeg/blob/bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa/libavcodec/dovi_rpudec.c#L505): profile/compression guards, cache lookup/failure and optional CRC checking.
- [FFmpeg HEVC metadata error handling](https://github.com/FFmpeg/FFmpeg/blob/bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa/libavcodec/hevc/hevcdec.c#L3753): raw RPU removal and nonfatal metadata parse error; flush near line 4187.
- [libdovi header](https://github.com/quietvoid/dovi_tool/blob/83e1fdad6dcd5995556235946e7c5c0f9010d5a1/dolby_vision/src/rpu/rpu_data_header.rs#L240): reuse flag/previous-ID serialization.
- [libdovi writer/reader](https://github.com/quietvoid/dovi_tool/blob/83e1fdad6dcd5995556235946e7c5c0f9010d5a1/dolby_vision/src/rpu/dovi_rpu.rs#L248): mapping omission, fresh DM and CRC handling.
