# Source leases and source readers — operations

**Status:** per-root capacity delta is WIP on [PR #1728](http://forge.lan:3000/noirr/plurx/pulls/1728); review and focused qualification remain · **Design:** [SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md](SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md) · **As built:** [current policy and review state](SOURCE-LEASE-AS-BUILT.md)

Every original-media reader has a lease in the replicated `source_leases`
table: playback, a background job, an analysis phase, a subtitle read
attestation or an offline package. A lease counts while
`expires_at_ms > now - 60 s`; the grace absorbs delayed renewals, and the
minute sweep only collects expired rows. It never decides whether a reader
has stopped.

For a file under a configured library path, the store chooses the longest
root that matches at a path-component boundary. Its key is
`source_io:root/<library_id>/<UPPERCASE_HEX_UTF8_ROOT_PATH>`. Trailing `/`
is removed for the root identity except for `/`. Root budgets are shared
across the cluster; independent roots do not acquire a node-wide cap by
default.

Limits (Settings → Libraries → Background source readers):

| Setting | Default | Meaning |
|---|---|---|
| `jobs.source_readers_per_root` | 2 | Maintenance readers per configured root; one additional slot is reserved for live demand |
| `jobs.root_source_readers.<root_id>` | global default | Root override from 1 to 16; `root_id` is `<library_id>/<UPPERCASE_HEX_UTF8_ROOT_PATH>` |
| `jobs.domain_source_readers.<domain>` | unset | Optional aggregate cap for roots mapped to this domain; unset means unlimited |

The API uses `source_readers_per_root` and
`source_readers_root_overrides` (`{root_id: 1..16}`); an empty or null
override inherits the global default. Root and default limits are admission
limits: lowering them never stops an existing reader and never refuses the
save. New readers wait until capacity is available. A domain mapping by
itself adds no aggregate cap; configure `jobs.domain_source_readers.<domain>`
only when that shared limit is wanted.

Legacy unrooted holders keep the generic `source_io` key and its bounded
node-local fallback, using the root-reader default. Existing generic holder
keys remain conservatively charged until their leases retire. Rooted claims
are checked at the shared Store boundary, including older writers; expired
offline metadata alone cannot readmit a reader, and resuming a paused reader
recomputes its current root keys and applies the maintenance limit. There is
no physical-stop proof barrier.

**How to read Activity.** Activity shows per-root occupancy and caps alongside
the leases that consume them. Node occupancy is informational, not the rooted
admission policy. A lease inside its 60 s grace still counts. One whose expiry
is more than a minute old holds nothing; a row still present three minutes
after expiry means the sweep is not running
(`plurx_background_source_recovery_pending`).

Metrics: `plurx_source_leases{kind}` (this node's leases),
`plurx_source_readers_per_root_default` (the configured default),
and `plurx_source_lease_schema_version` (2 while v1 objects remain for an
older binary, 3 after the contract runs once every active node has
heartbeated the `source_leases_v2` capability).
