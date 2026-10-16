# Subtitle recognition — reproduce quality and CPU measurements without overstating them

**Status:** open; bounded component measurements, human and service acceptance outstanding · **Written:** 2026-10-15

Companion to the [managed generation plan](SUBTITLE-GENERATION-USABILITY-IMPLEMENTATION.md#6-qualification--what-usable-must-prove).
This document owns the reproducible corpus, measurement limits and observed
recognition failures. The canonical plan owns implementation and overall
completion. These offline measurements do not qualify Settings installation,
durable worker execution, cluster placement, foreground playback or clients.

## Run the bounded offline diagnostic

`scripts/qualify-subtitle-recognition` downloads nothing and writes a new
receipt directory. Supply the qualified package, a pinned catalog model and
an absolute FFmpeg executable. Each child receives only `LC_ALL=C`, held
source/model descriptors, two CPU threads, one processor and `--no-gpu`.
Decode uses the production mono 16 kHz signed-16 PCM clock filter and strict
FFmpeg error policy. The diagnostic executes the verified public package
path; it does not replace the daemon's private native executable handoff,
resource admission, identity fencing, Stop or durable publication tests.

```bash
# Short speech, controlled noise and digital silence; no external downloads.
python3 scripts/qualify-subtitle-recognition \
  --engine-package /tmp/qualified-subtitle-engine \
  --model /tmp/ggml-base.en.bin --decoder /tmp/qualified-ffmpeg \
  --output /tmp/subtitle-recognition-short

# One committed independently referenced German utterance; no download.
python3 scripts/qualify-subtitle-recognition \
  --engine-package /tmp/qualified-subtitle-engine \
  --model /tmp/ggml-base.bin --decoder /tmp/qualified-ffmpeg \
  --cases german-aligned --output /tmp/subtitle-recognition-german

# Pre-obtain the exact external files listed in the corpus catalog.
# Multilingual base is required for the German case.
python3 scripts/qualify-subtitle-recognition \
  --engine-package /tmp/qualified-subtitle-engine \
  --model /tmp/ggml-base.bin --decoder /tmp/qualified-ffmpeg \
  --corpus-root /tmp/subtitle-corpus --cases german film \
  --output /tmp/subtitle-recognition-external

# Prepared two-hour repeated-speech stress case, not a film-quality case.
python3 scripts/qualify-subtitle-recognition \
  --engine-package /tmp/qualified-subtitle-engine \
  --model /tmp/ggml-base.en.bin --decoder /tmp/qualified-ffmpeg \
  --cases stress --stress-seconds 7200 --inference-timeout 3600 \
  --output /tmp/subtitle-recognition-stress
```

The stress generator streams repeated PCM to disk rather than allocating a
feature-length waveform in memory. The prepared two-hour input is
230,400,044 bytes, SHA-256
`6ac4c32a2f586fd626408b6c44097e6613f15004ec98299b9a01a21615763a24`;
generation and hashing are complete, but inference is not executed. Duration is bounded to four hours, PCM
to the production ceiling, inference to the explicit timeout and WebVTT to
256 KiB. Bounded pipe readers cap each child log at 256 KiB. A timeout or
failure kills and reaps the process group before removing private scratch.
Output-file growth is sampled every 50 ms; samples can understate transient
scratch peaks. A successful command means measurements were retained, not
that recognition passed a quality threshold. Inspect `structural_valid`,
error counts and limits in `receipt.json`.

The receipt binds the driver and corpus hashes, Git revision/dirty state,
actual platform engine digest, model and decoder digests, input hashes,
profile, wall time, child CPU, effective UID and VTT. Model hashing before
inference warms the filesystem cache. These are fresh processes with warm
file caches, not cold-start or concurrent-load measurements. `child_maxrss`
is the driver's cumulative child high-water mark, not an isolated per-stage
RAM measurement. RTF is wall seconds divided by input audio seconds; lower
than one is faster than the audio on this host and workload only.

## Corpus rights and references are separate from recognizer output

The committed [catalog](../../tests/fixtures/subtitle-recognition/catalog.json)
pins source URLs, bytes, SHA-256, language, reference provenance and recipes.
External media stays outside the repository; the driver never fetches it.
Obtain media only where its stated rights apply, retaining the original
credits. No private library media is searched or uploaded.

| Case | Source and purpose | Reference and limit |
|---|---|---|
| `clean` | The committed 11-second JFK archival excerpt; upstream MIT and US government speech provenance are retained with the packaged fixture. | Independent 22-word speech text. No word/cue alignment or human audio review. |
| `noise` | The same speech plus deterministic LCG32 uniform noise at target 10 dB SNR; seed and algorithm live in the driver. Clipped samples are counted. | Same text. Controlled additive noise does not represent film music, room acoustics or overlapping speakers. |
| `silence` | Eleven seconds of exactly zero PCM, mono 16 kHz s16. WAV SHA-256 `1a092b23d9dfe64d53943d18262c0a49eaf32c5ab5f2b016cd8edd16ec9440dd`. | Empty speech reference. Any recognized word is fabricated; non-speech labels are reported without silently deleting them. |
| `german-aligned` | One complete 12.96-second German read-speech utterance from official [Google FLEURS](https://huggingface.co/datasets/google/fleurs), licensed CC BY 4.0. Attribution: Conneau, Ma, Khanuja, Zhang, Axelrod, Dalmia, Riesa, Rivera and Bapna, *FLEURS* (2022), Google. | Independent publisher-provided utterance text, selected before recognition. No local human listening, semantic scoring or cue-aligned reference. One utterance cannot establish German-language quality. |
| `german` | First 90 seconds of Markus Wachenheim's *Die Verwandlung*, including its spoken introduction. [LibriVox](https://librivox.org/die-verwandlung-von-franz-kafka/) provides the German narration and [dedicates recordings to the public domain](https://librivox.org/pages/about-librivox/); its catalog describes US public-domain status. | An independently sourced [original book paragraph](https://www.gutenberg.org/ebooks/22367) is committed for later review. It is not aligned to the excerpt, so the driver reports no German WER. Recognition output is never promoted into its own reference. |
| `film` | The complete 734.167-second official Blender *Tears of Steel* 720p short film, including credits. [The film and published project data use CC BY 3.0](https://mango.blender.org/sharing/); attribution: **(CC) Blender Foundation \| mango.blender.org**. | Publisher English SRT, also pinned and attributed. It omits audible material and contains on-screen text; mismatch against it is a diagnostic comparison, not audited ASR WER. The separately licensed soundtrack album is not used. |
| `stress` | Repeated JFK PCM, configurable to 14,400 seconds. | Synthetic bounded-load input only. Repetition cannot establish dialogue accuracy, diverse acoustics or feature-film quality. Two-hour inference and near-limit runs remain unexecuted. |

The German source is 39,452,911 bytes, SHA-256
`96b670c97fc93e794387066699bb7a60e0c7fc514c237060a72a689b77e41beb`.
The film is 372,178,639 bytes, SHA-256
`efa9062d9cdb7a338e40ad530dfdf234806743f29ae6a1a136b97ece4e588e8f`.
The English SRT is 4,764 bytes, SHA-256
`ce9578f6fe098a5821045a38884b229160f9e7ba27890448a92f5be330c6e6fe`.
Source URLs are not immutable revisions; the driver refuses bytes that differ
from these reviewed hashes rather than silently accepting a replacement.

The FLEURS audio, publisher reference TSV and license card use immutable
revision `70bb2e84b976b7e960aa89f1c648e09c59f894dd`. The German development
archive's actual 227,694,463 bytes verified against authoritative LFS SHA-256
`72cac13bbddbddf674d8102a9e7b90a3b1db5d777ed92e88eccc5ae69cb8fc9d`.
Selection took the first regular audio member with a matching reference,
before running recognition: `dev/10031412691154736538.wav`, dataset ID1586.
Only that 829,498-byte source is committed, SHA-256
`1942763aa0b6903ba0305fe95d665497efdd75aacbb605f79d6484ac6470b33d`.
Its [reference record](../../tests/fixtures/subtitle-recognition/fleurs-de.reference.json)
retains independent source text and TSV/license-card hashes. The source
float PCM is unchanged; FFmpeg performs the production s16 conversion.

## Observed baselines include failures

Initial macOS arm64 measurements used an Apple M3 Max (16 physical cores,
64 GiB memory), whisper.cpp 1.9.2, the pinned portable
CPU profile and qualified executable SHA-256
`43b757eda907535bc13789db47f7316a5571ce32c5c7b714a2746bb9fe84679b`.
Both models were the immutable catalog downloads. The decoder was FFmpeg
9.0.2; its exact hash and the sampling/runtime metrics are in the receipts.
The committed [baseline receipts](../../tests/fixtures/subtitle-recognition/baseline-receipts.json)
retain the historical driver/corpus hashes and original text observations;
the [film fixture provenance](../../tests/fixtures/subtitle-recognition/base-film.provenance.json)
binds raw output to the actual media/model and exact decoded sample count.
The command processed one case at a time, without deliberately imposed
foreground playback. Other developer activity, including Rust builds, was
not isolated; these are not idle-host benchmarks. These are single
observations, not medians or a
hardware-independent performance promise.

| Model / case | Inference seconds / RTF | Text result | Timing/quality limit |
|---|---|---|---|
| `base.en` clean | 1.822 / 0.166 | 0/22 word errors | One cue; no human timing reference. |
| `base.en` noise | 1.816 / 0.165 | 0/22 word errors | Controlled noise only. |
| `base.en` silence | 1.557 / 0.142 | **Failure: “You” from zero samples.** | Structurally valid 0–2 s cue does not make text truthful. |
| `base` clean | 1.790 / 0.163 | 0/22 word errors | Short English sample only. |
| `base` noise | 1.772 / 0.161 | 3 substitutions/22 words, 13.64% WER | “Americans”/“ask” changed; do not infer universal language quality. |
| `base` silence | 1.629 / 0.148 | Emits `[BLANK_AUDIO]` | Original raw label retained. |
| `base` German | 11.650 / 0.129 | German text produced, including book content; e.g. reference “Gregor” appears as “Krieger.” | No exact spoken reference alignment or human-scored quality. |
| `base` German aligned | 2.126 / 0.164 | 6 edits/28 reference words, 21.43% lexical WER (1 deletion, 2 insertions, 3 substitutions). | Mixed English phrase “Did not Finish” and German wording misrecognized. Publisher reference is independent; local listening/timing review remains unperformed. |
| `base.en` film | 115.407 / 0.157 | Publisher-reference mismatch 41.60%: 8 deletions, 79 insertions, 64 substitutions / 363 reference words. | Raw tail 730.920–738.980 s exceeds decoded EOF. |
| `base` film | 76.413 / 0.104 | Publisher-reference mismatch 50.14%: 8 deletions, 114 insertions, 60 substitutions / 363 reference words. | **Timing failure:** final raw cue 733.400–743.400 s exceeds exact decoded PCM duration 734.0930625 s. Publisher text is not a verbatim transcript; all insertions cannot be called hallucinations. |

The film output contains repeated counts, sound labels and `[Birds chirping]`.
These need comparison with the actual source before classifying individual
sections as hallucinations. The short clean sample cannot establish the
film's semantic correctness. Raw captions are retained without filtering.

The digital-silence control also tried existing engine options: lowering
`--no-speech-thold` to `0.1` alone and `--suppress-nst` each still emitted
“You.” Combining `--logprob-thold 0 --no-speech-thold 0.1` produced empty
WebVTT, but changes omission behavior and is not the production profile.
The approved corrective scope is a bounded, cancellable all-zero PCM guard
after real successful decode. Its actual runtime regressions must pass;
it does not solve room tone, music or silence embedded in speech. Strict
caption validation remains required. The approved managed-only EOF policy intersects cues with actual decoded
PCM time: clip straddling cues to floor milliseconds, discard cues wholly
after EOF and reject malformed, decreasing-start or empty results. It
preserves text and ordered overlaps. The original raw failures remain in
evidence alongside normalization counts and output hashes; this is no
semantic cleanup. Advanced recipe behavior remains unchanged.

## Proposed review thresholds do not close acceptance

For these exact short samples, propose clean WER ≤5%, controlled-noise WER
≤20%, zero words from exact digital silence and 100% finite, ordered,
within-source cue bounds. They are review proposals from the observations,
not universal release bars. No numeric human timing or movie-quality bar
is asserted without an independently aligned reference and review.

To close the recognition rows in the canonical plan, retain human-reviewed
film/dialogue omissions, negations, names, invented speech and cue alignment;
extend the single independently referenced non-English utterance with
locally reviewed language/timing cases;
measure representative episode/feature duration and a near-limit synthetic
case; and separately test foreground waiting/yielding, service installation,
durable publication and client selection. A source can remain a useful
diagnostic while these acceptance rows stay open.
