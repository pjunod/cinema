# FEL shadow-loss investigation — handoff

**Status:** open — candidate meets the fixed bar; not production-qualified · **Handoff:** 2026-11-11

Companion to [FEL-PICTURE-QUALITY-INVESTIGATION.md](FEL-PICTURE-QUALITY-INVESTIGATION.md),
the canonical findings ledger. This replaces the 2026-10-28 GPT→Claude
handoff. Its boundaries still hold: offline diagnostics only. No deployment,
service restart, live playback, player or packaging change is authorized,
and packaging stays a separate workstream.

## Where it stands

- **Cause, concretely identified.** x265 under-allocates dark blocks in luma
  and especially chroma. AQ2 sets block QP from texture energy alone, the
  PPS chroma offset is one global value, and Main10 has no per-CU chroma
  offset. The evidence for this chain:
  - Component swaps isolate the loss to coding in those blocks.
  - A source-derived dark-block QP reduction removes the tail.
  - The effect is monotonic in dose (all-48 Michael5400 frames beating F:
    17 → 38 → 46).
- **Candidate.** Encode with a zero-mean per-frame quantOffsets map built
  from the encoder's own input. Each 16×16 block whose mean Y' decodes
  below 1 nit gets −3 or −6 QP, and every other block gets the balancing
  positive offset. Both doses pass the fixed selected-frame bar (16/16
  rows) and the 130 Mb/s screen. At −6 QP, Michael5400 beats F in 46/48
  sampled frames.
- **Not done.**
  - A held-out scene. Both windows were used for the design.
  - The dose choice, which trades shadow error against bright-content
    error and is the owner's decision.
  - Two marginal Michael5400 frames (5 and 6).
  - Any production implementation.

## Evidence and tools

Committed tools and portable results live under
`tools/dv_processing/diagnostics/fel_quality/`, indexed in its README and
`evidence-index.json`. Media, decoded planes and binaries are private.

| Private root (on the M3 MacBook, `pauls-macbook-pro-3.local`) | Contents |
|---|---|
| `~/.local/state/plurx-fel-quality/20261026-shadow-rate-control` | Sealed GPT archive; never modified |
| `~/.local/state/plurx-fel-quality/20261111-qg8-relocated` | APFS clone of the archive; qg8 qualification/scoring (`RELOCATION-20261111.json`) |
| `~/.local/state/plurx-fel-quality/20261111-roi-control` | ROI harness, gates, arms S/Z/Z6, M5400 source capture, lattice runs (`RELOCATION-20261111.json`) |

The original `/private/tmp` working roots were wiped by a reboot. Relocated
paths are derived with every hash re-verified, and sealed originals are
never edited. Refused attempts are kept under each root's `logs/`.

## Resuming

1. **Held-out scene.** Pick a FEL title not used so far. Capture it with the
   frozen `capture_layers` binary and verify it with
   `verify_layer_capture.py`. Then run the baseline CLI encode, the
   `encode_roi_control.py` gates (`none`, `zero`), the arm at the chosen
   dose (`lumazm`), `qualify_spatial_control.py --qg-size 32`,
   `prepare_quality.py`/`run_quality.py` and `lattice_eval.py`. Commit a
   protocol before any output, as every arm here did.
2. **Product path, if approved.** plurx builds encodes as FFmpeg CLI
   argument lists, and a per-frame map cannot be passed that way. The fit
   is an encoder-policy component that attaches the same zero-mean ROI map
   inside the filter chain, for example a small filter in plurx's shipped
   FFmpeg. It must be gated by the same byte-identity test as
   `roi_encode.c` (all-zero map ≡ baseline). It needs its own branch, plan
   and review.

## Validation limits

Selected-frame scores are full resolution. All-48 scores are on a sampled
lattice whose frames 0/24 equal the retained full-resolution RGB exactly.
None of this is a perceptual metric, HRD/CPB or Dolby peak-rate
conformance, a normative Dolby composer or a native-display qualification.
