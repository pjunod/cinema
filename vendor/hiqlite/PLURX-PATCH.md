# Vendored Hiqlite 0.14.0

This directory is the crates.io `hiqlite` 0.14.0 package, licensed under
Apache-2.0. Plurx carries twenty compatibility patches for clustered
deployments:

**Owner:** Paul Junod (repository owner). `pending M6` means the generic fix
still needs a public upstream issue or pull request; it is deliberately not a
made-up URL and prevents the fork from being declared fully tracked.

| # | Patch | Kind | Upstream | Drop condition |
|---:|---|---|---|---|
| 1 | `NodeConfig` duplicate-id rejection | generic bug | pending M6 | Upstream release selects by id and rejects duplicate durable ids. |
| 2 | Split-brain probe TLS policy | plurx policy | — | Never; self-signed cluster TLS is a Plurx deployment requirement. |
| 3 | Concurrent TLS key publication | plurx policy | — | Never; shared embedded-node startup is a Plurx lifecycle requirement. |
| 4 | OpenRaft election trigger | plurx policy | — | Never; graceful voter removal depends on this OpenRaft 0.9 bridge. |
| 5 | Local Raft metrics wrapper | plurx policy | — | Never; Plurx readiness owns this deliberately bounded view. |
| 6 | Quorum watermark wrapper | plurx policy | — | Never; bounded local reads require this proof shape and version. |
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
| 20 | Report the committed Raft log index of a write (`WriteAck`) | plurx policy | — | Never; Plurx's watch-state read-your-write fence (K-04 M2) requires this negotiated response shape. |

- `NodeConfig` selects the local node by `Node::id` and rejects duplicate ids.
  Raft ids are durable identities, so a roster such as `1, 3` is valid when an
  unredeemed join token still holds id 2. Padding that roster with a duplicate
  node creates duplicate connection targets and eventually exhausts file
  descriptors.
- The split-brain probe uses Hiqlite's configured HTTP client and API TLS
  verification policy. Auto-generated certificates are intentionally
  self-signed, so the default Reqwest client otherwise reports
  `UnknownIssuer` on every probe.
- Concurrent embedded-node starts share the first auto-generated TLS key
  without panicking when more than one task reaches the process-wide key's
  one-time publication boundary.
- The authenticated cluster API exposes OpenRaft 0.9's election trigger on a
  selected voter. That release has no dedicated leader-transfer operation;
  Plurx uses the trigger to elect a successor before a leader commits its own
  graceful removal.
- `Client::local_db_raft_metrics` exposes a synchronous local-only wrapper for
  OpenRaft's metrics watch. Remote clients fail immediately instead of falling
  back to management HTTP, and the copied snapshot omits membership, addresses,
  replication maps, and quorum-ack state so passive application metrics cannot
  be mistaken for a quorum-confirmed watermark.
- `Client::db_quorum_watermark` asks the current database leader to run
  OpenRaft's quorum-backed linearizable-read proof and returns only
  `(term, leader_id, committed_index, local_read_protocol_version)`. Followers
  use the existing authenticated leader stream; callers remain responsible for
  a monotonic lease and matching the proof to their local Raft observation. A
  P3a leader's three-column response remains a valid Authority/readiness proof
  but maps to protocol `0`, so bounded local reads stay closed during rollout.
- The SQLite snapshot builder and installer publish process-local, lock-free
  duration histograms through `Client::local_db_snapshot_metrics`. Explicit
  RAII start/finish hooks classify build/install success and every error or
  cancelled exit without polling storage or exposing paths and snapshot ids.
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
- Every Raft, cluster-API, proxy, and authentication WebSocket frame is flushed
  before its writer waits for more work. One 30-second budget covers the write
  and flush together; errors and expiry terminate the writer and wake the
  connection supervisor even while its reader remains live. Graceful Close
  frames use a separate 250-millisecond best-effort allowance, and the server
  challenge/response exchange remains inside the existing five-second
  connection deadline. The real-TLS frame regressions hold the ciphertext tail
  of both client and server writes, including 3 MiB frames, and prove that the
  same socket completes after backpressure clears.
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
- A dropped OpenRaft RPC signals its WebSocket manager through a dedicated
  retained notification instead of best-effort enqueueing `Reset` behind the
  request itself. The old one-slot queue could be full at the hard deadline,
  lose the reset, and leave a leader permanently waiting on the abandoned
  connection after a follower had installed its snapshot. The
  `handler_coordinator_consumes_retained_reset_when_request_queue_is_full`
  regression keeps cancellation independent of request-queue pressure.
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
- The replicated SQLite write enum always includes `QueryWrite::Backup`, even
  when the local backup implementation is not compiled, and appends that
  reservation after the deployed `QueryWrite::RTT` variant. `RTT` therefore
  remains ordinal 5 in both directions of a rolling deployment; reserving a
  new variant before it would itself be a wire break. `backup` and `backup,s3`
  remain independently compilable, and a build without backup returns an
  explicit feature error if it receives the reserved replicated variant. The S3
  half of that change is its own patch, described in the next bullet.
- The optional `cryptr` dependency no longer enables its S3 client
  unconditionally. Hiqlite's `s3` feature enables `cryptr/s3` instead, so
  downstream users that select `backup` or `s3` retain the same backend while
  builds such as Plurx that select neither do not compile an unused S3, QUIC,
  and second aws-lc stack.
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

Remove this vendor when an upstream Hiqlite release contains all twenty
patches and Plurx has upgraded to it. Until then, the sparse-roster regression in
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
