# Vendored Hiqlite 0.14.0

This directory is the crates.io `hiqlite` 0.14.0 package, licensed under
Apache-2.0. Plurx carries thirty-seven patches for clustered deployments. An
upstream release can retire the eleven generic bugs and the one dependency-only
constraint; it cannot retire the twenty-four Plurx policies:

**Owner:** Paul Junod (repository owner). `pending M6` means the generic fix
still needs a public upstream issue or pull request; it is deliberately not a
made-up URL and prevents the fork from being declared fully tracked.

| # | Patch | Kind | Upstream | Drop condition |
|---:|---|---|---|---|
| 1 | `NodeConfig` duplicate-id rejection | generic bug | pending M6 | Upstream release selects by id and rejects duplicate durable ids. |
| 2 | Split-brain probe TLS policy | plurx policy | — | Never; self-signed cluster TLS is a Plurx deployment requirement. |
| 3 | Concurrent TLS key publication | plurx policy | — | Never; shared embedded-node startup is a Plurx lifecycle requirement. |
| 4 | OpenRaft election trigger | plurx policy | — | Never; retire the 0.9 mechanism after native 0.10 transfer passes the [C5 handoff contract](../../docs/cluster/OPENRAFT-010-EVALUATION.md); graceful removal policy remains. |
| 5 | Local Raft metrics wrapper | plurx policy | — | Never; Plurx readiness owns this deliberately bounded view. |
| 6 | Quorum watermark wrapper | plurx policy | — | Never; replace the 0.9 mechanism after native linearizers pass the [C5 proof contract](../../docs/cluster/OPENRAFT-010-EVALUATION.md); bounded local-read policy remains. |
| 7 | Snapshot duration metrics | plurx policy | — | Never; fixed-cardinality snapshot observability is a Plurx contract. |
| 8 | Snapshot `RemoteError` preservation | generic bug | pending M6 | Upstream release preserves mismatch errors on SQLite and cache snapshot RPCs. |
| 9 | WebSocket write-and-flush budget | generic bug | pending M6 | Upstream release flushes every frame within one bounded write budget. |
| 10 | Connection-supervisor socket ownership | generic bug | pending M6 | Upstream release owns and joins both socket tasks across every terminal path. |
| 11 | Retained reset notification | generic bug | pending M6 | Upstream release cannot lose reset behind a saturated request queue. |
| 12 | Durable snapshot-generation ownership | plurx policy | — | Never; Plurx requires its documented crash-recovery and publication boundary. |
| 13 | Proxy endpoint trust boundary | plurx policy | — | Never; Plurx remote clients must remain inside the configured proxy set. |
| 14 | Bounded definitive-forward recovery | plurx policy | — | Never; Plurx owns the attempt and replay limits plus backup leader attribution. |
| 15 | Validation apply counters and controls | plurx policy | — | Never; Plurx's separate-process validation harness consumes this surface. |
| 16 | Reserve `QueryWrite::Backup` after the deployed `RTT` ordinal | plurx policy | — | Never; the ordinal is a deployed wire position and moving it would itself be the break this patch prevents. |
| 17 | Gate `cryptr/s3` behind Hiqlite `s3` | generic bug | pending M6 | Upstream release no longer enables S3 dependencies when backup and S3 are off. |
| 18 | Scope the vendored `s3-simple` override to this manifest | dependency-only | — | Upstream cryptr accepts `s3-simple` 0.9 or newer, which already carries these dependency-only corrections. |
| 19 | Install the `ring` rustls provider for the HTTP clients | generic bug | pending M6 | Upstream release installs or declares a rustls crypto provider for its `rustls-no-provider` Reqwest clients. |
| 20 | Compile `ring` as the only rustls provider | plurx policy | — | Never; Plurx installs `ring` in every process, so a second compiled provider is an unused C build and an ambiguous default. |
| 21 | Report the committed Raft log index of a write (`WriteAck`) | plurx policy | — | Never; Plurx's watch-state read-your-write fence (K-04 M2) requires this negotiated response shape. |
| 22 | Writer-fixed WAL snapshot cut, off-writer copy and bounded local storage admission | plurx policy | — | Never; Plurx owns the exact image/metadata cut, storage floor, and passive deferral evidence. |
| 23 | K-06 staged startup, clock observation and membership admission | plurx policy | — | Never; Plurx's clock-skew guard must own the actual Raft membership proposal boundary and a learner-only start that waits for authenticated clock observation. |
| 24 | Source layout guard rollback certificate | plurx policy | — | Never; rolling Source activation requires a definitive pre-application rollback without changing deployed transaction payloads. |
| 25 | Bounded snapshot transport observations and RPC budgets | plurx policy | — | Never; Plurx owns this process-local observation surface and its configured chunk, transfer and install budgets. |
| 26 | `TCP_NODELAY` on accepted Raft and API streams | generic bug | https://github.com/sebadob/hiqlite/pull/374 | Upstream release disables Nagle on the streams its Raft and API listeners accept. |
| 27 | Applied mark: registered-table revisions, applied index, image generation and change watches, plus read-pool wait | plurx policy | — | Never; Plurx's applied views (playback-start E1) need a local, publish-before-reply signal of what this replica applied, which no upstream surface offers. |
| 28 | Lease idle Raft client connections instead of dialing per `new_client` | generic bug | pending M6 | Upstream transport reuses established read-confirm sockets; for 0.10, an independent reusable heartbeat path passes the [C5 socket contract](../../docs/cluster/OPENRAFT-010-EVALUATION.md). The default heartbeat factory alone does not retire this fix. |
| 29 | `DbRaftMetrics` type re-export | plurx policy | — | Never; it only names a type Plurx's counted membership client forwards. |
| 30 | Local switch that stops this node's own election timer | plurx policy | — | Never; a stopping Plurx leader hands off and must not campaign again, and OpenRaft 0.9 exposes the switch only on the embedded `Raft` handle. |
| 31 | One proposal gate on the database Raft, with a local hold and metrics wait | plurx policy | — | Never; a Plurx leader handing off must stop its own log before triggering its successor, and OpenRaft 0.9 has no leadership transfer. |
| 32 | Keep the SQLite writer alive when a reply's receiver is gone | generic bug | pending M6 | Upstream release delivers every writer reply without panicking when a cancelled Raft apply or a teardown has dropped its receiver. |
| 33 | A dead database Raft core is reported, and a snapshot build that cannot fit is never started | plurx policy | — | Never; Plurx ends a process whose core quit and owns its snapshot storage policy; OpenRaft 0.9 makes any build storage error fatal. |
| 34 | Dead-core teardown releases the SQL writer and a failed build's files without panicking | generic bug | pending M6 | Upstream release shuts its SQL writer down whatever the WAL's shutdown returns, maps a closed writer channel to an error in `shutdown` and `build_snapshot`, and removes a failed build's temporary image. |
| 35 | Bounded passive DB AppendEntries timing observations | plurx policy | — | Never; Plurx needs bounded source/term/endpoint-scoped passive observations. An equivalent upstream mechanism may replace this implementation after the same contracts pass. |
| 36 | `NodeConfig::listeners`: serve on caller-owned Raft and API sockets | plurx policy | — | Never; Plurx's cluster fixtures own their ports for a config's lifetime and lend one socket to every voter generation they start. |
| 37 | Negotiated Raft pre-vote transport | plurx policy | — | Never; Plurx's C4 remedy needs pre-vote negotiated per Raft stream so a rolling upgrade never resets a stream. Retire the transport half only when native OpenRaft pre-vote travels on an equally negotiated, authenticated RPC. |

