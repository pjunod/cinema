# Engineering knowledge graph review — close findings before building

**Status:** done · **Approved:** 2026-10-21 · **Scope:** implementation-plan
review and author dispositions; see [live status](ENGINEERING-KNOWLEDGE-GRAPH-STATUS.md) for implementation

Companion to the [pilot plan](ENGINEERING-KNOWLEDGE-GRAPH-PILOT.md), which
owns the build contract and execution ledger. This document records actual
review findings, author corrections and exact-candidate review status. It
does not turn conceptual advice or author checks into independent approval.

## Fable R1 — changes requested on the frozen plan

Actual reviewer: Fable 5.1, `claude-fable-5-1`, Anthropic, Hermes profile
`hdr-fable-api`, task `t_9621e97b`, 2026-10-21. Reviewed plan SHA-256:
`06a476e30dd4d40474ea16c2dc3679db37b8ebf71e6fbd0d3a15e6a9587cfdba`.
Verdict: **REQUEST_CHANGES**, with three blocking findings. The reviewer
read the plan, brief, manifest, index context and prior conceptual verdict.
It did not open the supplied source context before its iteration budget
ended. Source questions below were unverified, not refuted.

## Author dispositions — R2 independently verified

| ID | Finding | Correction and remaining review |
|---|---|---|
| F1 · blocking | M5 depended circularly on a ten-PR process window; eligibility was undefined. | Plan §6 defines distinct landed PRs touching the frozen curated scopes. M5 lands comparative results with advisory capture. Later M5b decides enforcement after both ten eligible PRs and 14 days, starting when advisory M4 reaches main. |
| F2 · blocking | Gold answers and authored records could be tuned to each other; nine gold lists had no seal point. | Plan §8/§9.1 require all twelve gold lists and rubric sealed before M1, with digests in §11. Independent record authors see only cohort definitions/case subjects. Solver isolation excludes gold and future answers from files, Git objects and tool permissions. |
| F3 · blocking | B/C differed in formatting and budgets as well as traversal. | Plan §5/§9.2 use the identical interface, envelope, metadata, ordering and byte limit; B is enforced depth 0, C fixed depth 6. Only relationship expansion differs. |
| F4 · medium | Token comparison could cherry-pick correctly completed runs. | Plan §9.3 requires all three repeats in both B/C correct; fewer than three qualifying cases in either cohort is inconclusive. All-run failures/usage remain reported. Zero-gold cases measure false assertions separately. |
| F5 · medium | Graph code could appear to assign authenticated status. | Plan §4.3 permits copying only an existing validator verdict with identity/scope; synthetic fixtures claim no real authenticated execution. Missing real evidence remains unavailable. |
| F6 · medium | Dependency closure had no canonical owner. | Plan §4.3 names the imported descriptor/digest or catalog provider closure and retains incomplete scope as unknown. Safety correction to suggested wording: authored records can narrow retrieval, never remove proof inputs or invent wider qualification. Narrowing the proof itself could wrongly preserve stale evidence. |
| F7 · low | Invalid PR-field references had no clear enforcement surface before optional paperwork integration. | Plan §6 defines body-file-aware lint in existing PR preflight; local validation-lint checks graph data and explicitly reports absent PR context as unchecked. Paperwork only moves the same check earlier. |
| F8 · low | Review fixes could exceed the 40 KiB document limit. | Replaced the long example diagram and condensed research prose. Keep full dispositions here and validate final plan size independently, including while untracked. |

The same-task R2 recheck verified all eight corrections in the actual plan
text, accepted the F6 safety interpretation as stricter and correct, and
found no regression from the replacements. The reviewer independently
checked E1/E2, partially checked E3, and accepted E4's author-supplied source
excerpts for plan purposes without claiming independent source verification.

## Source answers — author verified, not reviewer conclusions

Inspected source baseline remains
`d1f61377096074b915856ae5c913e1dc68885738` on 2026-10-21.

