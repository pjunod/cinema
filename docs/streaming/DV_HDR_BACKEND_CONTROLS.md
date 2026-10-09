# Dolby Vision backend controls — reproduce the first real pixel experiments

**Status:** open — bounded M0 mechanics built and reviewed; playback unqualified ·
**Updated:** 2026-10-08

Companion to the [build handoff](DV_HDR_PROCESSING_BUILD.md) and
[reuse investigation](DV_HDR_PROCESSING_FEASIBILITY.md). This records two
isolated backend experiments and their source/evidence bundles. It does not
replace the required encoded-input, timing, quality or product qualification.
The bundles are research snapshots, not production dependencies. Subsequent
[parsed-metadata and authoring controls](DV_HDR_AUTHORING_CONTROLS.md) retain
the next experiments without broadening these original receipts.

## 1. What actually executed

| Backend | Executed operation | Observation | Unproven boundary |
|---|---|---|---|
| Pinned libplacebo API374 on Linux ARM64 software Vulkan | Direct structured Dolby metadata plus BL/EL textures, pre-map export and full render/export | Ten 16×16 known-answer comparisons pass; maximum normalized PQ error `6.185e-6`, RGB48 error at most one code | No HEVC/RPU parsing, half-raster chroma, decoded timestamp association, vs-placebo plugin execution or hardware throughput |
| Pinned DoViBaker CPU processor on Linux amd64 | Generated P7 RPUs, direct sample reconstruction and RGB48 export; full plugin also compiled | Five exact integer-reference comparisons pass; missing EL and truncated RPU reject | Plugin was not loaded into AviSynth; no decoded video/layer association or independent DV reference |

The GPU controls cover zero, nonzero, omitted, spatially shifted and disabled
residuals, with separate float and integer output checks. Shifted output has
its own positive reference and also fails the unshifted reference. Successful
render dispatch alone is insufficient: an initial harness subshader-identifier
collision silently omitted the residual, which the arithmetic controls caught.
The namespace fix is a harness correction, not an established upstream bug.

CPU controls cover nonzero, zero, disabled, bounded residual and adapted-RPU
base behavior. Its matrix mutation is deliberately an **unsupported-domain
finding**: changing `rgb_to_lms` leaves output unchanged. The backend cannot
be qualified for arbitrary Profile 7 matrices. Adapted RPU on the original
base matches the base-only control; this demonstrates disabled residuals,
not a valid newly reconstructed P8.1 picture.

These analytic references test specified equations and quantization. They do
not establish fidelity to an independent Dolby movie render or quantify a
universal quality gain. Full DV creative trims remain outside the demonstrated
subset. All product qualification fields remain false or null.

## 2. Run isolated source recipes

Use an existing Linux Docker daemon and a Python interpreter with tarfile's
safe data-extraction filter. Network access downloads pinned public source
archives and container build dependencies. No checkout history or credentials
are transferred; host and ROG libraries are not replaced.

```bash
scratch=$(mktemp -d)                     # create an evidence parent
scratch=$(cd "$scratch" && pwd -P)       # use its physical path
sh tools/dv_quality/backends/libplacebo/replay.sh "$scratch/gpu"
cp -R tools/dv_quality/backends/dovibaker "$scratch/cpu"
bash "$scratch/cpu/replay.sh"            # CPU replay writes into this copy
```

Do not run the CPU replay in the tracked bundle: its interface operates in a
fresh copied evidence directory and writes build/results beside its scripts.
The GPU interface instead requires a new scratch path outside its source
bundle. Neither replay changes production routing, installs a host driver or
makes its image eligible for fleet use.

The GPU container requests Linux ARM64, three CPUs and 4 GiB memory. The CPU
container requests Linux amd64, two CPUs and 2 GiB memory. The CPU receipt
records both requested platform and observed runtime without inferring
emulation from the client computer's architecture. Neither measures admission
or real-time throughput.

**Dependency limit:** base image and source archive identities are pinned,
installed Debian package versions and image/binary hashes are recorded, and
Python build tools have version pins. Live signed Debian and PyPI resolution
can still drift; these recipes are not snapshot-locked release builds. A new
build receives its own receipt and cannot inherit old qualification by name.
CPU source needs the explicitly retained workspace/lock adjustment; original
and resolved locks and the diff are included. No upstream runtime patch is
claimed. Read the GPU [recipe](../../tools/dv_quality/backends/libplacebo/README.md)
for its exact API374/Vulkan-Headers boundary.

## 3. Read and verify the retained evidence

| Bundle | Durable material |
|---|---|
| [libplacebo](../../tools/dv_quality/backends/libplacebo/bundle-hashes.json) | Source fetch/hash checks, Docker/build/replay recipe, readable C probe, Decimal references, finite-data checker, negative tests, dependency identity and slim actual receipt |
| [DoViBaker](../../tools/dv_quality/backends/dovibaker/SHA256SUMS.json) | Source pins, build recipe and lock adjustment, generated legal RPUs, tiny actual/reference RGB48 frames, CPU driver/checker, replay receipt, failure logs and licenses |

The GPU source bundle regenerates raw frame artifacts under scratch rather
than committing them. The CPU bundle retains tiny generated frame and metadata
controls. Neither vendors complete upstream source archives, build products,
Docker layers or credentials. Source/download and generated-artifact hashes
are different claims; the receipts label both. The CPU metric snapshot is
retained historical evidence, not a second product metric implementation.

**How to read the results:** a checker success means only that every declared
control met its explicit arithmetic/error bound and its expected failure state.
NaN, infinity, wrong output size, missing output and contradictory control
outcomes fail. New output receipts bind actual build/artifact identities; old
results are not silently promoted to a rebuilt dependency stack.

## 4. Review and local evidence

Adversarial review required five corrections: a retained CPU builder recipe,
executable CPU known-answer/adapted-RPU checks, finite GPU float validation,
a positive shifted-integer reference, and scratch-only GPU reference generation.
Both corrected bundles passed fresh isolated replay and independent recheck.
GPU checker regressions passed 11 cases; CPU checker regressions passed eight.

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s tools/dv_quality/backends/libplacebo -p test_check_probe.py -v
PYTHONDONTWRITEBYTECODE=1 python3 \
  tools/dv_quality/backends/dovibaker/check_cpu_outputs_selftest.py -v
```

Approval covers only these bounded synthetic mechanisms. Next work must bridge
real parsed RPUs and encoded dual-layer inputs, preserve actual decoder time
bases/order, encode and validate destination metadata, establish independent
DV-on/DV-off references, and measure the whole graph on representative hardware.
Existing HDR10 and compatible P8.1 fallbacks remain the product behavior.
