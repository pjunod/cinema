# Source leases and node capacity — as built

**Status:** built; review fixes committed, [PR #1728](http://forge.lan:3000/noirr/plurx/pulls/1728) in final qualification · **Plan:** [SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md](SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md) · **Written:** 2026-11-15


Built on `claude/source-lease-steps-2-4` (stacked on PR #1597) by Claude Opus
5.5, session `session_01BCWL9zn5U4msvhC85BdESw`. The plan is the design as written;
Continued by GPT-6 Codex, session `01a1e5c0-d4b1-7740-93b4-a0e0123a79cf`.
Where the build had to differ, this list is authoritative and gives the
reason. Section numbers (§) are the plan's.

1. **The expand reinstalls the claim effects, on both backends.** The
   trigger moved to `background_job_claim_effects.sql`, which the base job
   schema includes and the expand (`source_leases::EXPAND`, one text for
   both backends) appends. The replicated store installs its base job schema
   only when a cluster is created (`bootstrap_with_clock`); a voter start is
   `open_or_migrate` → `migrate_schema`, which never re-runs it. An upgraded
   cluster therefore keeps the v1 two-slot trigger unless the expand replaces
   it (review M1: the third `source_io` reader aborted the claim). The lease
   shape check (`source_leases::version`) includes
   `background_job_claim_effects`, so a lease tail beside the v1 trigger is
   refused as partial. Source reservations take `COALESCE(MAX(slot),-1)+1`:
   they are the bundle record, not a cap. No binary re-installs the trigger
   on an ordinary start, so a v1 node restarting mid-roll leaves it as the
   expand wrote it.
2. **Expand drops are `IF EXISTS`**, so hand-built test catalogues can run the
   expand; production runs it after every other extension.
3. **Lease ids.** `background_owner_key` *is* the lease id: `job:<id>:<fence>`,
   `analysis:<hex request>:<fence>` (request ids are opaque text; the trigger
   uses `hex()`), `lease:<resource>:<fence>`, `offline:<package>:<generation>`.
   A coordinated analysis phase has its own lease
   `analysis:<hex request>:<fence>:<phase>` (written by `begin_phase`, which no
   longer takes a caller UUID), so a delayed release of an earlier phase can
   never delete a later phase's lease; the analysis renew and success triggers
   therefore match `(owner_kind, work_id, fence)`. A companion keeps a
   `companion:<fingerprint>` key for the byte-service ledger only.
4. **Settings keys.** `jobs.source_readers_per_node` (default),
   `jobs.source_readers_per_node.<node>` (override) and
   `jobs.domain_source_readers.<domain>` (per-domain `source_readers`). The
   store has no setting delete, so "use default" saves an empty value; the
   claim reads any value outside 1–16 as absent (default), not as the floor.
   API fields: `source_readers_per_node`, `source_readers_node_overrides`
   (`{node: n|null}`), `storage_domain_source_readers` (`{domain: n|null}`).
5. **Domain cap.** A mapped key `K` refuses at `source_readers(K)+1` counted
   leases (holders) and `source_readers(K)` non-live leases (maintenance),
   cluster-wide; default 1 keeps today's two-and-one numbers.
6. **Self-exclusion where a claim is re-asked, and for a job's own older
   fence.** The analysis admission re-check leaves its own lease out; title
   preemption leaves out the job it would preempt. `CLAIM_SQL`'s holder and
   maintenance counts leave out leases of the claimant job itself (`held.owner_kind='job'
   AND held.work_id=<job>`, Paul's review decision 2): after a daemon restart
   on a full node a viewer's own index job no longer waits up to 90 s on its
   own ghost. The old row is not deleted and keeps charging every other claim
   until its grace, so the orphan-reader protection is unchanged. No trigger,
   no migration, and no "same node, different boot" shortcut.
7. **Retention.** Besides the two triggers, the Rust `MAINTENANCE_SQL` and the
   `ENQUEUE_SQL` queue-full check also lose the custody clause (v85/v86 text).
   `retention_keeps_a_job_that_still_owns_charged_source_custody` is deleted
   with the rule it pinned.
8. **Storage-domain holders** keep `holder_kind='foreground'` for playback (the
   panel's vocabulary) and report `expires_at_ms` including the grace, so the
   panel's "live" test matches the claim's.
9. **The local settlement journal and the activation step are gone**
   (`source_custody.rs` journal, `activate_*_with_journal`, the four
   activate/abort/mark/settled store methods and their callers). A reader's
   release deletes its lease once (three attempts on error); that is only
   used where the owner's own state also ends the lease (a job's settle
   trigger, an analysis request's success trigger or renewal stopping, a
   read attestation's lowered expiry). A coordinated analysis phase has no
   such owner trigger and its lease is renewed by the request heartbeat, so
   `finish_phase` deletes the phase's lease in the same transaction that
   closes the phase (review M2); no separate release precedes it.
9a. **Offline leases** are job-length (30 s from registration) and renewed
   by their reader's own service grants (`source_lease_offline_renew`, one
   grant per 5 s epoch while reading), not set to the package's 7-day
   expiry: a dead node's offline reader frees like any other (review S4).
   Carried v1 offline holders get `MIN(authority, updated_at+30 s)`; a reader
   still taking service renews it.
9b. **The roll bridge runs both ways.** Besides mirroring a v1 node's
   playback into `source_leases`, an upgraded node's playback lease is
   mirrored into `foreground_source_owners` (`source_lease_bridge_out_*`), so
   a v1 node's claims see it (review S3). Each side writes only a row that
   differs (upserts with an `IS NOT` guard), so a write ends at the other
   side with `recursive_triggers` on or off. The contract drops all six.
9c. **Roll-time coordinated phases.** Between expand and contract a v1 node
   records its coordinated phase in `background_source_custody`; the bridge
   projects it as a bounded lease under that exact UUID (review S2/F3).
   The orphaned-role sweep therefore uses only leases. A preserved v1 row
   cannot pin the role after its authority and grace expire.
10. **Contract on the replicated store.** A refused or failed contract leaves
    the complete expanded store and returns `Ok`, logged once at info; it
    retries on the next start.
11. **Activity.** The holders list is in the analysis summary card
    (`analysis.js`, rendered by Activity and the Analysis page), grouped by
    node. It now lists every lease, including one past its grace (not
    counted, `counts_against: none`), so the "not collected" banner can still
    show a stopped sweep.
12. **Metrics.** `plurx_source_leases{kind}` has no `node` label: every Store
    gauge on `/metrics` is the scraped node's.
13. **`analysis_source_capacity_clause(now, node)`** gained the node; the
    placeholder census grammar accepts the second placeholder.
14. **Dispatch turns over-yield across nodes (decided: no change now).**
    `dispatch_turn_attempt` has no node column, so a maintenance claim on
    node B yields while a ready scan elsewhere is admissible on its own node,
    when the two share only unmapped storage. It is bounded (such a scan
    claims within its ready window), costs throughput during scan bursts and
    cannot over-admit. Follow-up: add the node to `dispatch_turn_attempt`.
    Follow-up before the expand is deployed: `dispatch_turn_agrees_with_the_claim`,
    a property test that the dispatch view's hand copy of
    `claim_foreground_window!`/`claim_source_holders!`/`claim_maintenance_reader!`
    agrees with the claim on generated lease states (review S8). The copy
    uses `dispatch_turn_ordinary_occupancy`/`dispatch_turn_protected_jobs`
    where the claim uses `lease_live!` with the `live_range` context. The
    expand is shared static migration text on both backends and its object
    text is part of the schema identity. Generating it from Rust claim macros
    requires moving that migration boundary and replacing the view-specific
    live-demand context; a textual substitution would not establish semantic
    equality. Keep the static migration here and compare generated stores in
    the follow-up before deployment (review S8), rather than changing the
    migration representation as part of this review fix.
    The reviewer compared the texts term by term and found them equal; the
    Python cases stand in until then. The copy does not have the claimant
    self-exclusion of item 6 (it errs toward not yielding).
15. **Tests pinned to the old numbers** set `jobs.source_readers_per_node=1`
    (one background reader plus the live slot) rather than being rewritten.
16. **Replicated fixtures.** The replicated contract fixtures rewind a
    current (contracted) store's lease tail to the published v1 source
    owners and the v1 claim effects (`replicated_source_lease_rewind`, the
    plan shared with SQLite's `source_lease_rewind_plan`) in the same
    transaction as their other drops.
    `replicated_v1_source_store_expands_carries_holders_and_contracts_when_every_node_is_ready`
    is the upgrade test: v1 store with a live job reader → expand carries it
    → restart waits for node-b's capability → contract → restart is a no-op →
    three `source_io` readers on three nodes. The fresh-vs-chain schema
    comparison runs the chain's contract first. Main integration includes
    A1 (#1625): claim, local verdict and admissible candidate listing all
    share the lease admission groups. The provider-slot refusal belongs to
    the holder group too; a full provider pair is refused before proposing.
    A1's direct-SQL fixtures now write leases and preserve their original
    two-and-one limits with an explicit reader setting.


## Review disposition — remaining nits and accepted limits

The review is the 2026-11-16 steps 2–4 review at `33c267310`. M1/M2 and
M4, S1–S7, and decision 2 are implemented by the review-fix commits; M3 is
completed with the mapping repair. Item 14 records S8 and decision 1.

| Review item | Disposition and reason |
|---|---|
| N1: stale proof comments and deleted journal link | Updated with the lease model and mapping cleanup. |
| N2: vestigial recovery names | Renamed in `c65deb839` to the remaining lease actions. Historical migration identifiers remain stable. |
| N3: display parser differs from claim CAST | Fixed in `493c1711b`; the display follows SQLite CAST including malformed stored values. |
| N4: empty overrides and separate settings writes | Empty means inherit because Store has no settings-delete operation. Reader limits are saved together in one `put_settings`; the existing endpoint saves other setting groups separately. Cross-group atomic saves need an endpoint-wide change, deferred beyond this lease migration. |
| N5: resumed maintenance uses the holder cap | Existing behavior retained: resume restores a paused reader's reservation under the total holder cap. Changing resume scheduling is a separate admission-policy change, not required for expiry release. |
| N6: restart leaves charged leases through grace | Intended lease safety margin: a 30 s owner lease plus the 60 s grace. A job excludes only its own prior fence from its claim; every other claimant still counts it. No boot identity shortcut or recovery watcher. |

The playback write delta is recorded with M4 in
[PLAYBACK-START-D3-RECORD.md](../streaming/PLAYBACK-START-D3-RECORD.md):
the three source-reading create shapes lose one replicated write each.
Host-identity binds remain until step 2 is on every node for rollback; their
removal is a later operator change. This PR does not deploy anything.


## Main integration — preserve published history and shared admission

Both dependency PRs (#1597 and #1625) landed before final qualification.
The branch integrates `main` at `0680483c2`. Main already published SQLite
v133 for delegation, so the lease expand and contract follow at v134/v135;
v133 is preserved unchanged. Historical fixtures rewind leases before the
delegation tail and validate that neither rewrite discards owned data.
The import inventory combines delegation's three durable tables with the
lease replacement's net removal of one table (two owners become one lease table).

The claim's lease terms remain the same macros used by source registration,
analysis and resume. Claim, replica verdict and candidate listing wrap those
terms in the same three admission groups; no local decision grants authority
or replaces the replicated claim. Provider reservations keep their two-slot
limit. Only the claimant job is excluded from its own lease holder counts.


## Final integration review — capacity and rolling-owner fixes

The final independent source review of `60df4bcf0` found three remaining
admission/lifecycle gaps. The fixes stay in the replicated Store statements
and migration, without an additional grant RPC or runtime recovery loop.

1. **Independent registrations check both caps (F1).** Offline readers and
   read attestations apply the shared total-holder term as well as the
   maintenance term. A node/domain full of live readers must refuse another
   maintenance reader even when its maintenance count is zero.
2. **Offline service re-admits after expiry (F2).** A currently counted lease
   renews with its service grant. Once expiry plus grace elapsed, the same
   statement reconstructs resources from the exact preparing package and
   checks foreground, total-holder and maintenance admission before renewing
   or recreating the row. Swept and unswept rows therefore behave alike. The
   grant retains its existing receipt/spend identity. Legacy activation or
   cleanup metadata never extends its expiry, nor recreates a collected row;
   only an admitted grant may renew offline reading. If domain mappings
   changed while a v1 reader was uncounted, the writer refreshes its stored
   keys and refuses that stale proposal. Its existing next request then
   recomputes permission, byte service and admission from current keys. The
   canonical file-resource query is shared with current registration and
   re-admission, not copied into a separate policy. During expand, a
   compatibility admission trigger generated from the claim macros applies
   these checks to old writers too; contract removes that temporary trigger.
3. **The roll projects independent owners both ways (F3).** A v1 coordinated
   phase keeps its exact custody UUID as its projected lease identity; ordinary
   analysis claims remain represented once. V1 attestation/offline readers
   use their canonical owner key. Temporary insert/update/delete bridges make
   new v1 holders visible to lease readers and make independently registered
   new-node leases visible to v1 occupancy. Reverse upserts preserve a v1
   UUID, and lease collection never deletes that old-origin row: old writers
   still need it to ask for service. That preserved row cannot keep a phase
   role pinned after its projected lease expires. The contract removes these
   bridges.

Focused regressions cover live-only full node/domain capacity, offline
re-admission with and without collection, flattened v1 grant proposals,
and owners created after expand. Their mutation receipts and the final local
checks are recorded with the PR; source inspection alone is not execution
qualification. No deployment is part of this change.


## Local qualification and landing record

Owner: GPT-6 Codex, session `01a1e5c0-d4b1-7740-93b4-a0e0123a79cf`.
[PR #1728](http://forge.lan:3000/noirr/plurx/pulls/1728) carries current CI
and merge-train state; no deployment is part of this change.

The store module run passed 767 cases; its four fixture failures passed
failed-only retries. SQLite contracts passed 349/349. Claim-verdict and property
checks, cluster activity (10/10), touched web tests, dispatch contracts,
formatting, and both required Clippy checks passed on their recorded candidates.
After the final compatibility changes, core Clippy, 50 affected lease/census
tests, and both real replicated upgrade/bootstrap checks passed again. New
regressions have pre-fix or mutation failure evidence. All three formal mapping
checks pass.

The replicated selection passed 63/91. The same 28 names fail on the recorded
main baseline. Historical migration cases first encounter an earlier exact-tail
fixture mismatch there; a fixture-adjusted representative v6 probe reaches the
same downstream missing managed-schema marker. This is explicitly limited
baseline evidence, not a claim that all 28 failure messages are identical.

The daemon selection passed 881 cases with eight ignored and two failures.
Gap-fill reproduces main's `copy_sync_unverified` refusal. The viewer-ceiling
fixture depended on a process-global engine snapshot: its fake queued digest
could be classified stranded during a parallel run. It now fills the ceiling
with running background work, independent of that snapshot; the PR records its
exact failed-test retry. Production queue policy is unchanged.

Current main was integrated through `32fa105ee`. The enabled merge train
qualifies the actual candidate and owns landing. Local results above do not
substitute for its promotion verdict.
