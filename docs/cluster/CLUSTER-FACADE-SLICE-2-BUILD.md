# Cluster facade slice 2 — media rosters and Live TV authority

**Status:** built — implemented and independently reviewed.
**Owner:** Codex. **Prepared:** 2026-11-16.

Delivery and qualification evidence is tracked in
[promotion #1811](http://forge.lan:3000/noirr/plurx/pulls/1811). Earlier branch
names and checkpoint receipts below are historical. Fleet acceptance remains
after Paul’s deployment.

Companion to [the facade contract](CLUSTER-FACADE-BUILD.md) and
[the cluster audit](CLUSTER-LAYER-AUDIT-2026-11-14.md).
This is the bounded execution plan for slice 2; it does not implement B3–B5
or change membership, reachability, freshness or serving policy.

## Outcome

The four remaining media artifact roster readers use the shared cluster
Directory with Purpose::MediaWorkers. Live TV HTTP asks state.cluster for
its serving answers. Composition roots pass facade answers and subscriptions
to existing local authority consumers. Every removed census site lowers its
pin; legitimate local state is preserved rather than renamed to hide it.

## Source findings and invariants

The existing directory implements the same media roster admission as the
old media_peers method. Each consumer receives a clone of cluster.directory(),
never a new per-consumer directory. Keep None/local-only construction paths.

Subtitle hydration retains the existing source attestation and holder checks.
Subtitle range prefetch retains its two-second roster timeout, first-two-peer
selection and roster order. Copy index v3 keeps Unavailable distinct from
NoHolder: an unreachable or failing holder is not proof of absence. The
transport still needs MembershipManager until the later peer-call slice;
retain those handles honestly in the census.

ServingFence::try_restart_admission and ServingAuthority::try_restart_admission
currently differ (expires_at versus token). HTTP must keep the fence method's
semantics through an explicit facade delegate. Existing scheduled DVR work
keeps its authority admission path. Do not substitute between them.

LiveTvAuthority has its own running_floor and SessionGrace loop. Preserve its
running()/serving() decisions through brief authority loss. Its local fields
happen to be called serving and are counted by the spelling census; do not
rename them just to claim a smaller count. The old slice-2 ceiling is an
estimate, not permission to weaken the census or erase local state.

## Owned implementation scope

| Files | Change |
|---|---|
| crates/plurxd/src/cluster.rs | Explicit fence restart-admission delegate and authority subscription; shared directory remains the sole roster cache. |
| crates/plurxd/src/subtitle_source.rs, subtitle_ranges.rs | Carry the shared directory beside the existing transport dependency and use it for media rosters. |
| crates/plurxd/src/fragment_index_cluster.rs, copy_index_v3.rs | Thread a borrowed shared directory through hydration/resolution, preserving local and failure outcomes. |
| crates/plurxd/src/transcode.rs, transcode/manager/construct.rs | Retain and forward the shared directory to subtitle and VOD consumers, including local test constructors. |
| crates/plurxd/src/vodserve.rs, vod/serve/construct.rs | Carry the directory in Shared and forward it to v3/fragment readers. |
| crates/plurxd/src/state.rs, main.rs | Supply cluster.directory()/authority()/subscriptions at composition roots and background job hydration. |
| crates/plurxd/src/http/live_tv.rs, http/internal_live_tv.rs | Replace raw fence reads with facade answers without changing restart-admission semantics. |
| crates/plurxd/src/cluster/census.rs | Lower only genuinely removed per-file, per-class sites and recompute the exact ceiling. |
| verification/*/mapping.json | Review cited anchors and repin whole-file digests for changed sources; no invented semantic acceptance. |
| validation/points.toml | Map the new execution plan to the existing cluster.membership functionality point. |
| docs/cluster/CLUSTER-FACADE-BUILD.md, docs/cluster/README.md, docs/features/PROJECT-BACKLOG.md | Record slice ownership, source delivery, qualification and remaining slices without claiming deployment. |

No raw serving access exists in http/dvr.rs on the inspected main. Inspect it
again at implementation time, but do not manufacture edits to match a stale
file list. Local live_tv/** wrappers stay intact unless a concrete raw fence
access needs routing through the facade.

## Build and qualification

1. Use current main `7829748b9` as the `effort/cluster-facade` base. Slice 2
   depends on the already-landed #1629, not C7. Exclude #1646's pending
   commits from this effort; its train remains separate. Keep one owned
   external clone and requalify the exact task tree after transplanting.
2. Commit the in-repository plan before Rust edits, with its folder index and
   backlog row. The remaining facade slices share cluster/state/census files. Target this
   task at effort/cluster-facade, dispatch the Effort development gate, and
   merge only after it passes. Later slices remain unclaimed. Main promotion
   requires the completed effort and its current-tree qualification receipt.
3. Implement the complete slice, preserving failure results and local-only
   constructor paths. Run fmt and affected all-target compilation locally
   with pinned Rust 1.97.1 and the existing external Cargo cache.
4. Perform one independent adversarial review of the complete diff, touched
   outlines and this contract. Fix findings, then run the smallest relevant
   existing regressions plus any necessary behavioral regression.
5. Run facade census/ratchet, the focused artifact/subtitle/Live TV tests,
   affected formal mapping checks (source_drift empty), and paperwork with
   the concrete PR body. Requalify the exact intended base if it moves.
6. Run the required lane once after local checks; land only through the
   applicable effort gate or main merge train. Record actual commits and
   commands. Paul owns deployment and fleet acceptance.

## Evidence already available

Current main 0680483c2 passed local cargo check --locked -p plurxd --all-targets
in 136 seconds using Rust 1.97.1. This is compiler evidence for main only,
not a slice-2 test result. No slice-2 source has been edited or deployed.

## Baseline issue to preserve

Current main already fails the facade census: http/system.rs has five hiqlite
paths but is pinned at three (total 767, ceiling 765). The two newer writer
abandonment metric references landed after the facade. The same failure occurs
on the #1646 candidate and on main source with the unchanged census test. Do
not raise the pin or claim this is caused by slice 2. Track the root facade
migration separately from the media/Live TV scope.

## Implementation ledger

2026-11-16, Codex: the current effort candidate compiles with pinned Rust 1.97.1
(`cargo check --locked -p plurxd --all-targets`, 130 seconds). It routes the
four media rosters through clones of the existing shared directory, routes
14 HTTP serving accesses through the facade, and moves three authority
subscriptions at daemon composition. Production builders carry the directory
beside the membership handle still required by peer transport. Local-only
constructors continue to pass no cluster handles.

The census pins fall by 21, from 765 to 744. The 28 local LiveTvAuthority
field accesses remain counted; their grace/running-floor policy and field
names are unchanged. The approximate 480 ceiling in the original plan is
superseded for this bounded slice by this exact reduction; remaining
membership, transport, role and local-wrapper sites are not hidden.

Ten formal source rows retain unique anchors and unchanged statements:
five durable-job state citations and its startup dependency; operation
progress's fragment admission, two daemon loops and worker spawn. Only their
whole-file digests change for constructor plumbing and subscriptions. This
repinning is source correspondence maintenance, not new model qualification.
The one independent whole-diff review by Codex subagent
`/root/slice2_review` found no actionable issues. The post-review focused
batch on the independent effort base ran 98 tests: 97 passed; only the confirmed pre-existing system-metric
census mismatch failed (actual 746, pinned 744). This includes media roster
policy, copy-index holderless/unavailable outcomes, subtitle range failure
and cancellation, Live TV HTTP and grace behavior, restart-drain behavior,
local fragment hydration. C7 is excluded from this candidate. No failing behavioral
test was retried or hidden.

`cargo fmt --all --check`, catalog lint, 104 served-script syntax checks and
paperwork (16/16) pass. The three formal mappings have no source drift. Local
Clippy with `-D warnings` remains red on the existing baseline warnings:
`claim_skipped`, trial-admission fields/methods and the unused playback
qualification subscription. The same warnings were present in the baseline
compiler check; this slice does not silence them. No green Clippy result or
fleet acceptance is claimed. No deployment was performed.

## Integration base

Slice 2 is isolated onto main `7829748b9`, without C7's pending commits.
The reviewed media/authority changes are unchanged. The mechanical
transplant removes the unrelated `core_failure` method from the facade base
and refreshes source hashes against the pre-C7 main; unique formal anchors
remain outside the changed hunks. The original contract listed only #1629
as slice 2's prerequisite. Independent effort qualification therefore does
not wait for an unrelated hotfix train or indirectly land #1646.

Exact-base results at source candidate `7c8debc61`: all-target compilation
passed in 130 seconds; the focused batch passed 97 tests with only the
inherited census failure (actual 746 versus pinned 744); Clippy reports only
the same inherited unused items. All three formal mappings report no source
drift. Documentation-only result updates preserve the reviewed Rust patch.
The earlier C7 candidate's separate startup result is not qualification for
this independent effort tree.

Focused current-base command (one batch after the base transplant):

```sh
cargo test --locked -p plurxd --bin plurxd -- \
  cluster::tests:: cluster::census:: copy_index_v3::tests:: \
  subtitle_ranges::tests:: http::live_tv::tests:: \
  live_tv::tests::live_tv_ serving_fence::tests::restart_drain_ \
  requeue_through_the_no_holder_arm_after_converting_artifact_hydrates \
  --test-threads=4
```
