# Operations — running plurx and reading what it tells you

Companion to [FEATURES.md](FEATURES.md) (what it does) and
[ARCHITECTURE.md](ARCHITECTURE.md) (how it's built) — this is *how to run it day
to day, and what every status, pill, and log line actually means*. For a
copy-paste quickstart see [CHEATSHEET.md](CHEATSHEET.md); for install targets per
platform see [`deploy/README.md`](../deploy/README.md).

The guiding fact for everything below: **paths and hardware are as the server
process sees them, not as you see them.** Most "it found nothing" and "it won't
play" reports are that one gap — a Docker mount, a missing render device — not a
bug.

## Running it

plurx is one static binary, `plurxd`, plus an embedded web app. Three ways to
run it, in order of how most people do:

```bash
# Docker / Compose (recommended for homelabs) — builds from source the first time
cd deploy
cp docker-compose.override.example.yml docker-compose.override.yml   # your mounts + GPU
cd .. && make docker-up      # builds + starts, and stamps the commit so the server can name it

# Bare metal — one binary, needs ffmpeg/ffprobe on PATH (or PLURX_FFMPEG/PLURX_FFPROBE)
plurxd run            # serves :32400

# From source (development)
cargo run -p plurxd   # or: make run
```

Open `http://<host>:32400`, create the admin account, add a library. Library
paths you type in the UI are **container-side** paths under Docker (e.g.
`/media/movies`), which must be mounted in your override file. Full deploy matrix
(Unraid, TrueNAS/k8s, ports, GPU passthrough): [`deploy/README.md`](../deploy/README.md).

### Rolling back a deploy

The SQLite snapshot procedure below applies to a node that has not activated
Hiqlite, including the one SQLite recovery boot after an interrupted import.
When activation fails before atomic rename, run `plurxd run`: it removes the
partial incoming target, leaves `plurx.db` unchanged, and reports the same
command in its error. Once `<PLURX_DATA>/hiqlite/activation.json` exists, the
replicated target is authoritative; do not replace `plurx.db` and assume you
have restored current state.

M6 owns the quorum-aware replicated backup, restore, and older-binary rollback
runbook. Until that work lands, a post-activation rollback means roll forward
with an M2-capable binary against the retained active target. The commands
below are only for the pre-activation SQLite case.

### Upgrading an activated v5, v6, v7, or v8 cluster to v9

Replicated schema v6 added ebook reading state. Schema v7 adds nullable
`author`, `book_work_id`, `book_edition_id`, and `book_metadata_source` item
columns plus the partial work-id index. Schema v8 adds the monotone
`job_leases` coordination table. Schema v9 adds the distributed whole-title
pre-transcode queue and the nullable authoritative manifest digest on cache
locations; it does not change the peer protocol. Stop
application traffic and update every voter as one maintenance operation. The
first v9 daemon that reaches quorum accepts any activation marker from v5 to
the current v9 and applies every missing step in order. Each step commits its
table/columns/index and compatibility row in one Raft transaction before the
daemon starts producers or binds HTTP. A v5 source commits all four additive
steps through v9; v6, v7, and v8 sources start at their corresponding next
step. Each voter atomically rewrites its local `activation.json` to v9 only
after the replicated transactions are visible.

A death before a step leaves the preceding schema authoritative. A death after
the replicated write but before the local marker rewrite replays only the
marker write on the next boot. Do not restart a v5, v6, v7, or v8 binary after v9
commits: its strict compatibility check correctly refuses the newer schema. If
a node fails during the maintenance window, leave the v9 quorum authoritative
and roll that node forward with the same v9-or-newer binary. `reset-password`,
`refresh-metadata`, and other maintenance clients do not own migration and
will refuse until the running daemon has completed it. Replicated v4 has no
supported direct path to v9 and remains refused.

Every Ansible redeploy stops the Plurx Compose stack long enough to copy the
closed SQLite database. The three newest copies stay on each node under
`<PLURX_DATA>/backups/`, named
`plurx.db.predeploy-<UTC>-<last-good-sha>.bak`. The SHA in the filename names
the code that was running when the snapshot was taken.

**On an activated node those snapshots are not restore points.** The deploy
still copies `plurx.db`, but after activation that file is frozen at the moment
of import, so each new snapshot is another copy of the same pre-activation
state — the redeploy captures nothing written since. Restoring one does not
roll the node back; it only makes a stale database sit beside the authoritative
target, and `plurxd` refuses to import it precisely so that mistake cannot pass
silently (see the refusal below). Capturing current replicated state is M6's
quorum-aware backup work and does not exist yet; until it does, treat
`<PLURX_DATA>/hiqlite/` itself as the thing to copy while the daemon is stopped.

`plurxd` records `<PLURX_DATA>/hiqlite-activated.json` when it activates. If the
replicated target is missing while that file is present, startup refuses rather
than re-importing `plurx.db`, and names both the file and the loss accepting it
would cause. Deleting that file is the deliberate way to accept a rollback to
the pre-activation database, discarding everything written since activation.

An older binary deliberately refuses a database migrated by a newer binary.
Rolling back code alone therefore leaves the service crash-looping. Restore
the matching pre-deploy database before rebuilding the older revision:

```bash
cd /path/to/plurx/deploy

# Resolve the host path from the running container. Do not assume /srv/plurx;
# deploy/.env may move it.
data_dir="$(docker inspect plurxd --format \
  '{{range .Mounts}}{{if eq .Destination "/var/lib/plurx"}}{{.Source}}{{end}}{{end}}')"
test -n "$data_dir" && test "${data_dir#/}" != "$data_dir"

# Stop first. Preserve the failed newer database for a possible roll-forward,
# then restore the snapshot and remove sidecars that belong to the failed DB.
docker compose stop
cp -a "$data_dir/plurx.db" \
  "$data_dir/plurx.db.failed-$(date -u +%Y%m%dT%H%M%SZ)"
cp -a "$data_dir/backups/plurx.db.predeploy-<UTC>-<last-good-sha>.bak" \
  "$data_dir/plurx.db"
rm -f "$data_dir/plurx.db-wal" "$data_dir/plurx.db-shm"

# Rebuild exactly the revision named by the snapshot. The Make target carries
# its identity into the image; a bare Compose build does not.
cd ..
git fetch origin
git switch --detach <last-good-sha>
make docker-up

curl -fsS http://127.0.0.1:32400/healthz
curl -fsS http://127.0.0.1:32400/api/v1/server
```

Do not delete the failed database until the incident is resolved. A subsequent
forward fix may need data written after the snapshot. Return the node to its
normal protected branch only after the fixing PR is merged, then redeploy it
through Ansible so the next pre-deploy snapshot is taken normally.

**`credentials.key` is not in the snapshot.** It lives beside `plurx.db` and is
what decrypts the stored Trakt credential, so back up the two together and
never restore a database onto a node whose key file has been replaced —
startup refuses that combination rather than silently losing the link, naming
both the key it holds and the key the rows want. Restore the file **mode `0600`
and owned by the plurx user**: a group- or world-readable key file is refused on
every boot, not only the one that minted it, because a key anyone on the box can
read is not the node-local key the encryption assumes. `chmod 600` is the whole
fix, and neither refusal touches the database or the key. Rolling
back the *code* to a revision that predates credential encryption is safe in
the direction that matters: the older binary reads the envelope as if it were
the token, Trakt rejects it, and the link is dropped and must be reconnected.
It does not expose the credential, and it does not corrupt the row.

**A SQLite→Hiqlite import refuses a backup taken before credential
encryption.** The message names `trakt_auth` and how many rows are unsealed,
and the fix is one boot: start this build on the SQLite install, which seals
those rows in place, then take the backup again and re-run the import. Nobody
has to reconnect the Trakt account. The refusal happens before any row is
submitted, so a refused import leaves no partial replicated state to clean up.

### M2 activation and crash recovery

The first `plurxd run` against a legacy data directory imports into
`hiqlite.incoming`, verifies every durable table, publishes the fsynced marker,
and atomically renames that directory to `hiqlite`. Producers and the public
HTTP listener do not exist until this sequence finishes.

`PLURX_CLUSTER_ACTIVATION_FAILPOINT` is the process-exit test surface for those
durability boundaries:

| Value | Boundary after which the process exits |
|---|---|
| `after-quiescence` | No producer or listener has started |
| `after-incoming` | The fresh incoming voter exists |
| `after-marker` | The verified completion marker is fsynced |
| `after-rename` | The completed target has been atomically renamed |

Each injection exits with code `86`. Before rename, the next `plurxd run`
removes incoming state and consumes one unchanged-SQLite recovery boot; the
following restart retries activation. After rename, the completed target wins.
The variable is evaluated only while activation is pending, so a stale value
cannot take an already-activated node offline.

**Stopping the daemon while it is still starting.** SIGTERM and SIGINT are
answered from the first moment of startup, not only once the server is
listening, so `docker stop` during hardware probing or listener setup exits
cleanly without ever becoming reachable. The one exception is the import and
activation sequence above: it renames directories and fsyncs markers in a fixed
order and is never interrupted partway, so a signal arriving inside it takes
effect at the boundary immediately after. Give a container a stop grace period
longer than one activation — that sequence is bounded by library size, and a
grace period shorter than it means the runtime kills the process mid-activation.
That is recoverable rather than damaging: the next boot consumes one
unchanged-SQLite recovery boot and retries, exactly as for the failpoints above.

After activation, an ungraceful death may leave Hiqlite's state-machine lock.
Left to itself, Hiqlite would delete the state-machine database and rebuild it
by replaying the Raft log — which reconstructs nothing on an activated node,
because activation imports the operator's SQLite data directly into that
database rather than through Raft, and nothing snapshots before 10,000 log
entries. Startup therefore repairs a sole voter first, while the database is
still intact: the committed state machine is preserved and only the one-voter
Raft metadata is rebuilt around it, keeping acknowledged state newer than the
frozen SQLite source. A sole voter acknowledges a write only after applying it,
so nothing a client was told was durable is in the discarded log tail. A node
that has admitted a peer is left to Hiqlite instead, whose catch-up from the
leader is the correct recovery there and does not fork membership. Never delete
the lock by hand: unlinking it skips this repair and hands an activated node to
the rebuild that cannot restore it. If the active target itself is missing,
startup follows the lost-target refusal in
[Rolling back a deploy](#rolling-back-a-deploy) rather than importing stale
SQLite.

**One daemon per data directory.** Separately from that state-machine lock,
startup takes an advisory lock on `.plurxd.lock` in the data directory before
anything else and holds it for the life of the process, then records its own
process id in the file. Two servers pointed at one data directory is data loss,
so the second one refuses to start.

That lock is released by closing the handle, which is the last thing a
departing server does, so a restart can genuinely arrive before its predecessor
has finished leaving — `Restart=always`, `systemctl restart`, and a container
recreated under its old volume all do this. Startup therefore re-attempts for
five seconds before refusing. A quiet host never spends that time, because the
first attempt succeeds; a second live server is refused just the same, because
a running owner holds the lock for its whole lifetime and is still holding it
when the window ends. The refusal never means "busy at this instant" — it means
held continuously for five seconds.

Two refusals come out of that path and they are not variants of one problem:

| Refusal | What it means | What to do |
|---|---|---|
| `another plurxd process already owns the data directory <dir> (pid N)` | A different live process owns it. This is the double-start the lock exists to stop. The recorded pid is a best-effort diagnostic and can be stale; only the advisory lock proves that another owner exists. | Use `ps -p N` as a lead for finding the other server, then confirm which process has the data directory open before stopping it; otherwise give one server its own data directory. An unavailable pid reads `(owner pid not recorded)` and means the same ownership conflict. |
| `the data directory <dir> is still locked inside this plurxd process (pid N)` | This process never dropped an earlier activation's lock handle. | Nothing on the host will help — no second server exists. Report it with the log around startup; it is a defect in this code path. |

### Reading watch-state replication status

Open **Settings → System** and read **Watch state** in the Server card's
**Right now** row. The same projection is in the admin-only
`GET /api/v1/system` response under `replication`; it contains only backend and
Raft progress facts, never users, titles, media paths, tokens, or library data.

| Surface | What it means | What to do |
|---|---|---|
| `SQLite single-node` | This boot is using unreplicated SQLite. A pause or watched flag is stored only on this server; calling it "synced" would be false because there is no peer. | If this is the recovery boot after an interrupted activation, restart once the cause is fixed so activation can retry. |
| `Replicated one node` | Hiqlite is authoritative and this node has applied every known entry, but no second voter exists yet. The backend is ready for M3 membership; it is not HA by itself. | Nothing for replication. Add or remove nodes from **Settings → Cluster**. |
| `Replicated in sync` | This node has applied its latest known entry. On the leader, every reporting peer has matched that point; on a follower, only this node's catch-up is confirmed and the explanation points you to the leader for peer status. | No action. Read the plain-language explanation before treating a follower's local catch-up as a cluster-wide all-clear. |
| `Replicated DEGRADED` with two voters | Both voters are required for every quorum, so one failure stops writes and membership changes. This is a reconfiguration waypoint, never HA. | Add a third voter from **Settings → Cluster**, which reports the same state as a reconfiguration in progress. Do not stop either voter until three are present and in sync. |
| `Replicated DEGRADED` | This node has unapplied entries, a leader cannot be confirmed, or a reporting peer is behind or missing. Watch state is durable once quorum-acknowledged, but it may not be visible from every node yet. | Keep the available nodes online and check the last observed in-sync time. If the gap does not fall, inspect the logs before restarting anything. |

Every count in this table is a count of **voters**. A learner replicates like
any other member and appears in the roster, but it is not a voter, so it moves
none of these states: a one-voter install with a learner is still
`Replicated one node`, and a three-voter cluster with a learner is still three.

**How to read the numbers:** `applied term T, index I` is this node's latest
applied durable entry. `N changes behind` is the largest gap in the current
metrics, or a conservative worst-case upper bound when a peer stops reporting.
That bound stays anchored to the last observed convergence index and grows as
this node applies new writes. The missing peer may have received entries after
the last observation, so the inferred number can overstate its lag; it is not
proof of the peer's exact position.

**Last observed in sync** is process-local observation time, not a fabricated
Raft timestamp: a degraded sample preserves the last positive observation
instead of replacing it with "now." It is absent after a restart until the
endpoint observes an in-sync sample. The Settings page samples this when you
open it; the timestamp does not claim that an unseen convergence happened
between visits.

This row is status, not membership control. **Settings → Cluster** is the
membership surface, and the admin-only cluster endpoints below are the same
operations from a terminal. Their `replication` member is this exact projection
rather than a second answer for lag.

### The Cluster tab

**Settings → Cluster** does everything the endpoints below do, and it is the
easier path when you have a browser open. It is admin-only, matching the
endpoints' own gating; a non-admin never sees the tab or its data. The roster is
read when you open the tab, not on every Settings visit.

| What you see | What it means |
|---|---|
| `Not clustered` | This server is on unreplicated SQLite and has no membership. Normal and complete for a single machine; there is nothing to fix and no join control to press. |
| `One node` | Replicated, one voter. A supported configuration, not a half-built cluster. |
| `Reconfiguration in progress — not redundant` | Two voters. Both machines are required for every write and every membership change, so this survives no failure — read the same warning in the table above. Add a third node. |
| `Redundant — N voters` | Three or more voters. The panel names the majority required and how many nodes may be down. |
| `… is a learner` appended to any of the above | One or more admitted non-voting members. The sentence is appended, never substituted: a learner changes none of the quorum arithmetic in front of it. |

The node table leads with each machine's short OS hostname, labels the current
leader beside it, then shows the advertised host and stable node id underneath.
The advertised host may be a DNS name or IP; loopback is written `localhost`
instead of `127.0.0.1`, and listener ports stay private. A native daemon reads
the hostname from the OS. A container should set `PLURX_NODE_HOSTNAME` to the
Docker host's `hostname -s`, because its own OS hostname is normally a generated
container id. `GET /api/v1/cluster/nodes` also exposes the Raft id,
voter/learner role, heartbeat freshness, and last-seen. A fresh heartbeat is a
recent committed application heartbeat, not a direct socket probe. The role is
the durable one the node was admitted under, not a phase of joining; a learner
keeps it for as long as it is a member. Read the nested replication status for
leader and apply-lag health. Media paths and token material are not in that
payload and are not shown.

The **Cluster log** under the roster holds membership, Hiqlite, and Raft events
in its own 2,000-line process-local ring. Those events do not consume the
general Settings → System log ring; cluster warnings and errors still reach
stdout/journald so a startup failure remains visible without the web UI.

**Add a node** mints one token through the role's distinct endpoint: voters use
`POST /api/v1/cluster/join-tokens`, while read workers use
`POST /api/v1/cluster/learner-join-tokens`. The panel displays it exactly once,
with a lifetime you pick between 10 minutes and 1 hour. plurx keeps only its
digest, so the browser is the only copy: the panel never writes it to browser
storage, a URL, or a log, and it is dropped when you leave the tab. Everything
after that — the owner-only file, `join_token_file`, the fresh data directory —
is the terminal procedure below, unchanged. Treat the displayed token exactly
as the runbook treats the `curl` response: anyone holding it can join a node to
this cluster until it is redeemed or expires.

**Remove** calls the removal endpoint and renders its refusal as a sentence with
a next step rather than a code. `node_owns_offline_work` tells you to let active
transfers finish (or delete those packages), stop clients from creating new
downloads on that node, and retry; ordinary queued work is resolved by the
server as described below. The confirmation states what the terminal path
states below — the removed machine's data directory is tombstoned, and
rejoining means discarding it.

### Inspecting a stopped voter without changing it

Use `plurx-cluster-check inspect-wal` when a voter reports a missing log index,
fails immediately after snapshot installation, or will not open its cluster
listener. The production image includes this binary so the host needs Docker,
not a Rust toolchain. It reads and bounds-checks WAL files without mapping them
writable, opens standalone SQLite files immutable, contacts no peers, emits no
application rows or raw Raft payloads, and refuses a WAL directory whose lock
is live. If a crashed state machine still has a SQLite `-wal` sidecar, the tool
copies only that database pair to private temporary space so SQLite can include
the committed sidecar without writing to the evidence directory.

Stop and preserve the voter before inspecting it. A clean report does not make
the copy disposable: it proves only that physical Raft boundaries decode and
line up, not that every application row is semantically correct.

```bash
cd /path/to/plurx/deploy

# Resolve the exact image and host data path before stopping the voter.
image="$(docker inspect plurxd --format '{{.Config.Image}}')"
data_dir="$(docker inspect plurxd --format \
  '{{range .Mounts}}{{if eq .Destination "/var/lib/plurx"}}{{.Source}}{{end}}{{end}}')"
test -n "$image" && test -n "$data_dir" && test "${data_dir#/}" != "$data_dir"

# Stop one voter only, then make an evidence copy before changing membership or files.
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
docker compose stop plurxd
cp -a "$data_dir" "$data_dir.forensic-$stamp"

# Run the inspector from the same image, with the evidence mounted read-only.
docker run --rm --read-only \
  --tmpfs /tmp:rw,noexec,nosuid,size=2g \
  -v "$data_dir.forensic-$stamp:/forensic:ro" \
  --entrypoint plurx-cluster-check "$image" \
  inspect-wal --hiqlite-dir /forensic/hiqlite --output - \
  > "wal-inspection-$stamp.json"
jq . "wal-inspection-$stamp.json"
```

The private `/tmp` mount is used only when a crashed SQLite `-wal` sidecar is
present. Size it above the copied `plurx.db` plus sidecar on unusually large
catalogues; the source evidence remains mounted read-only.

For a source checkout, the equivalent command is:

```bash
cargo run --locked -p plurx-cluster-check -- \
  inspect-wal --hiqlite-dir /path/to/forensic/hiqlite \
  --output target/validation/wal-inspection.json
```

**How to read it:** the useful comparison is purge boundary → snapshot/local
applied boundary → first retained WAL entry. A healthy compacted state normally
has snapshot and local applied equal to `last_purged_log_id`, followed by a WAL
entry at the next index.

| Verdict or observation | Meaning | Next action |
|---|---|---|
| `clean` | Metadata CRC, retained WAL sequence, snapshot, and local applied boundaries agree. | Preserve the copy and investigate process-memory or transport failures; do not call the disk corrupt. |
| `wal_number_one_reused_above_initial_range` | Snapshot installation purged the old generation and reused WAL number 1 at a high index. This is an observation, not damage. | Current builds replace reader mmaps and memos when the WAL incarnation changes. On an older build, this condition plus `LogIndexNotFound` identifies the fixed stale-reader defect. |
| `metadata_corrupt` or `wal_missing` | Required physical state is absent or its metadata envelope cannot be trusted. | Keep the voter stopped. Recover by rejoining it from a healthy quorum; do not fabricate metadata by hand. |
| `wal_file_gap`, `retained_gap`, `snapshot_wal_gap` | At least one log index needed between the snapshot and retained WAL is physically unaccounted for. | Keep the evidence and rejoin from a healthy quorum. |
| `wal_header_payload_mismatch` | A WAL header's claimed boundary differs from its first or last decodable record. | Treat the local WAL as damaged and rejoin; keep the copy for a defect report. |
| `metadata_behind_wal`, `metadata_ahead_of_wal`, `missing_purge_boundary` | Purge metadata and retained WAL disagree. | Do not edit `meta.hql`; rejoin from a healthy quorum and attach the JSON report. |
| `snapshot_behind_purge_boundary` or `local_state_machine_behind_purge_boundary` | The state machine cannot account for entries already declared purged. | Keep the node stopped and rejoin it. |

An unreadable header, impossible record length, CRC failure, non-regular input,
or SQLite recovery failure makes the command exit nonzero and name the failed
boundary on stderr rather than emitting a reassuring partial report. Keep the
node stopped and preserve the evidence in that case.

The tool does not remove a voter, mint a join token, delete a data directory,
or start recovery automatically. Those are separate membership decisions: an
inspector should never turn ambiguous evidence into an irreversible action.

### Joining and removing voters

The rest of this section is the terminal path. It is the same three endpoints
the Cluster tab drives, and is the right path for a scripted or headless setup.

Use one voter for a deliberately non-HA install and three voters for ordinary
HA. Two voters are useful only while adding or removing a node: they require
both processes for every write and survive no failure. Four voters still
tolerate only one failure, but every write now needs three acknowledgements
instead of two; it adds replica work without adding failure tolerance. Use five
only when surviving two simultaneous voter losses is an explicit requirement
and its measured write cost is acceptable. Removing a fourth voter is always
an operator decision through the safe membership API, never an automated
"performance" action.

Every number above counts voters. Admitting a learner adds a replica, not a
vote: it changes no majority, adds no failure tolerance, and is not a substitute
for the third voter this section asks for.

`make cluster-check` now creates
`target/validation/cluster-topology-semantic.json`. It starts fresh independent
three- and four-process clusters, sends the same 64 quorum-acknowledged setting
writes to each elected leader, and records raw controller-to-node acknowledged
write round trips in microseconds, type-7 p50/p95/p99 values, quorum size,
physical Raft-entry count, the stable leader term, and every voter's applied
index. Every voter locally reads back the deterministic 64-row/4,096-byte
payload and must match the expected corpus digest. Every voter in that
artifact is a voter: the topology comparison admits no learner, so its
quorum-size and applied-index fields say nothing about one. `make cluster-check`
separately runs a three-voter-plus-learner scenario, described under *Admitting
a learner* below. The round trip includes harness IPC and scheduling; it is not
an internal Raft commit timer. This is
deterministic semantic CI evidence: resource fields are explicitly null and
the artifact cannot support a hardware or absolute-latency claim. A
counterbalanced semantic run can be requested with
`cargo run -p plurx-cluster-check -- topology <output.json> 4,3`; P0c's named
runner wraps the same schema with isolated load generation and real per-node
resource counters. The portable Draft 2020-12 schema enforces shape, topology,
and evidence-scope resource fields; `validate_topology_artifact` remains the
canonical check for cross-field hashes, recomputed percentiles, timestamps,
applied lag, and leader/term semantics that JSON Schema cannot express.

### The cluster protocol range and learner-protocol activation

Every voter carries two protocol numbers: the range its binary implements, and
the range the cluster is actively using. The cluster's range lives in
`cluster_meta.protocol_min`/`protocol_max`. A node may boot, join, or rejoin
only when its own range covers the cluster's whole active range — not when the
two merely overlap, because the active range names protocols the cluster's
features already depend on.

Binaries from this release implement protocol **4 through 5**. Protocol 5 is
the non-voting learner admission protocol. A cluster bootstrapped or upgraded
onto this release stays on `4..=4`:

- installing this binary requires no operator action and changes nothing;
- a voter still running the previous release keeps booting and keeps joining;
- `protocol_max` is never widened implicitly — nothing but the explicit
  activation below moves the cluster's range.

`GET /api/v1/cluster/nodes` reports the range under `protocol`
(`active_min`, `active_max`, `binary_min`, `binary_max`,
`learner_protocol_active`, and `learner_protocol_pending`), and each roster
entry carries `learner_protocol_ready`. A node is ready only when the binary it
is running *right now* has proven the learner protocol; the proof is written in
the same replicated transaction as the node's heartbeat, so a node that was
upgraded and then rolled back stops being ready on its next heartbeat rather
than keeping a stale claim.

Activate only after every voter reports ready:

```
curl -X POST -H "Authorization: Bearer $ADMIN_TOKEN" \
  https://plurx.example.net/api/v1/cluster/protocol/learner/activate
```

The response is `{"changed": …, "protocol": …}`. `changed: false` with
`learner_protocol_active: true` means the cluster was already activated — the
operation is idempotent, so retrying after a timeout is safe. Refusals are
distinct on purpose:

| Code | HTTP | Meaning |
|---|---|---|
| `learner_protocol_upgrade_required` | 409 | Named nodes are running a binary that has not proven protocol 5. Upgrade each, wait one heartbeat interval, retry. |
| `learner_protocol_node_absent` | 409 | Named members have not heartbeated for over two minutes, so nothing current is known about the binary they would come back running. Start them, or remove them. |
| `join_in_flight` | 409 | Named nodes have redeemed a join token and have not heartbeated yet. Wait for the join to finish, then retry. |
| `cluster_protocol_range_changed` | 409 | The range moved while this call was committing. Re-read `GET /cluster/nodes` and retry. |
| `learner_protocol_in_use` | 409 | Deactivation would strand the named members: those admitted as learners, and those that committed Raft membership currently lists as holding no vote. |
| `cluster_leader_unavailable` | 503 | There is no elected leader to commit the change. Retry after the election. |

Activation asks three separate questions, because "can this cluster speak
protocol 5" has three ways to be false and `learner_protocol_pending` answers
only the first. Each is checked twice — once as a read that names nodes, and
again inside the committing statement, so a leader that won the reads and then
lost a race cannot commit anyway.

**`learner_protocol_pending` is a staleness rule, not a liveness one.** The
capability row carries the heartbeat's own timestamp, and a node is "pending"
when its capability row's timestamp no longer equals its `last_seen_at`. That
is what catches a rollback: an older binary's heartbeat advances `last_seen_at`
without refreshing the proof, so the node reappears in
`learner_protocol_pending` within one interval, and upgrading it again clears it
within one interval too.

**A node that proved protocol 5 and then stopped stays "ready" forever, and is
refused by the absence rule instead.** Both timestamps freeze together when a
node dies, so the equality still holds and the node never appears in
`learner_protocol_pending` — the pending roster alone would activate straight
past a machine that is switched off, and a binary rolled back while that machine
is down never writes the desynchronising heartbeat the rule above leans on.
Activation therefore also refuses, with `learner_protocol_node_absent`, for any
member that is not tombstoned and has not heartbeated in over two minutes
(twelve heartbeat intervals). A node that died running the *previous* release is
named by this rule too, and by this rule first: it is the one whose next step —
start it or remove it — actually ends.

The two exits are the two in that sentence. Bring the node back on a binary from
this release and it clears itself within one heartbeat interval, or remove it
with `DELETE /api/v1/cluster/nodes/{node_id}` first. A node that has already
been removed is tombstoned and is not consulted by any of the three rules.

**A node that is mid-join blocks activation too, and clears itself.** Between
redeeming its join token and its first heartbeat a node is already a committed
member with no capability row at all, which `learner_protocol_pending` is
deliberately blind to — it has not heartbeated, so it is not "behind". Activating
past it would leave a voter that counts toward quorum and can never open its
store again; on a three-to-four growth that takes fault tolerance to zero. So the
commit refuses with `join_in_flight` and names it. A join that finishes clears
this within seconds. A join that was abandoned stops advancing its timestamp and
is named by `learner_protocol_node_absent` instead once it goes stale, so an
interrupted redemption cannot hold activation forever.

**After activation, a binary that only implements protocol 4 can no longer
boot, join, or rejoin this cluster.** Its refusal names the required protocol
and says the binary is too old. Upgrade every voter *before* activating.

### Rolling the learner protocol back

`POST /api/v1/cluster/protocol/learner/deactivate` narrows the range back to
`4..=4`. It is idempotent, and it is refused while any committed member holds no
vote.

The rollback is available in exactly one state: activated, with no learner
admitted. In that state it is a clean reversal — nothing depends on protocol 5,
and the previous release boots, joins, and rejoins again on the next heartbeat.

```
curl -X POST -H "Authorization: Bearer $ADMIN_TOKEN" \
  https://plurx.example.net/api/v1/cluster/protocol/learner/deactivate
```

**Once a learner is admitted, rollback is unavailable until that learner is
removed or promoted.** Deactivation is refused with `learner_protocol_in_use`
and names the member it would strand. Remove the learner with the ordinary
node DELETE, or wait for both readiness proofs and promote it; then confirm the
roster reports `non_voting_replicas: 0` before deactivating protocol 5.

`learner_protocol_in_use` carries two labelled rosters, and they answer
different questions. *Admitted as learners* comes from the durable role column
and names nodes this cluster admitted under protocol 5, including one that is
currently switched off. *Listed as non-voting* comes from committed Raft
membership. An ordinary voter that is mid-join appears in the second for a
moment — the join adds it as a Raft learner before promoting it — and settles on
its own; it is not a learner and must not be removed. Wait for the join to
finish and retry.

An admission interrupted after redemption — a port conflict, a crash, a `^C`
— leaves a `role='learner'` row and **continues to block rollback regardless of
age**. Age is not proof that the authorized process cannot resume. Restart the
joiner to finish admission, or explicitly remove that node id; learner removal
can fence and tombstone an unreachable target without changing voter quorum.

The refusal is decided twice — once as a read that names what is in the way, and
again inside the committing statement — so a learner admitted between the two
still stops the rollback rather than being stranded by it.

### Admitting and using a learner

A learner receives replication and can become a readiness-gated read/media
worker. It never becomes leader or counts toward quorum while its committed
role is non-voting, and it runs none of the cluster's leader-singleton work —
no scheduler, schema migration, provider, scan, or membership-control job.
Quorum size is unchanged by admitting one: three voters plus a learner still
needs two voters to commit and still tolerates one voter failure. Eligibility
for singleton work is re-derived from committed Raft membership on every
decision, not from the role the process booted with.

#### Eligibility and readiness

The learner route matrix is fail-closed. A route not listed here returns `503
learner_route_ineligible` before its handler runs.

| Surface | Learner behavior |
|---|---|
| Liveness, readiness, metrics, app assets, and `GET /api/v1/cluster/nodes` | Allowed. `/readyz` still returns 503 whenever the serving fence has no fresh quorum proof. |
| Native catalogue | GET libraries, library items, item detail, hubs, and home previews only; each bounded read still enforces its quorum/apply watermark. |
| Plex catalogue | GET sections, section contents, and metadata shapes only, under the same bounded-read policy. |
| Node-local media | Exact method-and-route shapes only: GET media bytes/status/playlists/images/subtitles, POST the declared HLS/publication and authenticated internal session starts, and DELETE those node-local sessions. The serving fence and peer proof remain mandatory. PUT audio offsets and every offline-package create/delete/lease/complete mutation are refused. |
| Self leave | `POST /api/v1/cluster/leave`, with the body bound to this backend's `local_node_id`. |
| Authority and mutations | Refused: settings, searches outside the bounded inventory, library/user/API-key mutations, providers, scans, Trakt, scheduler and repair jobs, protocol changes, token issuance, promotion, and remote-node removal. |

`GET /api/v1/cluster/nodes` exposes the proof instead of making an operator
infer it from a green heartbeat:

- `bounded_read_ready` is true only when the target's own passive quorum sample
  is current and its local applied index has zero gap;
- `apply_lag_entries` is that target-local gap;
- `voter_storage_ready` combines a periodically refreshed durable
  create/fsync/remove/directory-fsync probe with at least 512 MiB of current
  filesystem headroom. The probe runs on the blocking pool, publishes its own
  observation time, and expires after 30 seconds;
- `storage_headroom_bytes` is the current unreserved filesystem capacity; and
- `capacity` reports voter count, quorum, voter failure tolerance, non-voting
  replicas, and ready read workers separately.

A stale progress heartbeat immediately clears read-worker readiness and removes
the node from media placement. Learner lag is per-node capacity state; it does
not change the voter replication-health summary or claim that quorum redundancy
is degraded.

Admission is a separate protocol, not a flag on the voter join, and it is
available only after the activation above:

1. Activate the learner protocol (previous section). Before that,
   `POST /api/v1/cluster/learner-join-tokens` is refused with
   `learner_protocol_inactive` — at issuance, so nobody copies a token to
   another machine first.
2. Mint the token on a voter:

```
curl -X POST -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"expires_in_seconds": 600}' \
  https://plurx.example.net/api/v1/cluster/learner-join-tokens
```

   Voter admission stays on `/api/v1/cluster/join-tokens` and mints exactly the
   token it always did. A learner token is prefixed `plxjoin:v2:`; a voter token
   stays `plxjoin:v1:`.
3. Put the token in the new node's `cluster.join_token_file` and start `plurxd`
   on a **fresh data directory**, exactly as for a voter join.

The role is bound to the coordinator's issued-token record, not to anything the
joining node sends, so a joining process cannot ask to be admitted as something
other than what the token was minted for. It is also recorded in replicated
membership and in the node's own `membership.json`, which is written as
**version 2** for a learner. That file is deliberately unreadable by releases
that predate the learner role: such a build has no idea it must not campaign,
so refusing to start is the correct outcome. A voter's `membership.json` stays
version 1 and stays readable by the previous release, so rolling a voter back
remains possible.

Once a learner exists, `.../protocol/learner/deactivate` is refused with
`learner_protocol_in_use` and names it until it is removed or promoted. See
*Promoting or removing a learner* below and *Rolling the learner protocol back*
above.

#### Promoting or removing a learner

Promotion is deliberate and admin-only. Wait until the roster shows both
`bounded_read_ready: true` and `voter_storage_ready: true`, then use Settings →
Cluster → **Promote to voter**, or call:

```bash
curl -fsS -X POST \
  "$PLURX/api/v1/cluster/nodes/$NODE_ID/promote" \
  -H "Authorization: Bearer $PLURX_ADMIN_TOKEN" | jq .
```

The coordinator records a durable intent, obtains a quorum-confirmed commit
barrier, and waits for a later heartbeat from the target to prove its own
applied index crossed that barrier. It then asks Hiqlite for the voter
transition and reconciles ambiguous HTTP outcomes from a quorum of membership
observations. A retry whose outcome is not already committed records a **new**
barrier and requires a new target heartbeat; an old progress row can never
promote an offline learner. After the vote commits, the target atomically
rewrites and fsyncs `membership.json` and `hiqlite/activation.json` as voter
records. Only then does the coordinator publish role `voter` and clear the
intent. A crash between those two local writes resumes the second write on
startup, and the final version-1 membership record remains readable by the
previous release. The process may take singleton work immediately and retains
that authority after restart. Promotion refuses
with `learner_not_ready` for a stale/non-zero-lag proof and
`voter_storage_preflight_failed` when the durability probe or 512 MiB headroom
threshold is not satisfied.

To remove a learner, use the same DELETE as a follower voter:

```bash
curl -fsS -X DELETE \
  "$PLURX/api/v1/cluster/nodes/$NODE_ID" \
  -H "Authorization: Bearer $PLURX_ADMIN_TOKEN" | jq .
```

Learner removal does not apply the voter-quorum-size rule. It first commits a
route/placement/job-owner fence, supersedes active media ownership, waits for a
reachable target to apply that fence, settles node-local work, removes the
non-voting member, and leaves the durable node tombstone. An unreachable learner
can still be removed because it has no vote. A running removed process may
answer liveness and metrics while it drains, but every application route is
refused with `node_removal_fenced`; discard its old data directory before
joining that machine again with a fresh token.

#### A learner is inside the trust boundary

**Admitting a learner is admitting a full member of this cluster's trust
boundary.** The role is a capacity decision, not a security boundary, and this
release does not make it one.

A join token ships `secret_api` whole, before the joining node starts, and that
one credential is simultaneously:

- the Raft **membership-mutation** credential, which authorizes
  `POST /cluster/become_member` and `/cluster/membership` on the leader;
- the full replicated **read/write client** credential; and
- the artwork **HMAC** key.

A learner therefore holds everything it needs to call `become_member` against
the leader and promote itself to a voter, and nothing on the plurx side is
consulted when it does: those routes live in vendored Hiqlite and honour only
`validate_secret`. Every plurx-side refusal in this document — the role bound to
the token record, the `learner_only` startup hint, the job gate — is
defence-in-depth against mistakes, not authorization against a hostile or
compromised node.

In particular, **the cluster job gate is not authorization.** It stops a learner
from duplicating a provider pass or taking a scan lease away from the voters
that should own it. It does not, and cannot, stop a node that holds the shared
credential from doing cluster-wide work by other means.

Give a learner the same trust you give a voter: admit only machines you
administer, and treat a leaked join token as a full cluster compromise, exactly
as for a voter join. Splitting membership mutation onto its own credential is a
separate milestone, designed in
[MEMBERSHIP-CREDENTIAL-SPLIT-PLAN.md](MEMBERSHIP-CREDENTIAL-SPLIT-PLAN.md).

Refusals specific to learner admission and lifecycle:

| Code | HTTP | Meaning |
|---|---|---|
| `learner_protocol_inactive` | 409 | The cluster has not activated protocol 5. Activate it first. |
| `join_incompatible` | 400 | The joining binary does not implement the cluster's whole active protocol range. |
| `join_token_invalid` | 400 | Including a token framing this build does not implement — an older build reads `plxjoin:v2` this way, and refuses before it writes any cluster secret to disk. |
| `learner_not_ready` | 409 | The learner's own heartbeat is stale, behind the quorum commit index, or has not yet published a local apply proof. |
| `voter_storage_preflight_failed` | 409 | The durable-write probe failed or current filesystem headroom is below 512 MiB. |
| `learner_lifecycle_pending` | 409 | A prior promotion/removal may have reached Raft; retry the same operation so the durable intent can reconcile it. |
| `promotion_requires_learner` | 409 | The target is absent, fenced, or already an ordinary voter rather than an admitted learner. |
| `learner_route_ineligible` | 503 | A learner received a route outside the published bounded-read/media matrix. Send it to a voter. |
| `node_removal_fenced` | 503 | The local node is draining or removed; only liveness and metrics remain available. |

On the joining node's own console the first of those reads `the cluster refused
this join: learner_protocol_inactive: …`. It is deliberately not prefixed
`schema migration failed`: nothing is being migrated and nothing is broken.

#### Upgrade voters before learners

A learner never proposes a replicated schema migration — that is the point of
the role, and it is enforced at the boot path rather than merely intended. The
consequence is an ordering rule for every future upgrade:

**Upgrade the voters first, then the learners.** A voter migrates the
replicated schema forward when it opens the store. A learner only checks it, so
a learner running a *newer* binary than the cluster's replicated schema refuses
to start, with

```
cluster schema 11 is incompatible with voter schema 12
```

("voter schema" there is the schema this binary implements — the name says which
role is responsible for installing it, not which role is refusing.) There is no
override and no self-repair: the learner will refuse for as long as the cluster
sits behind it. Upgrade a voter and the learner starts on its next attempt.

The reverse order is safe: a learner still on the old binary is refused by the
same check the moment the voters move ahead of *it*, and the same fix applies —
move the learner forward. Either way the fix is forward, so keep the voters at
or ahead of every learner.

### Measuring page-route latency

Use the page-latency runner against an already-running deployment. It does not
start, stop, join, or reconfigure voters. First create an authenticated
Playwright storage-state file for an administrator and make it owner-only;
then record a run with safe, stable labels for the runner and target:

```bash
chmod 600 target/validation/page-storage-state.json
scripts/cluster-page-latency \
  --base-url https://plurx.example.test \
  --storage-state target/validation/page-storage-state.json \
  --output target/validation/cluster-page-latency.json \
  --build-sha "$DEPLOYED_BUILD_SHA" \
  --scenario healthy \
  --target-role follower \
  --voter-count 3 \
  --hardware-label m6-pro \
  --storage-label nvme \
  --network-label lan-ethernet
```

The storage-state file must be a non-symlinked regular file with mode `0600`.
The runner parses it through a no-follow file descriptor and passes the parsed
object to Playwright, so the credential path is not reopened after validation;
the state is never copied into the result, and there is deliberately no
bearer-token argument. The output contains the full expected Git SHA, the
server's matching build stamp, a bounded scenario, safe runner labels, raw
phase times, normalized API route templates, statuses, and bounded failure
codes. It rejects cookies, authorization fields, token-shaped values, URLs,
IP addresses, UUIDs, media paths, and dynamic error text. A server reporting
`unknown`, a dirty or fabricated stamp, a release tag that does not resolve to
the supplied SHA in the runner checkout, or a different commit stops the run.
The runner also requires the repository's Playwright `1.62.0` pin. It brackets
every sample with checks for the target's actual role, voter count, membership,
reachability scenario, Raft term, and leader-change count, and rechecks the
build at the end without retaining node identity.

For each route, `shell_us`, `content_us`, and `settled_us` measure from the nav
click to the route's generation-fenced DOM milestones. The summary recomputes
type-7 p50 and p95 values and the maximum from successful raw samples; failures
are counted separately and never receive invented times. Compare named runs
only when their role, voter count, hardware, storage, network path, and fault
condition match. GitHub CI checks the schema and state machine, but its shared
runner timing is not an absolute wall-clock performance gate. Choose `healthy`,
`voter_unavailable`, `delayed_home_optional`, or `delayed_system_optional` for
`--scenario`; never combine their artifacts. The result is written even when
a named-runner budget fails: exit `1` means a p95, failure-count, or 30-sample
requirement failed, while exit `2` means the run or artifact itself was
invalid. Healthy and voter-unavailable runs enforce the reviewed first-content
budgets plus Home settled. Each delayed cohort enforces first content and every
sample must prove both a 250 ms response time for its named optional endpoint
and a 250 ms settled-minus-content gap before its settled bound is relaxed.

### Replaceable cluster writes

Replicated cache activity is deliberately less chatty than cache ownership.
Repeated unfenced claim or use touches for one recipe and node within one
serving process share one successful quorum write for five seconds. Active
producer claim renewals use the fenced publication path: they remain
synchronous because the same transaction advances the producer lease. The
durable activity timestamp can therefore trail the newest in-process unfenced
activity by less than five seconds; no cache publication path moves it backward.
Completion, integrity invalidation, forgetting a cache location, and every
offline/job ownership transition remain synchronous quorum mutations and do
not use this gate.

Manifest integrity scrubbing is separately bounded rather than time-coalesced:
up to 128 location observations and their cursors share one quorum transaction.
Its observation timestamp is monotone even if two completed batches arrive out
of order; the cursor stays coupled to the batch that performed the checks.

Membership keeps its existing one-heartbeat-per-node, ten-second cadence and
thirty-second reachable window. Only concurrent or duplicate submissions
inside 250 ms are collapsed. A caller waits for the first durable result before
suppression is reported, and a failed first write reserves no window, allowing
the next waiter to retry. A node is reachable through exactly 30 seconds after
its last committed heartbeat and leaves rotation immediately after that
boundary.

There is no configuration or schema migration for these coalescers. Rolling
downgrade is safe: an older process resumes the prior higher write rate. If
diagnosing cache age, compare timestamps with the five-second durability
boundary rather than treating every request as a promised database update.

### Run the named four-machine topology campaign

The P0c runner uses four private Linux hosts for voters and a fifth, external
controller for load generation. It never joins, removes, pauses, or reuses a
live plurx voter. Each measured topology gets fresh containers, ports in the
IANA dynamic/private range (`49152--65535`), and a new
`/var/tmp/plurx-cluster-named.*` data root on local durable storage. A
controller-generated 256-bit nonce makes every root a single, predeclared
basename. The same nonce is part of every container name, so two campaigns can
never claim the same cleanup target. Successful and failed runs remove only
the exact container names and nonce paths they recorded before remote creation.
Each newly created root also receives a sibling marker derived from the
separate, private output-owner capability; cleanup refuses a colliding root
without that marker instead of treating the random basename as proof of
ownership. The marker remains until the root itself is gone, so interrupted
cleanup is safely retryable. Voter containers carry the same capability in a
private cleanup label, and cleanup refuses to stop a colliding container unless
its raw Docker inspection matches.

Build from the isolated clone and keep private hostnames and addresses in an
ignored config under `target/`:

```bash
cp benchmarks/cluster-named-runner.example.json \
  target/cluster-named-runner.json
$EDITOR target/cluster-named-runner.json

sha=$(git rev-parse HEAD)
export PLURX_NAMED_HOSTS='private-ssh-alias-1 private-ssh-alias-2 private-ssh-alias-3 private-ssh-alias-4'
scripts/cluster-named-runner image "$sha"
scripts/cluster-named-runner collect \
  target/cluster-named-runner.json \
  target/cluster-topology-named
```

The image command refuses a dirty or untracked clone and sends `git archive` of
the exact full source SHA to Docker, so ignored credentials and controller-only
files cannot enter the build context. It builds for the common native
architecture reported by the four voters and requires that SHA as the image
tag. Both Dockerfile stages are pinned by manifest digest. The build embeds the
full source SHA in the runner binary and the runtime image's OCI revision
label; collection executes that binary by immutable image ID and verifies both
identities locally and on every voter. Collection independently requires a
clean controller tree with the same HEAD at its opening and closing
boundaries. It rechecks the image digest, native Linux architecture, four
distinct salted machine identities, and an external controller identity before
and after measurement. It also resamples and binds those fingerprints to each
raw run. Every measured and cleanup container executes the verified digest
with pulls disabled, never the mutable tag.

Runtime SSH names, private addresses, and raw machine ids are transport-only;
committed artifacts use bounded, opaque classification labels and build-salted
fingerprints. The runner rejects public labels that contain transports, URLs,
IP addresses, credential/identity markers, or token-shaped values, and accepts
only the local `plurx-cluster-check:<full-sha>` image name. Before collection,
confirm the configured ports are free, the controller and voters have no
concurrent build, scan, backup, or benchmark work, and every `/var/tmp` root is
the declared local durable device rather than tmpfs or a network mount.

Collection alternates `3→4` and `4→3`, creates independent clusters, and keeps
every raw `pair-NN.json`. Each run seeds the same 32-row catalogue, then issues
256 bounded reads at concurrency 32 against a caught-up follower. The raw
artifact records every read latency, p50/p95/p99, whether any read fell back to
Authority, and the configured `read_pool_size`; a fallback invalidates the
run. After at least three pairs the campaign computes paired four-voter ÷
three-voter ratios for write p99, local-catalogue read p95, aggregate peak RSS,
CPU seconds, storage-write bytes, and network-transmit bytes. Resource counters
are captured concurrently on all voters at the stable pre-workload barrier and
again after convergence; the artifact retains monotone workload-window deltas,
while Linux `VmHWM` is reset at the opening barrier. It may stop only when all
two-sided 95% Student-t intervals have at most 5% multiplicative half-width, or
after seven pairs with an `inconclusive` result. `campaign.json` records the
pool size, medians, intervals, stopping verdict, isolated load-generator
declaration, and reviewed budgets. Every raw run repeats the same isolation
declaration and records the exact controller and voter fingerprint set used for
that topology. Validate retained bytes and every cross-file hash with:

```bash
cargo run --locked -p plurx-cluster-check -- \
  topology-campaign-validate path/to/campaign.json
```

The output directory must not exist. `collect` generates a per-invocation
256-bit owner and must win one atomic directory claim before it arms its cleanup
trap, reserves ports, or creates remote state. A losing controller never owns a
manifest and therefore cannot clean the winner. Pair files and `campaign.json`
are published with same-directory, no-replace hard links, so a competing writer
cannot overwrite earlier evidence. During an active topology,
`.active-cleanup.json` is bound to that owner and atomically created with mode
`0600` because it contains private transport details. The helper traps
interruption and replays only its manifest through time-bounded SSH cleanup. It
opens both the manifest and owner marker without following links, requires
regular mode-`0600` files, retains their descriptors during remote cleanup, and
rechecks their device/inode identities before deleting the manifest. It does
not automatically clean a stale manifest from another invocation. If the
controller itself is killed, recover the retained manifest explicitly before
reusing the runner hosts. Preserve that directory as evidence and choose a
fresh output directory for the next campaign:

```bash
scripts/cluster-named-runner cleanup \
  target/cluster-topology-named/.active-cleanup.json
```

### Split durable state, persistent cache, and scratch

`storage.data_dir` remains the compatibility root and the only place plurx
selects authoritative SQLite/Hiqlite state, identity, secrets, migration
markers, or backups. Optional node-local roots isolate heavy I/O without
changing that selection:

```toml
[storage]
data_dir = "/srv/plurx-state"
cache_dir = "/srv/plurx-cache"
transcode_dir = "/var/cache/plurx-transcode"

[cluster]
read_pool_size = 4
install_snapshot_timeout_secs = 120
```

When omitted, both new paths preserve the exact legacy layout. With
`cache_dir` set, persistent children are `artwork/`, `transcode/`, `subs/`, and
`renditions/`; regenerable ffmpeg state lives under `runtime/`.
`transcode_dir` names the disposable live-session directory itself and must sit
outside `data_dir`. Naming the legacy path explicitly — `transcode_dir =
"<data_dir>/transcode"`, or any other scratch below `data_dir` — is refused at
startup. Omitting the key is the supported way to run the legacy layout.

Scratch ownership is one contract for both roots: the legacy
`<data_dir>/transcode` and an explicit `transcode_dir` are treated identically.
The root must be owned by the daemon uid, and a root owned by another uid
refuses startup. A daemon-owned root that is group- or world-writable is
repaired to `0700` with a warning, and startup continues. Plurx writes an exact
owner-only `0600` `.plurx-transcode-scratch` marker naming the durable root it
was claimed for, verifies it before every cleanup, removes only verified
children, and fails startup on an incomplete cleanup. First ownership uses a
durable non-authorizing `.plurx-transcode-scratch.claiming` state; restart
promotes it only when the directory is still otherwise empty. A `.claiming`
marker left empty or truncated by a crash is an interrupted claim and is
retried instead of wedging startup; a truncated *published*
`.plurx-transcode-scratch` still fails closed, because that one authorizes
deletion. A populated root plurx does not own refuses startup and is left
untouched, byte for byte. A scratch directory that cannot be read — an EIO from
a FUSE, NFS, or mergerfs backing store — is that error, never an empty
directory.

Inside a root plurx owns, the entries are its own leftovers. Their permission
bits and owning uid are not refusal conditions; they are removed. A symbolic
link, device, socket, or FIFO inside scratch still aborts startup without
touching anything, and a mount point inside scratch is reported explicitly as
one. Traversal is also bounded by entry count and depth, as a safety device for
a directory plurx does not already own: exceeding a bound in an owned root
leaves that scratch untouched for the boot, logs a warning, and lets startup
continue, and the next boot retries. In an unowned root a bound is a refusal.
The depth ceiling has a test in the owned-root direction; the entry-count
ceiling does not, so read that half as design intent.

Startup requires scratch, authority, and every resolved persistent-cache child
to be disjoint in both directions and to have distinct device/inode identities;
symlink aliases are refused before cleanup. Cleanup retains no-follow directory
and marker descriptors throughout, so a concurrent rename, mount replacement,
or marker swap aborts startup without touching the replacement.
Do not mount another filesystem below the scratch root. A mount point inside
scratch is named in the startup error, and a cross-device child is refused.
Neither is covered by a default gate: the mount-point error and the
bind-alias refusal are exercised only by `make bind-mount-check`, which calls
`mount --bind` and therefore needs a privileged Linux host, and the cross-device
bound has no test at all. Treat the group as design intent and mount elsewhere.

An available `cluster.shared_cache_dir` is a fourth persistent root. It must be
disjoint from authority, the local cache, and scratch; its verified identity is
held as protected throughout scratch cleanup. Plurx neither creates a missing
shared-cache mount nor turns its absence into a startup failure: the node-local
fallback from the shared-cache contract remains active. Never reuse one host
directory for both shared and local cache, and never place the shared mount
below scratch.

| Path | Durability | Placement |
|---|---|---|
| `storage.data_dir` and the authority set below | authoritative voter state and restart/rollback identity | durable local SSD/NVMe; never tmpfs, NFS, or SMB |
| `storage.cache_dir` or the legacy cache/artwork children | persistent node-local bytes, including user-visible offline packages and admitted VOD renditions; only the `runtime/` child is disposable | stable local persistent storage with capacity monitoring |
| `storage.transcode_dir` or `<data_dir>/transcode` | disposable live-session scratch | fast local scratch or a tmpfs mounted `-o uid=<daemon-uid>,mode=0700`; emptied only at daemon startup |

Create and mount every child before the first `plurxd` start; an empty fallback
directory on the root filesystem is not a successful installation. The
data-root authority set is the entire `hiqlite/` tree (including its
`activation.json`), `node.id`, `membership.json`, `secret_raft`, `secret_api`,
`hiqlite-activated.json`, `hiqlite-readdress.json` when present, and the
`migration/` directory when present. Preserve the retained `plurx.db` and
`backups/` with that set for rollback and recovery; do not confuse the root
`hiqlite-activated.json` lost-target fence with `hiqlite/activation.json`.
Keep every secret and marker owner-only while copying. Keep the credential key
with the same durable backup set. If
`cluster.credential_key_file`/`PLURX_CREDENTIAL_KEY_FILE` overrides the default,
that exact owner-only file is authoritative and belongs in the same backup and
move procedure. Its canonical path must remain outside scratch and every
managed cache root; startup refuses a key inside either. Startup also passes the
selected key inode into post-lock scratch cleanup as a protected identity, which
is meant to stop a bind alias erasing it. The cleanup half of that is tested —
a protected identity found at any depth aborts cleanup with its bytes intact —
but nothing tests that the credential key is the identity handed in, so place
the key outside the managed roots rather than relying on the belt. Each voter owns its own Hiqlite storage: sharing that directory
between machines defeats Raft's independent failure model.

Create every configured root with the daemon uid/gid and monitor space and
inodes independently. Do not mount over a populated child: that only hides its
data. Move one non-leader at a time. Eject it from the load balancer, stop
`plurxd`, copy persistent bytes with ownership, modes, timestamps, and links,
set the new paths, and restart. Require `/readyz`, current applied-index
catch-up, and the expected cache/offline/artwork inventory before moving the
next voter. Expect a full cache or scratch device to fail local work without
causing a database to be created or relocated there: `storage.data_dir` is the
only root plurx selects a durable target from. That is an operational
expectation, not a proven property — the deterministic test substitutes an
ENOTDIR failure for a full device, and any real out-of-space exercise is
privilege-gated.

Setting `storage.cache_dir` migrates nothing. The legacy trees stay where they
are; the daemon logs a warning while they still hold bytes and starts anyway.
Copy each tree to its own new name, because the children do not nest the same
way on both sides:

| Legacy path | With `cache_dir` set | Action and reason |
|---|---|---|
| `<data_dir>/artwork` | `<cache_dir>/artwork` | Copy; artwork is persistent node-local data. |
| `<data_dir>/cache/transcode` | `<cache_dir>/transcode` | Copy; this includes completed offline packages visible to users. |
| `<data_dir>/cache/subs` | `<cache_dir>/subs` | Copy; extracted subtitles survive restarts. |
| `<data_dir>/cache/renditions` | `<cache_dir>/renditions` | Copy; admitted VOD copy-cache entries are persistent. |
| `<data_dir>/cache/runtime` | `<cache_dir>/runtime` | Do not copy. Stop the daemon, delete the old tree, and let the next start regenerate it. |

`cp -a <data_dir>/cache <cache_dir>` is the obvious move and the wrong one: it
lands every child below `<cache_dir>/cache`, where nothing reads it. Completed
offline packages live in the transcode tree and admitted VOD bytes live in the
rendition tree; stranding either silently discards user-visible or already
admitted work.

Moving `storage.data_dir` needs one more step. The scratch marker names the
durable root it was claimed for, so a daemon started with a new `data_dir` and
the same scratch root refuses with a `claimed by another durable root` error
naming the marker file. Stop `plurxd`, delete
`<transcode_dir>/.plurx-transcode-scratch` along with the scratch contents —
they are disposable by definition — then start on the new `data_dir` and let
the daemon claim the empty root again. Never delete the marker under a running
daemon.

Rollback in reverse. Stop one non-leader, copy persistent bytes back to their
legacy data-root children, discard the regenerable runtime tree, remove both
new strict `[storage]` keys, restart and prove readiness/catch-up, then
continue. Do not install an older binary until
every voter has its bytes back and its config no longer contains the new keys:
`[storage]` rejects unknown fields, so an older binary does not ignore
`cache_dir`/`transcode_dir` — it fails to parse the file and the node does not
start at all. Never move two voters concurrently and retain both verified copies
through a soak period.

`cluster.read_pool_size` is bounded from 1 through 16 and defaults to 4. It
changes only local read-only SQLite connections. WAL size/sync, the 10,000-log
snapshot trigger, disaster-recovery log retention, heartbeat, and election
timers remain unchanged. The setting now reaches the named-host runner's node
configuration and is repeated in schema-versioned raw and campaign evidence,
so a 4/8/16 sweep measures and attests three different pools; an earlier runner
built its own configuration and would have measured the default three times,
so no deferred artifact from before that change means anything.

Run three campaigns from the same clean source/image, changing only
`read_pool_size` and the new output directory. Validate all three directories,
then compare the four-voter medians in their `campaign.json` files. Retain the
smallest value whose local-catalogue p95 improves over pool 4 while neither
write p99 nor aggregate peak RSS regresses by more than the recorded 10%
guardrail. If no larger pool clears all three conditions, or any arm is
inconclusive, keep 4. Preserve all raw pair files with the three campaign
summaries; a summary without its hash-bound raw evidence is not a selection
artifact.

`cluster.install_snapshot_timeout_secs` is bounded from 10 through 3,600 and
defaults to 120 seconds. It is the deadline OpenRaft applies while sending and
installing snapshot segments because Hiqlite leaves its separate non-final
segment timeout disabled. The previous fixed 10-second deadline repeatedly
restarted a 72 MiB snapshot after about 50 MiB on the production LAN; 120
seconds completed the same transfer. Keep the value identical on every voter
so leadership changes do not change catch-up behavior. Snapshot frequency,
WAL size/sync, disaster-recovery log retention, heartbeat, and election timers
remain unchanged. During a rolling upgrade, an old-binary leader keeps its
fixed 10-second deadline until that voter is upgraded; do not treat the new
deadline as effective cluster-wide until every possible leader is current.

**Synchronize clocks before cluster work.** All voters and the external load
generator must run NTP/chrony (or an equivalent disciplined source), and
monitor offset continuously. Membership reachability and artwork repair proofs
currently compare Unix timestamps from different nodes. Treat an absolute
offset above 250 ms, loss of synchronization, or an offset outside the bound
recorded in the benchmark artifact as a go/no-go failure for membership changes,
failure drills, or performance runs. Bounded-replica freshness uses a local
monotonic deadline, but clock synchronization remains an operational
prerequisite for the existing cross-node protocols and comparable evidence.

**Prepare the existing voter.** Give each node reachable, unique Raft and
cluster-API addresses. `advertise_host` is a host or IP, not a URL. Set
`join_url` when the public API used for cluster admission is reached through a
different hostname, port, or HTTPS reverse proxy; this is the bootstrap URL
embedded in a token for redemption. Set `artwork_url` to the current node's
HTTP origin; membership retains that origin as the node's artwork and
activity-snapshot endpoint, but never returns it from membership/status APIs.
`advertise_host` is also the explicit opt-in that opens a never-joined voter's
internal listeners beyond loopback. Leaving it empty preserves the old
single-node network surface even though the configured bind defaults are
`0.0.0.0`. On an already-activated sole voter, the first restart with
`advertise_host` set takes a metadata-reset Hiqlite snapshot, proves the same
`instance.id`, and replaces only the one-node Raft history so the committed
peer address becomes reachable. That repair runs only while the committed
address actually differs from the configured one, so it settles after one
restart and later boots are ordinary; a loopback-literal `advertise_host` -
two daemons on one host - is a valid configuration that never triggers it.
The old target remains beside the replacement until that replacement opens
successfully. Startup refuses the transition once any learner or second voter
exists, decided from the replicated node records rather than from this node's
`membership.json`, so a coordinator that has already admitted a peer refuses
even on its first restart after the address change.

```toml
[cluster]
raft_bind = "0.0.0.0:32401"
api_bind = "0.0.0.0:32402"
advertise_host = "plurx-a.lan"
join_url = "http://plurx-a.lan:32400"
artwork_url = "http://plurx-a.lan:32400"
```

Raft and the internal cluster API use automatic TLS. Startup refuses a public
cleartext form of either listener; the public `join_url` still follows the
plain-HTTP/reverse-proxy boundary in [SECURITY.md](SECURITY.md). The joining
node opens the complete token locally and sends only its SHA-256 digest over
that public URL; the Raft/API secrets and credential-wrapping key do not cross
the redemption or finalization request.

Cluster activity fan-out calls `/_internal/v1/activity-snapshot` at those
explicit origins. The request carries a 30-second signature from the sender's
durable per-node key and names the intended target, not a household
user/admin/API-key bearer. Redirects are refused, responses are capped at an
exact 256 KiB serialized budget, wrong-node answers are invalid, and at most
64 peer calls share one concurrent two-second deadline. SQLite and
never-joined one-node installs have no peers, make no calls, and add no
listener. `join_url` may retain a reverse-proxy path prefix such as
`https://cluster.example/plurx`; `artwork_url` is deliberately an origin only.
The existing artwork-recovery HMAC remains unchanged for mixed-version v4
rollouts; the per-node signature applies only to the new activity route.

**Mint one token into a protected file.** The default lifetime is 10 minutes;
the API clamps requests to 60–3,600 seconds. It returns the token once, so do
not paste the response into a shell transcript, log, issue, or TOML file.
Several tokens may be outstanding at once — stage two machines together, or
replace a token you lost, without waiting out the first one's lifetime. Each
reserves its own Raft id until it is redeemed or expires; an unused token stops
reserving anything once it lapses.

```bash
export PLURX=http://plurx-a.lan:32400       # an existing voter
export JOIN_TOKEN_FILE=/secure/plurx.join   # local path copied to the new node
umask 077                                   # new files are owner-only
install -m 600 /dev/null "$JOIN_TOKEN_FILE"
curl -fsS -X POST "$PLURX/api/v1/cluster/join-tokens" \
  -H "Authorization: Bearer $PLURX_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"expires_in_seconds":600}' \
  | jq -er .token > "$JOIN_TOKEN_FILE"
```

**Start a fresh joining node.** Its data directory must not contain
`plurx.db`; joining never overwrites an installation. Copy the protected token
file there, configure this node's own reachable addresses, and start `plurxd`.
It checks schema/protocol compatibility before admission, creates a distinct
`node.id`, catches up as a non-voting Raft member, is promoted to a voter,
verifies the unchanged replicated `instance.id`, and deletes the token file only
after finalization. That intermediate step is Raft's, and is over in seconds; it
is not the durable **learner role** below, which nothing promotes.
An interrupted join reuses its staged identity instead of minting another one.
`membership.json` records that token's digest. A leftover token from another
node therefore produces a warning and is ignored rather than taking an already
healthy voter offline. If the coordinator is temporarily unavailable after
the voter and activation marker are durable, that voter still starts and keeps
its identity-bound token file; a later boot retries finalization.

```toml
[storage]
data_dir = "/var/lib/plurx"

[cluster]
raft_bind = "0.0.0.0:32401"
api_bind = "0.0.0.0:32402"
advertise_host = "plurx-b.lan"
join_token_file = "/secure/plurx.join"
```

**Read the roster.** `availability` is `single_node`,
`degraded_reconfiguration`, or `high_availability`. Node rows deliberately
contain only node id · short hostname · advertised host without its listener
port · Raft id · role · leadership · heartbeat freshness · last-seen; internal Raft
and API addresses, media paths, and token material never enter this payload.
`last_seen_at` is Unix milliseconds; read the nested `replication` object for
lag using the meanings above.

```bash
curl -fsS "$PLURX/api/v1/cluster/nodes" \
  -H "Authorization: Bearer $PLURX_ADMIN_TOKEN" | jq .
```

**Update voters one at a time.** A three-voter cluster has a majority of two,
so an update may stop exactly one voter. Never batch two Cinema restarts and
never call the leave endpoint as an update hook: leave is permanent membership
removal, while an update must reopen the same voter and the same data
directory. Wait for `GET /readyz` to return 200 before advancing to the next
machine. `/healthz` is insufficient here; it proves that HTTP is alive, not
that the voter can use replicated storage or sees a leader. The private Ansible
deployment uses `serial: 1`, fails the whole play on the first node error, and
now gates each Cinema host on `/readyz`.

**Enable bounded catalogue reads only after the rolling update settles.** Keep
`cluster.bounded_replica_reads = false` while any voter runs an older build.
After every voter is ready on the same bounded-read protocol, set the following
identically on every voter and restart them one at a time again:

```toml
[cluster]
bounded_replica_reads = true
bounded_replica_max_lag_entries = 64
```

The optimization is limited to library browse, item/file lookup,
recently-added, genre, Home previews, and technical-aggregate calls in the
native and Plex-compatible read handlers. Authentication, watch state,
settings, membership, leases, jobs, cache/offline ownership, mutations, and
every write remain Authority operations. Search remains its existing
node-local derived-index operation. Write-followed-by-read handlers continue to
use Authority.

A local result is returned only when a one-second quorum watermark, local
term/leader/epoch, negotiated protocol, and the configured `0..10000` entry
lag budget remain valid before and after the complete operation. Missing or
changing proof and local SQL/mapping errors discard the local result and retry
Authority. The existing multi-statement genre/count and media-shape operations
retain their Authority semantics; the permit is not a new cross-statement
snapshot guarantee.

Set `cluster.bounded_replica_reads = false` on all voters, one at a time, for an
immediate rollback that changes no schema or membership. `/readyz` removes a
voter whose quorum proof is absent or whose local apply lag is nonzero; the
read helper also falls back independently, so bypassing the load balancer does
not turn an expired proof into a stale response.

**Use readiness conservatively at the reverse proxy.** `/readyz` is an active
replicated-store proof, not a free process counter: it checks cluster health and
performs an authority `SELECT 1`. Start with a 10-second interval, a 2-second
timeout, and three consecutive failures before ejecting a backend. Use
`/healthz` for higher-frequency process supervision. Route new requests only to
ready nodes, drain an ejected backend's existing connections, and do not
automatically replay POST, PUT, PATCH, or DELETE requests after a backend
failure.

Keep HLS playlists, segments, and authenticated image traffic sticky to one
ready backend for cache and session locality. A cookie or source-hash policy is
fine; stickiness is an optimization, not an availability dependency. If that
backend becomes unready, the next request may move to a survivor and the
client-visible recovery contract still applies.

The proxy contract is independent of Caddy, nginx, Traefik, HAProxy, or a
managed load balancer. Use a 2-second backend connection bound and a 75-second
maximum connection drain. Retry `GET` and `HEAD` only; never automatically
replay `POST`, `PUT`, `PATCH`, or `DELETE`, because a lost response does not
prove the authority mutation was uncommitted. The exact contract and
ready-to-adapt examples live in
[`deploy/cluster-routing/`](../deploy/cluster-routing/).

Before rollout, run the product-neutral fixture and inspect its evidence:

```bash
cargo run --locked -p plurx-cluster-check -- proxy-fixture
make cluster-check
jq '{follower_loss,leader_loss,three_voter_plus_learner,hls_backend_loss,accepted_budgets}' \
  target/validation/cluster-failure-drills.json
```

The retained semantic artifact records every attempt, error, and latency from a
fixed-cadence 64-write workload that continues through node loss. It proves
leader recovery inside the accepted 10-second election transition budget,
lagged learner rotation, a response longer than the connect budget draining
inside the 75-second ceiling, HLS takeover from a stopped backend with one
discontinuity, and zero proxy replays of an unsafe mutation. It is not a
hardware latency benchmark. Preserve the CI artifact for the build being
deployed; do not infer a tighter absolute SLO from local stopwatch output.

### Cluster ingress, drain, and recovery

Ready-to-adapt HAProxy, keepalived, and Kubernetes Service/Ingress examples
live in [`deploy/cluster-routing/`](../deploy/cluster-routing/). All three use
`/readyz`, not `/healthz`, for new traffic. Configure each voter's
`cluster.artwork_url` as its node-specific public base even when
`cluster.join_url` names the shared VIP or proxy; `GET /api/v1/cluster/ingress`
returns the other currently reachable node bases to signed-in native clients as
bounded media-only failover candidates.

**Upgrade note.** Before this release `cluster.artwork_url` was read only by
other nodes, fetching artwork from each other. It is now handed to every
signed-in household client. If a voter's value names an address only the
cluster can reach — a container name, a private interface, a management VLAN —
change it to that node's real public base before enabling the media pool, or
clients will spend a failover attempt on an address they cannot resolve. It is
never given to an unauthenticated caller: `/api/v1/server`, which clients probe
without a credential to identify an unknown server, does not carry it.

Inspect the media plane directly on each backend before and during a rollout:

```bash
curl -fsS "$PLURX_NODE/api/v1/cluster/media" \
  -H "Authorization: Bearer $PLURX_ADMIN_TOKEN" | \
  jq '{remote_placement_ready,session_takeover_ready,local_active_sessions,nodes}'
```

For a rolling restart, remove one voter from new load-balancer traffic without
removing it from Raft membership. Keep its existing connections draining and
wait for `local_active_sessions` to reach zero, then restart that same node and
data directory. Note what that counter is: **transcode and remux sessions on
this node only.** A direct-play viewer holds no session, so a node serving
nothing but direct play reports zero while a dozen people are watching — drain
those by connection count at the load balancer, not by this number. Re-admit it only after `/readyz` succeeds and the media status
shows the current protocol. Advance to the next voter only then. The permanent
leave endpoint is not a rolling-drain command; it refuses an active media owner
and permanently changes quorum membership.

If a node dies instead of draining, P7 takeover is a separately gated recovery
path. `session_takeover_ready` is true only when both placement and takeover
policy are effective. Native clients retry the unchanged relative direct/HLS
URL through advertised survivors on transport failures; this does not consume
their codec/HDR fallback ladders. Counters and latency are exported as
`plurx_media_session_takeovers_total{method,outcome}` and
`plurx_media_session_takeover_seconds{method}`.

Backups do not become interchangeable merely because the database is
replicated. Preserve the data directory, node identity, and cluster secrets for
each voter in host/storage backups so the original majority can be restored.
The shared cache, node-local transcode scratch, and session directories are
rebuildable accelerators and must not be treated as the authoritative backup;
the media sources remain external inputs. Do not restore one voter's copied
Raft state as a fresh cluster or start two restored copies with the same node
identity. Until quorum-aware restore is shipped, disaster recovery means
restoring enough original voters to recover the original majority.

**Let artwork converge before relying on a voter for failover.** Item rows name
poster and backdrop files through Raft, while the image bytes remain in each
node's local artwork directory. Every voter reconciles those names in the
background: it pulls a missing file from a reachable peer under a short-lived
cluster proof and atomically installs it. If no peer retains the file, a
bounded, leader-arbitrated repair recreates provider-backed artwork. Home and
Books voters instead recreate already-published bytes—including EPUB and
audiobook embedded covers—only when their own node can read the local media,
without republishing the catalogue row. A
successor waits a full monotonic lease before fencing an abandoned older Raft
term, so host clock skew cannot create overlapping provider work. Every
provider-origin and catalogue mutation also requires that replicated
owner/term/generation row and the exact `provider:artwork` singleton-job lease
inside the same database statement; a timed-out old Raft command therefore
commits as a no-op after leadership changes, lease takeover, or a later
same-term repair attempt. Completion and timeout both retire the exact
generation with a later conditional Raft command.
For a Curator book, the provider owns recovery only after its validated cover
was successfully cached and bound to the published filename. If Curator sent
no cover, or its fetch failed, an embedded EPUB cover remains eligible for
node-local byte recovery while Curator's title/work facts remain authoritative.
Re-pairing an edition that already owns provider art is stricter: the edition
and cover advance together only after the replacement bytes are fetched and
their publication slot is reserved; otherwise the previous coherent pair is
retained for a later retry. Curator filenames include a content version, so a
replicated edition change is observed as a missing new name on every voter;
no voter can mistake nonempty bytes from the prior edition for the new cover.
If the allowed upstream URL later returns different validated bytes, the
fenced repair publishes that new content-addressed generation before advancing
the replicated filename, so the old pair survives any interruption. Voters
scan for orphan generations every six hours and remove only exact
Plurx-managed filenames that have been unreferenced for at least 24 hours.
Each candidate is checked consistently against replicated catalogue state
again while holding the same per-file reservation used by fetch/publication;
ordinary user files in the artwork directory are never sweep targets.
Requests also use the peer path immediately, so a newly joined voter does not
show broken cards while the first inventory pass is still running. The configured
`cluster.artwork_url` (or its default derived from `advertise_host`) must be a
node-specific public HTTP base other voters can reach. It deliberately does not
inherit `cluster.join_url`: the join URL may name a shared load balancer, while
an artwork request must reach the voter that actually owns the local file.

**Recover a temporary quorum loss by restoring the original voters.** With one
of three voters available, Plurx deliberately commits no writes or membership
changes. Bring back any second original voter with its existing data directory,
node identity, and cluster secrets; Raft elects a leader and service recovers
without an operator reconfiguration. Do not delete Raft state or mint a
replacement node while the old majority may still exist.

A permanently lost majority has no supported force-reconfigure or
backup-to-fresh-cluster procedure in this release. A minority cannot safely
declare itself the new cluster without proving the old majority is dead; doing
so would create split brain if those machines returned. Preserve every
surviving data directory and secret, keep the nodes stopped, and recover the
original majority from host/storage backups. Quorum-aware backup/restore and a
deterministic one-node disaster-recovery drill remain the explicit M6 work in
[CLUSTERING-PLAN.md](CLUSTERING-PLAN.md).

**Gracefully remove the node you are connected to.** Settings → Cluster →
**Leave this cluster** calls the same admin-only operation. It resolves the
node's offline work and coordinates a safe membership change with the
surviving quorum. A durable removal fence and a final pre-proposal sweep prevent
new or stranded local work. For an even voter set, the current leader commits
the joint then uniform removal directly, then drains so the new odd quorum can
elect. For an odd voter set, it confirms a successor first. The daemon then
drains HTTP and exits.
Membership changes are refused before offline-work settlement, leadership
handoff, or fencing while active nodes are on mixed versions; finish the
rolling upgrade and retry. Replicated guards keep an older binary from deleting
a newer removal fence if versions change during an already-started operation,
and refuse an older coordinator's new fence write before it can submit a stale
absolute voter set—even when that older process is still the Raft leader.
The command-line equivalent must target one node directly (or use a sticky
route) for both the roster read and leave POST. The body binds the destructive
request to the backend that produced `local_node_id`, so a load balancer cannot
move a confirmed leave to another voter:

```bash
export PLURX_NODE=http://plurx-a.lan:32400
LOCAL_NODE_ID=$(curl -fsS "$PLURX_NODE/api/v1/cluster/nodes" \
  -H "Authorization: Bearer $PLURX_ADMIN_TOKEN" | jq -er .local_node_id)
curl -fsS -X POST "$PLURX_NODE/api/v1/cluster/leave" \
  -H "Authorization: Bearer $PLURX_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d "$(jq -cn --arg node_id "$LOCAL_NODE_ID" '{node_id:$node_id}')" | jq .
```

This is permanent and refuses a 2→1 change. To reuse the machine, discard the
old Plurx data directory and join it again with a fresh token. Do not use this
operation for a rolling update or ordinary restart.

As soon as removal enters its durable pending state, Plurx invalidates every
singleton-job lease owned by that node in the same replicated transaction.
The removed node identity is permanently refused new scheduler leases by
replicated database triggers, so a still-running or resumed process—including
one on the preceding rolling-upgrade version—cannot publish background work
after the membership change. Startup and an idempotent removal retry restore
missing owner fences for older tombstones. A definitively rejected removal
clears that owner fence only when no concurrent, ambiguous, or older-version
attempt still owns the shared removal state. Otherwise the API returns
`membership_removal_pending`, the roster labels the node **Removal pending**,
and the fence stays authoritative. Finish upgrading every cluster node and
retry that same removal; do not return the fenced node to service. Invalidated
job tokens remain stale even after a clean rollback, and the node must acquire
fresh ones.

**Remove a follower from three or more voters.** Use the node id from the
roster, not its Raft id. The request refuses the current leader and any change
that would leave fewer than two voters.

```bash
curl -fsS -X DELETE "$PLURX/api/v1/cluster/nodes/$NODE_ID" \
  -H "Authorization: Bearer $PLURX_ADMIN_TOKEN" | jq .
```

#### What removal does to the node's offline downloads

Removal resolves the departing node's offline packages before the membership
change commits, so it leaves nothing owned by a machine that no longer exists.
You do not have to drain them by hand first, and you do not have to stop that
node from serving downloads while you do it: it keeps answering requests until
the change commits, and removal re-reads its work and resolves the new arrivals
before committing rather than acting on the list it started with.

**Stop the removed node once the removal returns**, or repoint its clients at a
surviving server. Removal takes the machine out of the cluster roster; it does
not switch the machine off. A removed-but-still-running node that keeps
receiving download requests is refused at the write path with a legible
`node_removed` response — the durable insert checks the `cluster_nodes.removed_at`
tombstone and returns `NodeIsTombstone` before any admission or quota step.
Nothing ever reaches the encoder or holds a reservation against a tombstone.

This is a fence on the offline package write path only. A removed-but-running
node keeps its other write paths open, and the operator advice below still
applies — but the offline case removal could not clean up for you is now closed.

Before it commits, the cluster asks every other node whether it can actually
read each package's source file — not whether the path looks the same, but
whether opening it returns the same bytes, size, and modification time the
request recorded. That question is asked and answered for real, because a
replicated source path does not prove another node mounts the same media.

| The package was | What removal does |
|---|---|
| queued or preparing, and another node proved it reads the source | Moved to that node and prepared there. The download continues; nothing is lost. |
| queued or preparing, and no node could prove it | Failed with `node_removed`. Its reservation is released immediately. |
| ready | Failed with `node_removed`. Its bytes only ever existed on the removed node. |
| already failed | Left alone. It holds nothing and expires normally. |

A ready package is failed rather than moved on purpose. The prepared bytes
lived only on the departing node, and plurx does not promise byte-identical
transcodes across machines with different encoders — so re-preparing it behind
the same download URL could hand a partly-finished download a different set of
bytes. Failing it is honest: the user's client sees the package is gone and
requesting it again prepares a fresh one on a remaining server.

`node_removed` is a stable code. Nothing is wrong with the media and nothing is
wrong with the server: the machine that was preparing that download left. The
fix is always the same — ask for it again.

The removal still refuses while a client is downloading from that node right
now, and the error says how many transfers are in flight. Wait for them to
finish or delete those packages, then retry.

Two other offline refusals exist, and both are "retry this", not "drain it by
hand". A node that keeps admitting new downloads faster than the removal can
resolve them refuses after a bounded number of rounds and says so; point those
clients elsewhere and retry. A package that changes state while its resolution
is being applied — a producer finishing at exactly the wrong moment — also
refuses, because the removal will not commit on a plan it could not finish
applying. In both cases the packages that were resolved stay resolved: moved
work is claimable on its new node, failed work has already released its
reservation, and the retry picks up whatever is left. Nothing is left half
owned.

`cluster_leader_removal_refused`, `removal_would_lose_quorum`, and
`node_owns_offline_work` are operator-facing refusal codes. After a successful
3→2 removal, keep both survivors online and add a third voter; the status must
remain `degraded_reconfiguration` until then. M3a deliberately does not support
2→1 removal, even while both voters are reachable: every membership change from
two depends on both nodes, and this release keeps the degraded waypoint visible
until a third voter is added instead of presenting an unproved downgrade path
as rollback.

A removed node's old data directory is intentionally tombstoned: startup sees
that its Raft id is no longer a voter and refuses. To add that machine again,
stop it, discard its old plurx data directory, and join from a fresh directory
with a newly issued token. Never copy the removed directory back into service.

## Configuration surface

Precedence, lowest to highest: **built-in defaults → TOML file → `PLURX_*` env**.
Settings you edit at runtime (TMDB key, libraries, users) live in the database,
not here — this surface is only what's needed before the database opens.

The TOML file is looked for at `./plurx.toml` then `/etc/plurx/plurx.toml` (or the
path in `PLURX_CONFIG`). Environment-backed keys are named below; cluster
membership addresses and token-file paths are intentionally file-only:

| Env var | TOML | Default | What it does |
|---|---|---|---|
| `PLURX_BIND` | `server.bind` | `0.0.0.0:32400` | Address the HTTP API binds to |
| `PLURX_SERVER_NAME` | `server.name` | `plurx` | Bootstrap seed for the human-visible server name. The replicated setting is authoritative after first boot; rename it through the admin API |
| `PLURX_NODE_HOSTNAME` | — | OS hostname | Short physical-machine name shown in Settings → Cluster. Native installs normally leave this unset; containers set it explicitly so a generated container id is not mistaken for the host |
| `PLURX_DATA_DIR` | `storage.data_dir` | `./data` | Authoritative database, identity, secrets, migration markers, and compatibility root |
| `PLURX_CACHE_DIR` | `storage.cache_dir` | empty | Optional node-local artwork, transcode, subtitle, rendition, and ffmpeg-runtime root; everything except `runtime/` is persistent. Empty preserves the legacy layout under `data_dir`. Setting it on an existing install moves no bytes — copy the persistent legacy trees and discard the old runtime tree, per the path table above |
| `PLURX_TRANSCODE_DIR` | `storage.transcode_dir` | empty | Optional disposable live-session scratch directory, emptied on startup. Must be owned by the daemon uid and outside `data_dir`; naming `<data_dir>/transcode` or any other path below `data_dir` is refused |
| `PLURX_SCAN_PRUNE_PERCENT` | `storage.scan_prune_percent` | `10` | Maximum percentage of known files one complete scan may remove; `0` disables automatic removal |
| `PLURX_CREDENTIAL_KEY_FILE` | `cluster.credential_key_file` | `<data_dir>/credentials.key` | Node-local key that encrypts the stored Trakt bearer credential. Minted mode-`0600` on first boot, and required to stay owner-only. **Back it up with the database** — plurx refuses to start if the sealed rows outlive it, or if the key present is not the one that sealed them ([SECURITY.md](SECURITY.md)) |
| `PLURX_SHARED_CACHE_DIR` | `cluster.shared_cache_dir` | empty | Optional node-local path to a writable cache filesystem mounted on every participating voter. Requires `PLURX_SHARED_CACHE_ID`; a path alone is never trusted as proof of shared storage |
| `PLURX_SHARED_CACHE_ID` | `cluster.shared_cache_id` | empty | Stable operator name for that shared filesystem: 1–64 ASCII letters, digits, dots, dashes, or underscores. Every voter mounting the same filesystem must use the same value |
| `PLURX_CLUSTER_BOUNDED_REPLICA_READS` | `cluster.bounded_replica_reads` | `false` | Cluster-wide opt-in and Authority-read kill switch for the named lag-gated catalogue slice. Enable only after every voter advertises the current bounded-read protocol |
| `PLURX_CLUSTER_BOUNDED_REPLICA_MAX_LAG_ENTRIES` | `cluster.bounded_replica_max_lag_entries` | `64` | Maximum quorum-commit to local-applied gap admitted for a bounded catalogue operation; `0..10000`, identical on every voter |
| `PLURX_CLUSTER_READ_POOL_SIZE` | `cluster.read_pool_size` | `4` | Local replicated-read connection pool, bounded 1–16; tune only with retained 4/8/16 evidence |
| `PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS` | `cluster.install_snapshot_timeout_secs` | `120` | Snapshot transfer/install deadline in seconds, bounded 10–3,600; keep identical on every voter |
| — | `cluster.raft_bind` | `0.0.0.0:32401` | Raft listener for this voter. A never-joined node still binds loopback until `advertise_host` opts into membership. Remote traffic uses automatic TLS; every node needs a unique reachable address |
| — | `cluster.api_bind` | `0.0.0.0:32402` | Authenticated Hiqlite cluster API with automatic TLS. It follows the same loopback-until-opt-in rule |
| — | `cluster.advertise_host` | empty | Host or IP placed in committed peer records and the explicit membership-listener opt-in. Leave empty for an ordinary one-voter install; set it on every joining node. A sole voter whose committed address differs from this value performs one crash-recoverable local metadata readdress on restart, then settles. Once any peer or remote membership exists, changing the advertised host or either listener port is refused until an online membership-reconfiguration path exists |
| — | `cluster.join_url` | `http://<advertise_host>:<server port>` | Public plurxd base URL a fresh node uses to redeem/finalize its one-time token. Set the HTTPS proxy URL when applicable |
| — | `cluster.artwork_url` | `http://<advertise_host>:<server port>` | Node-specific public plurxd base peers use to recover local artwork. Set an explicit per-node URL when `join_url` names a shared proxy or load balancer |
| — | `cluster.join_token_file` | empty | Owner-only file containing one token on a fresh joining node. Empty bootstraps or reopens; a successful join deletes it |
| `PLURX_CONFIG` | — | — | Explicit config-file path (must exist if set) |
| `PLURX_FFMPEG` | — | `ffmpeg` | ffmpeg binary — point at jellyfin-ffmpeg for best hwaccel |
| `PLURX_FFPROBE` | — | `ffprobe` | ffprobe binary (inspection + chapter markers) |
| `PLURX_HWACCEL` | — | `auto` | Preferred encoder: `auto` · `qsv` · `vaapi` · `nvenc` · `videotoolbox` |
| `PLURX_VAAPI_DEVICE` | — | `/dev/dri/renderD128` | VA-API render node |
| `PLURX_TONEMAP` | — | zscale | The **CPU** tone-map operator: `zscale` · `libplacebo` · `off` (no tone-map — plays HDR washed, but a useful test/escape hatch). Which *pipeline* runs — GPU or CPU — is probed at boot, not configured; this only chooses the operator when the CPU chain is the one running |
| `PLURX_HWDECODE` | — | on | Set `off` to force software decode (still hardware-encodes) — for a GPU that decodes a stream to garbage, e.g. some Dolby Vision |
| `PLURX_GDM_PORT` | — | `32414` | Host UDP port for GDM discovery (move if Plex owns 32414) |
| `PLURX_MDNS_ADVERTISE` | — | `true` | Run Bonjour inside the server process; Compose sets this to `false` because its host-network companion advertises instead |
| `PLURX_DISCOVERY_SERVER_URL` | — | `http://127.0.0.1:32400` | Server URL read by `plurxd advertise`; normally only the Compose companion uses it |
| `PLURX_LOG` | — | `info` | Log filter (`tracing` EnvFilter syntax, e.g. `plurxd=debug`) |
| `PLURX_CLUSTER_ACTIVATION_FAILPOINT` | — | — | Test-only activation exit: `after-quiescence` · `after-incoming` · `after-marker` · `after-rename`; each exits `86` |
| `PLURX_HLS_CLOSED_CAPTIONS_NONE` | — | off | **Experiment.** Adds `CLOSED-CAPTIONS=NONE` to the HLS variant. Set `1` to enable |
| `PLURX_HLS_FORCED_AUTOSELECT` | — | off | **Experiment.** Puts `AUTOSELECT=YES` on forced subtitle renditions. Set `1` to enable |
| `PLURX_PGS_OVERLAY` | — | off | **Staged feature.** Advertises and serves the authenticated `pgs-v1` overlay producer. Keep off until native-client and physical HDR/DV acceptance is complete |

### The two HLS master experiments

These two are not tuning knobs, they are a ladder — candidate changes to the
HLS multivariant playlist, compiled in but inert until you set one, so a rung
can be tried, watched, and kept or dropped without another build. Both are
Apple authoring-rules items and both are candidates for the one open failure
in the Apple native-subtitle work: a physical Apple TV rejecting a copied
Dolby Vision master with CoreMedia `-12927`
([docs/APPLE-NATIVE-SUBTITLES-PLAN.md](APPLE-NATIVE-SUBTITLES-PLAN.md) §5.4).

| Var | What it adds | Why it might matter |
|---|---|---|
| `PLURX_HLS_CLOSED_CAPTIONS_NONE` | `CLOSED-CAPTIONS=NONE` on the variant | Apple's authoring rules ask for it, and it stops AVFoundation synthesising a phantom closed-caption option into the legible group — a phantom option shifts every rendition ordinal |
| `PLURX_HLS_FORCED_AUTOSELECT` | `AUTOSELECT=YES` on forced renditions | Apple's authoring rules require it on forced renditions; this master withholds it when two forced tracks share a language |

**How to run them: one per deploy, and let the device decide.** Set exactly one
variable, restart plurxd, and play the affected title on the actual Apple TV.
Both are read once at startup, because a master that changed shape between two
fetches of the same session would be a worse problem than either rung solves.
Never enable both at once — a master that then plays tells you nothing about
which change did it.

**The device is the only oracle here.** Every master regression in this arc so
far passed the unit tests and failed on physical hardware, and one of them
(`ed38ea9`, deriving exact codec data from the init segment) had to be reverted
in production. A green `make check` says the playlist is syntactically what was
intended; it does not say AVPlayer will accept it. Treat an unobserved rung as
untested, and turn it back off if the device does not visibly improve.

## Ports

| Port | Proto | Purpose |
|---|---|---|
| 32400 | TCP | HTTP API + web app (and the Plex-compat façade) |
| 32401 | TCP + TLS | Raft replication between voters; never expose it as public cleartext |
| 32402 | TCP + TLS | Authenticated internal cluster API; never expose it as public cleartext |
| 32414 | UDP | GDM discovery so Plex/Kodi clients find the server on the LAN |
| 5353 | UDP multicast | Bonjour `_plurx._tcp` discovery for native clients |

GDM discovery only works on 32414 (the protocol hard-codes it), but the *host*
port is movable via `PLURX_GDM_PORT` when a still-running Plex owns it — you lose
LAN auto-discovery on that host port, not the server.

Bonjour is best-effort and link-local. It does not cross a guest network, VLAN,
VPN, routed subnet, or Docker bridge. The tracked Compose stack therefore keeps
`plurxd` on ordinary or external networks and runs `plurx-discovery` as a
host-network companion. The companion fetches the public server identity at
`http://127.0.0.1:32400`, then publishes that same `_plurx._tcp` service on the
physical LAN. In cluster mode each Bonjour record has the shared logical `id`
and name plus its local `node_id`, node-derived hostname, and distinct service
label. GDM likewise publishes the node id as Plex `Resource-Identifier`, keeps
the logical id in `Logical-Identifier`, and returns that node id from the Plex
`/identity` facade. Native `/api/v1/server` continues to expose the logical
`instance_id` for grouping all voters. A never-joined install keeps the legacy
single-record bytes. Do not expose UDP 5353 to the internet.

The Compose companion shares the host's UTS namespace only to read its
hostname. On a never-joined install, if the durable server name is the generic
default `plurx`, that hostname is the discovery label; an explicit name
replaces it. The LAN address is appended in either case (`m6 ·
192.168.1.20`). Cluster records instead use the replicated logical name plus a
short local-node suffix. Rename the logical server through the admin settings
API so every voter converges; changing a joining node's TOML seed does not
rename it.

Do not add `network_mode: host` to `plurxd`: Compose rejects a service that also
has a `networks:` attachment. The companion is a different service, so an
override can safely keep `plurxd` on an external `media` network. On a Docker
host without host-network support, manual server entry remains the fallback.

**How to verify it:** run this from another Mac on the same LAN:

```bash
dns-sd -B _plurx._tcp local.
```

A line naming your server means the advertiser reached the LAN. Silence means
the deployed stack is too old, `plurx-discovery` is not running, host networking
is unavailable, or multicast is filtered between the devices. Opening TCP
32400 alone does not create a missing multicast advertisement.

## Reading the activity pill

Every page shows a pill of what the server is doing right now. Empty = idle and
hidden. `Scanning Movies · 182 / 210 files · 86%` means a scan is in its file
pass; it flips to `fetching metadata…` for enrichment, then disappears. It's a
live read of server-side job state, not a client guess — if it's spinning,
something is actually running; if a scan looks stuck, the pill (and the logs)
will say where.

On a cluster, the stream count includes direct play, progressive remux, HLS
copy, and transcode deliveries from every voter that answered within the common
two-second peer deadline. Open **Activity** to see the stable node id beside
each row. A remote row has no **Stop** button: this surface is read-only across
nodes, and stopping work on another voter requires a separately fenced control
protocol.

An incomplete cluster read leads with **Activity incomplete**. The page names
each known voter that was unhealthy, unreachable, timed out, or returned an
invalid response, and states that streams on those nodes may be missing. A peer
directory failure is visible by the same rule. Do not read a short table below
that warning as proof that the cluster is idle; restore the named voter or the
cluster directory and wait for the next three-second refresh. Ordinary SQLite
and never-joined installations perform no peer read and retain the historical
payload and page behavior.

## Reading library scan status

In Settings → Libraries, the Status column is the truth about each library:

| Status | Meaning | What to check |
|---|---|---|
| `idle` | No scan running; last scan finished | Item count looks right? |
| `scanning… N / M files` | File pass in progress | — |
| `fetching metadata…` | Files done, enrichment running | TMDB key set? |
| `error: …` (red) | The scan failed, with the reason | Almost always a path the **server** can't see |

**How to read it:** the single most common failure is a library that scans `0`
files while you can see the folder full of media. That means the path you typed
isn't the path the server process has — under Docker, the container-side mount
path must match. Fix the mount, not the library name.

### Scan deletion and root-identity safety

A complete scan refuses vanished-file cleanup when it would exceed
`storage.scan_prune_percent`. A non-zero percentage permits at least one
removal from a non-empty library, so one missing file cannot permanently wedge
a small library; `0` still disables automatic removal. Above that floor, the
percentage is a hard ceiling rounded down. A refused scan still records new and
changed files, but keeps every apparently missing row and reports the refusal
in library status and logs.

The first verified, non-empty scan records the library's canonical path-set
identity. Changing a library's paths clears that identity automatically. For a
deliberate storage replacement at the same configured path, an admin can call
`POST /api/v1/libraries/{id}/root-identity/reset`; the next verified non-empty
scan establishes the replacement. Never reset it merely to silence an empty
scan—verify the mount first.

Search is derived state. If one or more voters report missing search results,
an admin can call `POST /api/v1/system/search-index/rebuild`. The server rebuilds
the index from authoritative item rows across the cluster; library and watch
truth are unchanged.

## Reading the Server card (Settings)

The Server card is the health-at-a-glance panel:

- **ffmpeg** — the version string if it ran, or a red "not found" if the binary
  is missing. Red here means scanning and transcoding will both fail; fix
  `PLURX_FFMPEG` before anything else.
- **Hardware** — a pill per encoder (NVENC · QuickSync · VA-API · VideoToolbox),
  green ✓ if startup validation test-encoded through it, grey — if not
  available. Startup validation actually runs a probe encode, so a green pill
  means it worked once, not that a driver merely exists.
- **Transcoder** — the encoder the server will actually pick, and your preference
  (`PLURX_HWACCEL`). If you set `qsv` but see software selected, the QSV probe was
  rejected — the log line says why.
- **Storage** — what each library's storage reads at, and what a cold seek into
  a file costs there. See below; it is the one number on this card that can
  change without a restart, so it has a **Re-measure** button.
- **Right now** — active streams, library count, user count.

## Reading the storage numbers

A remux and a direct play both send the source's own video untouched, so the
storage has to sustain that file's bitrate for the whole film — and the
transcode paths have to read it at least that fast to keep an encoder fed.
When storage cannot, there is nothing in the playback UI that says so: the
viewer sees a `supply` stall, the Stats overlay shows the encoder under 1×,
and both readings are true while pointing away from the cause.

The probe answers it directly. It runs a few seconds after boot (so a sleeping
array never delays startup) and again whenever you press **Re-measure** or
`POST /api/v1/system/storage`. Per mount — libraries on the same device are
measured once, together — it reports:

- **Read rate**, in Mb/s, the same unit as a file's bitrate. A file whose
  bitrate is above this cannot play from here, whatever the encoder or the
  client does. A file above `read rate ÷ stream_readrate` plays but never
  builds a reserve, so any hiccup on the link becomes a stall.
- **Cold seek**, the median time to the first bytes at an untouched offset.
  Single-digit milliseconds is local storage; tens of milliseconds is a
  network mount, and it is what every scrub and every resume pays before
  ffmpeg can emit a frame.

It reads a real media file from the library rather than a fixture it wrote —
a fixture would still be in the page cache and would report a NAS at several
GB/s. If a number still comes back impossibly high the card says so rather
than quietly boasting; treat that mount's figure as a floor.

`scripts/perf-report` prints the same numbers with the comparison already
done, which is usually the faster way to answer "is my storage the problem".

### When the average looks fine and it still stalls

An average cannot show a gap, and a gap is what stalls a viewer: a stretch
where supply fell below demand for longer than the client's buffer covers. A
mount reading 250 Mb/s has ample room for a 69 Mb/s film on paper and can
still stall it several times an hour.

`scripts/perf-report --sustained` answers that. It reads continuously for 30
seconds per mount (pass a number for longer, up to 120), samples every 250 ms,
and reports the distribution — min, p10, median, max — instead of one figure.
It then *replays* that trace against a simulated client at a ladder of source
bitrates and two buffer depths, and prints what would have happened:

```
        source         2.2s buffer     15.0s buffer
          75 Mb/s    3 stalls/6s dry   ok (low 4.2s)
```

The left column is the progressive `/stream.mp4` path, whose read-ahead Chrome
caps at about 2.2 seconds; the right is what an MSE path (hls.js — your
transcodes and cache hits) holds. Same trace, same file, different outcome. A
row that stalls on the left and not the right means the storage is fine and the
delivery path is the problem.

`ok` is not automatically comfortable — the low-water figure in brackets is how
close the buffer came to empty. `ok (low 0.2s)` is a near miss.

It costs real reading: seconds × the read rate × the number of mounts. Run it
when something is wrong, not on a schedule.

## Reading playback (and the stats overlay)

The overlay's first row is **Build** — which binary is answering, as
`0.2.0 · v0.2.0-7-g0e80e42`, or `0.2.0 · built 30 Jul 16:35Z` when the image
was made from a context with no commit in it (deploy with `make docker-up` and it
can name the commit). It is there because "which build is this?" is the first
question of any debugging session, and the answer used to live behind an admin
login on the System page — invisible in every screenshot anybody sends. The web
UI is compiled into the binary, so that one line covers both halves.

Every playback resolves to one of three methods; open the player **Stats**
overlay (the ⓘ button, or press `i`) to see which:

- **Direct play** — the file is sent untouched. Ideal; zero transcode CPU.
- **Remux** — container repackaged, `-c:v copy`. Cheap; a little CPU for audio at
  most.
- **Transcode · QuickSync** (or NVENC/VA-API/VideoToolbox) — the GPU is
  re-encoding, usually because of codec/resolution/HDR the device can't take.
- **Transcode · software** — it fell back to CPU x264. Expected on a first-gen or
  driver-mismatched GPU, or as the self-heal after a hardware session stalled —
  check the logs for the rejection reason.

The overlay's **Source** vs **Now decoding** lines are the useful comparison:
Source is what the file is (from the server's probe); Now decoding is what your
browser is actually rendering. A 3840×2160 source showing 1920×1080 now-decoding
is a working downscale transcode.

### Delivery speed

A remux is a copy: ffmpeg can read it off disk and push it into the socket far
faster than it plays — over 200× real time on a local link. Left alone it will
take the whole link for as long as the burst lasts, and since every seek opens a
fresh stream, scrubbing means doing that repeatedly. On wired gigabit that is
merely impolite. Over Wi-Fi it monopolises airtime, and a client that happens to
need its DHCP lease renewed mid-burst can lose the lease and then fail to get it
back, because the broadcast `DISCOVER` goes out at the lowest basic rate and is
the first thing a saturated AP drops.

**Settings → Playback → Delivery speed** bounds it. The first 30 seconds of any
stream always arrive flat-out, so starting and seeking stay instant; the limit
applies after that.

| Setting | Use when |
|---|---|
| 2× | Marginal Wi-Fi, a powerline/MoCA bridge, or a link shared with anything latency-sensitive |
| 4× (default) | Anything normal. Absorbs the peaks of a variable-bitrate film and keeps building buffer |
| 8× | Fast wired LAN where you want a deeper buffer sooner |
| Unlimited | Wired-only, and only if you actively want the old behaviour |

Below 1× is refused: a stream delivered slower than it plays can never buffer,
so playback would stall by construction.

The limit needs ffmpeg 5.1 or newer (`-readrate`), and the initial burst needs
6.1 (`-readrate_initial_burst`). plurx probes for both at startup and logs a
warning if the ffmpeg it found has neither, in which case streams run unpaced
whatever this is set to.

### Transcode buffering

Delivery speed above governs the progressive remux, which the browser pulls at
its own pace. An HLS session — a transcode, or the copy-video repackaging
Safari and Apple TV get — is different: ffmpeg writes segments to disk and the
player fetches them, so *the server decides how much buffer the viewer is
allowed to have.* **Settings → Playback → Transcode buffering** is that
decision, in three parts.

| Control | Setting key | Default | What it does |
|---|---|---|---|
| Head start | `playback.hls_burst_secs` | 90 s | Content delivered flat-out before pacing engages. This is the buffer a stream *starts* with |
| Then pace at | `playback.hls_readrate` | 2× | How fast the input is read afterwards. Every second of wall clock adds a second of runway at 2× |
| Buffer limit | `playback.hls_ahead_max_secs` | 180 s | How far ahead of the client the session may get before it pauses itself |
| *(no dropdown)* | `playback.hls_ahead_max_bytes` | 2 GB | The same limit in bytes, per session — 180 s is a few hundred megabytes at a transcode rung and over a gigabyte of 4K copy, so time alone is not a disk bound |
| *(no dropdown)* | `playback.hls_scratch_max_bytes` | 8 GB | Ceiling across *every* live session. A per-session cap bounds one runaway; it says nothing about four healthy 4K streams between them |
| *(no dropdown)* | `transcode.max_hw_sessions` | 2 | Concurrent transcodes on the hardware encoder. An iGPU has one video-processing block, and a third 4K session on it does not run a third as fast — it drags all three under realtime. `0` disables hardware transcoding entirely (useful when the GPU is doing something else). Raise it on a card with more than one encode chip |
| *(no dropdown)* | `transcode.software_pool_threads` | cores − 1 | Encoder threads the software CPU pool may hand out at once. Software sessions used to pick their own thread counts and could oversubscribe every core between them; each session now reserves a weight (which is also its explicit x264 `-threads`) and joins only if it fits. The lone session on an otherwise-empty pool always starts, whatever its weight — a budget must never be a ban. Lower it on a box whose CPU has other jobs |

These have no dropdown because they are safety limits rather than
preferences — the right value is a property of the disk or the silicon, not a
taste — but all are settable through `PUT /api/v1/settings` when the defaults
don't suit the hardware.

**How to read it:** the head start is the single number that decides whether a
4K stream survives a network hiccup ten seconds in. Until 2026-07-28 the copy
path ran at exactly real time with no head start at all, which is why 4K
"started fine and then buffered", and why an Apple TV — which wants about three
segments before it plays anything — took a dozen seconds to start. If 4K still
stutters, raise the head start before touching anything else. If sessions eat
too much disk, lower the buffer limit: a session's directory holds roughly the
buffer limit plus a minute of already-played content, whatever the encoder's
speed.

"Ahead of the client" means ahead of what the player has **downloaded**,
which is not the same as ahead of what you are watching — a player fetches
its whole forward buffer in advance, so the download frontier normally sits
about a minute past the picture on screen. Everything plurx keeps and deletes
is measured from that frontier with the difference allowed for, which is why
segments survive roughly three minutes behind it rather than one.

The pause is real: at the buffer limit plurx sends the session's ffmpeg a
`SIGSTOP`. The default time window holds above 180 seconds and resumes at 150
seconds; per-session and global byte holds resume at half their limits.
`Settings → Activity` and the player's stats overlay (press `i`) name the
active `time`, `bytes`, or `global` reason and show its release value beside
how far ahead the session is. A held session is normally healthy — it has built
everything it is allowed to — but a `global` hold can remain until aggregate
scratch across other sessions falls below its release point.

### Playback telemetry

Performance II stores structured playback observations beside the local
database. They are operational data, not catalogue truth: on a Hiqlite voter
they live in that voter's own SQLite sidecar and never pass through Raft.

| Runtime setting | Default | Meaning |
|---|---:|---|
| `telemetry.retain_days` | 30 | Days to keep raw playback rows. `0` disables raw row writes and their pruning while preserving the existing client log lines. It does **not** disable network priors — see the note below |
| `playback.network_priors` | off | Whether to accumulate per-network playback history and let it choose Auto's starting rung |

`playback.network_priors` has no Settings control yet. The only way to change
it is the API:

```
curl -X PUT http://<server>:32400/api/v1/settings \
  -H 'Authorization: Bearer <admin token>' \
  -H 'Content-Type: application/json' \
  -d '{"playback_network_priors": true}'
```

Off is the default and off is exactly today's behavior: nothing is stored, no
response gains a field, and Auto starts where it always did. On, plurx keeps
one row per `(user, client class, network fingerprint)` holding a sustained
throughput estimate and the lowest ladder rung at which that network was seen
to starve, and consults it when Auto picks a starting height. `/decision` and
session-create then carry an additional `prior_kbps`; the fingerprint itself is
never returned or logged.

What it stores is deliberately coarse and bounded. The client class is a broad
family (`chrome`, `safari`, `edge`, `firefox`, `apple`, `android`, `other`)
derived from the request's `User-Agent`, and the network is an IPv4 `/24` —
never a full address. IPv6 clients get no fingerprint at all and always start
cold. Each user and client class keeps at most 64 networks, rows unused for 30
days are pruned by the same daily pass as telemetry, and a starvation verdict
stops being believed 7 days after the stall that recorded it, so one bad
evening does not cap a link permanently.

Priors are node-local. In a Hiqlite cluster they live in the voter's own
sidecar and are not replicated, so a client that reaches a different node
starts cold there. That is accepted, not a defect.

The `/24` is taken from `Forwarded`, `X-Forwarded-For`, or `X-Real-IP` when
present, and otherwise from the socket peer. plurx does not maintain a
trusted-proxy list, so behind a reverse proxy that does not overwrite those
headers an authenticated client can choose which of its own buckets it lands
in. It cannot reach another user's rows — the user comes from the session, not
the header — so the effect is bounded to that user churning their own 64.

Note on `telemetry.retain_days = 0`: the two switches are independent on
purpose, so an operator can keep the small aggregate prior without retaining
raw playback rows. With priors enabled, `0` still folds each client report into
its prior row and still schedules the daily prune pass for those rows; what it
disables is raw playback-row writes and their retention.

The scheduler runs one bounded prune pass daily and records its clock in
`jobs.last_telemetry_prune`. Administrators can query newest-first rows with
`GET /api/v1/system/playback-events?since=<unix-ms>&event=<name>&limit=<n>`;
the limit is capped at 2,000. Settings → System shows a small seven-day TTFF,
stall, and suspended-time summary. `scripts/perf-report` uses the same endpoint
and falls back to the log ring when pointed at an older server.

### Quality-bounded transcodes

N1 adds two admin runtime settings. `transcode.rate_mode` is `bitrate` by
default and preserves the pre-N1 encoder arguments and cache identity.
`quality` requests the family-specific quality mode below. An optional
`transcode.quality` integer overrides the family default; JSON `null` clears
that override. The server validates the complete production argument list on
the real driver before atomically storing the pair and publishing it to new
sessions.

| Encoder family | Effective quality arguments |
|---|---|
| Software | `-crf q`, with the existing max-rate and buffer caps |
| Intel QSV | `-global_quality q`, with target bitrate and caps |
| VA-API | `-rc_mode QVBR -global_quality q`, with target bitrate and caps |
| NVIDIA NVENC | `-rc vbr -cq q`, with target bitrate and caps |
| Apple VideoToolbox | `-q:v q`, with target bitrate and caps |

The request and the effective result are deliberately different facts. A
driver that refuses its quality arguments remains usable for bitrate mode; new
sessions on that family fall back to VBR and the boot/runtime log names the
refusal. `GET /api/v1/system` exposes the boot-probed per-family verdicts under
`encoders.quality_rc`. `GET /api/v1/settings` returns the requested mode and
override, not proof that a particular session executed quality mode. Confirm
that from the behavioral-probe log plus a production session's ffmpeg
arguments during the named-machine acceptance run.

```bash
# Request one explicit calibration candidate.
curl -fsS -X PUT \
  -H "Authorization: Bearer $PLURX_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  --data '{"transcode_rate_mode":"quality","transcode_quality":22}' \
  http://nynuc:32400/api/v1/settings

# Return to the byte-for-byte legacy path and clear the override.
curl -fsS -X PUT \
  -H "Authorization: Bearer $PLURX_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  --data '{"transcode_rate_mode":"bitrate","transcode_quality":null}' \
  http://nynuc:32400/api/v1/settings
```

QSV's built-in value is **22**, selected by the 2026-08-14 nynuc D5 sweep.
Values 21 and 22 produced byte-for-byte identical QVBR captures on both corpus
halves; 23 was the first failing value because the easy clip grew by 940 bytes
and lost 0.004328 VMAF. Choosing the highest passing value keeps the most
compression headroom without crossing either binding gate. The other family
defaults remain calibration candidates until the corpus runs on hardware that
can select them; a successful 15-frame validation probe is capability evidence,
not D5 calibration.

The q=22 result is deliberately modest: easy-content bytes fell from
31,764,104 to 31,763,916, while the hard clip gained 0.020034 VMAF and grew
0.53%. It proves bounded flat-quality behavior on nynuc; it does not claim the
plan's expected 10–35% typical-content savings from this two-clip corpus.
Effective VBR retains the literal legacy recipe bytes; each effective quality
value has a distinct recipe identity, so a requested mode that falls back
cannot poison the cache.

Both rate-control fields are required whenever either is updated; the server
stores them as one pair. In a cluster the requested pair replicates, while a
two-second background loop on each node validates changes against that node's
encoder. Runtime probes share the serialized offline/speculative encoder lane,
reserve background admission, and yield to the existing viewer/offline priority
signals: they do not start while higher-priority work is waiting, and an
in-flight probe is killed and retried if a viewer or offline job arrives. Each
family probe also has a three-second deadline; a wedged child is killed and
reaped instead of retaining the lane or request indefinitely. The previous
validated snapshot stays active; timeout or temporary contention is never
recorded as an unsupported driver. A direct quality-mode PUT that cannot obtain
a safe probe window returns HTTP 409 and writes neither field, so retry it after
the node is idle. Session creation does no store read or probe: it copies the
latest in-memory snapshot. A live session keeps that captured setting
generation even if it later falls back to software.

An absent or empty stored quality means the valid per-family default. A
nonempty value that cannot parse as the bounded integer marks the durable pair
corrupt; boot, refresh, and `GET /settings` all report/use legacy bitrate with
no override rather than silently turning corruption into default-quality mode.

Live sessions, speculative production, and newly requested offline packages
use the validated effective identity. An offline package stores exactly `vbr`
or `qvbr:<q>` when it is created and reuses that value after every yield or
restart; it never reads the current global pair on resume. SQLite v18 adds the
non-null column and backfills existing packages to `vbr`. Replicated schema v5
has the same column while retaining protocol v4. N3's cache-artifact migration
therefore moves to SQLite v19. Production opens the activated one-voter
Hiqlite backend after fresh bootstrap/import; retained SQLite is only the
pre-membership rollback source. N1 does not invent the cluster
rolling-migration protocol: an existing replicated v4 cluster remains
incompatible and must not be pointed at this binary.

The effective value is server policy, not a client request option. The first
accepted `request_id` owns it: retrying the identical create after a global
rate-control change returns the original package and original snapshot.

Deploy this schema-bearing N1 server change to the fleet as one coordinated
maintenance operation through `scripts/ship`, which takes the documented
pre-deploy SQLite backups. The migration is additive and does not rewrite or
delete media, but an older binary refuses the newer database. A rollback is
therefore the standard restore of each node's matching pre-deploy database
snapshot plus its old binary, not a code-only downgrade.

### Rate-control acceptance captures production, then scores offline

Performance II N1 is judged on bytes created by the deployed server, not by a
second copy of its encoder arguments. Run `scripts/bench rate-control` on a
controller that can reach nynuc. The harness opens real plurxd HLS sessions;
the server's configured production Jellyfin FFmpeg and hardware encoder create
every segment. It rejects cached VOD and copy sessions, and it refuses to start
or mutate global settings unless the immediately preceding `/system` and
`/activity/detail` responses show no transcode, delivery, speculative producer,
or offline work. During capture, every poll must show exactly the harness-owned
HLS session as the sole session and delivery. Reserve nynuc as a maintenance
node and stop playback for the whole run. The repeated checks catch competing
work visible at a boundary, but the GET and following POST/PUT are not atomic.
Reserved-node exclusivity is the operational lock.

The controller saves each served segment under a generated local filename,
ends the production session, and only then invokes the explicit
`--vmaf-ffmpeg` to decode the capture and reference for scoring. That separate
FFmpeg is scoring-only: never set it as `PLURX_FFMPEG`, add it to the compose
service, or make it a production/live-path dependency. Ordinary `ffmpeg` is the
CLI default only when its requested model passes the executable `libvmaf`
behavior probe with exactly one finite score. No scorer is installed or needed
on nynuc or in compose. Nynuc performs production encoding; the accepted laptop
controller scorer runs only after DELETE succeeds and `/system` confirms the
node returned to zero active transcodes.

Build the standard fixtures first, scan that directory as a plurx library, make
the same files available as local controller references, stop all playback,
and run the current-main VBR smoke:

```bash
scripts/bench fixtures --dir bench-media  # builds the standard fixture matrix
```

Pass `--library <id>` when the server has more than one library; omission is
accepted only when `/libraries` returns exactly one entry. The library must be
the one that scanned the fixture bytes whose local copies are used as VMAF
references. Set `PLURX_FIXTURE_LIBRARY_ID` to that numeric ID for the commands
below.

```bash
scripts/bench rate-control \
  --base http://nynuc:32400 \
  --token "$PLURX_ADMIN_TOKEN" \
  --library "$PLURX_FIXTURE_LIBRARY_ID" \
  --corpus scripts/perf2-rate-control-smoke-corpus.json \
  --modes vbr \
  --vmaf-ffmpeg /opt/homebrew/Cellar/ffmpeg/8.1.2_1/bin/ffmpeg \
  --vmaf-model vmaf_v0.6.1 \
  --json out/rate-control-vbr.json
```

N1's full comparison uses
`scripts/perf2-rate-control-n1-corpus.json`. It pins the standard
`1080p-h264.mkv` easy fixture and `grainy.mkv` hard fixture by SHA-256; the
relative controller references resolve to `bench-media/` created by the
fixture command above. Every full manifest reference must carry its actual
SHA-256, `dynamic_range: sdr`, and one of equal, nonempty `easy` and `hard`
halves. Every fixture must have a unique server filename, resolved controller
path, and pinned SHA-256. Relabeling the same clip as both easy and hard is not
a corpus. `scripts/perf2-rate-control-smoke-corpus.json` remains VBR-smoke-only
and is not D5 calibration or full acceptance.

Capture server hashes from the exact files in nynuc's fixture library, then
produce the same ordered list locally and compare it before running. Replace
the server directory below with the library's real path; do not hash a second
copy merely because it has the same filename:

```bash
mkdir -p out

ssh nynuc \
  'cd /mnt/qnap/media/plurx-perf2/bench-media && sha256sum -- 1080p-h264.mkv grainy.mkv' \
  > out/nynuc-perf2.sha256

(
  cd bench-media
  shasum -a 256 1080p-h264.mkv grainy.mkv
) > out/laptop-perf2.sha256

diff -u out/laptop-perf2.sha256 out/nynuc-perf2.sha256
```

An empty `diff` is the preflight. The harness then parses the nynuc file
fail-closed, records its own SHA-256, and requires every server filename digest
to equal the pinned local-reference digest. It also records the corpus
manifest's SHA-256 and requires every server file to be probed, available, and
backed by usable duration/video facts before a null HDR field can prove SDR.
Missing or false scan state, unusable video facts, unknown HDR facts, and any
non-SDR delivery fail.

```bash
scripts/bench rate-control \
  --base http://nynuc:32400 \
  --token "$PLURX_ADMIN_TOKEN" \
  --library "$PLURX_FIXTURE_LIBRARY_ID" \
  --corpus scripts/perf2-rate-control-n1-corpus.json \
  --server-sha256-manifest out/nynuc-perf2.sha256 \
  --modes vbr,qvbr \
  --vmaf-ffmpeg /opt/homebrew/Cellar/ffmpeg/8.1.2_1/bin/ffmpeg \
  --vmaf-model vmaf_v0.6.1 \
  --vmaf-subsample 1 \
  --rate-window 10.0 \
  --poll 0.25 \
  --capture-timeout 180.0 \
  --idle-timeout 30.0 \
  --settings-settle 3.0 \
  --json out/rate-control-final-default.json
```

The command above is the binding deployed-default run. During calibration,
repeat it with explicit `--quality` candidates, select the highest passing
value, update that family default, deploy it, and then run the command exactly
as shown without `--quality`.

Omitting `--quality` explicitly sends and verifies
`transcode_quality: null` for both captures; it does not preserve a preexisting
override. The original pair is still restored in the final cleanup path.

The 2026-08-14 nynuc QSV acceptance used deployed build
`v0.2.7-167-gb6aaed6` and selected default 22. The no-override run passed with
these results:

| Fixture | Mode | Bytes | VMAF | Speed p10 | Binding 10 s peak |
|---|---:|---:|---:|---:|---:|
| easy 1080p H.264 | VBR | 31,764,104 | 55.443244 | 14.434x | 8,555.504 kb/s |
| easy 1080p H.264 | QVBR | 31,763,916 | 55.443244 | 15.392x | 8,555.354 kb/s |
| hard 1080p grain | VBR | 31,119,452 | 49.597362 | 12.653x | 8,349.456 kb/s |
| hard 1080p grain | QVBR | 31,284,516 | 49.617396 | 12.467x | 8,717.184 kb/s |

The final JSON SHA-256 is
`88e47f2bb20cf166d61f4470419f9ff7c3ea966a77b1f701b909b6952b60cbba`.
It records `transcode_quality: null` for both modes, the exact measurement
parameters above, the deployed build, and stable Intel QuickSync identity.
The production log ring contained exactly four matching new-build argument
lines: two legacy VBR and two QVBR with `-global_quality 22`. The isolated
forced-fallback proof SHA-256 is
`4b5efac48d77459cddf20894bbdc2318cda1dad88745e3d5a9e12c2216d715ab`.
Afterward the node reported zero work with `bitrate` and a null quality
override.

Full comparison accepts exactly model `vmaf_v0.6.1`, `--vmaf-subsample 1`, a
`10.0`-second served-segment window, `--poll 0.25`, and
`--settings-settle 3.0`. Subsampling can conceal a short quality regression; a
different poll can miss a slow speed sample; and a different settle delay can
change which post-update state gets measured. Any deviation is
VBR-diagnostic-only. A longer rate window can dilute a 10-second burst. The
capture/idle timeouts may vary under the plan, but the artifact records them.
They can only refuse an incomplete or unsafe run; they do not shorten the fixed
source-timeline trim that is scored.

The harness sends the complete rate/quality pair on every update and records
only those verified fields from the settings response. Its
`finally` path stops starting new `/system` polls when the local
`--idle-timeout` deadline is reached and restores only after a completed
response reports zero. Each sleep is capped at the deadline's remaining time.
The deadline bounds time between completed responses; one HTTP request already
in flight can add up to `Api.call`'s 30-second timeout. Immediately before the
automatic PUT, the last completed response must report zero, but that
GET-to-PUT gap is not race-free and reserved-node exclusivity is still
required. If the deadline expires, the run fails with
`setting_restore_failed` and may leave the last requested mode active. The failure's
`required_manual_restore` object contains only the original rate and quality.
It never writes the token, the full settings response, or another
setting/secret to the artifact.

After stopping playback, apply a reported manual rollback exactly once. The
first request must return `true`; the second must return a settings response
whose two fields equal the rollback body:

```bash
rollback_body="$(jq -c \
  '.failures[] | select(.code == "setting_restore_failed") | .required_manual_restore' \
  out/rate-control-full.json)"

curl -fsS \
  -H "Authorization: Bearer $PLURX_ADMIN_TOKEN" \
  http://nynuc:32400/api/v1/system \
  | jq -e '.active_transcodes == 0'

curl -fsS -X PUT \
  -H "Authorization: Bearer $PLURX_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  --data "$rollback_body" \
  http://nynuc:32400/api/v1/settings \
  | jq --argjson expected "$rollback_body" -e \
      '.transcode_rate_mode == $expected.transcode_rate_mode and
       .transcode_quality == $expected.transcode_quality'
```

Do not retry the PUT until the system check is true. The manual GET and PUT are
also not atomic, so keep the node reserved through the response verification.
An active viewer is the reason automatic restoration stopped.

The manifest is fixture identity, not an encoder recipe:

| Manifest field | Meaning |
|---|---|
| `version` | Schema version; N1 begins at `1` and rejects anything else |
| `purpose` | `vbr_smoke` or `n1_acceptance`; full `vbr,qvbr` mode refuses a smoke corpus |
| `fixtures[].identity` / `class` | Stable safe name and `easy`/`hard`; only easy content carries the bytes-must-not-grow gate |
| `dynamic_range` | Must be literal `sdr`; the server file fact and delivered session must agree |
| `filename` / `reference` | Server-visible filename and the controller-local identical source; relative references resolve beside the manifest; full acceptance requires each filename and resolved path to be unique |
| `reference_sha256` | Optional pinned lowercase SHA-256; every run records the actual local SHA; full acceptance requires unique pins and matching local/server bytes |
| `trim` | Source-timeline start and capture duration; `StartResponse.media_origin_ms` is authoritative and must agree within 50 ms. A legacy transcode response without that field falls back to the manifest start and records `media_origin_source: manifest_start_legacy_transcode`; copy and cached sessions are already refused before that fallback |
| `rung` | Requested production output height |

The controller preflights free disk and removes each capture immediately after
scoring. `--keep-artifacts` retains generated playlists/segments for diagnosis;
`--work-dir` chooses scratch. `--only` is diagnostic: the JSON marks it as a
subset, forces `passed: false`, and exits nonzero even when its measurements
look good. A retained fixture/mode directory is never overwritten.
`--vmaf-subsample N` scores every Nth frame for diagnostics; full comparison
requires `N = 1`. Stable JSON records the VMAF model, subsample, rate window,
poll interval, capture timeout, idle timeout, and settings-settle delay, so
measurement timing cannot disappear from review.

VMAF is comparative at one delivered rung in this harness. The filter graph
uses `scale2ref` to scale the source reference down to the captured stream's
distorted resolution, then scores both there. That is a no-op for the checked-in
1080p-to-1080p smoke and is valid for VBR/QVBR comparisons forced onto the same
rung. It is not an industry-style absolute score at source/viewing resolution,
and scores from different rungs are not comparable. Do not calibrate D5 from a
sub-source-rung absolute score until the plan explicitly chooses whether to
upscale the distorted stream to the source/viewing resolution; this harness PR
defers that normalization decision rather than silently baking in an offset.

The JSON keeps the evidence roles distinct: controller/scorer host, production
server/build/Jellyfin-FFmpeg/encoder identity, and scoring-only FFmpeg
path/build/configuration/hash/model/filter fingerprint. Each capture requires a
stable status encoder matching StartResponse and `/system.encoder_selected`;
the two modes must also use the same encoder per fixture. This prevents a
stable software fallback from masquerading as nynuc QSV evidence. VBR and the
requested quality-mode capture must also advertise identical requested/rung
height, total, peak, and derived nominal/audio/max/buffer facts. An inflated
quality-mode cap is not an equal comparison. Peak rate uses the same
complete-served-segment rolling windows as `scripts/perf-report`; incomplete
tail windows are ignored.

The owner ratified the 10-second complete-served-segment gate on 2026-08-12.
For each VBR and requested-quality capture, the observed peak over exactly
`10.0` seconds must be no greater than the unchanged advertised
`Rung.peak_kbps`. Ten seconds spans five nominal two-second HLS segments and is
stable under this served-segment measurement. The derived
`video_bufsize_kbits / video_maxrate_kbps` window is about 1.33 seconds under
the current caps, shorter than one nominal segment; its shortest qualifying
complete-segment observation remains useful diagnosis but is not a portable
binding gate. The JSON also keeps the theoretical 10-second
`maxrate + bufsize/window` allowance separate and nonbinding.

The full comparison also checks that
both modes supplied finite, strictly positive server-speed p10 measurements
and that the requested quality-mode capture did not lower VMAF, grow bytes on
easy content, or lose more than 10% server encode speed. It does not prove QVBR
flags executed. Failures are explicit
`vmaf_regression`, `easy_bytes_regression`, `speed_invalid`,
`speed_nonpositive`, `speed_regression`, `encoder_identity_mismatch`,
`ladder_identity_mismatch`, `duration_mismatch`, `peak_evidence_invalid`,
`advertised_peak_exceeded`,
`diagnostic_subset_not_acceptance`, `harness_error`, or
`setting_restore_failed`.
Missing `libvmaf` fails; it never skips quality scoring. A quantitative harness
run proves settings acknowledgement and measured output only after every gate
passes; it does not prove that the intended encoder flags
or forced fallback executed. N1's production tests and boot validation must
prove settings-to-flags behavior, and the plan's separate forced-fallback run
must supply that evidence. TTFS is not N1 evidence. Keep the JSON as the PR
artifact, and do not claim the named-machine full comparison without the actual
run.

### Where the transcode scratch lives

Session segments are written under `<data_dir>/transcode`, or under
`storage.transcode_dir` when that key names a root outside `data_dir`. Either
way it is wiped at every start. Two consequences worth acting on:

- **Keep the data directory off the NAS.** If `PLURX_DATA_DIR` sits on a
  network mount, every segment crosses the network twice — written by ffmpeg,
  read back by the HTTP handler — on top of reading the source. Local disk for
  the data directory, network mounts for media only.
- **tmpfs is a good fit if you have the RAM.** The buffer limit bounds a
  session, so the size is predictable: roughly `(ahead limit + 60 s) ×
  bitrate`. At the 180 s default that is well under a gigabyte for a 720p
  transcode and around 1.5 GB for a 4K copy-video session. Size it for the
  concurrent sessions you expect, or lower the buffer limit to fit. **Mount it
  with `-o uid=<daemon-uid>,mode=0700`.** A default tmpfs is `root:root` mode
  `1777`; under a non-root daemon that is a foreign owner and startup refuses
  it, and under a root daemon plurx repairs the mode to `0700` and warns on
  every boot. The fstab form carries the same `uid=`, `gid=`, and `mode=0700`
  options:

  ```
  tmpfs /srv/plurx-state/transcode tmpfs rw,uid=1000,gid=1000,mode=0700,size=8G 0 0
  ```

For media on NFS or SMB, the head start is read from the NAS at whatever rate
the mount can serve, so a starved mount now shows up as an encode speed below
the configured pace in the stats overlay rather than as an unexplained stutter.
Larger read sizes (`rsize=1048576` on NFS, SMB3 multichannel where the NAS
offers it) help the burst land quickly.

### Playback cache protection and a temporarily soft ceiling

The pre-transcode budget never wins over an active viewer. A foreground
transcode start first announces itself, then waits up to five seconds for any
speculative or offline encoder to checkpoint and terminate before it starts
its own ffmpeg. This handoff applies across both hardware and software: a
numerically free hardware slot or spare CPU threads do not authorize overlap
with background work. Once admitted, the viewer's permit keeps background
hardware and software work parked until the live session stops, is superseded,
fails, or changes pools during hardware-to-software fallback. The permits are
RAII guards, so every one of those exits returns ownership without a separate
counter-cleanup branch.

If a background encoder does not release within the five-second admission
window, the start returns HTTP 503 with `transcode capacity is temporarily
unavailable`; retrying is safe. Cached playback and copy/remux sessions do not
need an encoder permit and never pay this wait.

A cached HLS session protects its recipe from LRU, stale-claim, and orphan
cleanup until the last reader leaves; a later enabled sweep may then reclaim
it. This can hold the cache above `cache_max_gb` temporarily, which is safer
than turning the next playlist or segment request into a 404.

Sweeps run only when `transcode_cleanup_mins` or `cache_produce_mins` is set;
both default to `0` (off). With both off, nothing reclaims playback-cache
entries automatically. Set at least one interval before treating “the next
sweep” as an operational guarantee.

Look for `cache: swept` in Settings → Logs. `protected > 0` means the sweep
attempted an eviction and was refused by an active reader; it is a per-sweep
skip count. The Prometheus gauge
`plurx_cache_protected_entries{reason="active_playback"}` instead reports every
recipe currently held by a live cached session, whether or not housekeeping
tried to evict it. A persistent non-zero gauge with no active sessions means
the session reaper is stuck. Low free space plus a non-zero `protected` sweep
count means stop new cache production and let the viewer finish before
reducing the budget.

Ownership is process-local. It coordinates handlers and housekeeping inside
one `plurxd`; two processes pointed at the same data directory have separate
registries and are unsupported because one can delete bytes the other is
serving.

### Distributed speculative production

When `cache_produce_mins` is non-zero, one cluster lease owner ranks Continue
Watching, Next Up, and Recently Added candidates and enqueues immutable source
generations. It does not encode them. The generation identity includes the
requested encoder and rate-control policy plus normalized audio language,
subtitle language, and subtitle mode. Every voter with a configured local
cache then competes for a distinct compatible row, so three idle workers can
prepare three titles at once without three schedulers selecting the same work.

Workers advertise the capabilities the local daemon actually proved at boot.
A row that needs an unsupported decoder, encoder family, HLS output contract,
tone-map path, output grade, or scratch budget stays queued instead of failing
on the wrong node. Decoder names come from this ffmpeg's boot inventory;
scratch is current free space on the cache filesystem after a 512 MiB safety
reserve, compared with a full-title peak-output estimate plus 64 MiB overhead.
Dolby Vision remains on the live path because its RPU renderer requires a more
specific proof than the queue's ordinary HDR bit. After claiming, a worker
opens and stats the source snapshot before starting ffmpeg. A node without that
mount yields without consuming a shared failure attempt and temporarily
excludes that job only from its own claims, so a mounted peer remains eligible
immediately. A claim lasts 30 seconds and renews every 10 seconds. A renewal
response at or after the predecessor's exact expiry self-fences even if the
backend committed it. If a worker dies, another voter may take over after
expiry; because staging is local, that voter starts the title from zero. A
yielded job reclaimed on the same node resumes its numbered parts. Foreground
playback still has priority and receives the encoder lane inside the existing
five-second admission window.

The queue admits at most 4,096 active and 10,000 total rows. Each candidate
pass removes up to 512 ready rows whose local location has been evicted and
retains only the newest 4,096 failed/cancelled dedupe tombstones. This bounds
Raft history while preserving recent terminal suppression; terminal rows
discard their capability/policy and staging payloads. Capability scans
page in groups of 128 until they find the highest compatible row; a large band
of GPU-specific work cannot starve a software-capable title behind it.
Worker polling measures current free space and makes one cheap claim; it does
not run a cache walk on every empty-queue poll. Each producer-enabled node also
rate-limits one bounded local cache sweep to every 15 minutes, even while its
queue is empty; the normal cleanup schedule remains an independent backstop.
Both paths recognize queue staging and fenced final-directory syntax, so
abandoned queue bytes remain reclaimable after restart even when there are no
cache-location rows. Rename-to-publication holds both the recipe eviction
guard and final-directory orphan guard until fenced completion.

The cache bytes remain node-local. With P5 remote placement enabled, any
ingress may select a voter that advertises the exact verified generation and
proxy that worker's session, so a completed title is no longer useful only
when the client happened to reach its holder. Enablement is still explicit:
`PUT /api/v1/settings` with
`{"cluster_media_pool_enabled": true}` succeeds only when every committed
voter has a fresh current-protocol snapshot, and `GET /api/v1/cluster/media`
separates enabled, rollout-ready, and effective-ready state.

P7 can replace an expired HLS owner without changing the public session URL.
Keep this second rollout gate off until P5 placement is healthy, then enable it
with `PUT /api/v1/settings` and
`{"cluster_session_takeover_enabled": true}`. Enabling is refused unless remote
placement is enabled and every committed voter publishes the current media
protocol; disabling always succeeds. A replacement resumes behind the last
fetched frontier with bounded overlap, advances the owner epoch, and inserts an
HLS discontinuity before publishing new segments. Direct-play range requests
remain stateless and continue through any healthy ingress without this worker
replacement path.

Three operator-visible consequences of enabling it. Sessions created while the
gate is on serve a playlist with no `EXT-X-PLAYLIST-TYPE`, because a
replacement generation cannot satisfy EVENT semantics and the shape must not
change under a player mid-film; this is the same shape the
`hls.typeless_sliding` experiment serves. A replacement's segment numbers jump
— each ownership epoch owns a range a million wide, so a first failover begins
at `seg2000000`. And sessions that were already playing when the gate was
turned on are **not** covered: they were created serving EVENT, so a takeover
refuses them and those viewers restart exactly as they would have before. None
of the three indicates retention pressure or a renumbering bug.

Expect recovery on the order of **fifteen to twenty seconds**, not the ten the
plan's acceptance names. Nothing contests a session until its lease expires,
and the media-session lease is twelve seconds with a three-second renewal
(`LEASE_TTL_MS`, `LEASE_INTERVAL`); the contest tick and the takeover deadline
sit on top of that. Recovery is bounded and correct at these values, just not
fast — closing the gap is tracked in `docs/CLUSTER-MEDIA-POOL-PLAN.md` §8.8.

#### Optional verified shared cache

P6 adds a direct shared-cache fast path without making it a cluster
requirement. Local cache and P5 owner routing remain complete fallbacks. Use
the shared path only when all participating voters mount the same writable
filesystem; never point two daemons at one ordinary node-local cache directory.

Configure both node-local values on every participating voter. Paths may differ
between hosts, but the id must describe the same underlying filesystem:

```toml
[cluster]
shared_cache_dir = "/srv/plurx-shared"
shared_cache_id = "media-cache-a"
```

The equivalent container variables are `PLURX_SHARED_CACHE_DIR` and
`PLURX_SHARED_CACHE_ID`. Create and mount the directory before starting plurx,
and give the daemon uid permission to create, rename, read, and remove entries.
The id is combined with the durable cluster identity, so the same operator name
in two unrelated clusters does not alias replicated cache state.

Startup does not trust matching configuration. Every committed voter must pass
an authenticated two-way canary: each node writes unpredictable bytes, a peer
reads them and writes a response, and the origin reads that response back. Only
then does the node classify completed portable generations as shared. A
one-voter cluster performs the same write/read proof locally. Missing mounts,
read-only mounts, different filesystems, identity mismatches, and unreachable
voters leave the fast path unavailable while P5 local-holder routing continues.

Portable, manifest-fenced speculative generations are copied into immutable
shared generation directories after local publication succeeds. A nonproducer
may then serve the verified generation directly instead of proxying through its
producer. Every requested manifest and object is still authenticated; the
canary proves common writable storage at admission time, not permanent byte
integrity.

Media sessions, ready offline packages, and active offline downloads hold typed
replicated pins for the exact storage/recipe/generation identity. Session pins
renew in the existing owner-liveness batch rather than per segment. Shared GC
requires the exact `shared-cache-gc:<storage_id>` lease and retires the pointer
only when no unexpired pin exists in that same transaction. Retirement first
turns the row into a non-readable cleanup tombstone, then quarantines and
deletes the deterministic generation path, and finally removes the tombstone.
A crash at any boundary leaves bounded retryable state for the next lease owner.

Any runtime shared-root `ENOENT`, I/O, manifest, or identity failure immediately
marks this node's proof suspect and disables shared classification. Look for
`shared cache proof lost; falling back to node-local holders`. The local ready
generation remains usable when shared publication fails, and a later successful
canary readmits the shared path. The two TOML keys are forward-compatible and
already-running mixed-version P5 nodes ignore them. That is not a database
downgrade guarantee: once a restarted P6 node advances SQLite to v26 or the
replicated cluster schema to v11, a P5 binary refuses that newer schema. Roll
forward, or restore the matching pre-P6 database snapshot with the older binary;
do not attempt a code-only downgrade.

Operational evidence is available in Settings → Activity and Logs:

| Evidence | Meaning |
|---|---|
| `queued speculative transcode` | The singleton scheduler inserted a new immutable generation |
| `distributed speculative transcode ready` | This node atomically published a complete local generation |
| `pre-transcode queue job lost its fence` | Renewal failed; the child self-preempts and cannot publish |
| `producer_candidates` telemetry | Counts enqueued, deduplicated, missing, and rejected candidates |
| `producer_pass` telemetry | Counts completed, yielded, capacity-delayed, lease-lost, and failed work |

Every distributed generation has `generation-manifest.json`. Starting a cache
hit authenticates that bounded manifest without walking the film; each playlist
or segment request authenticates the exact bytes or open file it returns. A
corrupt or unlisted requested object fails closed. The fenced manifest digest
is stored on the cache-location row and must match the manifest before a cache
start, so a self-consistent replacement is not trusted.

Scheduled cache cleanup also scrubs unrequested objects. One pass considers at
most 128 locations. It first performs a descriptor-bound manifest-presence
heartbeat for broad holder freshness, then deep-verifies up to 4,096 objects
under a two-second wall-clock ceiling and a physical-read ceiling of 132 MiB
plus two EOF-probe bytes: one 128 MiB object · one 4 MiB manifest · one probe
for each. The per-location object cursor is durable, and a location that
consumed deep I/O sorts behind presence-only peers on the next rotation. One
maximum-sized object therefore cannot starve every later generation.

Cursor and heartbeat updates across a pass use one backend
transaction/consensus entry. The scrubber holds a reader guard, checks for
same-title foreground readers between objects, and yields that title when
playback arrives; unrelated playback does not stop integrity work for the
rest of the cache. On corruption it retires only the exact location row;
guarded orphan cleanup owns the later directory removal. Unsafe relative paths
are invalidated without touching the filesystem. In `cache: swept`, `corrupt`
counts retired generations and `scrub_bytes` is physical manifest/object data
read during this pass. Scheduled deep scrubbing is opportunistic, not a promise
that every object was checked within a fixed time. Requested playlists and
segments are always authenticated before they are returned.

Ready offline downloads use the same manifest contract. If their exact cache
generation fails an integrity check, the location is retired and the package
changes to failed with `cache_integrity`; prepare it again. A request racing a
newer replacement may fail itself, but its stale identity cannot retire the
new package generation. Legacy and non-queue cache entries have neither an
authoritative digest nor a scrub cursor and remain serveable as the explicit
legacy path.

### Offline package storage and quotas

Phone downloads are prepared beside finished content-addressed transcodes:
under `<data_dir>/cache/transcode`, or `<cache_dir>/transcode` when
`storage.cache_dir` is set. Ready and in-progress offline recipes are pinned:
ordinary playback-cache cleanup cannot evict them, and their bytes do not
consume `cache_max_gb`. Give whichever root holds that tree enough local space
for both budgets; unlike session scratch, this cache must survive a daemon
restart and must not live on tmpfs.

The authenticated settings API exposes four operator controls:

| Field | Default | Meaning |
|---|---:|---|
| `offline_enabled` | `true` | Kill switch. Turning it off cancels active producers at a segment boundary, leaves their durable rows queued, and returns `503 offline_disabled` from every leased media route until it is re-enabled. |
| `offline_max_gb` | `25` | Per-node reservation ceiling for offline packages. `0` disables offline admission. |
| `offline_max_gb_per_user` | `15` | Per-user reservation ceiling. `0` disables offline admission. |
| `offline_max_rows_per_user` | `50` | Per-user package count. `0` disables offline admission. |

Creation reserves a conservative peak before ffmpeg starts, so a full queue is
rejected early instead of filling the cache halfway through a title. Completed
packages and their leases expire after seven inactive days; active media reads
renew the same lease. Deleting a download from a signed-in profile removes the
server package best-effort and always removes local device bytes. The queue
takes turns across profiles instead of draining one profile's backlog first.
An administrator can stop one visible preparation or transfer from Settings →
Activity without disabling offline work for everyone else. Look for
`offline package ready`, `offline preparation failed`, and
`offline expiry sweep failed` in Settings → Logs when diagnosing preparation.

## Pairing another application (monarr) — the runbook

Another application can ask plurx to index exactly the folder it just wrote,
instead of plurx finding it on the next scheduled sweep. Three steps.

**1. Issue a key.** Settings has no keys screen yet (it lands with the rest
of this work), so today it is one call with your admin token:

```bash
curl -sX POST http://plurx:32400/api/v1/keys \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"name":"monarr","scopes":["scan:trigger","status:read"]}'
```

The response carries `key_secret` — `plx_…` — **once**. Copy it now; it is
not stored in a form anyone can read back, including you. Losing it means
issuing a new key, which is the correct cost of never storing it.

**2. Check what it can do.** `scan:trigger` asks for scans; `status:read`
reads the result of one. A key holds exactly what you gave it: it cannot read
`/api/v1/settings` (where the TMDB and Trakt secrets live), manage users,
stream media, or mint another key. That is the entire reason keys exist
rather than "give it an admin account" — see [SECURITY.md](SECURITY.md).

**3. Point the other application at it** — plurx's URL and the `plx_` secret.

### What a scan request looks like

```bash
curl -sX POST http://plurx:32400/api/v1/scan \
  -H "Authorization: Bearer plx_…" -H 'content-type: application/json' \
  -d '{"path":"/media/movies/Heat (1995)","ids":{"tmdb":949},
       "correlation_id":"t-42-a3f9c1","source":"monarr"}'
```

**How to read the answer:**

- `200 {"status":"scanned", "report":…, "items":[{item_id,file_id,path}]}` —
  it ran now. `items` is what the path became; `report` counts what changed.
- `202 {"status":"queued","request_id":"sr-…"}` — that library was already
  scanning, so the request is queued rather than dropped. Poll
  `GET /api/v1/scan/requests/sr-…`. This is normal, not a warning: a season
  import fires one request per episode within seconds.
- `422 {"error":"path is not under any library root","roots":[…]}` — **the
  common one.** The path plurx was handed is not inside any library it knows
  about, and the body lists the roots it checked. Nearly always a container
  path-mapping mismatch: the other application says `/data/media/...` and
  plurx has that same folder mounted at `/media/...`. Fix the mapping on the
  sending side; nothing is wrong on plurx's.
- `401` — no key, an unknown key, a revoked key, or a *user token* (user
  tokens do not open this route by design). `403` — a real key without the
  scope.

`correlation_id` is echoed back, recorded on the request, and logged under
the `plurxd::integrate` target, so one `grep t-42-a3f9c1` across both
applications' logs reconstructs the whole transfer.

**Where to look afterwards.** Settings → the Server card carries
`scan_requests`: the recent asks, what they were for, and what came of them.
If it is empty, the other application has never successfully reached plurx —
check its URL and key before looking at anything else.

**What a targeted scan will never do:** remove anything. It indexes what it
finds under the path and touches nothing else, because it only saw one
folder — pruning against that view would delete the rest of the library. The
scheduled scan (per library, Settings → interval) remains the thing that
notices files that vanished.

**What the `ids` do.** They are not decoration. An item that arrives carrying
a `tmdb` (or `imdb`) id is enriched **by that id** — plurx fetches the detail
record directly and never runs a title search. That matters because the title
search is the step that can be wrong: `Heat (1995) Directors Cut Remux` is a
folder name, and a search for it can land on a different film. A wrong match
does not stay local either — Trakt sync matches on TMDB id, so it propagates.
If the sending application knows the id, sending it is strictly better than
not. Items sent without ids are matched by title, exactly as a manual scan
would do.

An item can also arrive with an id *before* it has any metadata, and that is
normal: the id says what it is, and enrichment then fills in title, overview
and artwork on the same pass. An item that keeps its filename as its title
means enrichment has no TMDB key configured — the scan itself succeeded.

## Logs

Structured `tracing` logs are split by job. Settings → System shows the general
2,000-line process-local ring with a level filter (`info` / `warn` / `error` /
`debug`) and auto-refresh. Settings → Cluster has a separate ring for
membership, Hiqlite, and Raft detail, so replication traffic cannot evict the
playback or library line you are looking for. General events and cluster
warnings/errors go to stdout/journald; lower-severity cluster detail stays in
the Cluster tab. Both rings obey `PLURX_LOG` and disappear at restart.

Each line is `time  level  target — message`. The general targets that matter
most are `plurxd::scan` (library passes), `plurxd::meta` (provider matches),
`plurxd::transcode` (encoder selection + why a hardware path was rejected), and
`plurxd::stream` (session lifecycle). Raise verbosity for one subsystem with
`PLURX_LOG=plurxd::transcode=debug`; use
`PLURX_LOG=info,openraft=debug,hiqlite=debug` when the Cluster log needs Raft
detail.

## Health & metrics

| Endpoint | Use |
|---|---|
| `GET /healthz` | Liveness — the process is up |
| `GET /readyz` | Readiness — storage is reachable (use for load-balancer health) |
| `GET /metrics` | Prometheus text: process, playback, storage, Raft progress, and cached cluster-membership health |
| `GET /api/v1/system/playback-events` | Admin-only node-local playback rows; filter by unix-ms `since`, exact `event`, and capped `limit` |

The image's `HEALTHCHECK` runs `plurxd healthcheck`, which probes `/readyz`.
Docker therefore marks a process with no leader or usable replicated store
unhealthy instead of accepting the shallower `/healthz` liveness answer. Keep
`/healthz` for a separate process-restart signal only when your supervisor can
also keep unready nodes out of traffic.

Cluster metrics are fixed-cardinality counts; they do not publish node ids,
hostnames, addresses, media paths, or tokens, and a scrape performs no Store or
Hiqlite operation.

| Metric | How to read it |
|---|---|
| `plurx_cluster_membership_sample_valid` and `_age_seconds` | `1` with age below 30 seconds means the background membership sample is current. Treat every derived cluster count as stale when this is `0`. |
| `plurx_cluster_nodes{role="voter",heartbeat="fresh|stale"}` | Counts committed voters by application-heartbeat freshness. A stale row says its heartbeat did not commit in 30 seconds; it does not claim a direct TCP probe failed. |
| `plurx_cluster_quorum_required` | Majority arithmetic from the committed voter set: `floor(voters / 2) + 1`. |
| `plurx_cluster_heartbeat_quorum_available` | `1` means heartbeat-fresh voters meet that count. Confirm `plurx_raft_leader_known` and `/readyz` before calling the node serviceable. |
| `plurx_cluster_local_is_voter` | `0` on a replicated process means this node is not in the committed voter set; it may still be joining, removed, or stale. |
| `plurx_cluster_removals_pending` | Nonzero means a durable removal fence still needs an operator retry or resolution. |
| `plurx_raft_current_term`, `plurx_raft_leader_known`, `plurx_raft_is_leader` | Local Raft leadership state from the cached watch. A known leader is required but does not by itself prove this node is caught up. |
| `plurx_raft_applied_index`, `plurx_raft_commit_index`, `plurx_raft_apply_lag_entries` | Local apply progress against the quorum-confirmed commit watermark. Healthy readiness requires zero lag. |

The compact Prometheus alert shape is: membership sample valid · leader known ·
heartbeat quorum available · apply lag zero. `/readyz` remains the final active
serving check because the metrics are deliberately passive and cached.

## Hardware transcode & recent Intel GPUs

The Docker image defaults to **jellyfin-ffmpeg**, which bundles a current Intel
media driver + libva + oneVPL. This matters for newer silicon: an Arc / Meteor
Lake / **Arrow Lake** iGPU (on the kernel `xe` driver) is years newer than the VA
driver Debian ships, so the distro ffmpeg fails VA-API init with an I/O error
while jellyfin-ffmpeg drives it fine. Pass the GPU through and add the render
group in your compose override:

```yaml
    devices:
      - /dev/dri:/dev/dri
    group_add:
      - "992"          # stat -c '%g' /dev/dri/renderD128 on the host
```

On Intel **Arc**-class GPUs, QuickSync (oneVPL) is usually more reliable than
VA-API — set `PLURX_HWACCEL: "qsv"`. Two concurrent QSV sessions on one iGPU can
stall; plurx's watchdog catches that and self-heals to software x264 (you'll see
the loading overlay a few seconds longer, then playback).

## Common problems → cause

| Symptom | Cause | Fix |
|---|---|---|
| Scan finds `0` files | Path isn't what the **server** sees | Match the Docker mount to the library path |
| `error: … ` on a library | Server can't read the path | Check mount, permissions, that the share is mounted |
| Item won't play, shows a missing-file notice | File not on disk (unmounted share) | Remount; plurx correctly refuses to open a dead player |
| `docker … pull access denied` | Image isn't published under that name | Build from source: `docker compose up -d --build` |
| GDM won't bind / port conflict | A running Plex owns UDP 32414 | Set `PLURX_GDM_PORT`, or stop Plex |
| Gray screen then playback | Hardware session stalled, watchdog fell back to software | Expected under concurrency; check `PLURX_HWACCEL` |
| HLS playback errors immediately at startup | ffmpeg exited unsuccessfully before publishing a usable playlist; plurx now reports that known server failure promptly instead of waiting for a client-side `manifestLoadTimeOut` | Read the final `plurxd::transcode` line for the source, filter, decoder, or encoder failure |
| A transcode start returns HTTP 503 with `transcode capacity is temporarily unavailable` | The five-second foreground admission window expired before a background encoder yielded, or before configured live hardware/software capacity became available | Retry after the named work releases. If the response says background encoding did not yield, read `pre-transcode yielded` and session-end lines in `plurxd::transcode`; absence after five seconds means that worker is stuck rather than permission to start beside it |
| 4K HDR is slow but plays | The tone-map is running on the CPU. `GET /api/v1/system` → `tone_map` names the graph in use and why each candidate was rejected — no GPU device, a driver that refused the filter, output that didn't match the reference, or a graph that wasn't faster than the CPU chain | A rejection naming a missing device is usually a container passthrough (`--device /dev/dri`) or a missing driver package. HLG and Dolby Vision always use the CPU chain by design |
| "All hardware transcode slots are in use" | The cap (`transcode.max_hw_sessions`, default 2) is doing its job. A start waits up to 5 s for a slot, then runs in software *only if this server has measured that class of stream above realtime there* — never because the output is small, since the decode and the tone-map happen at source resolution whatever size you ask for | `GET /api/v1/system` → `hw_slots_in_use` / `hw_slots_max`. Refused at 0 of 2 is a bug; refused at 2 of 2 is the design. A 4K HDR source is the shape software cannot carry, so it is refused rather than started to stall |
| A GPU tone-map worked and then stopped | The pipeline downgrades once, per session, to the CPU chain and logs it | Look for `pipeline=` on the session's ffmpeg log line: it names what actually ran, not what the box can do |
| 4K HDR / Dolby Vision won't play | Heavy HEVC is hardware-decoded (Intel too); if the GPU can't decode it and software can't either, the session now fails fast with a clear log line instead of hanging gray | Read `plurxd::transcode` — the last ffmpeg line names the real cause (decode vs tone-map). DV profile 5 is the hardest case |
| Playback is software when you set `qsv` | The QSV probe was rejected at startup | Read `plurxd::transcode` logs; usually a driver/`/dev/dri` gap |
| No posters, just filenames | No TMDB key (movies/TV) | Add a key in Settings → Metadata (anime needs none) |
| Playing or seeking knocks a Wi-Fi client off the network (loses its IP and can't get another) | An unpaced stream is taking the whole link, starving the client's DHCP renewal of airtime | Lower **Settings → Playback → Delivery speed** to 2×; confirm with a `ping` to the gateway during a seek |
| 4K starts, then buffers a few seconds in | The session never built a head start — the classic cause was realtime pacing on the copy-video path | Raise **Settings → Playback → Transcode buffering → Head start**; check the stats overlay's Server block for the encode speed |
| Stutters every 20–40 seconds through a whole film | The encoder cannot keep up: the head start drains at (1 − speed) per second played | Stats overlay (`i`) → Server → encode speed. Below 1× means transcode, not network — pick a lower quality, or check that hardware encoding validated at startup |
| The transcoder seems to stop partway through | It reached the buffer limit and suspended itself | Expected. `Settings → Activity` marks it held; it resumes when the playhead catches up |