- `NodeConfig` selects the local node by `Node::id` and rejects duplicate ids.
  Raft ids are durable identities, so a roster such as `1, 3` is valid when an
  unredeemed join token still holds id 2. Padding that roster with a duplicate
  node creates duplicate connection targets and eventually exhausts file
  descriptors.
  Files: `src/config.rs`, `src/start.rs`.
- The split-brain probe uses Hiqlite's configured HTTP client and API TLS
  verification policy. Auto-generated certificates are intentionally
  self-signed, so the default Reqwest client otherwise reports
  `UnknownIssuer` on every probe.
  Files: `src/split_brain_check.rs`, `src/start.rs`.
- Concurrent embedded-node starts share the first auto-generated TLS key
  without panicking when more than one task reaches the process-wide key's
  one-time publication boundary.
  Files: `src/tls.rs`.
- The authenticated cluster API exposes OpenRaft 0.9's election trigger on a
  selected voter. That release has no dedicated leader-transfer operation;
  Plurx uses the trigger to elect a successor before a leader commits its own
  graceful removal.
  Files: `src/helpers.rs`, `src/network/management.rs`, `src/start.rs`.
- `Client::local_db_raft_metrics` exposes a synchronous local-only wrapper for
  OpenRaft's metrics watch. Remote clients fail immediately instead of falling
  back to management HTTP, and the copied snapshot omits membership, addresses,
  replication maps, and quorum-ack state so passive application metrics cannot
  be mistaken for a quorum-confirmed watermark.
  Files: `src/client/mgmt.rs`, `src/lib.rs`.
- `Client::db_quorum_watermark` asks the current database leader to run
  OpenRaft's quorum-backed linearizable-read proof and returns only
  `(term, leader_id, committed_index, local_read_protocol_version)`. Followers
  use the existing authenticated leader stream; callers remain responsible for
  a monotonic lease and matching the proof to their local Raft observation. A
  P3a leader's three-column response remains a valid Authority/readiness proof
  but maps to protocol `0`, so bounded local reads stay closed during rollout.
  Its quorum-read and local-apply stages share the original absolute one-second
  deadline. Slow observations (at least 100 ms) carry the phase, pending read
  index and local applied index; the final leadership checks and required apply
  wait are preserved. Slow state-machine applications report only Raft index,
  term, elapsed milliseconds and a static operation class, never SQL or binds.
  Files: `src/client/mgmt.rs`, `src/client/mod.rs`, `src/network/api.rs`,
  `src/query/rows.rs`, `src/store/state_machine/sqlite/state_machine.rs`.

- The SQLite snapshot builder and installer publish process-local, lock-free
  duration histograms through `Client::local_db_snapshot_metrics`. Explicit
  RAII start/finish hooks classify build/install success and every error or
  cancelled exit without polling storage or exposing paths and snapshot ids.
  Files: `src/snapshot_metrics.rs`,
  `src/store/state_machine/sqlite/snapshot_builder.rs`,
  `src/store/state_machine/sqlite/state_machine.rs`, `src/client/mgmt.rs`.
- SQLite and cache snapshot RPC responses preserve peer-side Raft errors as
  `RemoteError` instead of flattening them into `Unreachable`. OpenRaft uses
  that type boundary to recognize `SnapshotMismatch`, reset an interrupted
  chunk stream to offset zero, and let a restarted follower converge. The
  `sqlite_install_snapshot_preserves_mismatch_for_offset_reset` and
  `cache_install_snapshot_preserves_mismatch_for_offset_reset` tests drive the
  production `install_snapshot` methods through a nonzero mismatch and guard
  the remote-error boundary. OpenRaft's retained offset-reset test covers its
  private chunk sender, while Plurx's three-voter learner snapshot contract
  covers convergence; these vendor tests do not claim to drive that private
  sender themselves. Remove this patch only after upstream Hiqlite preserves
  peer snapshot errors on both transports.
  Files: `src/network/raft_client.rs`.
- Every Raft, cluster-API, proxy, and authentication WebSocket frame is flushed
  before its writer waits for more work. One 30-second budget covers the write
  and flush together; errors and expiry terminate the writer and wake the
  connection supervisor even while its reader remains live. Graceful Close
  frames use a separate 250-millisecond best-effort allowance, and the server
  challenge/response exchange remains inside the existing five-second
  connection deadline. The real-TLS frame regressions hold the ciphertext tail
  of both client and server writes, including 3 MiB frames, and prove that the
  same socket completes after backpressure clears.
  Files: `src/network/frame_io.rs`, `src/network/raft_client.rs`,
  `src/network/raft_server.rs`, `src/network/api.rs`,
  `src/network/handshake.rs`, `src/network/web_socket_connect.rs`,
  `src/network/mod.rs`, `src/client/stream.rs`, `src/server/proxy/stream.rs`.
- Raft and cluster-API connection supervisors own and join both split socket
  tasks. Reader EOF, malformed frames, writer errors, task panics, reset,
  shutdown, and leader handoff all wake admission even when a bounded queue is
  full. Queue admission uses an ownership-retaining `try_send` loop, so a
  timeout or handoff cannot claim a mutation was undispatched after a
  cancellable send future already transferred it. Accepted snapshot receive
  operations move to one node-owned executor per Raft group with one running
  and one queued request; disconnecting a socket drops its reply but cannot
  cancel a partial file write. Queued work whose reply owner disappeared is
  skipped before execution. Graceful shutdown keeps Raft and its state-machine
  worker alive until this executor releases accepted work, and cancellation of
  one shutdown waiter cannot detach the retained task. Connections capture
  their originating Tokio runtime so an off-runtime drop still owns cleanup.
  Files: `src/network/raft_client.rs`, `src/network/raft_server.rs`,
  `src/network/snapshot_executor.rs`, `src/network/mod.rs`,
  `src/client/stream.rs`, `src/client/helpers.rs`, `src/client/mgmt.rs`,
  `src/client/shutdown_handle.rs`, `src/dashboard/query.rs`, `src/error.rs`,
  `src/app_state.rs`, `src/store/mod.rs`.
