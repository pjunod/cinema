# Explicit RPU reuse controls

**Status:** exact-source offline replay 2 passes 27 checker controls and four actual late-failure controls. Adversarial result review is pending. This is a bounded implementation experiment, not playback or Dolby conformance qualification.

## What this slice proves

The public pinned FFmpeg `dovi_rpu` filter runs offline and emits limited mapping compression. The actual serialized configuration, re-read from the Matroska container before decoder creation, is **Profile 8, compatibility ID 6, compression 1, base plus RPU, no enhancement layer**. All six actual emitted RPUs pass pinned libdovi parsing and CRC validation. Key pictures 0 and 3 have full mapping tables; pictures 1, 2, 4 and 5 explicitly reuse mapping ID 0. Static DM compression remains zero.

This combination is described by its observed fields. It is **not labelled P8.1**, and parser acceptance does not establish that the configuration is a standards-conforming HDR10-compatible destination. The approved design's proposed “HDR10-compatible” description was an inference not established by the experiment. The base is independently observed as Main10 with BT.2020/PQ tags; those tags do not validate the DV profile/compatibility combination. This limitation does not authorize changing configuration checks or widening the cohort.

The six base pictures and B-frame reorder come from the approved synthetic timeline slice. Actual display PTS are 0, 40, 110, 140, 230 and 300 milliseconds, with authored intervals 40, 70, 30, 90, 70 and 41 milliseconds. The terminal 41 ms interval is a synthetic policy. The wrapper copies the encoder's packet PTS and supplies those explicit authored durations; initial missing DTS are preserved and produce a Matroska timestamp warning during muxing. This is not a claim that unspecified source DTS or nominal durations were recovered.

Strict HEVC decoding through runtime `dovi_split=bl_rpu` resolves identity mapping A before the second key and the luma affine mapping B (`1/16 + 3x/4`) afterward. Chroma remains identity. Native decoded picture bytes match the independent source input exactly. Each picture has a distinct synthetic static `source_min_pq` tag and L1 block, preventing old metadata from masquerading as a fresh RPU. These are association markers, not content analysis or display metadata qualification.

## Reset and refusal evidence

| Control | Expected observation | Acceptance |
| --- | --- | --- |
| Same epoch | Actual full A seed followed by valid explicit ID-0 reuse resolves A | Accepted within this cohort |
| Actual seek | After two reordered packets, `avformat_seek_file` to 200 ms plus splitter and HEVC flush; 140 ms full B key seeds the new epoch, then 230 and 300 ms reuse B | Accepted; 140 ms is recorded as preroll |
| Cold cache | The same valid reuse RBSP without a seed returns `AVERROR_INVALIDDATA` and `Unknown previous RPU ID: 0` | Controlled parser refusal |
| Flushed cache | Configuration remains Profile 8/compression 1 immediately after `ff_dovi_ctx_flush`; the same valid reuse then fails for unknown ID and clears configuration through unreference | Controlled parser refusal |
| Wrong compression | Seed A, set compression to none, parse valid reuse; exact uncompressed-reuse diagnostic and invalid-data return, despite old A getters remaining visible | Controlled parser refusal |
| Declared new epoch without reset | Parser still resolves cached A from the old epoch | Diagnostic parser success; epoch acceptance refused |
| Omitted RPU | At 110 ms picture decoding succeeds, raw RPU is absent, cached identity metadata and L1 max 4095 remain | Explicit-reuse acceptance refused |
| Profile 7 compression | Public BSF initialization returns `-22` with the exact profile guard before creating an output container | Controlled authoring refusal |

The compressor is closed after offline generation. Pinned `dovi_rpu` has no flush callback: this slice makes no claim that flushing it clears encoder caches. The real seek uses `dovi_split` and HEVC flush. The private cache probe uses the pinned `ff_dovi_ctx_flush` boundary and the actual serialized configuration from the container. It records state immediately before and after the expected parser error. These private symbols are not a production API commitment.

The real seek starts again at the 140 ms key. The 140 ms display interval contains the requested 200 ms point; choosing later frames as selected output is only this fixture's policy. The experiment does not qualify general presentation selection, seek recovery after metadata errors, multiple mapping IDs, ID fallback, extended compression or compressed DM.

## Reproduce in new scratch

Run `replay.sh NEW-scratch verified-input-replay verified-offline-Cargo-registry`. The input replay supplies the checksum-bound base/container and public source archives plus `image-id.txt`; the registry directory contains only the captured public Cargo registry cache/index files. Its full file manifest is checked before and after copying, and each crate archive is checked against the pinned resolved Cargo lock. No credential, Git checkout or host library is used.

The script refuses an existing destination, validates the prerequisite image digest and Linux ARM64 platform, and builds a new static FFmpeg prefix from the pinned archive. Rust 1.97.1 is checked explicitly. Compilation and execution containers have no network, 2 CPU / 2 GiB caps, explicit bytecode suppression, scratch-local temporary files and actual process status capture. The Docker image was originally built with live apt/pip dependency resolution, so source pins do not imply deterministic system dependency rebuilding. A different image requires its own recorded identity and review.

