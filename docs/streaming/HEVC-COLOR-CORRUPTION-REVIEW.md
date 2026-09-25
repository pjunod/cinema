# HEVC color corruption review — adversarial findings and dispositions

**Status:** final implementation review complete; one finding addressed; CI pending ·
**Written:** 2026-09-25 · **Reviewer:** independent `header_path_audit` agent

Companion to
[HEVC-COLOR-CORRUPTION-RCA-AND-FIX.md](HEVC-COLOR-CORRUPTION-RCA-AND-FIX.md).
This records the original design review and the final batched implementation
review in §6, including the changes made in response. It is not a deployment
or physical browser qualification receipt.

## 1. Review scope and evidence

The reviewer independently read the shared HEVC copy argument builder,
configuration promotion, fragment-index inspection, VOD admission, rolling
fallback and cache identities at checkout
`bafeb08766ce057634f3fab0850cdd9e03507a98`. It then reviewed the written RCA
and proposed fix. It did not modify code or run the live media experiments;
those observations and exact raw-frame hashes were supplied by the primary
agent and are distinguished from the reviewer's source-code findings.

The review challenged whether the source/browser distinction was proven,
whether keeping headers was actually a deployable fix, whether every route
would enforce the new guard, and whether older workers or cached media could
evade it. The sampled stripping mechanism is supported by the source-versus-
preserved exact raw-frame match and the differing stripped result. Whole-file
correctness, new HLS compatibility and deployment performance remain unproved.

## 2. Independent code audit changed the proposed repair

| Finding | Consequence for the proposal |
|---|---|
| The current analyzer sees output after VPS/SPS/PPS removal. | Observe original NALs before destructive filtering or muxing; an empty observation is not stability proof. |
| `PromotionInputs` reads only the first video sample of clean fragments and includes HDR10 SEI. | Inspect every access unit; separate configuration proof from HDR static-metadata equality. |
| Init promotion fills missing arrays and skips already-present types. | Do not treat promotion as a repair for conflicting PPS definitions. |
| VOD refusal can fall back to rolling copy; explicit Live also avoids VOD admission. | Put proof enforcement in shared admission and use a typed reason that cannot re-enter unsafe copy. |
| A named varying-configuration test only compares constructed promotion values. | Require a real encoded fixture through the actual analyzer/filter pipeline and decoded-pixel assertions. |
| Existing index identities do not automatically version every Rust analysis/normalization change. | Add an explicit revision to legacy and cluster artifacts and invalidate old proof results. |
| Retaining in-band headers does not preserve the existing `hvc1` contract. | Treat the Matroska A/B as causal evidence; defer changing-configuration HLS to separate client qualification. |

The code anchors and required implementation behavior are in §§3 and 5 of
the RCA; that document is the authoritative proposal rather than duplicating
the full design here.

## 3. Draft review requested three changes

The first verdict was **request changes to the proposal**, while accepting
the sampled server-side stripping root cause as well supported.

| ID | Severity | Adversarial objection | Amendment |
|---|---|---|---|
| R1 | P1 | Immutable definitions under different IDs can select incompatible resolution, bit depth, profile, chroma format or color signaling without redefining any ID. No same-ID conflict is insufficient proof. | §5.1 now requires one supported decoder/sample-entry contract across all reachable active sets. Initial containment conservatively rejects unproved multi-ID/configuration switches. §6 adds distinct-ID SPS geometry/bit-depth cases. |
| R2 | P2 | The A/B isolates removal of types 32–34, not each differing PPS field. The draft named chroma offsets too definitively and omitted IDs/references. | §1 narrows causality to removed parameter-set updates, records both PPS 0/SPS 0 definitions and inspected slice references to PPS 0, and states that individual fields were not independently tested. §2.3 reproduces the expanded trace. |
| R3 | P2 | The cluster pipeline identity citation linked the store module instead of the daemon's pipeline-digest implementation. | §5.4 now links `crates/plurxd/src/fragment_index_cluster.rs`. |

The primary agent applied these amendments and requested independent
verification of the changed document. The reviewer reread the amended
sections and returned: **approve as an RCA and proposed-design document**.
All three findings were resolved; no must-fix documentation findings remain.
This verdict does not approve an implementation or authorize claims that
the production defect is repaired.

## 4. Implementation questions remain blocking before delivery

Even an accepted proposal does not supply the following evidence:

- Numeric parser/scan limits, queue admission, cold-start and load thresholds.
- The exact proof-to-source-snapshot binding and producer-time revalidation.
- A concrete mixed-version worker fence and affected-session transition plan.
- A redistributable encoded changing-configuration fixture exercising the
  real parser/filter path, with full-duration decoded-frame comparison.
- Independent qualification of allowed encode fallbacks, including Profile 5
  and truthful, policy-authorized delivered dynamic range.
- Safari and Chrome playback/resume/seek acceptance on affected media and
  stable-HEVC stutter regression checks.

The original review was documentation-only. Implementation now follows in the
same workstream; deployment and library rewrites remain outside this PR.

## 5. Implementation findings addressed before final PR review

These reviews preceded the latest instruction to consolidate review at the
merge boundary. They are retained as history, not a second final approval.

| Finding | Disposition |
|---|---|
| Unknown/error trace headings and lost stderr could yield an untrustworthy result. | Whitelist recognized headings, use `-xerror`, and require clean trace EOF before publishing. |
| Filesystem stamps have meaning only on their issuing node. | Bind local proof to node plus object; cross-node proof requires full-content equivalence. |
| A sampled digest can miss changed HEVC bytes. | Use a separate whole-file hash regime for HEVC copy, with bounded buffers and existing queue budgets. |
| Node-local proof fields make portable blobs nondeterministic. | Remove local fields from the authenticated blob and rebind only after full attestation. |
| Subtitle observations could overwrite stronger hash memos. | Preserve full memos for identical objects in both Store implementations. |
| Same-size/mtime replacement could retain a completed preparation job. | Include current object identity in requested generation. |
| Rolling/progressive paths lack equivalent retry/source binding. | Refuse by default; latest operator instruction adds an unrestricted, explicitly advised Developer override. |
| Old workers could bypass admission. | Bump media protocol to 7 and document required ingress/session draining. |

The final candidate review, disposition and fast-lane receipt will be recorded
here after the batched PR is ready. No final approval is claimed yet.

## 6. Final batched PR review — PR #535

The independent adversarial agent reviewed candidate `8369dae7d` against
`8ae8cab1e` without running tests. Verdict: **request changes**, with one P2
finding and no other must-fix findings.

**F1 — stale local proof never refreshes with cluster indexing disabled.**
The periodic local pass skipped any index matching catalog size/mtime and
pipeline, even when ctime/inode or proof revision/node had changed. The
object-bound queue request could not help while the cluster resolver was off.

**Addressed:** the local pass now skips HEVC only when the proof names the
current analyzer revision, source object and node. Missing/stale proofs are
rebuilt through the existing bounded pass. A completed refusal for the current
object still counts as analyzed, preventing repeat full reads. The new
`local_hevc_index_refreshes_object_proof_without_cluster_queue` regression
exercises stale-proof replacement and retention of a completed refusal through
the real local indexer. The fast-lane validation phase follows this disposition.

The reviewer confirmed that previous trace completion, portable proof,
full/sampled memo, cached admission and worker fencing findings are addressed,
and the explicit enable bypasses proof restrictions without readiness gates.
