# Source leases and node capacity — as built

**Status:** built on `claude/source-lease-steps-2-4` · **Plan:** [SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md](SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md) · **Written:** 2026-11-15


Built on `claude/source-lease-steps-2-4` (stacked on PR #1597) by Claude Opus
5.5, session `session_01BCWL9zn5U4msvhC85BdESw`. The plan is the design as written;
where the build had to differ, this list is authoritative and gives the
reason. Section numbers (§) are the plan's.

1. **Claim effects live in the base job schema.** The replicated store
   re-runs `background_jobs_resources.sql` (DROP + CREATE of
   `background_job_claim_effects`) on every start, so rewriting that trigger
   inside the expand would be reverted at the next restart. The trigger moved
   to `background_job_claim_effects.sql`, included in the base schema; SQLite
   (which applies the base schema once) reinstalls it in v133
   (`source_leases::SQLITE_EXPAND`). Source reservations take
   `COALESCE(MAX(slot),-1)+1`: they are the bundle record, not a cap. An old
   binary that restarts mid-roll reinstalls the two-slot text; that only
   limits source claims to two reservations per key until a new binary
   restarts (§12.1).
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
6. **Self-exclusion only where a claim is re-asked.** The analysis admission
   re-check leaves its own lease out; title preemption leaves out the job it
   would preempt. Claims themselves exclude nothing (as §4.1).
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
   release deletes its lease once (three attempts on error); expiry frees it
   otherwise.
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
14. **Dispatch turns.** `dispatch_turn_attempt` still has no node column, so
    a maintenance claim can yield to a ready scan on another node that shares
    only unmapped storage (an over-yield, never an over-admit). Open question
    for Paul.
15. **Tests pinned to the old numbers** set `jobs.source_readers_per_node=1`
    (one background reader plus the live slot) rather than being rewritten.
16. **Not done here:** the replicated contract-test fixtures in
    `tests/store_contract.rs` (`hiqlite-contract-tests` lane) still rewind the
    v1 source tail and need a lease-tail rewind like the SQLite one in
    `tests/support/source_extensions_fixture.rs`; A1's differential test
    (#1625) is not on this base.