`execute_stage.py` records exact decode/cache invocations, consumed container/RBSP/helper hashes before and after execution, actual status and immediate output hashes. The checker refuses replacement inputs or tools alongside stale successful outputs. `check_reuse.py` checks exact ordered event sequences, current native input identities, actual emitted versus decoder-attached raw RPU bytes, parser-output bindings, typed public semantic mapping fields, fresh DM tags, same-epoch full seeds, exact negative return/diagnostic stages and process statuses. It refuses floats, overflow, NaN, booleans in integer fields, late resets, missing drains and stale bindings. The lifecycle schedule is a bounded regression expectation for this pinned single-threaded synthetic decoder, not a universal decoder scheduling rule.

`test_reuse.py` exercises false acceptance mutations. `test_driver.py` executes the actual decoder and then injects exit 37 or SIGSEGV after complete artifact writes in missing-RPU and seek cases. The shell must preserve status 37/139 and omit its clean success marker; the checker refuses before reading those otherwise complete artifacts.

## Source and remaining limits

FFmpeg revision `bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa` (9.0.1) is built without source patches, adding the public compression BSF to the previous minimal decoder configuration. libdovi revision `83e1fdad6dcd5995556235946e7c5c0f9010d5a1` uses the previously reviewed appended workspace and resolved Cargo lock. `generate_reuse.rs` authors full source metadata through the public serializer. `inspect_reuse.rs` validates actual emitted NALs and obtains unescaped RBSP through the public writer for the private cache probe. FFmpeg is LGPL 2.1-or-later; the linked libdovi library is MIT. Dependency licenses remain in their extracted upstream sources; no dependency code or binaries are vendored in this bundle.

The independent input SHA binds the previously source-generated synthetic pixels. It is not an independent Dolby authoring oracle. No GPU reconstruction, target mapping, trims, FEL, general Profile 7 reuse, certified container/profile compliance, real-film fidelity, device interoperability, physical display, throughput, production fallback or HDR10-E badge qualification is established here. Earlier approved evidence remains frozen and is not retroactively extended by these controls.

The retained fresh command is:

```sh
sh bundle/replay.sh /tmp/plurx-dv-m0-rpu-reuse-replay2 \
  /tmp/plurx-dv-m0-rpu-reuse/runtime1 \
  /tmp/plurx-dv-m0-rpu-reuse/offline-cargo-registry
```

The first prerequisite is a checksum-bound source/input collection, not a reused FFmpeg library prefix. Fresh static FFmpeg and standalone libdovi example builds ran inside the verified prior Docker image with copied verified Cargo cache/index bytes. `artifact-inventory.json` declares exactly which runtime evidence is inventoried and which upstream/build trees are excluded; it does not claim to inventory every compiler intermediate. Actual native frames, raw RPUs, containers, stage receipts, tools and cache archives remain in scratch, with their hashes recorded. The small `traces/` copies are text evidence, never replacement runtime inputs.

## Retained prerequisite package

This is a fresh-scratch replay from retained, verified prerequisites. It is **not a source-only bootstrap** of the prior Docker image or its sparse Cargo index snapshot. Obtain `plurx-dv-rpu-reuse-prerequisites.tar` from the effort's retained evidence artifacts and place it in a local directory. The archive is **82,626,560 bytes**, SHA256 `d405ec20062c294d74c8e2377068cc0d6798c1f6573d26170e8d95e790326154`. The local preserved copy is outside this source bundle; it contains only the four checksum-bound public source/fixture inputs, `image-id.txt`, and the exact 393 public registry cache/index files. It contains no Docker image contents, tool binaries, Git metadata or credentials. `prerequisites.json` lists all 398 file hashes. The actual safe extraction verified all 398 identities; ten focused unpack tests pass.

```sh
python3 bundle/unpack_prerequisites.py \
  /path/to/plurx-dv-rpu-reuse-prerequisites.tar /tmp/NEW-prerequisites
sh bundle/replay.sh /tmp/NEW-replay \
  /tmp/NEW-prerequisites/inputs /tmp/NEW-prerequisites/registry
```

The image ID in the package must already be available locally. Its original prior-bundle Dockerfile is the build recipe, but live apt/pip rebuilding can produce another identity and requires separate review; this archive does not solve that bootstrap. Likewise, `cargo fetch --locked` originally populated the public cache, but fetching today's sparse index is not promised to reproduce these exact index bytes. The preserved prerequisite package makes that inherited state explicit. The timeline source fixture and source acquisition provenance remain in the approved predecessor recipes and the checksum-bound source archives.

The packaging/unpack scripts, manifest, extraction result and unpack tests were added after replay 2. They do not alter its executed decode/cache source or results. Only safe-unpack controls and actual extraction were rerun for this documentation/packaging delta; no new FFmpeg execution is claimed.
