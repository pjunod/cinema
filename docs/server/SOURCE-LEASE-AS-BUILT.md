# Source leases and root capacity — as built

**Status:** built; root-capacity continuation WIP in [PR #1728](http://forge.lan:3000/noirr/plurx/pulls/1728); review and focused qualification pending · **Plan:** [SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md](SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md) · **Updated:** 2026-11-17


Built on `claude/source-lease-steps-2-4` (stacked on PR #1597) by Claude Opus
5.5, session `session_01BCWL9zn5U4msvhC85BdESw`. The plan is the design as written;
Continued by GPT-6 Codex, session `01a1e5c0-d4b1-7740-93b4-a0e0123a79cf`.
Where the build had to differ, this list is authoritative and gives the
reason. Section numbers (§) are the plan's.

> **Qualification boundary.** Earlier `9a…` qualification remains evidence for
> that prior snapshot only. The preserved external `da3…` merge is historical
> source history, not qualification of this root-capacity delta. PR #1728 is
> WIP; review and focused checks against the current tree remain. No deployment.

The expanded catalog is forward-only. The origin/main binary at 32fa
rejects its rewritten pause/resume SQL at the exact source_arbitration_shape
check; its managed-v6 and dispatch validators also reject the new resource
views. Existing v1 writers remain bridged while already running, but restart
or rejoin after expand requires the new binary. Keep quorum during the binary
roll and do not downgrade the expanded catalog. M1's note that ordinary
startup does not reinstall the old claim trigger does not establish that the
old executable can restart against this catalog.

Current resource lookups share source-job classification and the ordinary
`source_lease_owner_scopes` metadata view. Each point lookup binds its owner
key before expanding the union of resource keys; file-backed owners expand
one file scope, and library-wide jobs expand their configured roots. Orphan
fallback checks metadata presence without expanding the same root projection
again. The focused plan regression requires primary-key owner searches on the
store's bundled SQLite; system-SQLite diagnostic plans are supporting evidence,
not a qualification claim for a different engine.

The bundled-engine regression exposed two remaining barriers: correlating
the DISTINCT file-root fallback, and then correlating the compound file-resource
view from job, analysis and lease lookups. The shared file-resource view now
selects one file before aggregating its matching longest-root resource keys.
Only configuration roots and domain mappings expand inside that aggregate.
No matching item/root yields the generic fallback; a missing file yields no
row.

Historical fixture rewind verifies all current lease-owned objects and
overlaid resource definitions before replacing them, so a rewind cannot erase
hostile schema drift and then accept the rewritten catalogue.

Historical replicated fixture rewinds also remove the exact, empty v127
coordination adjunct before altering old analysis tables. Its published witness,
singleton marker and accepted-data counts are checked before destructive DDL;
child tables are removed first in the same fixture transaction. The complete
source-tail removal drops its restored storage-holder view before removing the
pause table that view depends on.

Replicated historical-upgrade retries exposed a second bootstrap ownership
fault: replaying the older retention base schema replaced the valid custody
retention triggers before their strict validator ran. Bootstrap now validates
the complete retention shape and marker before base DDL, preserves an installed
owner, and installs both predecessor trigger definitions atomically only while
no retention extension owns them. A concurrent winner is accepted only after
the same strict shape and marker readback. The extension validator is unchanged;
an owned partial catalogue is refused before bootstrap can recreate its objects.

Replicated qualification also exposed a stale bootstrap installer ceiling:
the canonical managed-layout validator accepted v6, while two installer paths
recognized only v1–v5. A repeated bootstrap then entered the legacy-only guard
and failed with SQLite's intentional malformed-JSON refusal. The installer
now trusts the strict validator's successful result for both initial read and
lost-acknowledgement readback. Unknown shapes and incorrect markers still
refuse; the legacy bootstrap guard is unchanged. Historical tests distinguish
published managed DDL from the current root overlay, and both backend rewind
helpers validate that overlay before any replacement.

The unused intermediate file-root view is removed. Every captured owner
plan remains subject to the same no-catalogue-scan assertion.

The Python dispatch fixture consumes the current contender view generated
from the same Rust claim macros. `scripts/source-dispatch-fixture` emits the
standalone Rust exporter; compiling it in the normal bounded compiler loop
and running it refreshes `crates/plurx-core/tests/fixtures/source-lease-dispatch-current.sql`.
A Rust drift assertion compares that fixture with the actual expand text.
The fixture does not retain the pre-lease contender policy.

A v1 foreground row can survive file deletion. Where its physical file scope
cannot be resolved, carry and old-writer bridge fallbacks use the existing
generic `source_io` scope. It protects roots added later through the same
lease expiry and grace; it neither pins metadata nor adds a recovery timer.
An already-known active rooted snapshot remains preserved.

Receipt scope is captured only when its receipt/lease pair is absent. A grant
replay or refresh therefore preserves the first physical-source provenance
instead of attempting a duplicate insert under the outer grant UPSERT's
conflict policy. Job, offline and companion replay regressions cover this
boundary; their current check results live on the pull request.

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
4. **Root settings and keys.** `jobs.source_readers_per_root` defaults to
   2; `jobs.root_source_readers.<root_id>` overrides one root with a value
   from 1 to 16. A `root_id` is `<library_id>/<UPPERCASE_HEX_UTF8_ROOT_PATH>`.
   The API fields are `source_readers_per_root` and
   `source_readers_root_overrides` (`{root_id: n|null}`). Empty or null means
   inherit because Store has no setting-delete operation. Values outside
   1–16 are absent, not clamped to the floor. `jobs.domain_source_readers.<domain>`
   is an optional aggregate limit; absent means unlimited.
5. **Canonical root admission.** The key is
   `source_io:root/<library_id>/<UPPERCASE_HEX_UTF8_ROOT_PATH>`. Root paths
   trim trailing `/` except `/`. A file uses its longest configured root that
   matches at a component boundary. Each root allows two maintenance readers
   by default and one additional live reader, cluster-wide; roots are
   independent and do not receive a node-wide cap by default. A mapped domain
   adds no cap unless its explicit setting is present. Lowering a root, default
   or domain limit never interrupts a holder or rejects the save; new
   admissions wait.
   Unrooted legacy `source_io` holders retain a bounded node-local fallback
   using the root-reader default. Historical holder keys are charged
   conservatively until their leases retire.
6. **Self-exclusion where a claim is re-asked, and for a job's own older
   fence.** The analysis admission re-check leaves its own lease out; title
   preemption leaves out the job it would preempt. `CLAIM_SQL`'s holder and
   maintenance counts leave out leases of the claimant job itself (`held.owner_kind='job'
   AND held.work_id=<job>`, Paul's review decision 2): after a daemon restart
   on a full root a viewer's own index job no longer waits up to 90 s on its
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
11. **Activity.** Activity exposes per-root occupancy and caps alongside
    the leases that consume them. The holders list still shows every lease,
    including one past its grace (`counts_against: none`), so the "not
    collected" banner can still show a stopped sweep. Node occupancy is
    informational; it is not the rooted admission budget.
12. **Metrics.** `plurx_source_leases{kind}` has no `node` label: every Store
    gauge on `/metrics` is the scraped node's. The scalar capacity metric is
    `plurx_source_readers_per_root_default`; root occupancy and caps are
    available through Activity.
13. **`analysis_source_capacity_clause(now, node)`** gained the node; the
    placeholder census grammar accepts the second placeholder.
14. **Dispatch turns share the claim admission groups.** The dispatch view
    is generated from the same three claim macros for foreground windows,
    total source holders and maintenance readers. It therefore uses the same
    canonical root and optional explicit-domain limits as claim admission.
    The generated-state property comparison remains a pre-deployment
    follow-up for full behavioral equivalence; the earlier hand-copied
    predicate has been removed. D3's expected 7/8/7 accounting is unchanged;
    the policy adds no RPC or replicated write.
15. **Tests pin the root policy.** Rooted tests use
    `jobs.source_readers_per_root` or
    `jobs.root_source_readers.<root_id>`; unrooted compatibility tests use the
    generic `source_io` fallback. The live slot remains separate from the
    maintenance limit.
16. **Replicated fixtures.** The replicated contract fixtures rewind a
    current (contracted) store's lease tail to the published v1 source
    owners and the v1 claim effects (`replicated_source_lease_rewind`, the
    plan shared with SQLite's `source_lease_rewind_plan`) in the same
    transaction as their other drops.
    `replicated_v1_source_store_expands_carries_holders_and_contracts_when_every_node_is_ready`
    is the upgrade test: v1 store with a live job reader → expand carries it
    → restart waits for node-b's capability → contract → restart is a no-op →
    three `source_io` readers on three nodes. This fixture applies the v1 catalog using the current binary; it does not
    start an old executable on the expanded catalog or qualify a downgrade.
    The fresh-vs-chain schema comparison runs the chain's contract first. Main integration includes
    A1 (#1625): claim, local verdict and admissible candidate listing all
    share the lease admission groups. The provider-slot refusal belongs to
    the holder group too; a full provider pair is refused before proposing.
    A1's direct-SQL fixtures now write leases and preserve their original
    two-and-one limits with an explicit reader setting.


## Review disposition — remaining nits and accepted limits

The initial steps 2–4 review is the 2026-11-16 review at `33c267310`.
M1/M2 and M4, S1–S7, and decision 2 were addressed on that snapshot; M3 was
completed with the mapping repair. The root-capacity continuation is WIP:
rooted admission, optional domain aggregation and the resume rule below still
need final review and focused qualification against the current tree.

| Review item | Disposition and reason |
|---|---|
| N1: stale proof comments and deleted journal link | Updated with the lease model and mapping cleanup. |
| N2: vestigial recovery names | Renamed in `c65deb839` to the remaining lease actions. Historical migration identifiers remain stable. |
| N3: display parser differs from claim CAST | Fixed in `493c1711b`; the display follows SQLite CAST including malformed stored values. |
| N4: empty overrides and separate settings writes | Empty means inherit because Store has no settings-delete operation. Reader limits are saved together in one `put_settings`; the existing endpoint saves other setting groups separately. Cross-group atomic saves need an endpoint-wide change, deferred beyond this lease migration. |
| N5: paused resume used only the total holder cap | Root-capacity continuation changes resume to recompute canonical resource keys and enforce the maintenance cap. The extra total-holder slot applies only to live demand; a paused maintenance reader cannot consume it. Focused qualification remains pending. |
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

1. **Independent registrations check rooted admission (F1).** Offline
   readers, read attestations and older writers at the shared SQL boundary
   apply the root maintenance and total-holder terms, plus a domain term only
   when explicitly configured. A full root must refuse another maintenance
   reader even when its maintenance count alone is zero.
2. **Offline service re-admits after expiry (F2).** Metadata alone cannot
   readmit an offline reader. A currently counted lease
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
   re-admission, not copied into a separate policy. It refreshes root and
   optional domain keys from current library paths. During expand, a
   compatibility admission trigger generated from the claim macros applies
   these checks to old writers too; contract removes that temporary trigger.
3. **Paused resume uses current rooted admission.** Resume re-derives keys from
   the current file and root catalogue, then applies the maintenance limit.
   The extra total-holder slot is available only when the paused job has live
   demand.
4. **The roll projects independent owners both ways (F3).** A v1 coordinated
   phase keeps its exact custody UUID as its projected lease identity; ordinary
   analysis claims remain represented once. V1 attestation/offline readers
   use their canonical owner key. Temporary insert/update/delete bridges make
   new v1 holders visible to lease readers and make independently registered
   new-node leases visible to v1 occupancy. Reverse upserts preserve a v1
   UUID, and lease collection never deletes that old-origin row: old writers
   still need it to ask for service. That preserved row cannot keep a phase
   role pinned after its projected lease expires. The contract removes these
   bridges.

Prior-snapshot focused regressions covered live-only full node/domain
capacity, offline re-admission with and without collection, flattened v1
grant proposals, and owners created after expand. Their mutation receipts and the final local
checks are recorded with the PR; source inspection alone is not execution
qualification. No deployment is part of this change.


## Prior snapshot qualification — retained historical evidence

Owner: GPT-6 Codex, session `01a1e5c0-d4b1-7740-93b4-a0e0123a79cf`.
The qualification results below belong to the prior `9a…` snapshot only.
They do not qualify the root-capacity continuation. The external `da3…` merge
is preserved as source history, not as current qualification. PR #1728 is
restored to WIP; root-delta review and focused checks against its exact tree
remain. No deployment.

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
successful exact failed-test retry. Production queue policy is unchanged.

The historical candidate integrated `main` through `32fa105ee`. Its merge
train and local receipts remain part of that snapshot's record. The current
root-capacity delta still needs review and focused qualification against its
exact tree; no deployment is authorized by the earlier results.


Root-policy review fixes (R1-R3): active lease ownership stays unchanged while
its effective resource projection resolves the owner's current file/root scope.
A library-root edit therefore cannot hide an existing reader or playback window.
Unresolvable retired owners conservatively charge overlapping snapshot scopes
during their bounded grace; explicit physical-domain associations remain charged
until the phase closes. The shared writer rechecks old-binary resume proposals
against current roots and maintenance/live capacity, while exact already-active
resume acknowledgements remain idempotent. Resource CTEs are explicitly not
materialized so identity-filtered lookups retain queue-key pushdown. Regression
and mutation results are recorded separately after review; this paragraph is not
a pass claim or a new model/liveness proof.


The receipt-to-source relation also snapshots the immutable library/path scope.
It survives physical lease retirement for the rest of the receipt epoch, so a
root edit cannot erase bytes already allocated before an owner finishes.
Receipt deletion cascades this metadata; it owns no lease, file or library and
adds no recovery wait, pin, timer or RPC. Import inventory is 109 tables.
