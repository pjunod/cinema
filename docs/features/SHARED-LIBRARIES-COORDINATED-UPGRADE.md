# Sharing coordinated upgrade — daemon and rollback qualification

**Status:** open · **Scope:** S3 isolated coordinated-upgrade evidence

This records the process-level qualification path for the candidate principal
schema. It complements the [historical Store qualification](SHARED-LIBRARIES-UPGRADE-QUALIFICATION.md)
and [implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md) §16.4.
The candidate remains uninstalled in production, with no capability advertised.

## 1. Build immutable historical and candidate daemons

[`qualify-sharing-coordinated-upgrade.py`](../../scripts/qualify-sharing-coordinated-upgrade.py)
archives historical source `971265536a259dea38b0f7a9a8752a5a74e8c025` and the
exact committed candidate HEAD. Neither archive carries `.git` or repository
credentials. Historical production source receives no overlay. Both actual
`plurxd` executables and the candidate test-only control process compile with
Rust 1.97.1 from those archives. A receipt identifies both source commits,
archive hashes, compiler version and daemon binary hashes.

Use a new disposable source directory and a dedicated warm compiler directory:

```bash
export PATH=/Users/pjunod/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH
python3 scripts/qualify-sharing-coordinated-upgrade.py \
  --source-dir /private/tmp/plurx-coordinated-qualification-source \
  --target-dir /private/tmp/plurx-coordinated-qualification-target
```

The runner refuses an existing source directory or unpinned compiler. It
requires its own bytes to match the archived committed source. Failure retains
owned fixture directories and logs; only successful completion writes
`qualification-receipt.json`. The runner starts processes on allocated loopback
ports and stops only its own processes. It changes no deployed service or route.

## 2. Verify future-schema startup refusal

For each historical and current daemon, an isolated SQLite source uses the
actual SQLite schema with `user_version` increased by one. Startup must refuse
with the specific future-schema error before HTTP or Raft activation, preserve
the source version and row count, retain a session-directory sentinel, and leave
all configured ports available.

A separate single-voter replicated fixture commits `cluster_meta` one version
above the supported replicated schema. Actual daemon startup must report the
specific incompatible cluster/voter schema error before HTTP becomes available
and retain its source sentinel. Replicated startup needs Raft before it can
read committed metadata; this case does not claim refusal before Raft starts.
It does not prove that the unversioned candidate ownership rebuild is a released
future schema, or that an old daemon safely runs against that rebuild.

## 3. Stop, back up, install, restart and restore three voters

The runner starts three unchanged historical daemons, performs owner setup,
redeems join tokens, and verifies three actual voters. It stops all daemons
using SIGTERM and requires successful bounded shutdown. The workload is empty;
this proves process shutdown, not active media drain.

The [test-only control process](../../crates/plurx-core/examples/qualify_sharing_upgrade.rs)
opens those saved daemon stores through `select_daemon_store`, without HTTP or
media workers. It seeds retained local-principal rows from the frozen fixture,
with resolved requests, ended sessions, current fixture creation/update clocks
and bounded future retention deadlines. The frozen fixture's diagnostic 1970
clocks would otherwise exercise expiry cleanup rather than retained-data
compatibility. Only the seeded session lease receives a future lease deadline;
existing daemon worker leases keep their original clocks. The second ended
route deliberately keeps its stale clock to qualify retention cleanup. It reads
all columns of eleven relevant tables through quorum reads. It then stops all
three processes and copies each entire stopped data directory into one owned
whole-topology backup. Individual live SQLite, WAL or Raft files are never
copied as a backup.

After restarting the control processes, the leader installs the candidate
membership declaration/intent factory before any principal marker. One Raft
transaction asserts the closed transition-absence predicate and applies the
unchanged frozen seven-table rebuild. There are no shared principals and no
capability advertisements. Retained pre-existing columns and rows, including the full job-lease inventory,
must match immediately across the rebuild.
Store methods are not called through a stale cached shape after rebuilding.

All control processes stop before three current actual daemons restart on the
candidate. After their successful shutdown, the control processes verify
retention again with exact maintenance outcomes. The fresh ended route and its
lease remain within the resolved-retention window. A second, deliberately stale
ended route, its lease and acknowledgement must be removed. The pointer whose
current route is nonactive and the preparation whose staged successor is absent
must be removed. Every other original fixture row and column must match; the
full inventories are saved separately. Unrelated daemon worker lease clocks,
revisions and expirations are mutable runtime state.

The runner parks the entire candidate topology, restores all
three stopped pre-upgrade data directories together, and starts the historical
daemons with the same node identities and endpoints. Owner login and the
three-voter roster must survive. A final quorum inventory must match the
pre-upgrade retained rows, with those same required cleanup outcomes.

This is a test-only whole-topology restore drill. It does not qualify the
portable backup API or an operator migration coordinator.

## 4. Receipt and rollout limits

The runner and helper are prepared for execution; no successful coordinated
receipt is claimed by this initial checkpoint. The separately executed unchanged
historical daemon already refused a synthetic future SQLite source before HTTP
or Raft activation and preserved its source sentinel. The reproducible runner
must repeat this against its committed snapshots before receipt publication.

Production activation requires completed principal and allocator writers,
qualified atomic source-grant and exact member-floor admission, a released
schema/version boundary, and a coordinated operational procedure. Stop and
drain old daemons and membership operations before installing the factory; an
RPC issued before the factory existed cannot be fenced retroactively. Install
factory guards before principal or allocator markers, require transition
absence in the installation transaction, and restart only compatible members.
Missing or partial guard/marker state must fail closed.

The empty-workload fixtures do not qualify active playback, relay, worker or
recovery drain, interruption during installation, a production backup tool,
physical hardware, Tailscale, CGNAT, or rolling upgrade. Sharing stays behind its
saved advisory Developer switch; readiness never overrides the saved choice.
S3 remains incomplete until those relevant rollout and write-admission proofs
are recorded against the current integrated candidate.
