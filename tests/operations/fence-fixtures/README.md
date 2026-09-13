# Fence fixtures

One directory per fence, each holding a file that **must trip** it and a file
that **must not**. The tests beside them (`tests/operations/test_*_fence.py`)
run the fence's own scanner over both, so an exemption that widens fails here
rather than in the field.

- `input/` — `scripts/player-input-fence`: platform key decoding belongs to the
  three player input adapters.
- `surface/` — `scripts/playback-surface-fence`: a playback surface is written
  only by the presenter's render.

These fixtures are `.swift`, `.kt` and `.html` on purpose, and they are safe
where they sit: both fences scan `clients/**` and `crates/plurxd/src/web/*`,
never `tests/`.