- A dropped OpenRaft RPC signals its WebSocket manager through a dedicated
  retained notification instead of best-effort enqueueing `Reset` behind the
  request itself. The old one-slot queue could be full at the hard deadline,
  lose the reset, and leave a leader permanently waiting on the abandoned
  connection after a follower had installed its snapshot. The
  `handler_coordinator_consumes_retained_reset_when_request_queue_is_full`
  regression keeps cancellation independent of request-queue pressure.
  Files: `src/network/raft_client.rs`.
- Snapshot build, install, read, and asynchronous cleanup share one
  file-ownership boundary. Completed files use fsync plus atomic rename, and a
  separately fsynced pointer publishes the exact current generation (including
  an explicit empty state). Install uses a durable pending-generation marker;
  publication failures stop further state-machine work and startup completes
  the pending recovery before serving. Read-only inspection leaves generations
  immutable. Startup migrates legacy directories from live database metadata
  or applied Raft order; normal reads and recovery never infer recency from UUID
  ordering, so delayed cleanup and interrupted publication cannot replace or
  delete current state.
  Files: `src/store/state_machine/sqlite/snapshot_builder.rs`,
  `src/store/state_machine/sqlite/state_machine.rs`.
- Remote clients created in proxy mode retain only their configured proxy
  endpoints across WebSocket reconnects, metrics discovery, and
  `ForwardToLeader` responses. Connection failures, established-stream closes,
  and leader-forwarding responses advance independent DB/cache cursors through
  that original endpoint set; ambiguous in-flight requests fail without
  replay, and directly advertised voter addresses never replace the boundary.
  A proxy therefore remains an enforceable routing, partition, and trust
  boundary after failover and recovery. Remote shutdown uses out-of-band
  cancellation and joins both stream managers, the rate ticker, and the remote
  listen-notify loop even when every endpoint is unavailable; active and queued
  work receives a stable error instead of a dropped-acknowledgement panic.
  Files: `src/client/create.rs`, `src/client/mod.rs`, `src/client/stream.rs`,
  `src/client/helpers.rs`, `src/client/listen_notify.rs`,
  `src/client/rate_limit.rs`.
- A client receiving `ForwardToLeader(None, None)` treats it as a definitive
  unaccepted request, probes its configured authenticated peers concurrently,
  and reconnects through a detached, bounded discovery and acknowledged stream
  handoff. Database requests make at most three attempts, recovering after no
  more than two definitive forwarding refusals; this covers the interval where
  the first replacement is still learning the election winner without replaying
  an accepted write or allowing raw client calls to hang. Management clients
  never follow redirects, so their custom API-secret header cannot leave the
  configured roster. Proxy-mode clients reconnect through the next configured
  proxy instead of escaping that trust boundary. Replicated backups derive the
  upload owner from the committed log entry's leader rather than a client-side
  cached leader sample, so a handoff cannot acknowledge a backup that no node
  uploads.
  Files: `src/client/helpers.rs`, `src/client/stream.rs`,
  `src/client/create.rs`, `src/client/query.rs`, `src/client/execute.rs`,
  `src/client/batch.rs`, `src/client/transaction.rs`, `src/client/migrate.rs`,
  `src/client/cache.rs`, `src/client/dlock.rs`, `src/client/backup.rs`,
  `src/store/state_machine/sqlite/state_machine.rs`.
- Under `validation-test-helpers` only, the SQLite state machine keeps
  process-local monotonic counters of applied Raft entries by payload kind
  (blank, membership, normal), exposed as
  `validation_applied_payload_counts()`, and attributes applied normal
  entries to caller-registered SQL classes
  (`validation_register_applied_sql_classes` — set-once before the node
  starts; `validation_applied_sql_class_counts` reads them in registration
  order; an entry carrying statement SQL counts toward the first class whose
  needle matches — a `^` prefix anchors the match to the statement start —
  while migration, backup, and RTT payloads stay unclassified and therefore
  fail exact counts loudly). Exact-count drills sample both around their
  write windows so a
  contaminating entry — a blank leader-establishment commit, a membership
  change, or a background writer's SQL — is named rather than merely
  counted. Setting `PLURX_VALIDATION_LOG_APPLIED` in a validation process
  additionally logs each applied entry's index and payload to stderr, which
  is how a contaminating entry is identified down to its SQL. Production
  binaries compile none of it.
  Files: `src/store/state_machine/sqlite/state_machine.rs`,
  `src/store/state_machine/sqlite/writer.rs`, `src/network/raft_client.rs`,
  `src/client/query.rs`, `src/client/mod.rs`, `src/lib.rs`.
- The replicated SQLite write enum always includes `QueryWrite::Backup`, even
  when the local backup implementation is not compiled, and appends that
  reservation after the deployed `QueryWrite::RTT` variant. `RTT` therefore
  remains ordinal 5 in both directions of a rolling deployment; reserving a
  new variant before it would itself be a wire break. `backup` and `backup,s3`
  remain independently compilable, and a build without backup returns an
  explicit feature error if it receives the reserved replicated variant. The S3
  half of that change is its own patch, described in the next bullet.
  Files: `src/store/state_machine/sqlite/state_machine.rs`.
- The optional `cryptr` dependency no longer enables its S3 client
  unconditionally. Hiqlite's `s3` feature enables `cryptr/s3` instead, so
  downstream users that select `backup` or `s3` retain the same backend while
  builds such as Plurx that select neither do not compile an unused S3, QUIC,
  and second aws-lc stack.
  Files: `Cargo.toml`, `src/backup.rs`, `src/client/backup.rs`,
  `src/error.rs`.
- This manifest carries its own `[patch.crates-io]` row pointing `s3-simple`
  at `vendor/s3-simple`. Gating the edge removes S3 from the configuration
  Plurx builds, but it does not make the configuration Hiqlite still
  advertises safe: with `backup` or `s3` enabled, registry `s3-simple` 0.8.0
  resolves `quick-xml` 0.39.4 (RUSTSEC-2026-0194, RUSTSEC-2026-0195) and a
  retired `aws-lc-sys` 0.39.1 that `deny.toml` forbids. cryptr 0.10.0 requires
  `s3-simple ^0.8.0`, so upstream 0.9.x cannot be selected instead. The
  override lives here rather than in the workspace root because the workspace
  graph contains no `s3-simple` at all, where the same row would be an unused
  patch. `tests/operations/test_hiqlite_patch_ledger.py` resolves this
  directory's lockfile against `deny.toml` and the advisory floor, so the
  advertised backup/S3 graph cannot regress unnoticed.
  Files: `Cargo.toml`, `Cargo.lock`.
