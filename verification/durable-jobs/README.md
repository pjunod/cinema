# Durable jobs — check bounded publication authority with TLC

**Status:** implemented and independently reviewed; all 16 bounded configurations
and 11 focused runtime tests qualified. Delivery and CI are tracked in the status page. This package checks a finite model of bound
`library_scan` / `metadata_refresh` workers publishing through
`insert_item_fenced`. It is a bounded protocol check and a source mapping,
not a refinement theorem or a result about the entire cluster.

Read [CONTRACT.md](CONTRACT.md) for source guards, transaction boundaries,
caller classifications and backend examples. The
[pilot proposal](../../docs/cluster/FORMAL-VERIFICATION-PILOT-PROPOSAL.md)
owns acceptance; the
[status page](../../docs/cluster/FORMAL-VERIFICATION-PILOT-STATUS.md)
records review and qualification. `mapping.json` binds the selected source,
model, configurations, properties and expected outputs; changed inputs require
a deliberate mapping review.

## Run the pinned package

```bash
scripts/bootstrap-formal-toolchain fetch --cache-dir target/formal-toolchain  # Fetch pinned artifacts.
scripts/check-formal --help         # Show smoke, full and single-configuration options.
scripts/check-formal smoke --cache-dir target/formal-toolchain  # Run bounded preflight selection.
```

The runner owns output paths, timeouts and receipts. It must fail closed on
missing tools, source drift, malformed receipts, unexpected violations,
incomplete exploration or timeout. A positive result requires the explicit
TLC completion marker and zero states queued. A control/witness requires its
named violation and a real counterexample, not an unrelated checker error.
Do not infer success from process exit alone.

The pinned release artifact is TLC 1.7.4; its startup banner identifies the
internal checker as **TLC2 Version 2.19**. `toolchain.json` pins the artifact
and Temurin **21.0.12+8** archives and SHA-256 hashes. Development runs used
`-Xmx4g -XX:+UseParallelGC`, four workers, fingerprint polynomial47, and
hard process kill/wait limits. The source/config hash and exact runtime
options in each receipt identify the checked candidate. Peak process RSS is
separate from the4GiB heap cap. Final qualification is recorded after review,
not inferred from these development checks.

## Seeds share one safety transition relation

`Fragment` selects **initial seeds only** for authority operations. It does
not switch off claim, takeover, renewal, cancellation, binding, publication,
replay or resolution in `SafetyNext`. The all-seed configurations include:

| Seed | Initial authoritative and local state |
|---|---|
| Claim | Queued valid demand, no local authority, two potential claim senders |
| Publication / renewal | One live queue attempt, both reservations, matching domain/binding and locally held tuples |
| Binding | Either a fresh unbound live attempt, or current claim2 with an obsolete claim1 binding under the same domain fence |
| Retention | Current claim2 plus retained finished claim1; maintenance eligibility classes may settle and prune/compact/retire it |

Worker/node identity, boot identity and claim identity are separate. A
bounded restart clears local held authority while retaining old request
records and durable rows; stale reply adoption checks the current boot.
Claim read and CAS, send/apply/reply, successful bind, three publication
samples, transaction application and facade delivery are separate actions.
Bind refusal, domain release and queue settlement are separate actions too.

Each execution explores one queue heartbeat, one domain acquire/renew/release
operation, one selected environment displacement (cancellation, removal,
resource loss, takeover, outage or restart), and **one facade publication
call** with up to four identity-conflict submissions. Each worker can issue
one initial claim and one replay/resolution. Finite claim identities never
recycle; no counter wraps. All seeds assume valid selected payloads, capable
workers and sufficient remaining job-attempt budget for their bounded claims.

This excludes arbitrary repeated fault combinations, repeated publication
calls, a full daemon lifecycle, general queue admission fairness and
cross-seed lifecycle composition. Local deadline/revocation mechanisms are
represented only through lost local authority, reboot and post-commit facade
outcomes, not every runtime timer. Old pending publication samples remain
unchanged while other authority changes occur; they are never clamped away
or automatically failed for age.

## Finite bounds are part of the result

