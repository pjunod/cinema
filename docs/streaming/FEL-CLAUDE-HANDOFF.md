# Claude handoff — continue the offline FEL shadow-loss investigation

**Status:** open — no qualified shadow-tail fix · **Handoff:** 2026-10-28

Companion to [FEL-PICTURE-QUALITY-INVESTIGATION.md](FEL-PICTURE-QUALITY-INVESTIGATION.md),
the canonical findings ledger. Continue from the existing investigation branch;
do not restart the calibration or change live playback.

## User objective and boundaries

Find an offline export/encoder combination that preserves Dolby Vision FEL's
picture benefit without the remaining shadow regression. Keep the packaging
repair separate. Commit progress promptly and regularly. The user explicitly
requested this handoff for Claude to continue.

No deployment, service restart, live playback setting, player behavior or
packaging change is authorized. Diagnostic source patches stay experimental.
The user already allowed short diagnostic media, raw frames, RPUs, traces,
source/tools/build scripts and binaries to move between this Mac and the media
worker, only in task-owned folders; no Git history or credentials transfer.
Do not ask again for those same transfers. No remote operations were made in
the latest follow-up; all its tests were Mac-local. Do not start a Hermes
collaboration: this is a document handoff, and the user previously objected
to using Hermes to obtain an Astra review. Astra reviewed directly.

## Start with the saved state

Branch: `codex/fel-quality-investigation`, pushed to `origin`.
Last experimental checkpoint before this handoff: `8c5d3f1bc`.
Earlier checkpoints: `ea7545329`, `72a73c99a`, `ec1007656`.
Frozen runtime source: `087e836d369eb2b8f94ec1d0454ea6c157612df9`.
Only documentation and offline diagnostic files changed; no runtime fix is applied.

Use the existing owned worktree at
`~/.codex/worktrees/validate-fel-picture-quality/plurx`.
Read its `AGENTS.md`; keep its storage ownership and preserve this worktree
while the investigation continues. Do not reset or update the base casually:
existing evidence is bound to the frozen source. Use Ripwire first, then
bounded searches. Its lock and knowledge commands may need filesystem access;
a refusal or empty coverage is not evidence that obligations are absent.

Committed tools and portable results are under
`tools/dv_processing/diagnostics/fel_quality/`.
Read its README, `evidence-index.json`, and the canonical ledger first.
`results/rc-comparison-20261026.json` contains all sixteen selected model/frame
rows, reference-defined luminance bands and failures for the current controls.
Media and binaries are private evidence, not Git blobs.

The private evidence roots on this Mac are:

| Variable | Value | State |
|---|---|---|
| `task_cal_root` | `~/.local/state/plurx-fel-quality/20261026-measurement-calibration` | Sealed calibration bundle |
| `task_shadow_root` | `~/.local/state/plurx-fel-quality/20261026-shadow-followup` | Sealed earlier shadow controls |
| `task_rc_root` | `/private/tmp/plurx-fel-shadow-rc-20261026` | Existing working paths used by current scripts/receipts |
| `task_rc_archive` | `~/.local/state/plurx-fel-quality/20261026-shadow-rate-control` | Durable APFS clone, hash manifest and location map |

Use the existing `task_rc_root` while it exists. The archive preserves everything
needed if it disappears. Original scripts/requests contain the original absolute
paths: relocation requires new derived requests and provenance, not edits to
sealed originals. The archive's `PRIVATE_TRANSFER_CONTEXT.json` contains the
previously observed SSH endpoint and exact short M5400 source path/hash; keep
that file private and out of Git. No remote probe is necessary just to resume.

## Findings that should constrain the next experiment

1. Earlier code had storage-dependent Dolby colour offsets, NLQ precision
   sensitivity and a cancelling chroma-grid bias. Bound diagnostic patches
   restore representation/backend consistency for these fixtures. Absolute
   composition units and normative spatial filtering remain unresolved.
   Four explicitly different models A/B/C/D are retained; compare R/F/C
   **inside the same model**, never one model's reference against another's C.