- `http_client::ensure_rustls_crypto_provider` installs the `ring` rustls
  provider once per process, and every `reqwest::Client` this crate builds goes
  through it. `reqwest` is declared with `rustls-no-provider`, so
  `ClientBuilder::build` reads the process-wide default and panics when there is
  none — for any client, TLS or not. Before the patch above dropped
  `backup -> s3 -> cryptr/s3`, that edge pulled a second `reqwest` whose
  provider feature Cargo unified onto this one, so the provider was present by
  accident and no build asked for it. `ring` is what this crate's `rustls`
  dependency and `axum-server`'s `tls-rustls-no-provider` already select and
  what `plurx-core`'s `install_default_crypto_provider` installs, so one
  provider stays in the process. Naming it at client construction rather than in
  a `main` is deliberate: a test binary or a library consumer runs no `main` of
  ours, which is how the accidental inheritance went unnoticed.
  `crates/plurx-core/tests/hiqlite_tls_provider.rs` builds a real client in a
  binary of its own and fails if the install goes away.
  Files: `src/http_client.rs`, `src/network/management.rs`.
- The `rustls`-family dependencies no longer ask for `aws-lc-rs`. Upstream
  reached it three ways: `axum-server`'s `tls-rustls` feature (which is
  `tls-rustls-no-provider` plus `rustls/aws-lc-rs`), `rustls`'s
  `prefer-post-quantum` feature (which is `aws_lc_rs` plus a reordering of that
  provider's key-exchange groups), and `tokio-rustls`'s default features. This
  manifest now declares `axum-server` with `tls-rustls-no-provider` only, drops
  `prefer-post-quantum`, and declares `tokio-rustls` with
  `default-features = false` and `logging`, `ring` and `tls12` (the defaults
  minus `aws_lc_rs`). Every process that reaches this crate's TLS already runs
  on `ring` (`start_node`, the proxy, `http_client` and `plurx-core` all install
  it), so the removed provider was compiled, with its CMake/C `aws-lc-sys`
  build, and never selected; with `ring` alone rustls also stops refusing to
  choose a default. `RustlsConfig`, `bind_rustls` and `from_tcp_rustls` are
  all `tls-rustls-no-provider` items. `crates/plurx-core/tests/rustls_single_provider.rs`
  fails if any of the three routes returns. The optional `s3` and `full`
  configurations still resolve `aws-lc-rs`, through the `reqwest` `rustls`
  feature that cryptr and `s3-simple` select; Plurx builds neither, and every
  entry point in them still installs `ring` explicitly.
  Files: `Cargo.toml`.
- `Client::execute_acked` and `Client::execute_returning_map_acked` return a
  `WriteAck` carrying the Raft log index of the committed entry beside the
  usual result: from `ClientWriteResponse::log_id` on the leader, and through
  the API stream on every other node. The stream half is negotiated per
  connection so no request encoding changes: the client adds
  `x-hiqlite-write-ack: raft-log-index-v1` to its WebSocket upgrade request,
  and only a leader that saw that header answers `Execute` and
  `ExecuteReturning` with the new `ExecuteAcked` and `ExecuteReturningAcked`
  response variants, appended after every deployed ordinal. An older leader
  ignores the header and keeps sending the original variants, which the new
  client still accepts and reports as an unknown index (`None`); an older
  client never sends the header and so never receives a variant it cannot
  decode; a proxy decodes and re-issues requests through its own client, so
  its callers see `None`. The plain `execute` and `execute_returning*` methods
  keep their signatures and discard the index.
  `network::api::tests::write_ack_variants_are_appended_after_every_deployed_response_ordinal`
  pins the ordinals, `write_ack_log_index_is_sent_only_on_a_negotiated_connection`
  pins the negotiation, and Plurx's
  `watch_fence_serves_watch_state_locally_only_behind_the_acknowledged_write`
  store contract drives a real streamed write end to end.
  Files: `src/client/execute.rs`, `src/client/stream.rs`,
  `src/network/api.rs`, `src/lib.rs`.

- The SQLite writer persists the cut metadata and pins a dedicated read-only
  WAL transaction before dequeuing another apply. Online Backup copies that
  exact reader on the blocking pool. Completion updates only snapshot id on
  success; later applies and membership stay live. Blank entries now update
  the same last-applied boundary. Builds serialize with install/publication;
  shutdown joins an outstanding copy. The focused `snapshot_cut_tests` and
  builder generation/blank regressions pin concurrent apply, membership,
  failure and checkpoint behavior. `Client::trigger_local_db_snapshot` asks
  any member, leader or follower, to cut that snapshot at its own applied
  boundary without proposing an entry, so Plurx's portable backup can anchor
  its cut through the forwarding `WriteAck` write (row 21) on whichever voter
  holds the backup lease. Target-local storage admission runs
  before the writer request with a size-based floor and ten-second retries.
  A build that cannot fit keeps waiting (row 33); one that can fit but is
  short of the floor, or unmeasurable storage, is bounded at ten minutes
  (then the existing path), exposed through
  passive fixed-label metrics. This changes no replicated enum ordinal,
  metadata encoding, completed image format, or install format.
  Files: `src/store/state_machine/sqlite/writer.rs`,
  `src/store/state_machine/sqlite/snapshot_builder.rs`,
  `src/store/state_machine/sqlite/state_machine.rs`,
  `src/snapshot_admission.rs`, `src/config.rs`, `src/config_toml.rs`,
  `src/store/mod.rs`, `src/client/mgmt.rs`.
- K-06 clock-skew admission (`docs/cluster/CLOCK-SKEW-GUARD-DESIGN.md`,
  `docs/cluster/CLOCK-SKEW-ENFORCEMENT-IMPLEMENTATION.md`) binds Plurx's
  node-local guard at the vendored Raft proposal boundary without Hiqlite
  learning clock policy or depending on core. `src/membership_admission.rs`
  adds the caller-owned `MembershipAdmission` trait: a pure `prepare` captures
  the original decision before any await, and a synchronous `redeem` runs only
  for a new submission, never for committed-outcome reconciliation.
  `src/reduction.rs` adds `ReductionFenceReference`, an untrusted,
  shape-checked exact durable reference that lets a fenced voter removal or
  leave be admitted without any clock authority crossing the wire.
  `src/helpers.rs` redeems the prepared admission immediately before every
  `add_learner` and `change_membership` proposal and refuses a proposal that
  has none once a policy is installed; `src/network/management.rs` prepares it
  for `add_learner`, `become_member` and the new fenced remove-voter and leave
  requests, and `src/network/raft_server.rs` adds the cache-group
  `FencedRemoveMembershipCache` frame, so unfenced legacy reductions are
  refused under a policy. `src/lib.rs` adds
  `start_node_with_membership_admission` and
  `start_node_for_clock_observation`. `src/start.rs` carries
  `StartupPhase::ClockObservation`, which forces `learner_only`, binds the
  admission to local Raft metrics before any listener runs, and defers the
  split-brain check to Client handoff and the backup cron to
  `finish_clock_observation`. `src/startup_cleanup.rs`, `src/store/mod.rs` and
  `src/store/state_machine/sqlite/state_machine.rs` give that staged start
  cancellation-safe ownership of the WAL, SQLite writer, Raft groups and
  listeners: a dropped startup future drains accepted snapshot work and the
  actual writer instead of aborting or abandoning them. `src/app_state.rs`,
  `src/client/mod.rs` and `src/client/create.rs` carry the installed
  admission, the deferred backup configuration and the retained listener
  owner; `src/client/mgmt.rs` adds `local_membership_admission`,
  `promote_after_clock_observation` (one submission redeemed just before send,
  no redirect or retry), `finish_clock_observation` and
  `shutdown_retained_startup`. Normal `start_node` keeps its existing
  behaviour. The WAL half is row 4 of `vendor/hiqlite-wal/PLURX-PATCH.md`.
  Plurx's production callers are `crates/plurx-core/src/cluster/migration.rs`
  and `crates/plurx-core/src/cluster/membership.rs`; the `k06_*` tests in
  `src/startup_cleanup.rs` pin the drain boundary.

- **Row 24 — Source layout rollback certificate:** `src/error.rs` exports the
  exact guard SQL/DDL and reserved existing `Error::Transaction` certificate
  predicate. `src/store/state_machine/sqlite/writer.rs` emits it only for the
  exact first statement, a single integer layout 81 or 82, the actual named
  SQLite CHECK failure, and successful explicit rollback. Later statements,
  other SQL/parameters/constraints and failed rollback keep ordinary errors.
  The existing transaction/Raft payload is unchanged. The focused
  `source_layout_certificate_requires_exact_first_guard_and_completed_rollback`
  regression covers matching zero-row guards, mismatch rollback, and refusal
  boundaries; no unknown commit is certified.

- **Row 25 — Snapshot transport observations and budgets:**
  `src/transport_status.rs` retains bounded process-local peer/attempt/socket
  observations, byte progress, disposition and owned-task counts, not application
  payloads or replicated state. Membership binding bounds the peer set; attempt
  histories and retained snapshot labels are bounded independently. Outbound
  attempts and socket epochs, inbound chunk receipt and terminal disposition,
  and actual API/socket/executor task guards feed the shared handle. The
  authenticated SQLite management route and the local/selected-peer client
  accessors expose observations, not a quorum or durability certificate.
  `src/config.rs` validates chunk, overall-transfer and install budgets; chunk
  and transfer defaults are 30 and 1200 seconds, also carried by the
  `src/config_toml.rs` construction path. Completing a chunk does not renew the
  overall transfer deadline. The soft-TTL timeout in
  `NetworkConnectionStreaming::send` now returns `Unreachable` wrapping a
  `TimedOut` error that names the target node, request kind, append entry
  count, snapshot chunk bytes, elapsed and soft-TTL milliseconds, instead of
  the bare `Elapsed`; it adds no serialization (an append's encoded size is
  unknown at that point and is not reported) and changes no behaviour. This
  policy does not satisfy the generic transport
  repairs' upstream drop conditions.
  Files: `src/transport_status.rs`, `src/config.rs`, `src/config_toml.rs`,
  `src/network/raft_client.rs`, `src/network/raft_server.rs`,
  `src/network/snapshot_executor.rs`, `src/network/management.rs`,
  `src/network/api.rs`, `src/client/mgmt.rs`, `src/start.rs`,
  `src/store/mod.rs`, `src/app_state.rs`, `src/lib.rs`.
- The Raft and API listeners disable Nagle on every stream they accept. A node
  answers the WebSocket upgrade and then writes its handshake challenge as a
  second small segment; with Nagle on, that segment waits for the ACK of the
  first, and the connecting peer, which sends nothing until it has read the
  challenge, delays that ACK (40 ms on Linux). OpenRaft 0.9 opens a new stream
  to every other voter for each linearizable-read leadership check, so every
  consistent read on a TLS cluster paid the stall: a three-voter loopback
  cluster measured 44.0 ms per `query_consistent_map` before and 1.7 ms after,
  against 0.8 ms for a quorum write, which uses the long-lived replication
  streams. `start::nodelay_tests` drives the acceptors over real sockets, TLS
  included, and `tests/operations/test_hiqlite_patch_ledger.py` pins that
  every listener is built from them. The proxy listener is not changed.
  Files: `src/start.rs`.
- The applied mark (playback-start E1, `docs/streaming/PLAYBACK-START-E1-BUILD.md`
  §4.1). `NodeConfig::applied_mark_tables` registers up to thirty-two tables; the
  bounded mask is `u32` (including the full-width case); existing slot order
  is preserved when Plurx appends the four canonical roster tables. The
  writer installs an SQLite update hook (rusqlite `hooks`) that sets one bit
  per registered table, and `publish_then_send` turns those bits into
  revisions and stores the applied log index after the SQLite work has
  committed or rolled back and before every reply of an arm that applies an
  entry, early-`continue` paths included. The hook cannot attribute DDL or a
  table rebuild (it reports the copy under its temporary name and the drop
  and rename not at all), so the helper re-reads `PRAGMA schema_version`
  before every publish and bumps every slot when it moved, whichever arm
  carried the change; `Batch`, `Migrate` and a successful restore also bump
  every slot. `MetadataRead` publishes the persisted applied index, so a
  restarted node does not report 0 until its next apply. The state machine,
  not the writer arm, owns image replacement: a guard makes the generation odd
  before `SnapshotApply` is sent and even after the read pool is reconnected,
  in `update_state_machine_` (start-up restore and pending recovery) and in
  the install task; a failed restore leaves it odd until a later replacement
  completes or the node restarts. The hook only sees statements this binary
  applies; an unqualified `DELETE` replayed from an older binary's log entry
  would not move a slot (the plurx census refuses one in current source).
  `Client::local_db_applied_mark` is local only, with no remote fallback.
  `applied_mark_deletes_only` names registered tables whose slot moves on a
  deleted row only. `AppliedMarkHandle::watch` arms an `AppliedWatch` that
  wakes when its slots or the generation move (sharing A6 M3); `publish`
  notifies only for a slot a live watch counts, after bumping revisions.
  The same row times every read-pool checkout into a fixed process-wide
  histogram (`db_read_pool_wait`), the baseline the E1 contract asks for.
  Nothing crosses the wire. Tests: `applied_mark::tests`, `applied_mark::watch_tests`,
  `writer::applied_mark_tests` (including the precommit-hold ordering test
  under `validation-test-helpers` and a source test of the reply order),
  the replacement tests in `state_machine::snapshot_metrics_contracts`, and
  `mgmt::tests::local_applied_mark_accessor_has_no_remote_fallback`.
  Files: `Cargo.toml`, `src/applied_mark.rs`, `src/config.rs`,
  `src/config_toml.rs`, `src/store/state_machine/sqlite/writer.rs`,
  `src/store/state_machine/sqlite/state_machine.rs`, `src/store/mod.rs`,
  `src/app_state.rs`, `src/client/mgmt.rs`, `src/query/mod.rs`, `src/lib.rs`.
- The Raft client factory reuses connections. OpenRaft 0.9.25's leadership
  read-confirm (every `ensure_linearizable`, so every `query_consistent*`)
  calls `new_client` per voter per read and drops the client after one empty
  append; OpenRaft's contract is that `new_client` does not create a
  connection. Upstream's factory dialed a TCP + WebSocket handshake for each
  client and closed it on drop, so the handshake ran inside the read's
  heartbeat-interval budget, where one lost SYN outlasts the budget because
  TCP's initial retransmission timeout is one second (fleet capture
  2026-11-14: ~223 dials/s leader to one voter; 32 of 33 originating timeouts
  followed a lost SYN by 800–801 ms; `docs/cluster/RAFT-READ-CONFIRM-DIAL-RCA-2026-11-14.md`).
  A dropped client now parks its connection in the factory's
  `IdleConnections` when no reset, cancellation, queued request or snapshot
  attempt touched it, and the next `new_client` for the same group and target
  leases it. A lease is exclusive, so each connection still carries one
  client's RPCs; at most 64 are kept per target (sized to the measured read
  bursts). The parked supervisor owns
  its 30 s idle deadline and closes its socket when it passes, and it exits
  instead of reconnecting when its socket ends; nothing is leased after Raft
  stops. No timeout, heartbeat or retry changes. Tests:
  `raft_client::tests::read_confirm_cycle_reuses_one_socket_across_answered_rpcs`,
  `dropped_healthy_client_is_leased_without_a_new_socket`,
  `client_reset_during_lease_is_closed_not_reused`,
  `timed_out_rpc_closes_its_connection_instead_of_parking_it`,
  `parking_beyond_the_per_target_cap_closes_the_extra_connection`,
  `parked_supervisor_closes_its_socket_at_the_idle_deadline`,
  `parked_connection_whose_socket_ends_exits_and_is_not_leased`.
  Files: `src/network/raft_client.rs`, `src/store/mod.rs`.
