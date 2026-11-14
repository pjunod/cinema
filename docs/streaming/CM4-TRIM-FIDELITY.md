# CM4 trim fidelity — source hue labels and spatial weights

**Status:** open; source implemented, final qualification in progress · **Owner:** Codex · **Updated:** 2026-11-13

Companion to the [processing ledger](DV_HDR_PROCESSING_STATUS.md) and
[display-targeted build](DISPLAY-TARGETED-DV-BUILD.md). This records two
creative-trim omissions and their independent reference evidence.

## Six-vector labels follow the L9 gamut, not a fixed BT.2020 palette

The previous preparation calculated all six hue knots from BT.2020
R, Y, G, C, B and M at PQ 1. A blue saturation control consequently peaked
near 295.289 degrees. Normal licensed uniform hue-circle probes for a P3
source put that response near 306.894 degrees instead.

Independent controls vary source mastering peak (1000/4000/10000 nits),
target peak (600/1000 nits), and effective L1 maximum through an L3 offset.
The source-gamut palette stays fixed. L6 and the shot maximum must not move
these labels. Neutralizing only the six saturation controls in the first
real capture removes all 1865 pixels above 0.05 dE ITP, isolating this stage.

Preparation now converts the L9 source primary/secondary vectors to linear
BT.2020 at a fixed canonical 4000-nit PQ normalization, then to ICtCp. The
angles remain `atan2(Cp, Ct/2)`. This reproduces all six independently measured
responses for P3, BT.2020 and BT.709; the P3 knots agree within about 0.02
degrees. This is a bounded calibrated construction, not a claim to recover
the reference's exact internal numerics. No per-title knot adjustment is used.

CPU and GPU share these prepared angles through the existing uniforms.
Angular interpolation, transfer helpers and target clipping are unchanged.

## L8 MS weight belongs in the existing spatial parameter

The previous CM4 path decoded SOP and colour fields but omitted L8 MS weight,
then always set the spatial weight to zero. The original zero-weight
derivation covered L1-only metadata, not non-neutral L8 MS controls.

Decode the field around raw neutral 2048, interpolate it linearly in target
PQ with the tone/spatial trims, and fade toward neutral at the source
mastering peak. Colour-volume interpolation must not replace this value.
The existing `f->weight` feeds both CPU detail filtering and GPU final-pass
uniforms; no new spatial operator or rendering pass is introduced.

For the held-out real input, raw 1957 at 100 nits gives -0.04443359375;
at a 1000-nit target and 4000-nit master its effective weight is
-0.01697910683. That metadata-derived value removes the remaining large
intensity errors at strong edges. Independent sinusoidal/step controls check
exact negative, positive, disabled-detail, two-target interpolation and
source-peak fading, including a separate 1000-nit source.

## Qualification retains every strictly worsening coordinate

All measurements use normal licensed CPU reference renders with validation
enabled. Synthetic inputs are generated independently of film coordinates.
Full-frame CPU comparisons use the same source and reference for baseline
and candidate, with no epsilon when deciding whether a coordinate worsens.

| Case | Pixels above 0.05 dE, before → candidate | Maximum dE, before → candidate | Strictly worse coordinates | Largest error increase |
|---|---:|---:|---:|---:|
| Real CM4 capture A, hue correction | 1865 → 0 | 0.0974273 → 0.00374161 | 371537 | 0.000749992 |
| Real CM4 capture B, both corrections | 808822 → 0 | 0.595379 → 0.0133005 | 487453 | 0.00115000 |
| Independent full-size blue hue circles | 2350080 → 0 | 2.47419 → 0.00312338 | 16 | 0.00000773294 |

Every worsening coordinate is retained with both RGB values and errors.
The respective artifact SHA256 values are
`1dcc295aff78862af592cc821c306dbd744fe1eb1aafb714251425cece81172a`,
`77f40c3d1733a58d6c29b4022634cff3fb0f89990c7390f9b853aff94001c944`,
and `3bfaac4e66323a1c20fc133527a6b9dc75454a3a97cac56a1b7190e8cd6ed705`.
The MS step also retains every regression against the hue-only candidate.
Both real production rasters reproduce their private prototypes bit-for-bit.

The independent source-gamut and signed-vector controls have no remaining
pixels above 0.05 dE; their maximum error is below 0.005 dE. The qualified
spatial controls have maximum error below 0.0005 dE. Their complete rasters,
source/metadata hashes and coordinate records remain in private evidence.

Measured regressions live in `tests/operations/test_dv_dm_cm4_trims.py`.
The native controls also check actual decoded weight, CPU/GPU agreement and
licensed-reference pixels, so two implementations ignoring MS cannot pass by
agreeing with each other. They use a generated gray sinusoid/step texture.

A compact-probe equivalence guard caught a 16:9 metadata/15:1 raster mismatch;
those measurements are invalid and excluded. Corrected uniform interiors
reproduce the full-size reference bit-for-bit. The reference rejects mapping
to a different display ID at the source's exact peak; no successful result is
claimed for that boundary control. The existing no-trims-at-master path is
unchanged and has a separate neutral-weight invariant.

Final native parity, adversarial review and merge checks remain pending.
Bridged expanded-XML evidence retains its omitted/defaulted-field limits;
it does not establish raw-RPU, full-title or physical-display acceptance.
Existing backend identity includes renderer/source digests, so changed helper
content receives a different processing identity; no manual cache workaround
or new feature switch is required.
