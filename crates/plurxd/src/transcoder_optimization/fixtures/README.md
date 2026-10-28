# Comparison fixtures — fixed real-footage decode and processing inputs

**Status:** live · **Prepared:** 2026-10-20 · **Fixture version:** 1.

Ten seconds of *Sol Levante* (2020), by Netflix and Production I.G, are adapted
under [Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/).
The [official Netflix Open Content page](https://opencontent.netflix.com/)
identifies the title, its HDR master and the license. This use does not imply
endorsement. The full license is in [licenses/CC-BY-4.0.txt](../../../../../licenses/CC-BY-4.0.txt).

The three committed Matroska clips contain the same source frames 1440–1679
(60–70 seconds, 240 frames at 24 fps), with no audio. The UHD master is genuine
BT.2020/PQ footage. The SDR adaptations use zscale and Hable tone mapping;
1080p is resized and encoded as H.264, while the 4K variants use 10-bit HEVC.
All clips are lossy re-encodes; no mastering-display or content-light metadata
was invented. These clips exercise throughput and backend completion, not a
picture-fidelity acceptance corpus.

[manifest.json](manifest.json) pins exact sizes, SHA-256 hashes, original URL,
attribution, preparation versions and adaptations. The total embedded payload
is 26,367,959 bytes. The daemon materializes and verifies these trusted bytes
in its local runtime cache. It does not download a master, accept arbitrary
media URLs or include acquisition/preparation in timing.

## Regenerate in scratch storage

[prepare.sh](prepare.sh) records the successful preparation stages. It seeks
remotely into the official 37.4 GB ProRes master and retains only the excerpt;
network range reads can be slow. Never run this during normal daemon startup
or from an ordinary System read.

```bash
export HB_FFMPEG=/path/to/ffmpeg-9.0.2           # source and bounded re-encodes
export JELLYFIN_FFMPEG=/path/to/jellyfin-ffmpeg  # 8.1.3-Jellyfin SDR derivation
export FIXTURE_WORK=/path/to/empty/scratch      # outputs and intermediates
bash crates/plurxd/src/transcoder_optimization/fixtures/prepare.sh
```

Use ffprobe with frame counting to verify 240 decoded frames, 24/1 fps,
10 seconds, codec, dimensions and the manifest's color metadata on each final
file. Matroska identities, encoder versions and other muxer details can change
bytes even when preparation arguments match. Committed clips and their hashes
are canonical; regeneration requires reviewing new hashes, updating the
manifest and bumping the comparison fixture version. It does not promise
byte-identical regeneration.