- `hiqlite::DbRaftMetrics` names `openraft::RaftMetrics<NodeId, Node>`, the
  type `Client::metrics_db` returns. Plurx's counted membership client
  (`plurx_core::store::CountedClient`) forwards that call by name, and this
  alias lets it do so without a direct `openraft` dependency. A type alias
  only; no behaviour.
  Files: `src/lib.rs`.
- `Client::disable_local_db_elections` calls OpenRaft's
  `runtime_config().elect(false)` on the database Raft of a local client. A
  plurxd told to stop calls it first, leader or not, before it hands
  leadership off (`docs/cluster/LEADER-SHUTDOWN-HANDOFF-BUILD.md`): the node
  keeps voting, replicating and answering an explicit election trigger, but
  its own timer never starts a campaign, so a handoff that loses the vote
  costs the survivors one election rather than two. It is process state,
  not persisted; the next start campaigns normally. Remote clients fail
  immediately, with no HTTP fallback. Test:
  `mgmt::tests::local_election_switch_has_no_remote_fallback`.
  Files: `src/client/mgmt.rs`.
- Every proposal to the database Raft takes a shared permit from one gate
  (`proposal_gate.rs`): `client_write` from the local client paths and from
  the API handlers that serve forwarded writes, and the learner and
  membership changes, all through `StateRaftDB`'s three helpers.
  `Client::hold_local_db_proposals(deadline)` takes the exclusive side. It
  waits for in-flight permits, new proposals wait rather than fail, and the
  hold releases on drop on every path. A blocking learner add holds its
  permit only for the append; the catch-up wait runs after it drops. It returns `None` when it cannot
  be taken by the deadline. `Client::wait_local_db_metrics` waits on the
  metrics watch for a condition. A leader that hands off
  (`docs/cluster/LEADER-SHUTDOWN-HANDOFF-BUILD.md`) holds its log, waits
  until a survivor's matched index equals its last entry, and triggers that
  survivor. Before this, a trigger racing an in-flight append was refused
  by the old leader in 5 of 14 daemon runs. Heartbeats append nothing; the
  leader's blank entry comes only at its election. The cache Raft is not
  gated. Tests: `proposal_gate::tests`, including the source tests
  `every_database_proposal_goes_through_the_gate` and
  `a_learner_catch_up_wait_holds_no_permit`, and
  `mgmt::tests::proposal_hold_and_metrics_wait_are_local_only`.
  Files: `src/proposal_gate.rs`, `src/app_state.rs`, `src/store/mod.rs`,
  `src/lib.rs`, `src/client/mgmt.rs`, `src/client/execute.rs`,
  `src/client/transaction.rs`, `src/client/batch.rs`, `src/client/migrate.rs`,
  `src/client/backup.rs`, `src/backup.rs`, `src/helpers.rs`,
  `src/network/api.rs`, `src/network/management.rs`, `src/dashboard/query.rs`.
