# Desktop tray review — Astra findings and their implementation-plan disposition

**Status:** done · **Updated:** 2026-10-19 · Astra approved revision 2 after
R1–R8 were resolved. No implementation or runtime acceptance.

Companion to [the implementation plan](DESKTOP-TRAY-IMPLEMENTATION.md), which
owns requirements, task dependencies and acceptance. This file records the
actual independent review and how its findings changed that plan.

## 1. Candidate and reviewer

Astra (`gpt-6-astra`, provider `openai-codex`) reviewed the complete first draft
through Hermes task `t_9ed58371` on board `plurx-tray`. The task completed with
**CHANGES REQUIRED before implementation dispatch**. No model substitution
or implementation review is claimed.

Source baseline: `84ad750a978e9ba953e23b96702719068bb5adfc`.
Reviewed draft plan SHA-256:
`29c793c2710e76e952d163c7fa0192f493f139287ada8aa4c7892a207da28a5a`.
The review also covered the small folder-index and P31 backlog additions,
which had no independent defect. Astra read targeted installer, package,
public-release, restart/fence and identity sources and the installed launchd
manual; no build, service action, deployment or publication was performed.
The full original report and exact reviewed-source hashes are retained in
the task result. The findings below are a faithful condensed record; line
numbers in that original report refer to its immutable 492-line draft.

## 2. Findings and revision 2 corrections

| ID | Priority | Independent finding | Correction in the implementation plan |
|---|---|---|---|
| R1 | P1 | Existing rolling restart preparation cannot authorize ordinary sole-voter downtime. | §6.1 defines authenticated standalone planned downtime, distinct multi-voter/learner gates, uninitialized owner control and explicit recovery/unknown refusal. M2 owns the daemon/controller integration. |
| R2 | P1 | Legacy macOS releases may live in root-owned prefixes; app/runtime layout and migration were unspecified. | §3.1 chooses a stable per-user app/runtime/control layout and one-time installer migration. Protected old paths stay untouched; conflicting protected service registration requires explicit authorized bootstrap cleanup. |
| R3 | P1 | Supervisor Stop/bootout can escalate to SIGKILL, so PID disappearance does not prove graceful shutdown. | §6.1 uses daemon self-shutdown through authenticated local IPC and a clean drain/store-close receipt bound to exact process/operation. Hung, forced and uncertain exits cannot authorize install. OS kill-timeout policies stay finite. |
| R4 | P1 | A final prepare poll does not prevent expiry/cancel from reopening admissions before shutdown. | §6.1 adds atomic exact-generation commit to a non-expiring shutdown owner. Cancellation cannot reopen a committed fence; controller death leaves recovery ownership intact. |
| R5 | P1 | Journaling alone provides no boot-time recovery owner, lock order or protection from self-replacement and outside policy changes. | §6.2 defines nonblocking package locks and scriptlet transaction joining; §6.3 defines stable non-GUI startup guards, boot recovery and stage actions. Capsules survive helper replacement; upgrade never rewrites startup policy. Outside administrative operations are explicitly not universally serialized. |
| R6 | P2 | Public node IDs and a separate service PID do not authenticate the HTTP peer before saved credentials are sent. | §3.2 uses same-connection HTTP over a protected Unix socket with kernel peer PID/UID and process-generation proof. Same auth middleware, native-owner authorization and restricted helper inspection are explicit. No token-bearing loopback fallback. |
| R7 | P2 | Linux distro dependencies contradict a blanket offline promise, and unconditional companion selection could install desktop on headless hosts. | §5.1/§6.2 distinguish self-contained macOS from Linux prerequisites, require prereqs before drain, define server-only/desktop cohorts and refuse package holds or unrelated solver removals. |
| R8 | P2 | Task merge acceptance depended on later implementations and omitted daemon/controller integration edges; test timing was inconsistent. | §7 splits early compile/contract evidence from integrated/native acceptance, makes M2 integration explicit and moves whole-package checks to M5/M6. §8 preserves the current user-provided AGENTS.md requirement for focused per-task regressions and reuses unchanged evidence. |

R8's suggestion to defer every task regression until the final project review
came from the reviewer's inherited HDR workflow. The current chat's explicit
repository AGENTS.md requires the smallest focused regression on task branches
before push/merge and takes precedence. The revised plan keeps that requirement
while ordering whole-candidate review/fixes before its affected integrated
batch; it does not transfer unrelated HDR test deferral into this project.

Optional improvements were also incorporated: logind session proof and late
tray-host behavior (§4.2), explicit externally disabled launchd handling
(§4.1), skipped key/controller bridge manual bootstrap (§8.1), and consistent
backup scope/migration classes (§6.3). §8 names regression/receipt cases for
each required correction. These are design contracts awaiting implementation,
not passing test claims.

## 3. Follow-up and documentary checks

Astra task `t_ec1a09fb` independently checked revision 2 and returned
**APPROVE the implementation plan**: R1–R8 resolved at design-review level,
no residual blocking finding. The reviewer explicitly accepted the current
repository's per-task focused-regression requirement; the initial HDR-wide
test-deferral suggestion was overbroad for this project.

Approved revision-2 plan SHA-256:
`0f04aa9b48d19453aef2e67d462a34aae1a32fc5c667d88e33a19b468abfd17d`.
The full exact-candidate recheck, per-finding dispositions and companion
hashes are retained in that task result. The reviewer permitted subsequent
review-status/ledger updates without another architecture review and called
two summary phrases nonblocking: Linux controller/helper ownership belongs
to the server package, and initial Linux upgrades require already-installed
distro prerequisites. Both summaries now match those authoritative details.

Final plan SHA-256 after editorial/status changes and a mechanical document
split to meet the repository's 40 KB limit:
`ba892680f5f96e1fbe962561a14c3df36d19a1cdb58f3a81bbc868665377b8a4`.
Section 6 now lives in [the upgrade transaction contract](DESKTOP-UPGRADE-TRANSACTION.md),
SHA-256 `4e2456e40f29ee7e7b5c6a3797b24509adc16f951d4940eecaf0097b18532340`.
Its approved mechanisms and numbering remain unchanged; only placement,
introductory text and cross-document links changed. Earlier §6.1–§6.3
references in this record refer to that companion document.
Approval is for the plan, not implementation, native acceptance, deployment,
promotion or release readiness. Signing/key rotation and distribution
credentials remain the plan's launch prerequisites.

Documentation-index checks passed six tests on both the initial and revised
candidate. Final `make paperwork` passed all 14 checks after the mechanical
size split against all five staged documentation files. The staged whitespace
check also passed.
The normal commit hook passed catalog lint and Rust formatting, then Clippy
reported `dead_code` at `crates/plurxd/src/vodserve.rs:924`:
`PreparedRootAdmission::production_evidence` is unused. That source file is
unchanged from the inspected baseline; no Rust or native behavior changed in
this task. Remaining compilation was interrupted after the error. The hook
was not bypassed and the documentation remains uncommitted on its branch.
