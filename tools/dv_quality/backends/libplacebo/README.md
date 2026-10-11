# Direct FEL export probe — reproduce the bounded renderer controls

**Status:** partial M0 evidence · **Updated:** 2026-10-08

This source bundle builds the reviewed libplacebo API374 revision in an
isolated Linux ARM64 Docker container and runs five 16x16 synthetic controls.
It proves structured-metadata EL composition and CPU-readable frame export.
It does not decode HEVC, parse an RPU, qualify P8.1 or demonstrate production
throughput. The vs-placebo 2.0.3 plugin was surveyed but not built or executed
in this bounded run; its source-wrap API371 is a separate dependency pin.

## Run the isolated controls

You need an existing Linux Docker daemon and Python on the host. Supply a new
scratch directory with an existing parent, outside this source bundle. Replay
refuses existing directories and symlinks so evidence is never overwritten.
The script writes only its selected scratch directory and creates temporary containers.
It installs build dependencies inside the image; it does not replace host,
ROG or production libraries.

```bash
sh replay.sh /private/tmp/plurx-dv-m0-replay  # Download checked source and run the controls
```

The build uses libplacebo `0d043c7f6f79cd3687c023454bdacbe615e4d96f` and its
Vulkan-Headers submodule `74d8a6cb930c68ef617b202c3ff3c59d919e086b`. No upstream
source patch was applied. Vulkan-Headers1.4 was needed to compile API374;
the tested software device runs Vulkan1.3.230. glslang, Dolby reshaping and
Vulkan are enabled; shaderc, OpenGL, D3D11, libdovi and demos are disabled.
The compiler denies warnings on the standalone probe.

**Dependency-resolution limit:** the base-image index and source archives are
pinned, Python build packages have explicit versions, and installed Debian
versions and the tested image/library digests are recorded. Debian dependencies
were resolved from signed live bookworm repositories and pip wheel hashes were
not locked. A fresh image rebuild can drift. This is a reproducible source
recipe with a recorded build identity, not fully snapshot-locked dependency
resolution. Preserve the tested image identity or pin repository snapshots
before using a rebuilt image as equivalent qualification evidence.

## Read the output as controls, not quality claims

`probe.jsonl` records texture binding, NLQ enablement, export success and
renderer error state. `render-receipt.json` combines these observations with
known-answer checks. It always sets encoded P7, RPU parsing and production
qualification false.

| Control | Expected result |
|---|---|
| Zero EL residual | Base preserved with EL bound and NLQ enabled |
| Nonzero EL | Alternating signed PQ residual `-0.025`/`+0.025` |
| Omitted EL | Base preserved; differs from full-residual reference |
| EL shifted horizontally | Matches shifted arithmetic; fails the unshifted reference |
| NLQ disabled | Base preserved despite EL binding |

The direct public `pl_shader_decode_color_ex` path emits pre-display-mapping
BT2020/PQ RGB. The full `pl_render_image` path emits a separate frame using an
explicit matching 10,000-nit target with clip tone/gamut functions; it performs
no intentional target compression for these in-range controls. Both export
RGBA32F and RGB48LE. The probe verifies binary32, host byte order, texture
component order and precision before reading pixels. RGB48 is written with
explicit little-endian bytes. The independently authored Decimal reference
covers the declared identity arithmetic only.

Ten known-answer checks passed on software Vulkan. Maximum float PQ error was
`6.18505477906206e-06`; integer exports differed by at most one RGB48 code.
Omitted/disabled controls exactly matched the zero-residual output and failed
the full-residual reference. The one-code and float bounds are diagnostic
control tolerances, not held-out content quality thresholds.

The checker rejects non-finite components in every input/output plane, including
alpha, and invalid non-finite/overflowed JSON numbers. All JSON output refuses
NaN. The shifted integer export is checked against its own Decimal reference.
Eleven focused checker/lifecycle regressions passed:

```bash
python3 -m unittest discover -s . -p test_check_probe.py  # Run from the source bundle
```

A harness error initially assigned BL and EL subshaders the same identifier
namespace. Dispatch succeeded while residual composition was omitted. Using
a distinct EL namespace through public `pl_shader_alloc(... id=200)` corrected
the harness. This is why success alone is not accepted as evidence. It is
not an established upstream bug.

## Files to retain

| File | Job |
|---|---|
| `fel_export_probe.c` | Bounded renderer inputs and both export interfaces |
| `fetch_sources.py` | Verify archive hashes and safely extract pinned source |
| `Dockerfile`, `build.sh`, `run-probe.sh`, `replay.sh` | Isolated dependency/build/execution recipe |
| `identity_reference.py` | Separately authored exact Decimal reference |
| `check_probe.py` | Known-answer, negative controls and comparison manifests |
| `test_check_probe.py` | NaN/Inf/truncation, shifted integer and scratch lifecycle regressions |
| `build-lock.json`, `package-versions.txt` | Tested source/build identity and resolution limits |
| `render-receipt.json` | Slim actual results and explicit unqualified operations |

Do not track upstream source archives, extracted source, Docker layers,
binaries or generated frame output with this source bundle. Keep those in
hashed run artifacts. Frames and manifests live under the scratch directory;
manifests use schema1 with synthetic rational PTS and duration. Synthetic
PTS is not decoder timing evidence. Before product integration, add actual
encoded P7 BL/EL/RPU association, half-raster/chroma controls, target mapping,
metadata/output encoding, lifecycle/resource tests and physical qualification.