- `LocalDbRaftMetrics::core_stopped` waits on the local Raft watch until the
  core stops, and returns `LocalDbCoreStop`. `Stopped` means OpenRaft's
  `Fatal::Stopped` (an ordinary `Raft::shutdown`). `Fatal(text)` means any
  other terminal `running_state`, or a watch that closed without one (a
  panic). plurxd ends the process on `Fatal`
  (`docs/cluster/DEAD-RAFT-CORE-EXIT-BUILD.md`). In OpenRaft 0.9.25 the
  core turns a `build_snapshot` storage error into its own fatal quit
  (`raft_core.rs`: `command_result.result?`), so admission never attempts
  a build that cannot fit (available below one image, `max(WAL, 32 MiB)`
  and 64 MiB): it waits, rechecked every 10 s, and `snapshot_storage_deferral`
  does not bound that wait. A build that can fit but is short of the
  comfortable floor, or whose storage cannot be measured, keeps the old
  bound (ten minutes by default; 0 attempts it at once). A deferred build
  ends with an error once the SQL writer has stopped, instead of waiting
  forever. `LocalDbRaftMetrics::core_fatal` reads the same terminal state
  without waiting, so plurx-core's startup admission and catch-up loops end
  at once on a dead core instead of at the catch-up deadline. The teardown a
  dead core then runs is row 34. Tests:
  `mgmt::tests::the_core_stop_names_a_fatal_quit_and_ignores_an_ordinary_stop`
  and the `snapshot_admission` band tests.
  Files: `src/client/mgmt.rs`, `src/client/mod.rs`, `src/lib.rs`,
  `src/config.rs`, `src/snapshot_admission.rs`,
  `src/store/state_machine/sqlite/snapshot_builder.rs`.
