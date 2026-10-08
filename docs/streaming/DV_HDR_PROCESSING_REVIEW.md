# Dolby Vision processing — adversarial review record

**Status:** done — findings addressed and verified; ready for M0 only ·
**Updated:** 2026-10-08

Scope: the [proposal](DV_HDR_PROCESSING_PLAN.md) and
[Sol 6.1 build handoff](DV_HDR_PROCESSING_BUILD.md). This is a design review,
not approval of implementation, dependency packaging or production deployment.

## 1. Earlier findings carried into the design

The preliminary review identified tonemapx profile restrictions, libplacebo
7.360.1's residual-dependent bypass, incomplete creative trim support,
invalid use of changed hashes as eligibility proof, and re-encode cost/loss.
All remain explicit constraints. The subsequent user requirement supersedes
normalization-first design: full FEL reconstruction is the preferred quality
objective, with normalization retained only as partial fallback.

## 2. Independent detailed review and dispositions

A separate adversarial agent reviewed the complete proposal and build handoff
against the requested FEL preservation, fallbacks, settings, quantitative
quality comparisons and repository contribution rules. It inspected relevant
repository contracts without implementing or deploying the feature.

| Severity | Finding | Disposition |
|---|---|---|
| P2 | The evidence path included a real home directory, contrary to the public mirror naming rule. | Replaced with `~/native-hdr-test-2026-10-08/dv-hdr-research/`; private host resolution stays outside tracked prose. |
| P2 | A builder could wait for an automatic effort gate that never runs. | Build section 1 explicitly requires manual dispatch and a green current-candidate `Effort development gate` before each task merge. |
| P2 | A plausible DV-rendered P8.1 image does not prove that its base is independently HDR10-compatible. | Proposal section 3, M0 and M6 require DV-disabled base decoding, separate reference validation and retained DV-on/DV-off artifacts; either failure rejects enhanced P8.1. |

The reviewer judged the independent-reference requirement, bitrate controls,
identity-RPU handling, native-copy precedence, fallback generation isolation
and selected-worker capability proofs suitable for starting M0. It found no
unconditional claim that FEL export or P8.1 metadata authoring already works.
The 65.4458 dB preliminary measurement remains labeled as a difference between
two outputs, not a measured fidelity improvement.

## 3. Approval boundary and verification

The requested handoff is for gated feasibility work. FEL decode/export,
target mapping and valid P8.1 authoring remain explicit unresolved questions.
A successful HDR branch does not approve the enhanced P8.1 branch. Neither
this review nor document checks authorize product quality claims or replace
implementation review and exact-tree qualification.

The adversarial agent re-read the corrected files and verified all three fixes.
No review blocker remains to handing the documents to Sol 6.1 for M0 only.

Validation on the documentation tree:

- `python3 -m unittest discover -s tests/operations -p test_docs_index.py`:
  all four tests passed with the new documents staged, so the index and
  repository documentation-link checks included them.
- `git diff --cached --check`: passed.
- `python3 scripts/scrub-infra-names --check`: no findings in these documents;
  the repository-wide check still reports four pre-existing matches in
  `docs/clients/APPLE-BLACK-VIDEO-IMPLEMENTATION.md`. That unrelated document
  was not modified by this work.

No quality harness, feature code, full FEL output or enhanced P8.1 output was
built in this documentation task. There is no measured FEL quality gain yet.


## 4. Expanded documentation review — 2026-10-08

The user subsequently requested a committed/merged PR, a central project
backlog, the HDR10-E badge, and existing-tool reuse investigation for both
output routes. The same adversarial reviewer inspected the expanded scope
without running tests or approving implementation.

The added P2 finding was a stale Mac landing claim in backlog A01. Main already
contains #882 (`339abbced`); A01 now records that landing and preserves the
actual package/runtime/physical limits, explicit Windows waiver and absence
of a full-green promotion claim. The old PR #870 status is superseded.

The reviewer found the remaining backlog categories/maintenance, badge receipt
and generation rules, and tooling reuse boundaries suitable for a documentation
merge. Local link/index inspection and diff hygiene cover the additions; no
unit tests were run for this docs update. Earlier test results in section 3
remain historical results from before the user's no-unit-test instruction.
