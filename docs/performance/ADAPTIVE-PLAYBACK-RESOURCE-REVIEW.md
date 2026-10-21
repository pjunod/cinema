# Adaptive resource review — findings and their disposition

**Status:** done · Fable 5.1 design approved; implementation remains planned · **Written:** 2026-10-20.

Companion to [the plan](ADAPTIVE-PLAYBACK-RESOURCE-PLAN.md) and its
[acceptance contract](ADAPTIVE-PLAYBACK-RESOURCE-ACCEPTANCE.md). This ledger
records design review only; it does not establish implementation or hardware
acceptance.

## 1. Independent review identity

Reviewer: `claude-fable-5-1`, provider `anthropic`, profile `hdr-fable-api`.
Task `t_e3cef54d`, board `plurx-resource-plan`, initial run 1, dispatched through
the user-selected coordination chat. No substitute model or subagents.
Source baseline: `a06be68b90db5e32527f8f72a512ab09cce643fe`.
Initial plan SHA-256:
`7544cebeabe7eef98a2889f8b99952f5b55fa15ed140c4e0b3a815a059fd1cb9`.
Initial verdict: **CHANGES_REQUESTED** after reading all 566 plan lines and
bounded source ranges. Several lifecycle anchors were confirmed only by symbol
existence; the private report distinguishes them. No runtime checks were run by
the reviewer, and no runtime acceptance is inferred.

Raw input/report/receipt are retained in the coordinator's durable review
workspace named `adaptive-playback-plan-review-20261020`. Local absolute paths
and host names are deliberately excluded from the public-mirrored repository.
The original report and verdict must be preserved before the same-task recheck.

## 2. Changes closed by the same reviewer

| Finding | Concrete issue | Disposition in revised plan |
|---|---|---|
| F1 · high | Dropping a reservation could appear to release a still-running child | §4.2 makes release receipt-driven; Drop transfers custody or leaves UnknownHeld; panic, failed wait and no-child cases are specified |
| F2 · high | Prepare preceded the owner epoch required by existing transport | §7.2 creates a non-serveable provisional durable owner first, fences Prepare by epoch, and activates only the durable winner |
| F3 · high | A local trial could spend the deadline needed for peers | §6.1 ranks known-capacity peers first, reserves A/R budgets, limits local trial spend and defines bounded parallel attempts and winner cleanup |
| F4 · high | Paced output could look too slow; trial intervals were ambiguous | §6.2 defines monotonic observation intervals, state-specific evidence, refill/runway behavior and insufficient-evidence handling for long segments |
| F5 · high | Saved legacy defaults could permanently masquerade as intentional caps | §4.3 specifies absent/legacy-ambiguous/explicit/automatic states, operator resolution, untouched form round-trips and old-writer/rollback behavior |
| F6 · medium | Whole-budget background reservations could survive the redesign | §6.3 and M3 require registered preemptible work, tested yield/reap, same deadline budget and replacement of blanket thread accounting |
| F7 · medium | Unadopted or expired preparations might disappear from drain accounting | §9 names provisional, unadopted, losing and expired preparations; Prepare/Adopt reject the fence and cleanup remains counted |
| F8 · medium | Existing admission limits inputs to three roles | §4.2 preserves three per family, adds a bounded two-family/six-role atomic overlap contract and tests four/six roles plus rejection of seven |
| F9 · medium | Strict old peers could reject unknown fields before an explicit refusal | §7.2 requires fresh negotiated capability before send; authenticated pre-handler rejection differs from unknown transport outcome |
| O1 · low | Multiple daemons could overwrite local history | §5.3 specifies instance-owned paths, exclusive locking and isolated volatile fallback |
| O2 · low | False pacing failures could trigger cooldown | §6.3 limits cooldown to actual service/pressure failure; acceptance covers paced 0× |
| O3 · low | Local-trial budget exhaustion was not explained | §8 includes the distinct operator diagnostic and reserved-versus-spent trace |
| O4 · low | Latency comparison lacked a reproducible baseline | Acceptance §1 specifies alternating builds, fixed inputs, 100 measured events per cell, cache separation, warmups and failure accounting |

