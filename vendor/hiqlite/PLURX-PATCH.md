# Vendored Hiqlite 0.14.0

This directory is the crates.io `hiqlite` 0.14.0 package, licensed under
Apache-2.0. Plurx carries nine compatibility patches for clustered deployments:

- `NodeConfig` selects the local node by `Node::id` and rejects duplicate ids.
  Raft ids are durable identities, so a roster such as `1, 3` is valid when an
  unredeemed join token still holds id 2. Padding that roster with a duplicate
  node creates duplicate connection targets and eventually exhausts file
  descriptors.
- The split-brain probe uses Hiqlite's configured HTTP client and API TLS
  verification policy. Auto-generated certificates are intentionally
  self-signed, so the default Reqwest client otherwise reports
  `UnknownIssuer` on every probe.
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
  OpenRaft's quorum-backed linearizable-read proof and returns only the proof
  term, leader identity, and committed index. Followers use the existing
  authenticated leader stream; callers remain responsible for a monotonic
  lease and matching the proof to their local Raft observation.
- The SQLite snapshot builder and installer publish process-local, lock-free
  duration histograms through `Client::local_db_snapshot_metrics`. Explicit
  RAII start/finish hooks classify build/install success and every error or
  cancelled exit without polling storage or exposing paths and snapshot ids.
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
  and reconnects in a detached two-second recovery budget before using the
  existing single retry. Management clients never follow redirects, so their
  custom API-secret header cannot leave the configured roster. This closes the
  resumed-follower interval in which the local voter is healthy but has not yet
  republished the current leader without allowing raw client calls to hang;
  proxy-mode clients reconnect through the next configured proxy instead of
  escaping that trust boundary.

Remove this vendor when an upstream Hiqlite release contains all nine patches
and Plurx has upgraded to it. Until then, the sparse-roster regression in
`crates/plurx-core/src/cluster/migration.rs` keeps the first patch load-bearing.

Cargo records this package as path-sourced, which means cargo-audit skips it.
The weekly `rust-audit.yml` job uses `scripts/vendor-audit-lock` to restore this
exact release's registry source and checksum before a second advisory scan.
That check must remain until this directory is removed.

Source: <https://crates.io/crates/hiqlite/0.14.0>

The retained lockfile, README, tests, and static assets are upstream
provenance, not an in-place test suite; the workspace excludes this directory
deliberately.
