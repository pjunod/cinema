# Combined DV helper — a real finite source-to-HDR10 boundary control

Status: ready for focused adversarial review, not a production route.

The real helper demuxes/splits a six-picture B-reordered P7/FEL source, checks
actual native BL/EL/fresh-RPU associations, applies the accepted public
libplacebo reconstruction, muxes actual RGB48 packets into NUT with their
source PTS/duration, and encodes Main10 Matroska through the existing x265 CLI.
It preserves0/42/83/125/167/208ms PTS and41ms stored durations exactly. Stored
container durations have rounding gaps; this does not assert continuous
presentation coverage or a display target.

Inspect `run_combined.py`, `fel_export.c`, `mux_rgb.c` and
`check_combined.py`. The existing renderer's control loop is restricted to
actual nonzero BL/EL input; this proof still starts a GPU context per picture.
The next functional delta will remove inherited test branches and keep one
context through a dynamic-raster bounded segment loop. Current native geometry
is64×64 and frame count6; the control does not accept a film yet.

The independent public-matrix/PQ scalar control reports maximum1RGB48 code;
decoded HDR10 inverse-NCL agrees exactly and reconstructed-vs-decoded RGB is
323codes. Initial tighter YUVerror assumption3 failed at4. The unchanged prior
preregistered compression cap4 is retained separately and applied after the
current observation for diagnostic interpretation; no new preregistered
acceptance or movie-quality claim is made. This distinction is in receipt.json.

NUT initially tagged RGB48 bytes as RGB555. Actual decoded pixel comparison
caught it despite correct timestamps/HDR10 tags. The fixed mux uses the public
`avcodec_pix_fmt_to_codec_tag(AV_PIX_FMT_RGB48LE)` API. The other caught input
validation failures are retained in the working scratch receipts; none grant
fallback/output acceptance.

The Linux runner copies unchanged public `process::bounded` ownership source
at the recorded Plurx commit. It uses one process group and cancel-and-await;
children remain in that group. Cancellation/deadline controls verify both
actual helper and descendant disappeared after cleanup. The30s timeout covers
the awaited execution/capture phase after spawn; cleanup is awaited separately
and is not a hard total30s guarantee. Vec length caps are not allocation-capacity
or GPU-memory bounds. Existing2GiB/2CPU container limits are experiment caps.

To check the frozen evidence without rebuilding/rendering:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 check_combined.py evidence
PYTHONDONTWRITEBYTECODE=1 python3 test_combined.py evidence
```

The expected encoder inventory pins the observed immutable image binaries and
214resolved ELF dependencies; it is not an encoder source build. The reviewed
FF9 static decoder and parsed renderer prerequisite artifacts are reused.
No system/fleet libraries, shipping routes, settings or production registry
changed. Creative trims/MMR/reuse/nonstandard matrices/general curves and
certified profiles remain unsupported.10000/.005 nits is the PQ master working
range and synthetic mastering metadata, not a requested display policy.


## Exact execution and focused checks

The following commands were executed with the recorded image ID. The source
and approved prerequisite tools were staged to a fresh work directory mounted
at `/work`; no clean dependency bootstrap is claimed.

```sh
# Compile public-API stage programs using the already verified prefixes.
docker run --rm --network none --cpus 2 --memory 2g \
  --mount type=bind,source=/absolute/work,target=/work "$image_id" \
  sh /work/build_stages.sh
# The original standalone owner build used Rust1.97.1 and cargo build --release;
# its newly resolved Cargo.lock is retained. Subsequent builds may use --locked.
docker run --rm --cpus 2 --memory 2g \
  --mount type=bind,source=/absolute/work,target=/work "$image_id" \
  sh -ec 'cd /work/runner; rustc --version; cargo build --release --locked'
# Actual graph, capped capture and awaited-phase deadline; cleanup is awaited.
docker run --rm --init --network none --cpus 2 --memory 2g \
  --mount type=bind,source=/absolute/work,target=/work "$image_id" \
  /work/owned-runner 30000 0 /usr/bin/python3 /work/run_combined.py
# Focused lifecycle hold controls use this real wrapper plus a sleep descendant,
# not an active GPU/codec phase. Each returned70 and both PIDs were absent.
/work/owned-runner 5000 200 /usr/bin/python3 /work/run_combined.py lifecycle-hold
/work/owned-runner 200 0 /usr/bin/python3 /work/run_combined.py lifecycle-hold
```

`stage_contract.py` supplies the expected exact invocation and input/output
inventory independently of execution records. Recorded executable SHA256 is
checked against `build-identity.json`; current binaries are unavailable in
this slim snapshot. Updated execution records include actual cwd and material
loader/device environment. Seventeen focused checker controls pass, including
self-consistent wrong tool/argv/timing/empty maps/cwd. The first proof remains
separately preserved; this correction does not supersede any source-to-movie
or continuous processing qualification requirement.