Source inspection additionally confirmed the current family builder creates
video, optional audio and optional companion roles, and calls `try_admit_family`
with retained bindings. The new six-role overlap is a proposed extension, not a
claim that the present three-role API accepts four.

## 3. Validation and remaining work

**Author-run** initial docs checks (not reviewer-executed): paperwork 14/14; index tests 6/6; infrastructure tests 14/14;
relative local links and staged whitespace passed. Updated files and the new
acceptance companion require the same affected validation after final revision.
Final recheck identity, hashes and verdict are below. M0 contracts,
implementation, qualification and deployment remain unstarted.

## 4. Same-reviewer recheck

Round 2, same task/session `20261020_204116_1e0669`: **APPROVE** for design only;
all F1–F9 and O1–O4 closed, no blockers remaining. Reviewed plan SHA-256
`4531ce2855e10a0e6ab534567872539dace87d3a2700c7e112b49028d8c6c0e3`;
acceptance SHA-256
`16868e4e3eca9d870f3f32e3bff5d47aa11d89ef8c00eecbed18d2c1fe55d6e0`.
Round-2 report SHA-256
`d86f1bda1449a9f7a79f8669ba34aefd57610f83dedf408b1716666cb24e0bf4`.

Optional N1 is applied in §6.1: skip a trial that cannot cover verified startup,
cleanup and two seconds of useful production. Optional N2 is applied to M0:
verify the durable request-claim/route lifecycle before wire design. N3 is an
attribution clarification: validation counts are author-run checks, not reviewer
execution. The plan and acceptance remain unimplemented. The same reviewer confirmed these final optional edits in round 3 below.

## 5. Final approval receipt

Round 3, same Fable 5.1 task and saved session: **APPROVE**, no remaining
blocking or optional findings. F1–F9/O1–O4 stay closed; N1/N2 are confirmed
consistent with the approved budget, pacing and ownership rules; N3 attribution
is confirmed. Coverage was the two changed plan hunks and ledger changes;
unchanged round-1/2 evidence was retained. No new runtime evidence was claimed.

| Artifact | Approved SHA-256 |
|---|---|
| Normative plan | `be993867c7b1db568663f9d793fefbf87c9679e027f67064b7884d281209f77f` |
| Normative acceptance | `16868e4e3eca9d870f3f32e3bff5d47aa11d89ef8c00eecbed18d2c1fe55d6e0` |
| Final reviewer report | `25273670abe89ee197440620b6f89e9e2e3b43df801def5f0052eab793380cf7` |
| Final reviewer verdict | `2997cbb34417b1832c224108c095c85453f36cbdd7c2889b56a315780743bfb5` |

This receipt/status bookkeeping was appended after approval; the normative
plan and acceptance bytes are unchanged. Author-run revised paperwork passed
14/14 checks and the documentation index passed 6/6. The normal initial commit
hook passed catalog lint, rustfmt, workspace/all-target Clippy with warnings
denied and served JavaScript syntax. These are document/source checks, not
acceptance of an adaptive scheduler that has not been implemented.

## 6. Implementation-time compatibility clarification (not new approval)

The implementation manager source-verified that hardware zero disables hardware
while software-pool zero allows one saturating idle-pool job and serializes
further work. Nonzero undersized legacy service budgets also retain the existing
idle-pool exception where applicable; bundle/family paths require specific
coverage. These are legacy thread-service semantics, never exceptions to physical
CPU/memory/device safety limits. Plan §4.3 and acceptance now spell this out.
Original approved hashes in §5 remain historical and unchanged; current document
bytes differ by this explicit clarification, not a retroactive reviewer receipt.
The final independent implementation review must include this delta and its
key-specific regression expectations. No new runtime acceptance or test execution
is claimed by the manager clarification.
