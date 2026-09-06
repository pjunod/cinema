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
| M4 · transport status | `codex/cluster-transport-m4` | not opened | exact candidate `198482b0` passed its complete lane; two adversarial reviews were clean and one rejected its too-early credential-guard activation. The full-roster activation correction and three-voter mixed-version regression are green; replacement qualification and three fresh reviews are next | Authenticated non-cacheable pre-HTTP status · exact executor-admission identity · stage-accurate deadlines · bounded identities · local-monotonic replay aging · rollback-safe credential mutation |
| M5 · recovery campaign | `codex/cluster-transport-m5-final` | not opened | the reviewed campaign implementation has been rebuilt in an independent clone on the latest M4 line and passes pinned workspace check and denied-warning Clippy; it will move to the replacement M4 commit before qualification | Actual TLS transport matrix · 20 learner and 20 voter cycles |
| Final promotion | `effort/cluster-transport-recovery` | not opened | not started | Full suite once after all review fixes · current-tree qualification receipt |

## Current evidence — Rust 1.97.1 is the compiler of record

The current M4 integration tree is rebased onto merged M3 effort tip
`f5c688a9`. Its moved-base focused cluster/WAL lane, 197 operations contracts,
web suite, 23-point/29-check validation catalog, workspace all-target check,
root and vendored denied-warning Clippy, formatting, diff hygiene, and history
audit pass on Rust 1.97.1. Explicit current-check mappings cover every
corrective M4 commit.
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
latched. The next replacement added a replicated exclusion with claim-bound
Store writes, resolved both cancellation tokens, and retained permanent
anti-resurrection receipts only for ambiguous acquisitions. Its complete
focused lane, pre-review matrix, formatting, and diff hygiene passed before
commit `eba5dfc3`. Three fresh exact-SHA reviews rejected that candidate with
five findings: definitive singleton losers could still create permanent
receipts and cleanup tasks; restart or a locally expired peer fence could
reopen cached authorization while a replicated claim remained capable of
authorizing Store; an expired restart or maintenance token could become
unresolvable; and local transport status did not include peer-collection dwell.
Those corrections are implemented with a versioned local-apply acknowledgement
and persistent fast-lane mappings. The complete cluster/WAL lane, 197 operations
contracts, web contracts, validation catalog, workspace check, denied-warning
Clippy, formatting, and diff hygiene pass; the rejected candidate will not be
pushed or opened as a pull request.
Three exact adversarial reviews then rejected moved-base candidate `aa504834`.
They found that a server-projected active-to-stalled observation retained its
pre-deadline age, a stuck admission could retain the serialized planned-outage
lane past its exact fence, promotion plus password reset was not atomic,
ambiguous cache-admin cleanup grew one permanent UUID row per request, and two
vendored executor regressions were absent from the static lane inventory. The
replacement re-anchors stalled age at the deadline, bounds admission waiting by
the exact fence and shutdown, performs combined promotion/password/token
revocation in one Store transaction, compacts ambiguous anti-replay state into
one replicated expiration watermark, and maps every regression into the
persistent lane. Focused exact regressions, the membership web suite, workspace
all-target compile and denied-warning Clippy, and 197 operations contracts pass;
The complete cluster/WAL fast lane passed from the beginning on committed
replacement `315b462a`, after correcting one stale preflight assertion to the
new deadline-relative stalled age. Three exact reviews of candidate `cc7fa35a`
then found four P2 gaps: a completion inside the five-second cohort could hide
newer same-snapshot failure or progress, deadline-less active states did not
publish their effective fallback stall boundary, cache-admin propagation used
the eight-peer diagnostics limit instead of the supported committed roster,
and the new atomic promotion path lacked a direct three-voter Store contract.
Replacement `c0725954` fixes all four. The producer now serializes its effective
fallback deadline and mixed-version consumers derive the same boundary; only a
completion at least as fresh as live same-fingerprint evidence supersedes it;
credential revocation admits all 63 possible remote members while retaining
eight-request fanout concurrency; and the three-voter contract proves missing,
wrong, and exact claims plus rollback when token deletion fails after the user
update. The web suite, exact new regressions, 197 operations contracts,
workspace check and denied-warning Clippy, vendored denied-warning Clippy,
formatting, history audit, validation catalog, and the complete cluster/WAL lane
pass. Review of replacement `a54223ed` then found four final selection gaps:
the five-second completion cohort ran before attempt chronology, pairwise
mixed-version clock fallback was not a transitive order, and chronology was
lost across different snapshot fingerprints. Peer responses sampled at
different points in the bounded fanout were also aged from one shared
collection-start instant, which could invert two attempts' real chronology.
Runtime correction `794a4d30`
compares every non-expired attempt first, chooses one ordering basis for the
whole cohort, and applies that total order across fingerprints while retaining
the completion exception only inside a fingerprint. Runtime correction
`b54f5c2c` stamps each authenticated public or private response with its own
process-local monotonic receipt time and ages cached transport evidence from
that receipt. The permutation-complete web regressions and delayed-peer
paused-time collector regression pass. Three reviews of exact candidate
`195fd781` then found four final gaps: response transit was absent from the
chronology uncertainty bound; a predecessor acknowledgement could hide a
restarted receiver; login could mint a token after the password version it
authenticated had been replaced; and a rolled-back binary could mutate
credentials before other nodes refreshed their cached readiness. Correction
`c9ae845f` compares interval-bounded sample ages and applies attempt chronology
before completion authority. Correction `a76860d2` makes token insertion
conditional on the exact authenticated password hash and installs replicated
transaction-intent triggers that reject legacy user/token mutations as soon as
v3 has ever been witnessed. The focused browser, paused-time collector,
rollback-trigger, and three-voter Store regressions pass, as do workspace
all-target check and denied-warning Clippy. Complete exact-tree focused
qualification and three fresh reviews remain.
Exact candidate `bb84e9df` then passed the complete cluster/WAL lane, 197
operations contracts, validation catalog, workspace and vendored denied-warning
Clippy, formatting, and diff hygiene. Its first adversarial review found that a
broad uncertainty interval could connect two intervals that were themselves
provably ordered, placing a definitely older stalled attempt in the same
newest cohort. The candidate was rejected immediately and the other reviews
were stopped. Correction `ea7b8476` now builds successive non-dominated
interval frontiers; all six permutations of the bridge counterexample select
the newest installing attempt. Mapping checkpoint `58c1b00a` ties this and the
two preceding runtime corrections to their persistent focused evidence.
Replacement `198482b0` passed the complete cluster/WAL lane, 197 operations
contracts, validation and history audits, and two of its three frozen-SHA
reviews. The third found that the credential rollback triggers activated after
one voter advertised v3, which could reject still-supported legacy mutations
during a one-node-at-a-time rollout. The correction publishes a permanent
activation singleton only while the replicated cache-admin exclusion freezes
membership and every member of the exact committed roster concurrently proves
v3. Before that transition legacy mutations remain usable; after it, a rolled-
back heartbeat cannot reopen them. A three-voter mixed-version regression pins
both sides of that boundary. Candidate `198482b0` remains rejected.
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
| M4 twelfth-review corrections | fail · `eba5dfc3` rejected after green qualification | A dedicated replicated cache-admin lease excludes membership and planned-outage changes across the claim-bound Store mutation, but exact review found five remaining boundedness, restart, fence-expiry, token-resolution, and local-aging defects. This exact tree is not a release candidate. |
| M4 thirteenth-review corrections | fail · `aa504834` rejected after green moved-base qualification | Definitive singleton losers are receipt-free, cleanup ownership is fail-fast and bounded, readiness remains false while any claim can authorize Store, v3 Begin ACK proves exact local application, expired operation identities remain exactly resolvable, and locally collected transport evidence includes monotonic collection dwell. Three exact reviews still found five correctness and persistent-inventory gaps; this exact tree is not a release candidate. |
| M4 fourteenth-review corrections | fail · exact candidate `cc7fa35a` rejected after green qualification | Stalled age is deadline-relative, planned-outage waiting ends with its exact fence, combined promotion/password/token revocation is atomic, ambiguous cache-admin anti-replay state is one bounded expiration watermark, and all focused regressions are permanently inventoried. Three adversarial reviews still found four completion-freshness, fallback-deadline, revocation-roster, and clustered-Store-coverage gaps. |
| M4 fifteenth-review corrections | pass · committed `c0725954`, exact review pending | Completion authority is freshness-bounded; deadline-less active state publishes and projects the producer's 30-second fallback; cache-admin revocation covers up to 63 remote committed members with concurrency eight; and a three-voter contract proves missing/wrong/exact claims plus atomic rollback on token deletion failure. Exact regressions, web, 197 operations contracts, workspace and vendored denied-warning Clippy, validation lint, history audit, formatting, and the complete cluster/WAL lane pass; three exact-SHA reviews restart from zero. |
| M4 chronology total-order correction | pass · committed `794a4d30` and `b54f5c2c`, exact review pending | The selector orders all non-expired attempts before applying fingerprint-local completion authority, uses one clock basis for a mixed-version cohort, handles explicit null attempt ages as absent, and preserves retry chronology across different fingerprints. Per-response monotonic receipts normalize independently sampled ages before selection. Both input-order and all six mixed-cohort permutations plus the delayed-peer collector regression pass. |
| M4 sixteenth-review corrections | pass · committed `c9ae845f` and `a76860d2`, exact-tree qualification pending | Request-to-receipt transit is retained as an explicit age interval; predecessor completion cannot bypass a newer receiver attempt; token creation is bound to the password hash that login authenticated; and durable replicated transaction-intent triggers close the rollback/readiness-poll interval for credential mutations. Focused browser, Rust, and three-voter Store regressions plus workspace check and denied-warning Clippy pass. |
| M4 interval-frontier correction | pass · committed `ea7b8476`, exact review pending | Successive non-dominated frontiers preserve every provable interval ordering even when a third uncertainty interval overlaps both endpoints. All six input permutations of the adversarial bridge case pass, and `58c1b00a` records the current-check mappings. |
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
23. **Require a versioned local-apply acknowledgement before credential
    mutation.** Cache revocation capability v3 retires both older capability
    rows. A peer installs its memory fence first and acknowledges begin only
    after the exact replicated lease claim is visible in its local applied
    state; readiness remains closed until the ordered release is locally
    visible. This prevents restart, replication lag, clock skew, and the local
    five-minute fence bound from reopening stale cached authority while a
    delayed Store write can still commit.