| Question | Evidence supplied for the recheck |
|---|---|
| E1 · routing schema and IDs | Parsed [routing-decisions.toml](../../tests/playback/routing-decisions.toml): all 68 rows have exactly the five fields in plan §2; all six §3.1 IDs exist. No schema extension is required. |
| E2 · point fields/direction | AST inspection of [runner.py](../../validation/runner.py) confirms the six fields listed in plan §2. `_expand_consumers` explicitly states that A depending on B means B's changes can break A; impact walks provider to consumer. Proof dependency closure walks the declared provider relation, a distinct operation. |
| E3 · gates and exception | [Contributor rules](../../AGENTS.md) require manual effort dispatch and current-candidate main-promotion/qualification evidence, and define the disjoint-file exception. The [pipeline](../DEVELOPMENT_PIPELINE.md) describes the two gates; [effort-ci.yml](../../.github/workflows/effort-ci.yml) is dispatch-only and [ci.yml](../../.github/workflows/ci.yml) names Main promotion gate. Current amendments take precedence over older trigger descriptions; this plan does not authorize automatic full CI. |
| E4 · applicability signatures | [main_preflight_adoption.py](../../validation/main_preflight_adoption.py) defines `input_digest(commit, row)` over Git input identities/modes, path membership and optional history. [python_unit_receipts.py](../../validation/python_unit_receipts.py) defines `SourceApplicability(current_commit, source_reader=read_git_test_source, worktree_reader=read_worktree_test_source)` and explicitly limits its proof to method/local fixtures, not arbitrary production inputs. |

## Closure — exact R2 plan approved, implementation not started

R2 plan SHA-256:
`473d391e9b6a1e20ea1ecdd9d93b46339e69ba31923bd85f60bcc6a534d2821c`
(40,142 bytes). Fable 5.1 (`claude-fable-5-1`, Anthropic,
`hdr-fable-api`) returned **APPROVE** in same-task R2 run 49 on 2026-10-21,
with no blocking findings remaining and no further recheck required.
The reviewed specification is retained byte for byte in agent-owned inputs.
Later repository plan header/ledger changes record M0 progress; they are not
claimed as another independent approval. See the live status for execution
amendments from the owner.

The independent report is `REVIEW-R2.md` and machine verdict
`VERDICT-R2.json` in the durable Hermes review workspace for task
`t_9621e97b`. R1's requested changes and original candidate remain preserved.

Non-blocking R2 observations are disposed as follows; none changes the
approved plan contract or claims independent verification beyond its scope:

| ID | Disposition |
|---|---|
| O1 · arm enforcement | Accepted M0/M5 harness clarification: the evaluation wrapper pins depth by arm and rejects solver overrides. This implements §9.2's existing enforced-depth requirement; record it in the frozen evaluator manifest and test override refusal. |
| O2 · orphan short line | Cosmetic rewrap deferred to the next substantive plan revision, preserving exact-content approval. |
| O3 · gate descriptions | Retain §8's explicit deference to live pipeline and governing contributor rules. M0/integrator must recheck current triggers and mandatory gates rather than infer automatic full CI from historical prose. No rule is weakened here. |
| E5 · PR-body entry point | Author confirmed the existing main-fast-lane paperwork step passes `PR_BODY` via `--body-env`; `scripts/paperwork` accepts `--body-file` too and already materializes the body for regression validation. M4 must explicitly pass the same locally captured body into knowledge lint in preflight, with absent context reported unchecked; it may not assume unrelated preflight jobs already receive it. |

Author validation after R2: all 28 tests passed across documentation index
(6), size budget (5), status audit (3), and infrastructure-name (14) suites.
The new untracked documents also passed explicit relative-target, size and
whitespace checks. These are author checks, not Fable approval.

Plan approval closes this review only. It does not authorize a build, source
changes, benchmark execution or broader rollout.

## Implementation review — Astra R1, corrected source awaiting recheck