- A teardown after the Raft core died no longer panics or strands state.
  Upstream skips the SQL writer's shutdown, and so the lock-file removal,
  when the WAL's shutdown fails, so the next start's `auto-heal` mistakes a
  clean stop for a crash and rebuilds the state machine. Both client
  shutdowns now shut the SQL writer down whatever the WAL returns and return
  the WAL's error after it. The main shutdown also `.expect`ed the writer's
  channel and acknowledgement, and `build_snapshot` `.expect`ed its request
  and reply, so a stopped writer panicked the caller. All four now return an
  error. A build that fails before its rename removes its `<uuid>.temp` and
  `-journal`, so a full disk is not kept full by a dead build, and startup
  removes abandoned ones after the lock check and before the writer starts.
  Tests: `the_lock_file_is_released_whatever_the_wal_shutdown_returns`, the
  snapshot builder's and state machine's temp-removal tests, and plurxd's
  `a_follower_whose_raft_core_dies_exits_non_zero_and_rejoins_after_restart`,
  which checks the lock file is gone after the dead-core exit.
  Files: `src/client/mgmt.rs`,
  `src/store/state_machine/sqlite/snapshot_builder.rs`,
  `src/store/state_machine/sqlite/state_machine.rs`.

- **Row 35 — Passive append timing:** `src/append_timing.rs` owns the bounded
  process-local summaries. `src/network/raft_client.rs` records DBAppend RPC
  attempts with the stable factory/pool epoch; `src/network/raft_server.rs`
  brackets incoming handler entry and completion. `src/app_state.rs` and
  `src/start.rs` bind the receiver epoch to its source lifetime; `src/lib.rs`
  exports plain passive observations. Heartbeat and data appends both count.
  Source, term or endpoint changes reset gaps; stages have independent keys.
  These are not physical sends, delivery, network RTT or authority. Tests:
  `append_timing::tests::gaps_include_heartbeats_and_data_and_reset_on_identity_changes`
  and `append_timing::tests::registry_and_completion_latency_are_bounded_observations`.