24. **Compact ambiguous cache-admin anti-replay state into one watermark.**
    Every claim already carries an absolute expiry. Ambiguous cleanup advances
    one replicated maximum expiration boundary, so every delayed request at or
    below that boundary is refused without retaining an unbounded UUID set.
    A later claim with a later expiry remains admissible; a backward wall-clock
    step deliberately fails closed until time passes the watermark.
25. **Fail fast on concurrent planned-outage requests.** An accepted operation
    keeps the one process-local lane through definitive cleanup, while callers
    that arrive behind it receive a typed conflict instead of joining an
    unbounded mutex waiter queue. Its admission-settlement wait ends at the
    exact replicated fence deadline or process shutdown.
26. **Keep diagnostics width separate from credential correctness.** Operations
    status still probes at most eight peers, but cache-admin invalidation covers
    every remote member in the supported 64-member committed roster. Network
    concurrency remains eight, so the larger correctness bound does not create
    an unbounded request burst.
27. **Treat effective fallback deadlines as part of retained status.** Active
    ownership without an explicit RPC deadline still has the producer's
    30-second stall boundary. The producer now serializes it, while current
    daemon and browser consumers derive the same boundary for rolling peers.
    Same-snapshot completion can suppress live evidence only when it is at
    least as fresh, preventing an older terminal sample from hiding a successor.