Independent Astra (`gpt-6-astra`, `openai-codex`, `hdr-astra`) returned
**REQUEST_CHANGES** in Hermes task `t_ff338f21` on 2026-10-21. Reviewed
integration head `ac7a72c9e1700d0a4e216ad901995378ffb03386`, tree
`76f9b6a6c23f4929efe21f05e3d517922cf39d9b`, base
`00c74cd8c5ebd9ff3c6957532c42854ddc3d671a`. The report separately confirmed
all 27 authored records against governing sources; it found no fabricated
review finding or execution receipt. This does not qualify runtime behavior.

Actual R1 report SHA-256:
`355fe6a02ff120884416d75af50bf746927f3a98e595ee82e6222774ac4eb70b`;
verdict `77d77c96c4d3fba0ceee6d577d5003f265aed260d935d59d0ac4c889ae31e870`.
Durable source-only review inputs and reports remain with that Hermes task.
The private gold, case views, acceptance scripts and answers were withheld.

| ID | Required correction / current disposition |
|---|---|
| R1 | Smoke receipt inputs now bind reviewed physical source/test and governing document bytes/modes, with membership retained separately. Draft regression covers in-place doc/split-child edits and unrelated bodies. |
| R2 | Prune pristine semantic payloads unsupported by sealed visible bytes; regenerate structural metadata. Source corrected; synthetic controls drafted. |
| R3 | Bind the actual launched graph to the frozen manifest knowledge identity before spending. Source corrected; synthetic controls drafted. |
| R4 | Qualify frozen acceptance families and distinguish completed assertion failure from unavailable infrastructure. Source corrected; unsupported families remain unavailable and uncacheable. |
| R5 | Enabled/adaptive thinking configurations now refuse before counting or spending; unexpected signed reasoning responses refuse instead of producing invalid tool history. |
| R6 | Consensus now includes an opaque per-obligation judgment digest. Private scorers must supply it; equal counts with different judgments remain disagreement. |
| R7 | Historical intermediates must not confer governing authority; preserve discovery and independent current routes. Source corrected; synthetic controls drafted. |
| R8 | Publish the numeric eligibility, medians, intervals and uncertainty metadata supporting token decisions. Source corrected; synthetic controls drafted. |
| O1 | Removed extra final blank lines in authored data. |
| O2 | Descriptor-relative no-follow discovery reads implemented; bounded snapshot retry handles observed write-and-restore races. |
| O3 | Cold-index/warm-query performance remains unmeasured; optional paperwork shortcut stays unused. |

All eight corrections are now integrated or frozen as exact generic evaluator
source commitments. Three of four frozen acceptance bundle shapes remain
unsupported; the full study must refuse before spending. A review-gated
prerequisite-only entry point can witness this capability refusal without
asserting actual OS or dependency qualification. No threshold or frozen case
has changed. Missing scorer commitments also remain unscored, rather than
establishing consensus from matching aggregate counts.

All source corrections require the same reviewer's exact-source recheck.
No unit test, container, provider study or implementation approval is claimed
by these dispositions. The [live status](ENGINEERING-KNOWLEDGE-GRAPH-STATUS.md)
records acceptance and publication as they occur.

## Implementation recheck — Astra R2 and narrow R9 correction

R2 reviewed head `7ff429573757a2a9c18c527618e6edee075eb239`, tree
`efa5b2337c023274ac09f4369f33ecfef6795ac9`, with the same integration base.
Astra confirmed **all eight original findings addressed** in the expressly
limited capability scope. R4 does not require implementing the three
unsupported families or running a favorable subset. O1/O2 are closed within
their stated scope; O3 remains unmeasured acceptance.

R2 remained **REQUEST_CHANGES** for one new R9: ordinary Python loading could
execute a valid cached module or reopened replacement despite checking the
source hash. The corrected loader now retains the verified buffer, registers
its module metadata, compiles/executes those exact bytes, detects later
mutation and unregisters failed loads. Draft inert controls cover conflicting
valid cached bytecode and path replacement. Frozen prior sources are preserved.

O4 was a nonblocking description mismatch. The capability entry enforces
APPROVE, reviewer independence, the exact six-source map and study baseline.
The trusted coordinator separately verifies reviewer profile/model/provider,
implementation-approval/hash-verification flags and integration base/head/tree
before invocation. Source review never substitutes for runtime authorization.