2. The old swscale/inherited-colour-path scores were confounded. Historical
   rankings and stage attributions were superseded. Opus's proposed To81
   matrix repair duplicates the actual conversion on the bound fixtures.
3. The calibrated raw point export preserves broad shadow mean/p99 advantage
   in the selected frames. The large additional loss appears in encode/decode.
   The frozen Rust author preserves all48 VCL payloads, timestamps and RPUs
   for qualified candidates; packaging does not explain these pixel errors.
4. Mean signed Y/UV errors are near zero, while RMS coding error can exceed
   the exported chroma correction. This is local coding distortion, not a
   simple global black-level offset. Export-minus-BL is not a pure EL residual.
5. Simulated component swaps strongly implicate UV in the broad shadow tail:
   encoded Y + exact exported UV restores the selected shadow mean/p99 benefit
   in8/8 model/frame rows; exact Y + encoded UV does so in0/8. These are
   counterfactual arrays, not authored streams or an implementable fix.
   Very-deep-black luma loss remains even with ideal UV.
6. There is no qualified independent normative FEL composer. Licensed/vendor
   DM results start from the same reconstructed master and validate DM only.
   DoViBaker vectors corroborate explicit operators, not Dolby spatial/optical
   conformance. Do not promote these measurements to a certified DV master.

## Fixed criterion and measured failures

Keep the existing protocol and code comparisons. For both Michael1800/5400,
frames0/24 and all4 models, require full active-ROI mean advantage versus F
and reference-below0.1nit mean/p99 no worse than F. Existing executable
admission uses strict advantage; do not relax it after seeing results.
Require actual authored video-packet average <=130Mb/s as a coarse screen.
This is not HEVC/Dolby peak-rate, HRD/CPB or native-display qualification.
Report all other bands, maxima and parity errors too.

The retained single-pass policy is HDRopt0, Cb/Cr offset−9, AQ2/strength1,
psy-RD2, RDOQ2/psy-RDOQ1, SAO1, slow, no B frames, CU-tree, qg32,
48-frame closed GOP, TopLeft type2, native24000/1001, target115/max125/buffer136.

Michael5400 frame24, ModelB, reference<0.1nit:

| Arm | Shadow mean | Shadow p99 | Actual authored Mb/s |
|---|---:|---:|---:|
| Fallback | 1.752768 | 4.193800 | 76.235, earlier qualified arm |
| Single-pass115 | 1.597174 | 4.340212 | 123.749451 |
| RDOQ0 + coupled psy-RDOQ0 | 1.642327 | 4.435591 | 122.506182 |
| Two-pass, otherwise retained115 policy | 1.638568 | 4.511261 | 114.841706 |

All4 models retain that frame's shadow-p99 failure. Michael1800 passes all8
selected rows for both new controls; their M5400 result is4/8. No candidate
is selected. Earlier HDRopt0, psycho-visual-off, SAO-off, chroma−6/−9 and
110/115Mb/s controls are retained, including failures. Do not repeat them
without a distinct hypothesis. Two-pass statistics/CSV have48 exact records,
identical requested coding settings in both passes, actual pass2 consumption
and unchanged stats. Actual bits differ, limiting causal attribution.
125/125 was deliberately avoided because it also activates internal CBR.

## Immediate next action: qualify the completed qg8 encodes

**Both qg8 encodes have finished successfully. No encoder jobs remain pending.**
They were started after protocol/tool commit8c5d3f1bc. Their logs report
AQ2/strength1/qg8/CU-tree1. They have NOT been Rust-authored, raw-qualified,
scored, or admitted. Do not rerun them or interpret container average as
qualified authored packet rate.

Under `task_rc_root/corrected-encodes/`:

| Prefix | Encoded `-hdr.mp4` SHA256 | Container average Mb/s |
|---|---|---:|
| `michael5400-qg8-budget115` | `8a2cfe60a247ad891dc9f6282b2e6d9038c5c9fbf903e30a3ae1b28ca8dd7140` | 122.835353 |
| `michael1800-qg8-budget115` | `cd8f7cd719ab63abc11d2a67703ebf693c44bbb66a42aee95923700e22abe65e` | 129.513558 |

