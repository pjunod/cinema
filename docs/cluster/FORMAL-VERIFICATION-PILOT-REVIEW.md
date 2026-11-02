# Formal verification review — preserve retries, time samples and caller scope

**Status:** Fable design review completed; amendments incorporated by the
proposal author; no second independent approval · **Reviewed / amended:**
2026-10-20 · **Original verdict:** approve with amendments · **Review source:**
`9ab523c32e00e5b56c15bbc7a71ac2d3d48c0d5e` · **Follow-up source:**
`e8a8d0c4c5fa1e7f3c69485038bb0b984c1b0a86`.

Companion to the [proposal and canonical execution ledger](FORMAL-VERIFICATION-PILOT-PROPOSAL.md).
Paul supplied the review in chat. This records its six required amendments,
M0 obligations, remaining answers and the author's source-checked
dispositions. Source follow-up is not a second Fable approval, a formal
result, compilation evidence or fleet acceptance. Line anchors below refer
to the follow-up source; reverify them at implementation time.

The proposal is ready for M0 under the amended contract. M1 remains dependent
on M0's actual semantics/tool feasibility evidence. An unexecuted milestone
supplies no acceptance evidence.

## 1. Disposition — all six findings change the build contract

| ID | Review finding | Author disposition | Evidence still owed |
|---|---|---|---|
| R1 | Hiqlite insertion can submit up to four times with the same predecessor; committed work can coexist with `FenceRejected` and cleared facade state | Proposal §4.3/§8.2 now model one facade call as multiple guarded submissions, plus lost acknowledgement and post-commit error. Retry qualification in §2 below | Insert-specific three-voter fault case, real retry guard case and retained item/facade state observations |
| R2 | Three authority time samples and different backend replacement validation | Proposal §4.3/§8.1 preserve precheck, successor and per-submission backend samples and classify error-order differences | Per-sample state, clock/delay cases, backend mapping and conformance |
| R3 | Boundary comparisons need exact operators and source anchors | Proposal §7 adopts the table in §2 below rather than one expiry boolean | Equality cases and faithful finite abstraction |
| R4 | Binding is limited to two job kinds; other callers need classification | Proposal §4.1 narrows the selected route; §3 below records bound, domain-only and unfenced paths, including follow-up of daemon publishers | Revalidate caller chains on M0's candidate and map bind refusal to yield |
| R5 | Generic queue module does not identify job deletion; declared retention constant is insufficient | Proposal §5.1/§7 use named maintenance transitions anchored in §3 below | Map installed trigger version, guards, detail deletion and finite threshold abstraction |
| R6 | Workflows do not provision Java | Proposal §9/§10.1 and M0/M1 ownership now include pinned JDK provisioning and smoke steps in existing preflight jobs | Supported runtime/artifact pin, runner smoke receipt and measured job budget |

The original review verified the token/binding/insertion interfaces, lease
and resolution constants, legacy domain-only allowance, stable binding
identity, release/acquire behavior, unfenced facade branch, post-commit local
revocation, bind-before-work ordering, three named regressions and the
effort/main gates. It noted that the existing dual-owner regression uses
`put_setting_fenced`; it does not establish insertion conformance.

### 1.1 Additional M0 instructions retained from the review

- Treat Hiqlite's missing-output error-text match as an assumption. Pin the
  dependency and exercise actual rollback and classification in a conformance
  test, rather than testing a string constant alone.
- Strengthen S6: a different attempt cannot replace the binding under the
  same domain fence. Model refused binding followed by yield, not work.
- L1 needs time advancement through `not_before` after bind/lease refusal
  (5,000 ms) and failure `retry_delay_ms`, not merely old-lease expiration.
- Store contract feature is `hiqlite-contract-tests`, implying
  `hiqlite-store`; retain the `hiqlite-3-voter` backend result.
- Inspect mutation traces, per-configuration state counts, completion and
  the three-voter R1 case rather than accepting only a green summary.
- Four tasks remain credible. The author widened the planning estimate to
  8–15 engineer-days plus review because M2 now includes insertion fault
  behavior and broader caller classification. This is still unmeasured.