| Configuration | Jobs / workers / boots per worker / claims / domains / resource keys | Event horizon | Properties / intended outcome |
|---|---|---|---|
| `smoke.cfg` | 1 / 2 / 2 / 2 / 1 / 2 | 6 | S0–S7; complete without violation |
| `baseline.cfg` | 2 / 2 / 2 / 3 / 1 / 2 | 8 | S0–S7; complete without violation |
| `lifecycle.cfg` | 1 / 2 / 2 / 3 / 1 / 2 | 11 | S0–S7 from claim seeds through commit and facade delivery |
| `control-binding-ignored.cfg` | 1 / 2 / 2 / 2 / 1 / 2 | 6 | S2 violation after removing the backend binding guard |
| `control-replay-fresh-fence.cfg` | Same small domains | 6 | S4 violation after replay manufactures a new fence for the same identity |
| `control-renewal-revives-cancel.cfg` | Same small domains | 6 | S5 action-property violation after relaxing both state and exact revision guards |
| `recovery.cfg` | 1 / 2 / 2 / 2 / 1 / 2 | Unused | L1 under the concrete fair stable suffix |
| `control-retry-stalls.cfg` | Same recovery domains | Unused | L1 temporal counterexample when the first unsuccessful poll permanently suppresses later polling |

`MaxEvents` bounds every safety transition at the top-level `Next`, rather
than silently disabling a particular stale-message or cancellation action.
At the horizon only specification stuttering remains. Six events cannot
compose every lifecycle. Eight events at two jobs retain contention and
seeded bound publication, independent renewals, binding/refusal and stale
traffic; fresh-claim-through-publication composition at two jobs is outside
that horizon. `lifecycle.cfg` checks that longer composition at one job only.
Its exact path is send1→read2→CAS3→claim reply4→domain acquire5→bind6→facade
precheck7→successor8→submit9→commit10→facade delivery11. It uses the same
shared safety relation, failure actions, identity dimensions and view.

The two-job ten-event calibration was explicitly abandoned as **incomplete**:
at its last progress sample it had97,640,975 generated states,
12,616,232 distinct states and7,137,143 queued states, depth9; the owned
checker was stopped after647.03s (peak RSS4,514,365,440bytes). Its exact
model/config hashes, raw log and stopped receipt remain durable development
artifacts outside the required configuration registry. This cost result
justifies the declared two-job8 / single-job11 coverage split, not a safety
verdict or an equivalence to two-job long-run exploration. If the single-job11
configuration proves infeasible at900s, the predeclared fallback is10 events,
which reaches commit but excludes facade delivery; changing to that fallback
requires a new config hash and explicit coverage disposition. Nine events
would reach only pending submission and is not an accepted fallback.

Supplemental reachability probes explicitly show the longer normal retry
path; they do not prove exhaustive two-job long-run coverage. The horizon
is an exploration limit, not a production terminal state or liveness premise.
TLC is invoked with `-deadlock`, intentionally disabling ordinary deadlock
checking at these finite boundaries. No deadlock-freedom or testing reduction
claim follows.

Counters use finite ranges (job/domain fence at most5, revision at most8,
expiry at most8). These are checked type bounds, not modulo arithmetic or
silent overflow guards. Resource capacity is two slots per key; all jobs
require both keys. Attempt retention uses the actual finished/window/newer
predicate with reduced thresholds: one newer finished attempt, or pressure
at two retained attempts plus a newer attempt. The finished/window/newer
conjuncts are computed from modeled attempt rows. Receipt/job global-pressure
and age are explicit eligibility classes; ordering, pagination and external
receipt pools are outside the model. Enqueue pressure retirement is mapped
in CONTRACT but is outside the selected finite admission fragment.

## Samples preserve equality and backward observations

Safety samples are absolute classes0,1,2,3, independently chosen for each
operation. Initial queue expiry is1 and domain expiry is2; dispatch window is2; queue
lease duration is1, domain duration is2 and publication extension is3. Thus before/equal/after
expiry and window equality remain distinct. Later computed expiries can
lie beyond the sample horizon3; their subsequent expiry is not explored. Per-request timestamps persist
across delayed application; Hiqlite retries resample each submission while
keeping exactly the same predecessor/successor. SQLite resamples inside its
transaction action. The facade precheck, successor computation and backend
sample are separate state fields; the full submission history remains in
raw witness traces.

