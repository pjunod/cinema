# Source leases and source readers — operations

**Status:** current with branch `claude/source-lease-steps-2-4` · **Design:** [SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md](SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md)

Every reader of original media holds one row in the replicated
`source_leases` table: playback, a background job, an analysis request (one
per coordinated phase), a subtitle read attestation or an offline package.
Its expiry is a column. A lease counts against its node — and against a
mapped storage domain it names — while `expires_at_ms > now - 60 s`; capacity
therefore frees on time whether or not anything else runs. Job and analysis
leases are written, renewed and released by triggers on their owner rows, so
no daemon code path can forget to renew one. The store sweep deletes expired
leases every minute; that is garbage collection, never a release decision.

Limits (Settings → Libraries → Background source readers, all replicated
settings read inside the claim statement):

| Setting | Default | Meaning |
|---|---|---|
| `jobs.source_readers_per_node` | 2 | Background readers per node; one more is kept for live demand |
| `jobs.source_readers_per_node.<node>` | default | Per-node override |
| `jobs.domain_source_readers.<domain>` | 1 | Background readers of a mapped storage domain across the cluster (plus one for live demand) |

Playback is never counted and never refused. Outside its permission window
it blocks background reads of the same source key on every node, as before.
Lowering a limit never refuses the save and never stops a reader: claims on a
node above it wait until its readers finish; the panel and
`GET /analysis/summary` (`source_capacity`) say which nodes are still above.

**How to read Activity.** The analysis summary groups leases by node under
"`<node>` — N of M background readers"; each row names the work, why it holds
(maintenance · live demand · playback phase) and its lease age; a job row
links to the job, whose Cancel stops it. A node at its limit with every lease
renewing is busy, not stuck. A lease whose expiry is more than a minute old
holds nothing; one still present three minutes after expiry means the sweep is
not running (`plurx_background_source_recovery_pending`).

Metrics: `plurx_source_leases{kind}` (this node's leases),
`plurx_source_readers_per_node` (this node's effective limit) and
`plurx_source_lease_schema_version` (2 while the v1 objects are kept for a
node still on an older binary, 3 once the contract has run — on the first
daemon start after every active node has heartbeated the `source_leases_v2`
capability).
