# Cluster transport recovery — live implementation status

**Status:** active · **Effort:** `effort/cluster-transport-recovery` ·
**Started:** 2026-09-05 · **Last updated:** 2026-09-06

Companion to [OPERATIONS.md](OPERATIONS.md) (operator contracts) and
[DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) (branch, gate, and
qualification rules) — this page answers *what is implemented, what has been
proved, and what remains*. Update it in the same commit that changes a
milestone's state. A green helper test is evidence for that helper, not for the
whole recovery contract.

## Outcome — bounded recovery without weakening authority

After transient transport backpressure, every Raft and cluster-API frame must
be written and flushed, and a connection whose writer fails must be torn down
without waiting for its reader. Snapshot retry keeps OpenRaft's typed mismatch,
vote, snapshot-ID, and offset semantics while one absolute attempt budget
bounds chunk transfer and final installation.

The completed work will expose process-local transport progress before the
public HTTP listener opens. It will not authorize reads, replay ambiguous
mutations, weaken readiness, change election timers, or introduce a wire-format
change.

## Delivery — one effort, reviewable milestone PRs

| Milestone | Task branch | PR | State | Blocking evidence |
|---|---|---|---|---|
| M1 · frame completion | `codex/cluster-transport-m1` | [!56](http://192.168.4.7:3000/noirr/plurx/pulls/56) | merged into the effort after three exact-candidate adversarial reviews and the green effort gate | Both raw buffered-write failures are reproduced after transport writability returns; every production writer and terminal path passes its focused regression |
| M2 · connection and snapshot ownership | `codex/cluster-transport-m1` | same merged review | foundations merged; end-to-end acceptance in progress | Cancellation-safe admission, shutdown ordering, node-owned snapshot execution, real-file partial-write ownership, and 100 in-memory WebSocket reader/writer task cycles pass; production Raft install/socket-disconnect coverage is reserved for the M5 harness and is not yet claimed |
| M3 · recovery budgets | `codex/cluster-transport-m3` | [!60](http://192.168.4.7:3000/noirr/plurx/pulls/60) | merged into the effort at `f5c688a9` after three exact-candidate adversarial reviews and the green effort gate | Virtual-time exact bounds · config/env/Compose precedence · pulled-image revision proof |
| M4 · transport status | `codex/cluster-transport-m4` | not opened | every current review finding is fixed; moved-base Rust 1.97.1 fast-lane qualification and three replacement adversarial reviews are pending | Authenticated non-cacheable pre-HTTP status · exact executor-admission identity · stage-accurate deadlines · bounded identities · local-monotonic replay aging |
| M5 · recovery campaign | `codex/cluster-transport-m5-final` | not opened | early review findings are fixed and committed; the branch awaits the final M4 base before its replacement exact-candidate reviews | Actual TLS transport matrix · 20 learner and 20 voter cycles |
| Final promotion | `effort/cluster-transport-recovery` | not opened | not started | Full suite once after all review fixes · current-tree qualification receipt |

## Current evidence — Rust 1.97.1 is the compiler of record

The current M4 integration work is based on final M3 PR tip `2c018809`.
Candidates `7e220e0c` and `d1472a55` passed their pre-review fast lanes but
failed exact-SHA adversarial review; neither candidate will be pushed or opened
as a pull request. Their findings were corrected and committed on a replacement
tree that passed combined focused requalification. Fresh exact review of
`660e0794` then found a failed-refresh DOM freeze, stale cross-observer
completion precedence, stale status wording, an unfenced planned-outage
preflight, an in-flight authentication revocation race, and late-read producer
evidence resurrection. The exact review of `b7914f69` then found that retained
browser evidence still trusted cross-machine wall clocks, a failed manual
refresh left node transport DOM stale, snapshot status initialization could
lose to a ready reader EOF, local revocation generations did not block
cache-only authorization throughout the mutation, peer processes retained
stale recovery proofs, and an aborted planned-outage request could strand its
exact lease. Those corrections were committed after green focused
requalification, but the three exact-candidate reviews of `95247348` rejected
it. They found that queued or first-identity snapshot teardown could disappear,
cancellation could orphan a replicated outage claim or leave its local serving
fence armed, commit-ambiguous credential mutations could release their cache
fence, console and mixed-version writers could bypass peer revocation, and
asynchronous UI updates could replace an operator decision or retain obsolete
sender progress. Those findings were implemented. The replacement passed web,
operations, validation, formatting, diff, and its new exact Rust regressions;
the complete cluster/WAL lane then exposed an obsolete abandoned-snapshot
expectation: the new contract correctly retained a first-identity terminal row.
That regression now proves the active and terminal rows, exact error and retry
count, monotonic attempt identity, and persistence after the active install.
The complete lane passes from the beginning, as do workspace all-target compile,
root denied-warning Clippy, validation, operations, formatting, and diff checks.
All four standalone vendored compile and denied-warning Clippy matrices also
passed. Exact review then rejected committed candidate `cd344bd1`: an aborted
snapshot owner could leave active status, remote wall-clock skew could corrupt
cached aging, long-lived sockets reused an expired admission deadline, an
ambiguous maintenance transaction could land after local unfencing, and an
ambiguous lease acquisition could land after its cleanup. The corrections now
publish abort-terminal status, derive each request's admission deadline at
decode time, use local monotonic cache age, and make planned-outage claims
one-shot through permanent exact-release receipts. Focused requalification and
three fresh exact-SHA reviews remain; the tree will not be pushed or opened
beforehand. A final planned-outage preflight then found that a previous-release
binary could still bypass the receipt through its raw lease statements and that
local admission could reopen when exact cleanup outlived the original lease.
The corrected replicated schema now requires transaction-local intent for every
lease insert, update, and delete, while the node-owned cleanup retains a local
unresolved-release latch through every ambiguous result. Previous-release write,
delayed resurrection, both maintenance/release orders, retry-boundary, and
post-expiry admission regressions pass. The complete focused lane and all static,
compile, formatting, and denied-warning matrices pass on exact candidate
`7981061a`. Exact-head review of the following status checkpoint `7130f456`
found that a direct heartbeat delete conflicted with the new rolling-upgrade
guard, restart cancellation could clear a maintenance-owned local fence, and
credential revocation reused a diagnostics roster that omitted pending-removal
members. The replacement delegates expiry solely to the schema-owned heartbeat
trigger, makes local fences operation-owned, and uses a dedicated exact committed
revocation roster that fails closed on missing members or endpoints. Its complete
focused cluster/WAL lane, 197 operations contracts, web and validation suites,
workspace check and denied-warning Clippy, all four prescribed vendored matrices,
formatting, and diff hygiene pass on Rust 1.97.1. Three fresh reviews rejected
exact candidate `d0daa67f` with eleven planned-outage, credential-mutation,
roster-churn, and clock-aging findings. The first replacement implemented all
eleven, mapped their regressions into the persistent focused lane, and passed
that lane from the beginning plus the complete pre-review matrix. Pre-commit
audit then found that roster stability alone left a final membership TOCTOU and
that direct restart cancellation could leave the exact preparation token
latched. The current replacement adds a replicated exclusion with claim-bound
Store writes, resolves both cancellation tokens, and retains permanent
anti-resurrection receipts only for ambiguous acquisitions. The complete
focused lane, pre-review matrix, formatting, and diff hygiene pass before the
new exact commit.
The local Homebrew default is Rust 1.95.0, so every recorded Rust command uses
`rustup run 1.97.1`; unpinned results do not count.

| Check | Result | Detail |
|---|---|---|
| Vendored Hiqlite compile | pass | SQLite+cache and dashboard+SQLite+cache feature matrices completed on Rust 1.97.1 |
| Buffered-write reproduction | pass · 2 pinned cases | With underlying writability already restored, raw fastwebsockets `write_frame` leaves a 32-byte server response and 3 MiB client request unavailable for 200 ms; explicit flush delivers each on the same socket |
| Production writer regressions | pass · 5 surfaces | Serialized Raft/API requests and responses plus the proxy response use their production writer seams over real TLS; queue-consuming Raft/API client and server writers report injected flush failures to their supervisors |
| Deadline and failure regressions | pass | Flush error, blocked combined write/flush budget, 250 ms Close budget, and scaled server-handshake budget all terminate at their documented boundary |
| Vendored network suite | pass · 62 tests | Prescribed SQLite+cache+auto-heal+macros `network::` library filter on Rust 1.97.1; no failures or ignored tests |
| API client stream suite | pass · 21 tests | Includes cancellation-safe mutation ownership, decoded-response-before-handoff ordering, exact-socket proxy refusal ownership after caller cancellation, cancellation-safe retention of a decoded response blocked behind the bounded reader queue, concurrent-refusal coalescing, proxy/non-proxy EOF-before-drain handling, a dedicated leader-control queue that bypasses an application backlog during writer backpressure, stale/same-target leader handling, production request writer, and malformed-frame reader outcome |
| Snapshot ownership foundations | pass · 6 executor tests + 5 real SQLite state-machine tests | One running/one queued admission, cancellation-safe deadline/shutdown ownership, a controlled real-file partial write with digest verification, and real SQLite pending/current recovery pass. The remaining full Raft/socket-disconnect acceptance is explicitly open for M5. |
| Broad optional-feature probe | invalid baseline lane | `--all-features` enables mutually exclusive `cast_ints` modes and reaches unrelated existing optional-feature compile defects; the repository-prescribed feature matrices remain authoritative |
| Daemon check and denied-warning Clippy | pass | `plurxd --all-targets` completed on Rust 1.97.1 with no warnings |
| Vendored denied-warning Clippy | pass | Prescribed auto-heal+cache+macros+SQLite and cache-only library lanes pass with `-D warnings`; seven pre-existing Rust 1.97.1 lint findings were corrected rather than suppressed |
| Dependency resolution | pass | Standalone vendor lock now matches the daemon transport stack: Tokio 1.53.1 and rustls 0.23.42 |
| Persistent regression map | pass | The transport tests are in `cluster-wal-check`; a command wrapper fails exact filters unless one test executes, the broad snapshot filter requires ten passing tests, and operations contracts preserve both guards |
| Focused cluster/WAL fast lane | pass | `make cluster-wal-check` completed after the M3 review fixes, including both production `full_snapshot` wrappers, final-mismatch reread ownership, simultaneous timer/phase transition, hard-TTL shorter than C, configured receiver admission, and bounded standalone durations; every exact filter executed and loopback tests used the normal unsandboxed allowance |
| Operations contracts | pass · 197 tests | Persistent command/count guards include every new M3 regression, including immutable prebuilt-image identity; the deployment, CI, and UI-baseline contracts pass with the port-reservation fixture using the normal unsandboxed loopback allowance |
| M3 snapshot RPC suite | pass · 37 tests | The standalone `raft_client` suite covers one absolute transfer deadline, a latched final-install deadline, caller cancellation, exact-socket reset ownership, typed mismatch restoration before reread, simultaneous phase/timer readiness, both production snapshot wrappers, and stale-attempt isolation |
| M3 configuration suite | pass · 11 root tests + 1 standalone bound test | Chunk, transfer, and install budgets accept documented bounds, reject invalid ordering and oversized values, preserve defaults for empty environment variables, and cannot reach unchecked `Instant` arithmetic |
| M3 production timing seam | pass | The `hiqlite-store` migration test proves the production startup catch-up deadline is exactly transfer + install + 45 seconds |
| M3 deployment and operations contracts | pass · 197 tests | Compose health timing derives from the three startup phases; preflight validates explicit values, opaque mounts, bounds, chunk ≤ transfer precedence, immutable image identity, and both-service pinning |
| M3 compile and lint | pass | Root all-target check and denied-warning Clippy pass; standalone vendored SQLite, SQLite+cache, and full library matrices pass; root and standalone formatting pass |
| M3 corrective-history audit | pass · local replacement | Explicit current-check mappings cover both runtime corrective commits and the lint-only refactor; six status-only corrections are recorded as non-runtime. The lint refactor now owns a persistent vendored-Hiqlite denied-warning Clippy check in the effort and promotion workflows; static contracts pin its exact recipe, catalog record, and workflow order, and edits to either fail open into the cluster lane. The 1,339-commit history audit, 116 validation tests, 197 operations contracts, and 23-point/29-check catalog pass locally. |
| M3 adversarial review | pass · 3 tracks | After the earlier code review passed, three final operations-focused reviewers independently certified exact PR SHA `2c018809` clean, including immutable pulled-image identity and rollback behavior |
| M4 transport status | pass · 19 exact cases | Attempt-scoped sender state, receiver socket/deadline/byte ownership, terminal outcomes, reconnect counts, live-membership capacity, and bounded retired-peer churn pass on the current branch; combined-tree review remains pending |
| M4 private recovery observation | pass · production seams | The authenticated private route, roster-bound live-membership client, bounded body, and public-port-closed collector seam pass; full real multi-node acceptance remains M5 work |
| M4 daemon and UI contracts | pass · 14 exact daemon cases | Persistent exact tests cover per-peer timeout, explicit peer-bound omission, unavailable and stale cache states, source-timestamp rebasing with bounded clock skew, active cached deadline aging beyond five minutes, fresh cached deadline-to-stalled projection, inactive five-minute expiry, refresh margin, request-path isolation, slow-public/private-success collection, and both envelope- and nested-observer identity rejection; the cluster UI consumes only the sanitized row projection and all focused operations contracts retain the guards |
| M4 concurrency and identity remediation | pass · isolated branch | Pinned check and denied-warning Clippy, 19 status tests, 8 executor tests, all 39 Raft-client tests, the daemon 8-peer bound, membership UI suite, operations mapping, formatting, and diff checks pass; the commit still requires combined-tree qualification and three exact-candidate reviews |
| M4 second-review correction | pass · isolated branch | Rust 1.97.1 passes the 18-case status suite, 9-case executor suite, the production A-running/B-queued/C-cancelled FIFO regression, both active and inactive cache-aging tests, root and prescribed vendored check plus denied-warning Clippy, 48 operations contracts, and the Settings and membership web contracts; combined-tree integration and exact re-review remain required |
| M4 focused cluster/WAL fast lane | pass · combined tree | `make cluster-wal-check CARGO_RAW='rustup run 1.97.1 cargo'` passes after all four remediation commits were combined; every exact filter executed |
| M4 compile and lint | pass · combined tree | Root all-target check and denied-warning `plurxd` Clippy pass; standalone vendored SQLite, SQLite+cache, and default-library denied-warning Clippy matrices pass; root formatting and diff checks pass |
| M4 third-review correction | pass · integrated `0d868c32` | Rust 1.97.1 passes the 19-case status and 10-case executor modules, all 41 Raft-client tests, the three cache-aging regressions, root all-target check, root and vendored denied-warning Clippy, membership web suite, exact operations mapping, formatting, and diff checks; exact replacement review is in progress |
| M4 observation-identity and retention corrections | pass · candidate branch | Public fallback transport requires both a matching status envelope and matching transport observer; valid private evidence remains independent of public-listener identity; the authenticated private route returns `Cache-Control: private, no-store`; exact re-review is pending |
| M4 final review correction | pass · superseded pre-review tree | The combined recovery lane, 28-case daemon module, 23-case transport-status module, 45-case Raft-client module, 197 operations contracts, 23-point validation catalog, membership web suite, all-target compile and denied-warning Clippy, three vendored denied-warning Clippy lanes, formatting, and diff checks pass. Request logging is bounded to operation kind and byte count; natural reconnects advance a physical socket epoch; sender offsets remain monotonic; semantic snapshot identity cannot collide with its bounded display form; fresh snapshot evidence outranks a stale different-snapshot stall; committed membership reads are query-bounded; and aggregate GET/support reads use fresh node-owned caches |
| M4 exact-candidate replacement | fail · `7e220e0c` rejected | The pre-review focused cluster/WAL lane, executor/status/Raft-client modules, operations contracts, validation catalog, membership web suite, compile, denied-warning Clippy, formatting, and diff checks passed. Three adversarial tracks nevertheless found six correctness or coverage gaps: receipt IDs could diverge from real install order; cancelled admission tickets were unbounded; UI selection was order-dependent; receiver completion could hide stronger sender acknowledgement; mutation preflights used stale peer cache data; and the no-I/O test bypassed the production collector. The replacement must fix every item and repeat the complete focused lane and three reviews from zero. |
| M4 fourth-review corrections | fail · `d1472a55` rejected after green pre-review lane | Public attempt ownership follows actual executor worker start; cancelled admission retention is bounded to two waiters and overflow is explicit; UI evidence selection uses a deterministic two-stage total order; read-only GET/support collectors are cache-only while mutation preflights use fresh bounded evidence; and the no-I/O proof executes both production handlers. Exact review still found retained browser evidence that did not age, acknowledged completion losing to predecessor phases, Store-backed authentication before cache-only handlers, an unclustered refresh warning loop, and outer Raft socket cancellation bypassing inbound-status cleanup. |
| M4 fifth-review corrections | fail · `660e0794` rejected by three fresh tracks | Retained browser evidence aged in the model but failed refreshes did not repaint it in the DOM; old cross-observer completion could hide a demonstrably newer same-fingerprint attempt; the status page was stale; restart and maintenance proved safety before claiming their lifecycle fence; an authentication already reading Store could republish revoked admin proof; and a late first status read renewed producer evidence past its original expiry. This exact tree is not a release candidate. |
| M4 sixth-review corrections | fail · `b7914f69` rejected after a green lane | Failed automatic refreshes repaint projected transport state without replacing an open decision dialog; cross-observer completion authority is bounded to the five-second freshness cohort; planned outages claim the replicated lifecycle lease before fresh evidence and release it on rejection; Store-backed authentication publishes only across an unchanged revocation generation; and producer stalls retain their real deadline-derived boundary. Three fresh exact-SHA tracks still found seven UI, snapshot-lifecycle, cluster-revocation, and cancellation findings. |
| M4 seventh-review corrections | fail · `95247348` rejected after green focused requalification | The three exact-SHA tracks found first-select and first-identity snapshot teardown gaps, cancellation windows around both the replicated outage claim and local serving fence, cache authorization gaps on ambiguous writes plus console and mixed-version writers, a manual-refresh/modal race, and stale sender phases outranking durable receiver completion. All findings are assigned to bounded implementation tracks; the corrected tree must repeat focused qualification and three reviews from zero. |
| M4 eighth-review corrections | fail · `cd344bd1` rejected after green qualification | Every finding against `95247348` was implemented and the complete lane plus compile/lint matrices passed, but fresh exact review found five snapshot-status, clock, deadline, and planned-outage ordering defects. This exact tree is not a release candidate. |
| M4 ninth-review corrections | fail · exact head `7130f456` rejected | The transport reviewer was clean, but the other exact-head tracks found a heartbeat/delete-trigger deadlock, a cross-operation local-fence cancellation race, and a pending-removal credential-revocation omission. This exact tree is not a release candidate. |
| M4 tenth-review corrections | fail · `d0daa67f` rejected after green qualification | Exact review found two remote/local-age projection gaps; five planned-outage compatibility, ABA, cancellation, overlap, and reconciliation failures; unbounded detached release retries; two non-atomic credential mutations; and a membership-add revocation window. The replacement is active and must repeat focused qualification plus all three reviews. |
| M4 eleventh-review corrections | pass · superseded before commit | Local monotonic receipt age includes collection dwell; legacy heartbeats reach trusted receipt-owning expiry; local outage fences and cleanup are generation-scoped and serialized; retries are deduplicated, backed off, and shutdown-aware; password/session and last-admin mutations are atomic; and revocation requires a stable twice-observed exact committed roster. The complete focused lane and pre-review matrix passed, but pre-commit audit found two remaining races. |
| M4 twelfth-review corrections | pass · awaiting exact-head review | A dedicated replicated cache-admin lease excludes membership and planned-outage changes across the claim-bound Store mutation; a definitive direct restart cancellation resolves both its synthetic token and the exact preparation latch it clears; 256 definitive credential cycles leave no receipts while ambiguous acquisition retains its permanent receipt. The complete focused lane and pre-review matrix pass. |
| M5 Linux host preflight | pass · execution pending | `nynuc` accepts the supplied deploy key and has 16 CPUs, about 36 GiB available memory, about 153 GiB available under writable `/var/tmp`; the existing Linux/amd64 container was executed and reported `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| Full repository suite | deferred | Run once on the final fixed promotion candidate, as requested |

## Decisions to review — autonomous choices

1. **Work only in independent clones.** M3, M4 integration, and M5 campaign
   work live under separate `/private/tmp/plurx-*` clones. A mistakenly
   created worktree and its two branches in the shared repository were removed
   after a byte-for-byte patch comparison; no pre-existing shared changes were
   reset or deleted.
2. **Ship recovery always on, with no production feature gate.** Settings →
   Dev explains the majority, network, budget, and qualification prerequisites;
   it does not expose a switch that can disable the transport correction.
   Validation-only fault
   injection remains test-only because exposing a production fault switch
   would itself be unsafe; production transport behavior is always compiled.
3. **Keep protocol compatibility.** New budgets and observations stay local;
   serialized Raft and binary API variants do not change during the rolling
   update.
4. **Fold the first review's M2 findings into the M1 task PR.** Writer failure
   is only safe when the connection owns both split tasks and accepted
   snapshot work has a longer-lived owner. Keeping these coupled changes in
   one review prevents the flush fix from shipping with known teardown races.
5. **Open and merge only reviewed exact candidates.** M1 pull request !56 was
   merged as `5f96469a94` only after its replacement effort gate passed. Its
   first preflight exposed an unmapped validation helper; the merged correction
   routes changes to that helper through the cluster/WAL lane it protects and
   has its own scope regression.
6. **Treat receiver admission as the configured chunk budget.** The first M3
   adversarial pass found that sender C had been configured while the receiving
   snapshot executor still admitted work under the old fixed frame-write
   timeout. The node-owned executor now captures the validated C value at
   startup, and its capacity-one regression uses a non-default scaled budget.
7. **Use `nynuc` for the Linux campaign, without installing host tools.** It
   has materially more free memory and disk than `nuc3`, and already carries
   the pinned Rust container image. Source will be transferred with
   `git archive`; neither `.git` nor repository credentials leave the clone.
8. **Fail closed above 64 committed Raft members on diagnostic roster reads.**
   The product supports a much smaller operational cluster, so a corrupt or
   unsupported oversized committed roster returns unavailable instead of
   allocating an unbounded query/result set or silently truncating authority.
9. **Keep public recovery authentication local only after a real admin proof.**
   A successful ordinary authentication retains the token's SHA-256 digest and
   user ID for a fixed, non-sliding five minutes, capped at 64 entries. Recovery
   reads never fall back to Store, and revocation paths invalidate before their
   Store mutation. This deliberately means a cold process must complete one
   ordinary authentication before the two public recovery routes are usable;
   the private cluster listener remains the startup diagnostic path.
10. **Use a conservative global peer fence for proof revocation.** The signed
    begin/end request carries only a random operation UUID and phase; token
    digests and user IDs never leave their process. Every committed peer must
    acknowledge within the common two-second bound before success. While a
    fence is active, all cache-only recovery authorization on that peer fails
    closed; an uncertain end remains active for at most the existing
    non-sliding five-minute proof TTL. This trades a short cluster-wide
    diagnostic refusal for avoiding credential-derived or user-identifying
    egress.
11. **Fail the legacy console password reset closed.** The command had no
    daemon-owned authenticated control path and wrote the replicated Store
    directly, bypassing every peer's cache-revocation fence. Rather than invent
    a rushed privileged IPC protocol in this recovery effort, it now refuses
    before reading a secret or opening Store. Administrators must use the
    authenticated daemon API; a future console recovery design requires its own
    threat model and review.
12. **Retain exact planned-outage release receipts.** A disconnected API-server
    task may submit its prepared claim after any time-based cleanup barrier.
    Each random UUID claim is therefore one-shot: exact release records a tiny
    permanent replicated receipt before deleting the active lease, and delayed
    acquisition checks that receipt. Planned outages are privileged, rare
    operations; reclaiming these rows later requires a proven bound on detached
    write lifetime rather than an assumed wall-clock delay.
13. **Enforce the planned-outage protocol in replicated SQLite during rolling
    upgrade.** Permanent receipts alone cannot constrain a previous-release
    coordinator that still submits raw lease statements. Transaction-local
    intent rows now authorize current insert, update, and delete shapes; durable
    triggers reject old writers, and the schema-owned heartbeat cleanup creates
    its own exact receipt and intent before deleting an expired lease.
14. **Keep local admission fail-closed until exact cleanup is known.** Lease
    expiry bounds an ordinary preparation, but it cannot authorize serving while
    a disconnected replicated release is still unresolved. A synchronous local
    latch survives the original deadline and is cleared only after node-owned,
    idempotent cleanup receives a definitive result.
15. **Let the schema-owned heartbeat trigger expire planned-outage leases.** A
    leading raw delete in the current heartbeat conflicts with the mixed-version
    delete guard and prevents the later node update from firing its authorized
    cleanup. Current and previous-release heartbeats now share the same trigger
    path, which records an exact receipt before deletion.
16. **Separate security rosters from diagnostics rosters.** Operations status
    intentionally hides a removal-pending node; credential revocation cannot.
    Revocation now uses every exact committed remote Raft member and refuses the
    Store mutation if any identity or HTTP endpoint is unavailable.
17. **Make local planned-outage fences operation-owned.** Restart and
    maintenance share admission mechanics but not cancellation authority. Timed
    state and unresolved-release latches retain their owner, so restart
    cancellation cannot clear an in-flight or outcome-unknown maintenance fence,
    and maintenance exit cannot clear restart preparation.
18. **Keep old heartbeats live without trusting old release statements.** The
    final delete trigger ignores an unproved legacy row deletion, allowing the
    old heartbeat transaction to continue to its authenticated node update.
    That update owns expiry, writes the permanent release receipt, and removes
    the expired lease; legacy maintenance transitions still roll back.
19. **Make every local outage owner generation-scoped.** A process-local gate
    serializes prepare and cancel, while unique fence tokens and independent
    unresolved-release ownership prevent delayed same-operation cleanup, ABA,
    or overlapping operation types from clearing a successor. One detached
    cleanup owner retries with capped backoff and stops on process shutdown.
20. **Commit credential safety atomically behind a replicated exclusion.** Password
    replacement and token deletion share one Store transaction; demotion and
    deletion enforce the last-admin predicate in the mutation itself. A distinct
    singleton cache-admin lease blocks join, promotion, removal, and planned
    outage changes while a stable exact roster is fenced. Each credential Store
    write names that exact claim, so neither a late member nor a delayed write
    can cross the mutation boundary.
21. **Resolve every token covered by a definitive direct cancellation.** A
    direct restart cancel owns its own synthetic token, but it can also clear a
    guard-owned exact preparation. Both tokens are resolved in the same locked
    transition so a delayed redundant cleanup cannot strand local admission.
22. **Retain credential-mutation receipts only when ambiguity requires them.**
    Acquisition starts conservatively receipt-bearing and becomes disposable
    only through a separate exact transaction after the first response is
    definitive. Normal logout/password/admin churn removes its temporary
    receipt; cancellation, response loss, or ambiguous acquisition preserves
    the permanent anti-resurrection record.

## Next checkpoint — requalify, review, gate, and merge M4

M1/M2 are merged into the effort at `5f96469a94`. The first exact-SHA M3 review
found eight boundary gaps: final mismatch restored T too late, a stale T timer
could beat a ready final-stage update, standalone durations could overflow
`Instant`, both production wrappers lacked direct pins, hard TTL below C was
untested, numeric overflow evidence was missing, the Dockerfile comment was
stale, and receiver admission still used the old fixed timeout. All eight are
fixed and the focused fast lane is green. The replacement SHA passed the same
three adversarial tracks and is now in PR !60. The first replacement
review also caught a cache-only denied-warning lint in a helper's oversized
`Result`; the helper now performs only the phase restoration and the trait
implementations retain error mapping. All three prescribed standalone Clippy
matrices and the 37-test snapshot client suite pass after that change. The
evidence review rejected classifying the production-vendor refactor as
non-runtime, so the current candidate permanently routes `cluster.auth`
changes through its relevant standalone denied-warning Clippy matrix. A final
independent pass then found that the documented prebuilt-image command bypassed
the startup-budget proof. The replacement candidate adds a dedicated
pull/prove/no-build fleet target and persistent ordering coverage. Its first
exact review found that the host checker could still describe a different
revision from the pulled image and that the rollback runbook named the
release-only `latest` alias. The current candidate stamps and inspects the
runtime revision on the frozen image ID, requires a tracked-clean matching
checkout and identical server/discovery images, pins proof and replacement to
that ID, and restores the fleet to `main` or a named immutable tag. Real
socket/install acceptance remains explicitly open for M5.
The first M4 review of
`dbc473ce` found false idle stalls, direction collisions, stale deadlines,
phantom higher-vote acknowledgements, non-monotonic receiver offsets, a generic
terminal error overwrite, insufficient cache refresh margin, and an unused
private-listener client. Replacement `6ef4320a` fixed that first set, then its
adversarial review found incomplete production seams plus receiver epoch,
deadline, reconnect-count, and cached-age gaps. The isolated remediation now
drives public-failure/private-success collection through the production seam,
ages cached observations to the five-minute boundary, pins the refresh margin,
and renders stalled observer/age evidence. The private-listener route now
executes its production authentication boundary, private peer discovery reads
live Raft membership, and a 256 KiB streaming cap bounds its response even
without `Content-Length`. The final concurrency correction covers stale
executor identity, long active deadlines, receiver epochs, terminal outcomes,
reconnect counts, and per-group/per-direction capacity. The combined tree now
passes the pinned fast lane and compiler matrices. Root denied-warning Clippy
caught two `unwrap()` calls in the new cache-age test; they were replaced with
explicit expectations and the exact test plus Clippy were rerun green. All
three tracks must now review one exact SHA. Their review of exact candidate
`03a9c4a5` found two remaining status-projection defects; those corrections
were integrated into combined candidate `b1821b0b` and re-reviewed. The next
review found four final projection and race gaps: cached active state remained
active at zero, a first-time other-peer waiter published before executor
admission, an already-started connection attempt could escape attempt-local
reconnect accounting, and advancing transport observer age did not trigger a
repaint. Candidate `0d868c32` integrates fixes for all four with production-path
regressions and persistent validation mapping. Its next review found that a
public status with the wrong committed Raft identity could still contribute
nested transport evidence to that member's row. The candidate now requires
both the public envelope and nested observer to match committed membership.
The following exact review found that the authenticated, time-sensitive private
route did not prohibit intermediary caching; the candidate now returns
`Cache-Control: private, no-store` and pins it in the production-route test.
Two independent reviews of that candidate then found that the UI could revive
rejected public evidence, one slow public probe could discard already-complete
private evidence, and repeated immediate watermark errors could starve the
ten-second pre-listener progress log. The final-M3 integration now exposes only
one server-sanitized row projection, validates every nested observer, settles
public and private probes under independent deadlines, and checks the due log
outside the biased result race. The 22-case daemon module, paused-time startup
log, web suite, and exact operations mapping pass; exact combined-tree re-review
remains. The latest adversarial pass also found regressing sender offsets,
unbounded historical membership materialization, expired long-install evidence,
display/semantic snapshot-ID aliasing, payload-bearing debug logs, reconnect
epochs that did not follow physical sockets, and stale cross-snapshot UI
precedence. Those findings are fixed on the new combined candidate with exact
production-path regressions. Review of exact candidate `c37970b3` then found
an indirect abandoned-row capability scan, scheduler-dependent snapshot
admission order, one reset/physical-epoch mix-up, missing cross-observer
semantic identity, stale same-snapshot UI precedence, and an overclaimed
request-path isolation test. The replacement fixes all six, the combined lane
is green, and three clean reviews of its next committed SHA are the next gate.
M5 replacement work includes the earlier CI receipt and exact-test
count corrections plus the later zero-resource-growth, early archive-identity,
and cumulative-attempt corrections. It also refuses successful recovery or
final qualification receipts from rerun attempts, so a failed 40-cycle
campaign requires a new source candidate rather than a green rerun; it must be
rebased after M4 review closes.
No work is being done in the user's existing checkout.

**How to read this page:** “pass” means the named command completed against the
named tree. “In progress” does not mean shippable. The effort is complete only
after every milestone is merged into the effort branch, the current `main` is
integrated, the full suite passes once on that fixed tree, and the promotion
receipt exists.