Protocol: `task_rc_root/PROTOCOL-SPATIAL-COMMITTED.json`, SHA256
`7f303eb515d05845c78820db4afafeed84e83a6959c708416db8ff417d87e023`.
It changes only requested qg-size32→8 against the single-pass115 baseline.
Pinned x265 e9b8812 changes AQ energy sampling and QP-group granularity;
this is a local-allocation hypothesis, not guaranteed shadow protection.

For each fixture, first inspect recipe/log, verify the encoded hash, then run:

```bash
python3.14 "$task_rc_root/tooling/qualify_spatial_control.py" \
  michael5400 hdr-off-chroma9 michael5400-qg8-budget115
# Repeat for michael1800 with its matching prefix.
```

This provisional qualifier performs frozen-author/VCL/RPU/clock checks,
extracts native decoded frames0/24 and writes `-qualification.json`.
It checks encoder-reported qg8/RDOQ2/psy1 and colour/siting syntax.
**Before scoring, add an explicit receipt for the exact AQ log tuple and the
SPS/PPS-derived effective QP-group size.** It does not currently implement
those last two checks. Expected CTU64/depth3 is a hypothesis to verify from
actual syntax, not an assertion to force past a failure.

The prepared effective-settings file is
`task_rc_root/quality-effective-settings-qg8.json`.
Then use the existing generic binder, allowing ONLY the qg-size parameter
change and no nominal rate changes:

```bash
fixture=michael5400
qg_label="$fixture-qg8-budget115"
python3.14 "$task_rc_root/quality-tools/prepare_quality.py" \
  --qualification "$task_rc_root/corrected-encodes/$qg_label-qualification.json" \
  --label "$qg_label" \
  --protocol "$task_rc_root/PROTOCOL-SPATIAL-COMMITTED.json" \
  --expected-settings "$task_rc_root/quality-effective-settings-qg8.json" \
  --baseline-recipe "$task_shadow_root/corrected-encodes/$fixture-hdr-off-chroma9-budget115-recipe.json" \
  --allow-x265-param qg-size
python3.14 "$task_rc_root/quality-tools/run_quality.py" \
  --request "$task_rc_root/requests/$qg_label.json"
```

Repeat for the other fixture. The runner uses unchanged frozen models,
reference-band masks and finalizer, and rechecks R/F RGB identity. Requires
NumPy; existing Python3.14 ran these tools successfully. Commands create files
exclusively: preserve refusals/partial runs and derive fresh paths if needed.
A failed prerequisite is an invalid test, not a negative quality outcome.

## Broader validation: source/metadata ready, lattice scoring unfinished

`layer-capture/qualified-comparison.json` verifies all48 M1800 canonical BL/EL
planes, fresh coded/decoded RPUs, original packet hashes, clocks, durations
and geometry. Each dumped plane/RPU was rehashed. Source304 coded AUs yield302
paired outputs; two explicit pre-initial-random-access non-output pictures lie
outside the requested window. Their classification is timestamp-based, not
proven RASL syntax. A46-frame boundary subset excludes exactly retained0/47;
five refusal tests and clean `clang -Wall -Wextra -Werror` build pass.

`lattice-michael1800/handoff.json` is the unfinished-work inventory.
Its observer parsed metadata on the same raw-RPU-hash-verified BL frame,
replayed all48 canonical identities, constrained mapping/NLQ/matrices, and
matched retained parsed0/24 model semantics. Receipt:
`lattice-michael1800/metadata-qualification.json`, SHA256
`7b34b20973496801e798b03a3397f3f37b25251963dfe2eddfb9649eda5e0a0b`.
This is not lattice RGB qualification. Its raw observer placeholders are
empty by design: use `layer-capture/qualified/{bl.yuv,el.yuv,rpus.bin}`.