Actual R2 report SHA-256
`8e1b6d3e818cca790586c912fb97f93105cadeaa141f577d9078ef8c847e5e41`;
verdict `4b0592c76b5eb2a740e72177ef2fd1919542a6ee54fd6870f5d74b6bb12c0ce5`.
R9's exact refreshed source commitments require the same Astra task's narrow
recheck before any implementation imports, units or capability witness.

## Implementation recheck — Astra R3 and qualification corrections

Astra R3 **APPROVE** binds head `54ef1c02c86b5bd10df6ac1458b19aad6935d919`,
tree `bceb9feecaf3fa3d3a573dfac57d8a6be73473ad` and the exact six-source map.
R9 is closed. Actual report SHA-256
`169873e94221bdd9a261bfb9a767341ca537036d6921960a37b234eaf1f98a10`;
verdict `7892501e1f3b5d34b965cc6fe59ae286be4bd6045f5fc0674463076a63b4cbf0`.

Post-review qualification produced three bounded corrections for a same-task
recheck, preserving all historical results:

| ID | Observed cause and prepared correction |
|---|---|
| Q1 | Warm CLI queries rebuilt the graph and repeatedly globbed membership, taking 2.045–2.103 seconds. Cache authenticated graph/selection recipes locally, revalidate every live source/identity dimension, and handle literal membership without whole-tree globbing. Draft stale/forged/concurrent/race controls; no execution-proof authority added. |
| Q2 | One of nine private synthetic checks errored on an undeclared Mock admission method. Supply the actual interface, retaining the graph-mismatch and no-spend assertions. Retry only this failed check. |
| Q3 | Actual capability refusal found zero of four supported bundles: frozen commands kept nested grader paths while generic export/dispatch flattened them. Preserve safe relative identities across all three layers; reject ambiguous, unsafe and symlink layouts. Strict inert-byte grammar, frozen artifacts, commands and Gold stay unchanged. |

Seven unaffected private passes retain exact test/behavior AST commitments.
One protocol pass is genuinely invalidated; the failed check and three new
layout controls yield five selected checks after approval. A fresh capability
witness is required by the changed source/approval binding. New supported
counts remain unset; no partial favorable study or token-saving claim follows.
The refreshed source commitments and warm-cache correction are prepared,
unexecuted, for the same Astra task's exact-content recheck.

## Implementation closure — Astra R4 and bounded qualification

Actual R4 **APPROVE** binds head `ac0f247b5edfd455a53a3aa35999b9b89cb550ed`,
tree `56669c330f1459444a8f694f2af9ac89d0d306e6`, unchanged integration
base and all six refreshed generic source hashes. Q1/Q2/Q3 are addressed in
source; no new blocking finding. Report SHA-256
`912a062ce439ca2173b9d418bb29dabda03932d947e4e05e201f2044801c0cff`;
verdict `b3c722b90e6d906dbb4bb7f58ec86fcc96390e4021e316408080e72cac5fc273`.
The coordinator independently verified exact reviewer identity, source hashes,
clean candidate and seven retained test/behavior AST commitments.

After that approval, all five selected private synthetic checks passed; seven
unchanged prior passes remain retained. Cold CLI indexing measured 1.296 seconds;
five warm queries measured 0.639–0.696 seconds with identical bounded answers.
The fresh capability-only witness observed one supported and three unsupported
bundle shapes, refusing the full study with every spending/credential/container/
staging/cache spy untouched. This is not OS/dependency qualification or a token
comparison. The original 0/4 witness, warm failure and fixture error remain
historical evidence. Public fast-lane and landing state belong to the owning PR.

Main subsequently advanced to `7f5af4524` (already-landed worker refinement).
Its integration preserves every approved pilot implementation/test/CLI/skill
byte and all six evaluator source hashes. A documentary backlog collision
retains main's P35 and assigns this pilot P36. The unstarted R5 review request
was withdrawn; no R5 Astra approval is claimed. Independent mechanical
integration verification and current-head CI follow the retained R4 source
approval, honoring the owner's single adversarial review workflow.