- M1 owns a reviewed transition table. Automated source-hash drift detection
  in `mapping.json` is deferred to M3.

## 2. Publication and time boundaries

**R1 qualification:** [Hiqlite insertion:918](../../crates/plurx-core/src/store/hiqlite_publication.rs#L918)
loops `0..4` with the same predecessor and successor at lines 927–928.
Allocated mode rereads the candidate watermark at line 921. The retry branch
at lines 960–967 matches `item_identity_reuse` or
`UNIQUE constraint failed: items.id`; generic transport errors do not
automatically take that branch. Each submission through
[`atomic_publication`:187](../../crates/plurx-core/src/store/hiqlite_publication.rs#L187)
samples backend time again.

A committed-but-unacknowledged insertion is still an ambiguous outcome, and
a later submission of its exact predecessor must be stale. The R1 test must
target the selected insertion, retain its committed row and observe facade
state clearing. Test the real retryable identity-error branch separately;
if a combined commit/lost-reply/internal-retry trace is claimed, show the
concrete error path that makes it reachable. An artificial rule that retries
every error would verify a different implementation.

The facade clears local state on a backend error; it can also return
`FenceRejected` after backend success when locally revoked. See
[`fenced_call`:303](../../crates/plurx-core/src/store/publication.rs#L303).
S7 concerns transaction rejection/rollback, not every error returned by that
facade. No exactly-once insertion result is introduced.

| Sample | Source | Model obligation |
|---|---|---|
| Facade precheck | [publication.rs:318](../../crates/plurx-core/src/store/publication.rs#L318) | Reject local predecessor expiry at this sample; preserve revocation and lock ordering |
| Successor computation | [coordination.rs:52](../../crates/plurx-core/src/cluster/coordination.rs#L52) | Expiry is `max(sample + 90,000, predecessor expiry + 1)` with the actual saturation/overflow checks; revision advances |
| Hiqlite backend, each submission | [hiqlite_publication.rs:187](../../crates/plurx-core/src/store/hiqlite_publication.rs#L187) | Preserve sampled time before awaited transaction dispatch, not physical commit time |
| SQLite backend | [sqlite/mod.rs:1973](../../crates/plurx-core/src/store/sqlite/mod.rs#L1973) | Sample inside the connection transaction; preserve validation order |

The ordinary metadata clock `self.now()` at
[Hiqlite insertion:915](../../crates/plurx-core/src/store/hiqlite_publication.rs#L915)
is separate. Hiqlite renewal metadata `updated_at_ms` is successor expiry
minus 90,000 at lines 69–71, not its backend sample.

**R2 qualification:** SQLite explicitly checks successor expiry against the
backend sample. Hiqlite initially checks only successor > predecessor expiry,
but the matched predecessor must itself expire after the backend sample.
Thus accepted Hiqlite publication implies successor > predecessor > sample.
This alone is a validation/error-classification difference, not evidence of
a stronger SQLite accepted-publication safety property.

| Boundary | Exact comparison | Source anchor |
|---|---|---|
| Queue/domain validity at bind | Both expiries `> supplied now_ms` | [binding:39,51](../../crates/plurx-core/src/store/background_jobs_domain.rs#L39) |
| Queue renewal validity | Queue expiry `> supplied now_ms` | [queue:630](../../crates/plurx-core/src/store/background_jobs.rs#L630) |
| Queue takeover eligibility | Old expiry `<= supplied now_ms` | [queue:532](../../crates/plurx-core/src/store/background_jobs.rs#L532) |
| Reservation covers queue lease | Held expiry `>= job.lease_expires_ms` | [binding:45](../../crates/plurx-core/src/store/background_jobs_domain.rs#L45), [queue:636](../../crates/plurx-core/src/store/background_jobs.rs#L636) |
| Publication authority | Queue/domain expiries `> backend sample` | [Hiqlite:49,55](../../crates/plurx-core/src/store/hiqlite_publication.rs#L49), [SQLite:1996,2002](../../crates/plurx-core/src/store/sqlite/mod.rs#L1996) |
| Publication reservation coverage | Held expiry `>= queue expiry` | [Hiqlite:61](../../crates/plurx-core/src/store/hiqlite_publication.rs#L61), [SQLite:2008](../../crates/plurx-core/src/store/sqlite/mod.rs#L2008) |
| SQLite successor validation | Successor expiry `> predecessor expiry` and `> backend sample` | [SQLite:1984–1985](../../crates/plurx-core/src/store/sqlite/mod.rs#L1984) |
| Hiqlite successor validation | Successor expiry `> predecessor expiry`; UPDATE separately requires predecessor `> sample` | [Hiqlite:38,49](../../crates/plurx-core/src/store/hiqlite_publication.rs#L38) |
| Claim dispatch window closes | Elapsed `>= 120,000 ms` rejects | [queue:1495](../../crates/plurx-core/src/store/background_jobs.rs#L1495) |
| Initial no-fence resolution window closes | Elapsed `>= 120,000 ms` rejects | [queue:3465–3469](../../crates/plurx-core/src/store/background_jobs.rs#L3465) |
| Resolved token expiry | Expiry `<= now_ms` rejects | [queue:3497](../../crates/plurx-core/src/store/background_jobs.rs#L3497) |
| Domain acquire / renew | Old expiry `<= now` permits takeover; renewal requires `> now` | [SQLite:20,95](../../crates/plurx-core/src/store/sqlite/coordination.rs#L20), [Hiqlite:32,125](../../crates/plurx-core/src/store/hiqlite_coordination.rs#L32) |
| Domain release | Expiry becomes `min(old expiry, now)`; revision advances | [SQLite:123–131](../../crates/plurx-core/src/store/sqlite/coordination.rs#L123), [Hiqlite:162–168](../../crates/plurx-core/src/store/hiqlite_coordination.rs#L162) |

The second Hiqlite insertion guard checks the full lease tuple at
[lines 934–936](../../crates/plurx-core/src/store/hiqlite_publication.rs#L934).
[`bind_atomic_authority`:82](../../crates/plurx-core/src/store/hiqlite_publication.rs#L82)
replaces that predecessor parameter tuple with statement-zero returned
successor values. Model both checks in their real transaction, not a second
independent permission acquired earlier. Error classification matches
`StmtIndex(0) does not have observable row output` at
[lines 198–207](../../crates/plurx-core/src/store/hiqlite_publication.rs#L198).

## 3. Caller and maintenance inventory

“Fenced” does not imply queue-bound:
[`ActiveJobLease::publisher`:145](../../crates/plurxd/src/job_lease.rs#L145)
constructs a fenced facade for domain leases too. This is a scoped inventory
for the reviewed paths, not an exhaustive audit of every catalogue writer.

| Route | Classification | Verified source |
|---|---|---|
| Durable library worker | Bound fenced; only `library_scan` / `metadata_refresh` bind | [kind/resource guards:30](../../crates/plurx-core/src/store/background_jobs_domain.rs#L30); [worker bind:257–271](../../crates/plurxd/src/library_work.rs#L257) |
| Scan/enrichment invoked by that worker | Bound fenced through the passed lease/publisher | [worker:434–440](../../crates/plurxd/src/library_work.rs#L434); [state:5856–5889](../../crates/plurxd/src/state.rs#L5856), enrichment at 5932; [targeted scan:5172](../../crates/plurxd/src/state.rs#L5172), scan at 5203, enrichment at 5260 |
| Direct artwork metadata repair | Domain-only `provider:artwork` | [state:5478–5507](../../crates/plurxd/src/state.rs#L5478) |
| Artwork retry | Domain-only `provider:artwork` | [state:6430–6438](../../crates/plurxd/src/state.rs#L6430), sweep forwards to enrichment at 12357 |
| Genre backfill | Domain-only `provider:genres` | [state:6517–6546](../../crates/plurxd/src/state.rs#L6517) |
| Transcode manager with `publication_fence` | Domain-only fenced; not one of the two bindable kinds | [produce.rs:2083–2092](../../crates/plurxd/src/transcode/manager/produce.rs#L2083), also 2168–2169 |
| Core scan convenience wrappers | Explicitly unfenced | [scan/mod.rs:485](../../crates/plurx-core/src/scan/mod.rs#L485), also 717, 753, 1258 |
| Core metadata convenience wrappers | Explicitly unfenced | [metadata/mod.rs:801](../../crates/plurx-core/src/metadata/mod.rs#L801), also 850, 1255; [book.rs:157](../../crates/plurx-core/src/metadata/book.rs#L157), also 459; [local.rs:40](../../crates/plurx-core/src/metadata/local.rs#L40), also 148; [genres.rs:125](../../crates/plurx-core/src/metadata/genres.rs#L125) |

Bind refusal and unavailable domain lease yield until `now + 5,000` in the
[worker](../../crates/plurxd/src/library_work.rs#L260); failure additionally
uses its `retry_delay_ms`. A same-domain-fence binding replacement accepts
only the identical stable tuple; a different tuple needs a higher domain
fence, per [binding conflict guards:57–64](../../crates/plurx-core/src/store/background_jobs_domain.rs#L57).

### 3.1 Model installed maintenance statements, not a declared constant

| Statement/trigger | Eligibility and effect | Source |
|---|---|---|
| Enqueue admission trigger | At job count `>= 10,000`, retire up to 128 terminal jobs, excluding the admitted job, active/hydrating waiters and legacy fragment dependencies | [retention:20–40](../../crates/plurx-core/src/store/background_jobs_retention.sql#L20) |
| Current maintenance trigger | Later receipt-pressure definition replaces the earlier maintenance definition | [receipt-pressure:14](../../crates/plurx-core/src/store/background_jobs_receipt_pressure.sql#L14) |
| Prune attempts | Finished attempts with `resolve_until_ms <= now`; either at least 16 newer finished attempts for the job, or global attempts `>= 39,872` plus a newer attempt; limit 128 | [receipt-pressure:83–93](../../crates/plurx-core/src/store/background_jobs_receipt_pressure.sql#L83) |
| Compact waiters/internal receipts | Expiry deletion and receipt-pressure compaction at count `>= 15,360` | [receipt-pressure:94–111](../../crates/plurx-core/src/store/background_jobs_receipt_pressure.sql#L94) |
| Retire terminal jobs | Terminal plus age of at least seven days or global count `>= 9,000`; same active-waiter/legacy exclusions; limit 128 | [receipt-pressure:112–122](../../crates/plurx-core/src/store/background_jobs_receipt_pressure.sql#L112) |
| Retire job details | Before job deletion, remove its reservations and attempts | [schema:586–591](../../crates/plurx-core/src/store/background_jobs_schema.sql#L586) |

`MAX_RETAINED_JOBS` is declared at
[queue:50](../../crates/plurx-core/src/store/background_jobs.rs#L50); SQL
contains numeric literals rather than interpolating it. The SQL sources are
included at queue lines 74 and 77–78. SQLite installs them at
[migration:1255–1257](../../crates/plurx-core/src/store/sqlite/mod.rs#L1255);
Hiqlite at [migration:3585,3599](../../crates/plurx-core/src/store/hiqlite.rs#L3585).

M0 maps `PruneAttempts`, `CompactInternalReceipts` and `RetireTerminalJob`
with their predicates and deletion effects before reducing thresholds.
Do not infer a job-deletion action from a declared Rust constant or model
“keep exactly 16” as the production retention rule.

## 4. CI provisioning without another job

Neither [main CI](../../.github/workflows/ci.yml) nor
[effort CI](../../.github/workflows/effort-ci.yml) declares Java/JDK setup.
Ambient Java on a self-hosted runner is not a reproducible dependency pin.

M0 selects a supported exact JDK build and TLC artifact with recorded
SHA-256 values and explicit acquisition/cache paths. Provision and verify
`java -version` in the existing preflight jobs after checkout:
[main:239](../../.github/workflows/ci.yml#L239) and
[effort:97](../../.github/workflows/effort-ci.yml#L97). Prefer a pinned,
checksummed artifact or pinned setup action with equivalent provenance;
do not add an unqualified latest download or depend on global runner changes.
Local offline verification uses the same already-verified toolchain.

M1 adds the selected bounded smoke command to these jobs. Effort provisioning
and smoke must run under `compile_only`; copying the neighboring conditional
Node/oracle-source steps would silently omit them. Include provisioning plus
smoke cost in existing 10-minute main / 15-minute effort preflight budgets,
and justify any change to those existing timeouts. No new workflow/job is
needed. The full per-configuration 15-minute baseline remains explicit
task/promotion evidence, not work squeezed into a 10-minute preflight job.

M3 integrates measured catalog/path selection and drift checks. Missing or
unverified Java/TLC fails a selected check. Existing compiler, regression,
effort and promotion requirements remain in force.

## 5. Tool choice and review questions

The selected protocol tool remains [TLA+/TLC](https://github.com/tlaplus/tlaplus).
[Loom](https://docs.rs/loom/latest/loom/) explores instrumented concurrency;
[Kani](https://model-checking.github.io/kani/rust-feature-support.html)
documents unsupported concurrent features;
[Verus](https://verus-lang.github.io/verus/guide/) requires explicit
specifications and proof work for supported Rust. None automatically
establishes production conformance from a separate protocol model.

The original proposal asked eight questions: selected route and legacy
paths; adequacy of authority state; sampled versus commit time; bounded L1
and fairness; model/code correspondence; non-vacuous controls; task cost;
and preservation of gates/test obligations. R1–R6 and the M0 instructions
above record the actionable answers.

The review's remaining answers were: bound job checks use running state,
sampled-time expiry, same-fence reservations and owner removal while ignoring
renewable queue revision; Hiqlite insert rechecks statement-zero authority;
reviewers must inspect traces/counts/completion/R1 backend evidence; four
tasks are credible but M2 cost was optimistic; and the effort/main integration
and limits on test reduction/deadlock claims are adequate.

M0 may proceed after these design dispositions. The canonical ledger still
records M0–M3 as unimplemented and unaccepted. New source differences or
counterexamples reopen the corresponding assumption or property; this review
does not preapprove a changed model boundary.

## Final implementation review — 2026-10-20

Independent Fable 5.1 (`claude-fable-5-1`, task `t_9e6136d9`) reviewed
candidate `88f73b08c` against `2c0e8b14df`. The same saved session completed
a targeted continuation of initially unread areas, retaining its first
findings. It performed no builds, tests, TLC runs or source edits. Final
verdict: **changes requested**, not approval or runtime evidence.

| Finding/comment | Coordinator disposition |
|---|---|
| F1 · Recovery old-reply non-adoption claim was vacuous | Narrowed README/CONTRACT and mapping prose: `DeliverOldReply` is a Boolean scheduling placeholder with no payload or boot comparison. L1 checks eventual current claim, not rejection of that reply. Fresh-scenario delivery guards are described separately. No model/config bytes changed. |
| F1 · Recovery seed description | Recorded Init clock0, old running claim expiry2 and not-before2/3; fair time reaches the applicable barrier. No directly seeded queued/yielded/failure-retry case or time above3 is claimed. |
| F2 · Archive test expected an error never emitted | Test now expects the actual escaping-link rejection; extraction protection is unchanged. |
| C1–C5 · Reductions and scope | Documented monotone revisions, permutation invariance of S5–S7, absorbing cancelling state, takeover over-approximation and disabled finite-boundary deadlock checking. |
| C8 · Smoke could exceed a stricter mapped timeout | Runner now takes the minimum of mapped timeout, profile cap and remaining profile budget; existing contract test covers the stricter mapping. |
| C10 · Effort compile-only formal steps | Retained intentionally: the accepted CI disposition requires affected formal smoke in compile-only effort runs. No Rust/Apple/Android unit suite is added there. |
| C11 · S5 control exit13 | Confirmed on exact model `96d4e492…`: actual exit13, named action-property violation, four-state trace. Final runner must still observe its pinned outcome. |
| C12 · Review basis said pending | Updated to completed review with findings and coordinator dispositions; qualification remains separate. |

The full report and machine verdict are retained with PR1241 evidence.
Remaining review residuals concern maintenance SQL predicate re-derivation,
witness-depth derivation and previously untraced helpers; no additional
critical-code gap was declared. The real backend and strict-runner checks
remain required. No test reduction or whole-cluster deadlock claim follows.
