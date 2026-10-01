# Opus review — challenge the automatic quality plan before implementation

**Status:** initial Opus review received; optional follow-up prompt for revised plan
· **Written:** 2026-09-30 · **Implementation:** M0/B-R1 prerequisites in progress

Review [the plan](DISPLAY-AWARE-AUTO-QUALITY-PLAN.md) and
[the adversarial review](DISPLAY-AWARE-AUTO-QUALITY-REVIEW.md) together. The [initial Opus review](DISPLAY-AWARE-AUTO-QUALITY-OPUS-REVIEW.md) is now
received and reconciled in the review ledger. This handoff can be used for a
follow-up pass; no follow-up Opus verdict is claimed. Give Opus access to the repository and these
files, or attach their complete contents if repository access is unavailable.

## 1. Copy this request into Opus

```text
Perform an independent, adversarial architecture and implementation-plan
review. Do not implement, deploy, modify settings, or rewrite the plan yet.

Repository: /Users/pjunod/code/plurx
Read AGENTS.md and docs/README.md first, then:
- docs/streaming/DISPLAY-AWARE-AUTO-QUALITY-PLAN.md
- docs/streaming/DISPLAY-AWARE-AUTO-QUALITY-REVIEW.md
- docs/streaming/DISPLAY-AWARE-AUTO-QUALITY-OPUS-REVIEW.md (lifecycle wrapper)
- docs/streaming/DISPLAY-AWARE-AUTO-QUALITY-OPUS-REVIEW.txt (byte-identical supplied review)

User intent: Cinema should automatically deliver the highest useful quality
that the server, network and device can sustain smoothly. The immediate
example is a TCL tablet with a 2400x1600 screen: a 16:9 picture fits at
2400x1350. Keep original 4K when it plays well; when encoding is necessary,
consider 1440p rather than jumping straight to 1080p. Do not require the user
to manage resolution in Settings.

This is a follow-up to the preserved initial Opus review. Check each of its
four P1 and ten P2 dispositions, not only the original internal R1-R5.
Paul has explicitly decided:
- Keep one combined feature effort; accept native/Apple dependencies rather
  than shipping initial selection separately. Parser-only infrastructure
  compatibility remains a necessary prerequisite release.
- Allow up to 10% local enlargement: maximum scale factor 1.10.
- Include safely proved mid-play return to original, in addition to recovery
  at user seek/long-pause resume/next episode.
Do not reframe those preferences as missing approvals. Challenge whether the
revised implementation/release contract can honor them safely and concretely.
Also verify the correction that 12 Mb/s target means 18.16 Mb/s peak with
1.5x rate control and 160 kb/s audio, so 14 Mb/s cannot fit that candidate.

Check the actual code, not only the plan's descriptions. The plan records
its source baseline; report the revision you reviewed and any material drift.
Treat missing measurements as missing, not as failures already observed or
proofs already obtained. Distinguish blockers to building from qualification
work that belongs to implementation.

Challenge especially:
1. Whether useful-resolution selection is consistent with best picture
   quality, including HDR, bitrate and retaining the original.
2. Aspect-preserving fit, wide cropped movies, anamorphic SAR, rotation,
   backing pixels, PiP, external displays and unknown geometry.
3. Whether the candidate model can represent actual worker/decoder/grade
   constraints without changing offline behavior or promising absent HDR rungs.
4. Wire compatibility, capability advertisement, strict deserializers,
   preparation context, revision/sequence fences, replay and cache identity.
5. Ownership: Auto decisions, decode rescue, supply recovery and user actions
   must not race or produce multiple successors. Failed voluntary upgrades
   must leave the incumbent playing.
6. Whether upgrades and return to original can obtain sufficient evidence
   without getting permanently stuck or repeatedly disrupting playback.
7. Whether this is the smallest coherent effort, with explicit dependencies,
   feasible milestones, and tests that catch the failure modes rather than
   mirror the implementation.
8. Whether physical acceptance thresholds and Developer-switch graduation
   are concrete, feasible and faithful to repository policy.
9. Whether the internal review's fixes actually close the findings. Do not
   assume an accepted disposition is correct.

Return:
- Verdict: ready to implement, ready with named conditions, or revise first.
- P1/P2 findings ordered by consequence, each with exact plan section and
  source symbol, a concrete failing scenario, and the smallest corrective
  change. Avoid speculative complaints without a plausible failure path.
- Missing deterministic regressions and physical evidence.
- Any product trade-off that genuinely needs Paul's decision.
- A short list of strengths worth preserving.

If you cannot inspect the repository, explicitly label this a document-only
review and identify which source claims remain unverified. Do not claim tests
or physical measurements you did not run. Do not begin implementation.
```

## 2. Bring the result back before building

Attach or paste the complete Opus findings into this chat. Reconcile each
finding into the plan or record a reasoned disagreement, retaining its failing
scenario and evidence. Only then change the plan status to ready to implement.
The initial Opus verdict was “revise first.” This revised document records
its disposition; neither that disposition nor a follow-up prompt is a new
Opus approval. Paul has since authorized GPT-6.1 Sol implementation of the reconciled plan;
this optional follow-up prompt is historical review tooling, not a new approval
requirement or a waiver of implementation/release evidence.