The predeclared stride8/phases0/1 lattice protocol is
`lattice-michael1800/protocol.json`. **No evaluator or sampled candidate
metrics were implemented/run.** Next: reuse byte-identical frozen
`four_model_audit.py` operators, validate exact sampled RGB for0/24 against
retained full RGB, bind decoded all48 candidate/PRE planes to known0/24,
then score all48 on that fixed lattice. Label sampled statistics honestly;
they neither replace full-resolution p99 nor change the admission rule.
Retain RPU semantics per frame. Frame24 was not the worst of earlier48-frame
samples, so selected-frame success alone is insufficient.

M5400 full source clip has not yet been copied locally. The private transfer
context gives its task-owned source and verified historical hash/size.
Copy only that diagnostic clip if needed, hash it, then perform the same
canonical trace checks; do not invoke the live renderer or deploy helpers.
Disk space was about38GiB before the APFS archive; do not retain full RGB for
48frames×4models. Use sampled evaluation or one-frame streaming.

## If qg8 fails, a distinct next hypothesis exists

Astra reviewed a source-derived shadow quantOffsets map as a possible later
experiment. It is NOT implemented or prequalified. FFmpeg n9.0.2 libx265
supports ROI side data with AQ. For10-bit input normalized qoffset is scaled
by63; −2QP is−2/63. Grid is8 with qg8, otherwise16. Stock addroi rectangles
are configured statically and cannot accept a changing per-frame luminance
map through ordinary CLI expressions; rawvideo pipes do not carry ROI data.

First prove zero-ROI/no-ROI picture equivalence to the retained baseline.
A small in-process libavcodec harness could then attach input-derived maps;
predeclare luminance conversion, threshold, amplitude and geometry. Never
derive maps from reference/F errors. Offsets affect Y and UV and interact
with CU-tree/rate control; this is not a direct perceptual distortion metric.
Do not raise the rate screen to rescue it. Review source at the pinned
revisions before implementing:

- [FFmpeg ROI wrapper](https://raw.githubusercontent.com/FFmpeg/FFmpeg/n9.0.2/libavcodec/libx265.c)
- [Static addroi filter](https://raw.githubusercontent.com/FFmpeg/FFmpeg/n9.0.2/libavfilter/vf_addroi.c)
- [x265 allocation](https://raw.githubusercontent.com/Multicorewareinc/x265/e9b8812/source/encoder/slicetype.cpp)

## Packaging stays a separate workstream

The frozen Rust author declares hev1 through
`declare_hevc_parameter_sets_in_band`. Retained native failure decodes after
a two-byte hvc1 tag control. Proper out-of-band VPS/SPS/PPS normalization
preserves VCL/RPU/clocks/config and native decoded planes/attachments for one
window. That is stronger than a tag-only edit, but no packaging fix is applied.
Two newer windows contain no in-band PS and therefore do not test stripping.
Multi-window rewrite, playlist declarations, seeking and native HLS remain
unqualified. Keep any eventual packaging branch/commit/tests separate from
encoder quality work.

## Validation and known limits

Paperwork passes14/14. Frozen numerical source hashes, authored payloads,
RPU/clock bindings, native software planes and reference masks are checked.
Normal commit-hook Mac Clippy fails on unchanged Linux-only dv_runtime
references and unused held_dv_source_origin; the ledger and retained hook log
record this. Prior docs/diagnostic checkpoints used --no-verify with that
explicit limitation; they do not establish a passing Rust/release build.
No Rust/Cargo/runtime inputs changed. Do not alter playback to fix the
unrelated platform build merely to save diagnostic work. Run focused checks,
`make paperwork`, review the diff and commit promptly. Feature-branch pushes
have not deployed the change; no PR, merge or deployment is requested.

A prior worker snapshot showed installed-helper hash drift and an active
session. These tests use pinned isolated copies and do not certify the newer
live helper. No held-out scene, normative Dolby composer/filter, complete
HRD/CPB check, temporal/native HLS or optical-display qualification exists.
Report a conditional measured benefit honestly; do not present the unfinished
shadow fix as solved.
