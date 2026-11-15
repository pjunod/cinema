# Governed paths, one file per change

A change that puts a new file under an existing functionality point does not
edit that point's `paths` list in `validation/points.toml`: it records the
path here.

```toml
# validation/points.d/vod-plan-probe.toml
[[paths]]
point = "playback.core"
add = ["scripts/vod-plan-probe", "tests/playback/vod/**"]
```

`validation.runner.load_catalog` appends every fragment's paths to its point,
in file-name order, so `scripts/validate lint` and every lane see them. A file
holds one or more `[[paths]]` tables and nothing else; `point` must exist. A
new point, a new check, a timeout or a setting still goes in the catalog.
`scripts/paperwork` refuses a path added to an existing point's list there.
[docs/ci/SHARED-BOOKKEEPING.md](../../docs/ci/SHARED-BOOKKEEPING.md) covers
every per-change directory.