With integer samples and queue duration1, a valid queue renewal computes
`sample+1 <= old expiry`. It advances revision and tests stable binding,
reservation updates and backward-clock outcomes, but cannot demonstrate
ordinary forward-time queue expiry extension. The real independent-renewal
conformance example covers that behavior; this is an explicit finite S6
coverage limit, not a claim about all production renewal timings.

Time advancement is represented by **different observations in successive
sample actions**, including backward/fast/slow observations. There is no
safety `Tick` action or clock monotonicity premise. The three-samples witness
requires precheck0, successor2 and backend1 in an actual path; it cannot be
true at Init or be satisfied by a disabled clock-transition counter. Rebased
or clamped time from the inherited draft is not used.

`ModelView` in the positive safety configurations quotients publication
**diagnostics**: precheck/successor/backend sample history and bind-refusal
sample values. Every guard and checked safety/action property ignores those
fields. The computed predecessor/successor, retry `left`, outcome, commit
flag and originating boot remain in the view. Any two states equal under the
view enable the same projected successors: successor construction uses the
retained computed tuple, retries use retained `left`, and appending a sample
changes only the omitted history. S0–S7 depend only on projected state,
including accepted events' property truth and count; full snapshots remain
in raw state. The quotient therefore preserves
these finite safety checks. The view additionally quotients old **observed**
local/request token revision and expiry to stable identity plus an `Exact`
flag and a well-typed flag. Current tokens are fully determined by the
retained job row. Every observed revision was current or older when received;
job revisions never decrease, identities never recycle, and deleted jobs
never re-admit in this model. The revision writers `Cancel`, `Takeover`,
`ApplyRenew`, `ApplyClaim` and `RefusalSettle` each increment the retained
revision; all other positive safety actions preserve it. `RecoveryApply`
also increments it in the separate recovery relation. The defect-only
`ReplayClaim` writer increments it as well, although controls do not use
the view. There is no reset or wrap transition. A stale token therefore
cannot become exact later. In the positive unmutated relation, its numeric
revision/expiry are read only through `Exact`; stable identity remains for S4, and the explicit
type bit preserves S0. Failed reply tokens are unused and omitted. A claim
request's observed expected revision is similarly current or permanently
stale. Claim-context expected/sample fields are never read after dispatch;
job/node/boot/claim/dispatched remain for replay and resolution. Domain leases
and publication predecessor/successor tuples retain all numeric values,
because their expiry is read directly. These reductions preserve enabledness,
projected successors and S0–S7 on reachable states. They are not used for
controls, diagnostic witnesses or temporal recovery.

Terminal local phases (`done`, `facade_error`, `yielded`) cannot read their
publication record again, so the view drops those retired records. Accepted
events project to cardinality plus their S2/S3 truth values; raw states still
retain full pre-state snapshots. No protocol guard reads event content, and
one facade call permits at most one commit. After the first commit the item
set is absorbing: it never replaces, overwrites or deletes an event. S7's
item equality is therefore equivalent to unchanged cardinality on all
reachable transitions, and rejection still retains the domain tuple. S0
has no item-content predicate to lose. Finally, job receipt/retain/user-scope
and age/pressure/legacy diagnostics project to the exact terminal-retirement
eligibility and a full-row type bit. These fields can change only during
once-only retention preparation; subsequent compaction changes only the
omitted receipt field plus its retained budget. Every future authority guard
ignores them, and retirement reads exactly the retained eligibility predicate.
This preserves enabledness, projected successors and S0–S7 while avoiding
irrelevant receipt-history combinations.

Safety symmetry permutes only the independent Worker, ResourceKey and Job
model-value sets. Numeric boots, claims, fences and revisions are never
permuted: arithmetic and ordered freshness would invalidate that reduction.
The S5–S7 `PROPERTY` declarations are action safety formulas: consistently
permuting these model values in both states preserves their truth, just as
it preserves the actions and S0–S4. S5 quantifies over jobs and compares
state labels; S6 compares binding records and the unpermuted numeric domain
fence; S7 quantifies over workers and compares outcomes, catalogue sets and
domain records. None selects a distinguished model value. This argument
applies to those action safety properties, not arbitrary temporal formulas.
No symmetry or view is used for L1 or any liveness check.