**Row 26 upstream receipt, 2026-10-13:** [PR 374](https://github.com/sebadob/hiqlite/pull/374)
was accepted on 2026-10-05 as
[`80a3c2c60c920917d8e7f775557030cf515fe331`](https://github.com/sebadob/hiqlite/commit/80a3c2c60c920917d8e7f775557030cf515fe331).
The released [v0.15.2 listener source](https://github.com/sebadob/hiqlite/blob/18e3e71be1b40465f82e1f26f17b7d1bfcc6995d/hiqlite/src/start.rs)
uses `ConfiguredStreamAcceptor` before TLS and `tap_io(configure_tcp_stream)`
on plain streams for both Raft and API listeners. Its
[socket helper](https://github.com/sebadob/hiqlite/blob/18e3e71be1b40465f82e1f26f17b7d1bfcc6995d/hiqlite/src/network/tcp_socket.rs)
calls `set_nodelay(true)` on the accepted stream. This verifies the generic
mechanism and its accepted/released disposition, not a new upstream runtime
test, full fork equivalence or an upgraded Plurx dependency. The local patch
remains; the removal rule still requires a release Plurx has upgraded to.

**Partial upstream receipt, 2026-10-03:** row 1's local-node selection by
durable id is accepted in [upstream PR 368](https://github.com/sebadob/hiqlite/pull/368),
merged September 24 as
[`9824bcfcf186a10c747bba5960678f8b1589e9f8`](https://github.com/sebadob/hiqlite/commit/9824bcfcf186a10c747bba5960678f8b1589e9f8).
The [observed immutable configuration source](https://github.com/sebadob/hiqlite/blob/e0a6a8e9bdde7afb97156eef13a6e93574324feb/hiqlite/src/config.rs#L461)
checks local-id membership but does not reject duplicate durable ids. The
combined row therefore stays `pending M6`: duplicate rejection still needs
an exact submission/disposition, and its unchanged drop condition is not met.
This is read-only source evidence, not a newly executed upstream reproduction
or permission to upgrade or remove the patch.

**M1 ledger continuation, 2026-10-08:** row 25 owns the transport surface and
its existing plumbing previously disclosed as unledgered on October 4.
`PLURX-FILES.toml` now classifies its defining source as `patched`; the
`unledgered` table is empty. This is metadata reconciliation, not a new runtime
acceptance result, upstream disposition or permission to remove a patch.

**Row 19 constructor mechanism receipt, 2026-10-08:** the unchanged
[current public constructor at `18e3e71be1b40465f82e1f26f17b7d1bfcc6995d`](https://github.com/sebadob/hiqlite/blob/18e3e71be1b40465f82e1f26f17b7d1bfcc6995d/hiqlite/src/http_client.rs)
returned in one fresh-process source-inclusion toy, without any application
provider installation. Actual Reqwest 0.13.5 / Rustls 0.23.45 complete feature
arrays matched the ordinary public sqlite/cache metadata; the toy kept
`webpki-roots` off. Reqwest's declared AWS-LC fallback worked while the
process-wide default remained absent before and after. No HTTP request or
whole Hiqlite node/client was run. Exact terminal SHA256
`2ced5d568b1d3c41f1257048ae42371ea95bcb1e6903e196a87c487faa8c9e0d`,
source/lock/binary provenance and the distinct earlier zero-runtime setup
failure are recorded in the [K-08 plan §5.7](../../docs/cluster/HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md#57-m6--upstream).
The delegated owner therefore does not file a generic default-provider panic
against this tested ordinary graph. This does not prove historical 0.14,
Plurx's private ring-only policy, all configurations, end-to-end TLS, an
upstream maintainer disposition or the whole drop condition. The original
patch description remains the historical/local contract; row 19 stays
`generic bug` / `pending M6`, and all kinds, counts and removal rules remain.

- The SQLite writer no longer panics when the requester of a reply has
  stopped waiting. Upstream sends every reply with
  `.expect("oneshot tx to never be dropped")` (thirteen sites across
  `Execute`, `ExecuteReturning`, `Transaction` and `Batch`) or `.unwrap()`
  (`Migrate`, the metadata read, the membership update and the RTT probe),
  and `.expect`s the shutdown acknowledgement. Every applying request comes
  from the state machine's `apply`, which waits for its reply without a
  bound, so a receiver is gone only when that Raft apply was cancelled or
  torn down (the Raft core stopping, the runtime shutting down, a startup
  cleanup aborting); no caller timeout reaches the writer. The reply then
  panicked the writer thread: every later write on that node failed until
  the process restarted, and the panic skipped the metadata persist and the
  lock-file removal, so the next start rebuilt the database. All applying
  replies now go through `send_reply`, which tolerates a closed receiver,
  logs it at `warn` with the request kind, the applied log index and the
  abandoned result's error text if it was one (several arms report a failed
  statement, commit or migration only in the reply), and counts it by
  request and outcome in `db_writer_abandoned_replies`, which plurxd renders
  as `plurx_hiqlite_writer_abandoned_replies_total`. The metadata read
  applies nothing and stays at `debug`, uncounted. `publish_then_send`
  returns nothing, so no arm can make delivery a condition of the writer
  staying alive. Nothing else changes: the commit or rollback has already
  happened when the reply is sent and stays authoritative, the applied mark
  is still published before the reply, and nothing is retried. Real-writer
  tests abandon a transaction's success and constraint-error replies
  (`dropped_transaction_success_reply_preserves_commit_and_writer`,
  `dropped_transaction_error_reply_preserves_rollback_and_writer`); the
  success and common error replies of `Execute`, `ExecuteReturning` and
  `Batch`, a successful `Migrate`, the membership update, the applied
  marker, the RTT probe and the metadata read
  (`dropped_reply_on_other_arms_preserves_the_write_and_writer`); a failed
  migration and a failed commit
  (`dropped_migrate_and_commit_failure_replies_preserve_rollback_and_writer`);
  and the shutdown acknowledgement
  (`dropped_shutdown_reply_still_persists_metadata_and_releases_the_lock`).
  Each reads the rows and the applied-log metadata back from the database
  file. Parameter-bind errors, the `ExecuteReturning` column-mapping error, a
  failed transaction open, `SnapshotApply` and `Backup` are held only by the
  source pin in `every_applying_reply_publishes_first`. The same repair in
  the WAL writer is `hiqlite-wal` row 7. First written for the transaction
  arm in `f39353af2` and `8e2a8cb64` (#1261).
  Files: `src/store/state_machine/sqlite/writer.rs`, `src/lib.rs`.
- `NodeConfig::listeners` takes Raft and API listeners the caller already
  bound. Upstream binds `listen_addr_*` with the node's roster port inside
  `start_node`, so a caller that chooses a port first has to drop its socket
  and hope nothing else takes the port before the vendor binds it — and on a
  busy test host something does (hosted run 8904 lost `127.0.0.1:36959` to
  another socket). With the field set, startup serves on those sockets and
  never binds the address itself; it refuses a listener whose address is not
  the one it would have bound, so a socket cannot serve a port the roster
  does not advertise. Plurx's test fixtures own their cluster sockets and
  lend each voter generation a duplicate; production leaves it `None`,
  which keeps upstream's bind. Unit tests:
  `an_owned_listener_is_served_without_binding_its_address_again`,
  `an_owned_listener_on_another_address_is_refused`.
  Files: `src/config.rs`, `src/config_toml.rs`, `src/start.rs`, `src/lib.rs`.
- Raft pre-vote (C4, `docs/cluster/C4-PREVOTE-BUILD.md`) travels on the
  existing authenticated Raft stream. `PreVoteDB` and `PreVoteCache` are
  appended after every deployed `RaftStreamRequest` ordinal, and their answers
  after every deployed `RaftStreamResponsePayload` ordinal. An older server
  closes the whole stream, appends included, on a request variant it cannot
  decode. So each Raft stream server stamps `x-hiqlite-raft-prevote: v1` on
  its WebSocket upgrade response, and the client's connection task writes a
  pre-vote only on a socket whose response carried it. On any other socket it
  answers locally with `PreVoteUnsupported`, which is never written to a
  socket. OpenRaft counts that answer as a grant, so an older peer never
  blocks an election. A reconnect renegotiates, and a connection failure is
  no grant. `try_connect` keeps its signature for the API client;
  `try_connect_negotiated` also returns the flag. The OpenRaft half is
  `vendor/openraft/PLURX-PATCH.md`.
  `Cargo.toml` patches `openraft` to that vendored copy, as it does
  `s3-simple`, so the fork's own lock and test lanes build the patched engine
  for both itself and `hiqlite-wal`.
  Files: `src/network/raft_server.rs`, `src/network/raft_client.rs`, `src/network/web_socket_connect.rs`, `Cargo.toml`, `Cargo.lock`.

Remove this vendor when both halves of its exit hold. First, the rows an
upstream release can retire (rows 1, 8, 9, 10, 11, 17, 18, 19, 26, 28, 32 and 34: the
`generic bug` and `dependency-only` kinds) have met their drop conditions in
releases Plurx has upgraded to. Second, none of the `plurx policy` rows
(rows 2, 3, 4, 5, 6, 7, 12, 13, 14, 15, 16, 20, 21, 22, 23, 24, 25, 27, 29, 30, 31, 33, 35, 36 and 37) still needs a patch. Their
drop condition is `Never` by design (ARCHITECTURE §7 decision 10), so no
upstream release retires them on its own: a policy row leaves only when
upstream offers a way to express it without patching this source, or when the
owner decides Plurx no longer needs it, recorded as a change to that row. Until
both halves hold this vendor stays. Meanwhile, the sparse-roster regression in
`crates/plurx-core/src/cluster/migration.rs` keeps the first patch load-bearing,
and the snapshot RPC error-boundary plus queue-saturated reset tests above keep
the transport-recovery patches load-bearing.

Cargo records this package as path-sourced, which means cargo-audit skips it.
The weekly `rust-audit.yml` job uses `scripts/vendor-audit-lock` to restore this
exact release's registry source and checksum before a second advisory scan.
That check must remain until this directory is removed.

Source: <https://crates.io/crates/hiqlite/0.14.0>

The `validation-test-helpers` feature adds process-local apply-pause and
transport-partition controls used only by Plurx's separate-process acceptance
harness. Production binaries do not enable or compile those controls.

The README, upstream tests, and static assets remain provenance rather than an
in-place test suite. `Cargo.lock` is the deliberate exception: Plurx regenerates
it to pin OpenRaft 0.9.25 and its resolver-selected transitive dependencies, so
the focused vendor lane exercises the snapshot reset semantics actually
shipped by the application. The workspace excludes this directory
deliberately.

## 35. Passive DB append timing observations

`append_timing.rs` records at most64 local-node/remote/stage summaries. Outgoing observations start at RaftNetwork DB AppendEntries API entry; incoming observations bracket the existing server append handler. Empty heartbeats and data appends both count. These are RPC-attempt/handler gaps and handler latency, not physical socket writes, network delivery, RTT or a proof of scheduler starvation. Pool/AppState source-instance epochs, leader terms and endpoint fingerprints reset histories; old sources/terms cannot overwrite newer observations. This is bounded generic observability consumed by Plurx, not a new upstream dependency or a change to pooling, clock capture, RPC budgets or election policy. Poisoned or evicted observation state changes no Raft result.