28. **Order distinct recovery attempts by one cohort-wide chronology.** Phase
    severity remains authoritative inside one concrete boot/attempt/socket
    identity, while the more recently started non-expired identity wins across
    retries, restarts, and snapshot fingerprints. Attempt age is authoritative
    only when every representative supplies it; otherwise the entire cohort
    falls back to event age, avoiding a non-transitive pairwise comparator for
    rolling peers. The only cross-attempt override is validated causal
    completion inside the same fingerprint's five-second freshness cohort. A
    deadline-less observation that is already stalled also keeps its original
    zero-remaining boundary across repeated status reads. Independently
    completed peer responses retain their process-local monotonic receipt time,
    so cache projection ages each sample from its actual receipt rather than a
    shared fanout start; no cross-machine wall clock enters that comparison.

## Next checkpoint — requalify, review, gate, and merge M4

M1/M2 are merged into the effort at `5f96469a94`, and M3 is merged at
`f5c688a9` after three clean exact-candidate reviews and the green effort gate.
The first exact-SHA M3 review
found eight boundary gaps: final mismatch restored T too late, a stale T timer
could beat a ready final-stage update, standalone durations could overflow
`Instant`, both production wrappers lacked direct pins, hard TTL below C was
untested, numeric overflow evidence was missing, the Dockerfile comment was
stale, and receiver admission still used the old fixed timeout. All eight are
fixed and the focused fast lane is green. The replacement SHA passed the same
three adversarial tracks and merged through PR !60. The first replacement
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
request-path isolation test. Later exact review rejected `cc7fa35a` on four
remaining freshness, fallback, large-roster, and Store-contract gaps. Those are
fixed in `c0725954`; the complete focused lane and compile/lint matrix are green.
Review of exact candidate `a9ed1371` then found two final chronology gaps: a
second read moved a deadline-less stalled boundary forward, and phase severity
could let a four-second-old attempt hide a restarted attempt for the same
fingerprint. Runtime correction `3f7530e8` fixes both with repeated paused-time
and permutation-independent browser regressions. Review of exact candidate
`086eedcd` then found that a predecessor could publish a fresh failure after a
newer attempt had started. Runtime correction `b554ecc8` ordered distinct
attempts by their monotonic start age. Three reviews of `a54223ed` found that
the five-second completion prefilter could still discard a newer quiet retry,
that pairwise attempt/event clock fallback was non-transitive (and treated
explicit null as zero), that outer fingerprint selection reverted to event
age, and that independently completed peer responses were not normalized to a
common local selection time. Runtime correction `794a4d30` makes chronology one
cohort-wide total order, applies it before fingerprint-local completion
authority, and covers the same- and different-fingerprint late-failure cases
plus every permutation of a mixed current/current/legacy cohort. Runtime
correction `b54f5c2c` records an authenticated response's process-local
monotonic receipt and adds only elapsed time since that receipt when projecting
cached transport evidence. The focused web and delayed-peer collector tests
pass; the complete focused lane, compiler/lint matrix, and three clean
exact-candidate reviews are the next gate.
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