## Witnesses demonstrate reachable transitions

The seven `witness-*.cfg` configurations deliberately check false invariants.
A targeted violation is the required result; each trace is a normal-model
path, not a production-defect claim. Their event horizon is16. They use no
symmetry or view, retaining actual diagnostic samples.

| Configuration | Named false invariant | Required path |
|---|---|---|
| `witness-three-samples.cfg` | `Witness_ThreeSamples` | Precheck0, successor2, backend1 through separate actions |
| `witness-four-submissions.cfg` | `Witness_FourSubmissions` | Three actual conflict replies and a fourth submission, with same predecessor/successor |
| `witness-cancelled-renewal.cfg` | `Witness_CancelledRenewal` | Cancellation and an outstanding renewal overlap |
| `witness-replay.cfg` | `Witness_Replay` | Replay retains ledger boot/fence identity |
| `witness-bind-refusal.cfg` | `Witness_BindRefusal` | Exact live replacement attempt is refused under an existing same-domain-fence binding |
| `witness-precommit-rollback.cfg` | `Witness_PrecommitRollback` | Identity-conflict transaction leaves catalogue and predecessor domain unchanged |
| `witness-displacement.cfg` | `Witness_Displacement` | Successor claim displaces bound authority before publication rejection |

The three publication-only probes have the visible
`PublicationWitnessBound` constraint: no environment displacement, queue
renewal, domain change or additional claim allocation. This selects a valid
normal path to measure reachability without substituting for unrestricted
positive checks. The other probes are unrestricted. Supplemental
`control-bind-before-work.cfg` removes the caller ordering premise with the
binding mutation and demonstrates S2 failure through the real domain-only
backend allowance. Its targeted trace differs from the canonical S2 backend
binding-guard control.

S5's composite mutation permits cancelling state and ignores predecessor
revision while retaining stable identity, expiry, reservations and removed-
owner checks. Cancellation changes both revision and state in production;
dropping only one guard would not produce this control. In the unmutated
model, `cancelling` is absorbing: no modeled completion or settlement action
moves it onward. S5 therefore checks that modeled actions cannot revive a
cancelling job; it is not a general cancellation-versus-completion theorem.
The mutant trace supplies the reachable revival counterexample. S4's
mutation is an intentional wrong replay result that advances the fence; it is not a claim
that removing one SQL clause alone causes that behavior.

## L1 uses a stable recovery suffix

Recovery uses a constructed running-job seed with old claim1, retained
binding/reservations and surviving boot2. At Init, time is0, old expiry is2,
and not-before is either the yield delay2 or failure delay3. This combines
the expiry and backoff barriers in one seed; it does not directly model a
queued yielded/failed-retry row. Fair time advancement must reach the
applicable barriers: the delay2 case may claim at time2, while delay3 needs
time3. Time advancement stops once the fresh claim is held. Demand stays
valid, no new cancellation, removal, compaction or outage occurs, resources
are available and quorum is reachable. Concrete polling, retry, sending,
applying and acknowledgement actions have weak fairness, as does finite
time advancement. Fairness does not assume eventual claim success. Claim2
is reserved and cannot be exhausted before success.

`DeliverOldReply` is only a fair scheduling placeholder: it toggles a flag,
has no reply payload or boot comparison, and leaves authority unchanged.
L1 makes no old-reply nonadoption claim. Payload boot checks exist in the
fresh safety relation's `DeliverClaim` and `DeliverRenew`; those checks are
outside `RecoveryNext`.

The model checks eventual first fresh **current claim** in that constructed
suffix, not completed work, publication, useful throughput, bounded response
time or arbitrary production backoff deadlines. Its success state is
absorbing except acknowledgement/the scheduling placeholder. The
`retry_stalls` mutation lets a poll before eligibility disable every later
poll. The resulting fair stuttering suffix violates L1 despite time reaching3;
it demonstrates why poll/retry scheduling is a real premise. Production
plausibility comes from the library worker poll/backoff loop and functioning
authority operations, not a theorem about runtime scheduler fairness.
