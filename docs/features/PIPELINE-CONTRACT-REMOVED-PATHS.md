# Pipeline contract — removed paths, probe-only requests and their answers

**Status:** built · **Parent:** [PIPELINE-CONTRACT.md](PIPELINE-CONTRACT.md) ·
**Wire:** [INTEGRATION.md](../INTEGRATION.md) §1.10, [API.md](../API.md) §6.3

The detail behind `removed_paths` and `probe_only` on the targeted scan
request, moved out of INTEGRATION.md to keep it under its size rule.

## Request fields

`removed_paths`: absolute files or removed folders under `path`, free of control
characters; at most 64, each at
most 4096 bytes and free of control characters, 16 KiB of JSON together with
`expected_files`. `probe_only: true` records exactly `expected_files`, walks
nothing (even at a library root) and never shares a pass with a request that did
not send it; a request with `removed_paths` is always probe-only. A `400` covers
a relative or out-of-`path` entry and any limit exceeded.

## Answer entries

```json
{"state":"gone","path":"/media/movies/Dune (1984)","item_id":501,"item_ids":[501],"item_removed":true}
{"state":"unknown","path":"/media/movies/Never (2000)/Never.mkv"}
{"state":"present","path":"/media/movies/Heat (1995)/Heat.mkv","reason":"exists"}
{"state":"retained","path":"/media/movies/Ran (1985)","item_id":77,"code":"root_identity_unset","retryable":true,"reason":"…"}
```


## Removed paths

With `removed_paths` (or `probe_only: true`) the request is **probe-only**:
plurx probes only the expected and removed paths, walks nothing else under
`path` (their common parent, possibly a whole library root), reconciles
nothing else and admits no full scan. It is admitted as a job of its own
(`library:targeted-probe:<library>`), shared only with other probe-only
requests, so a probe-only request and a plain one for the same path are
two passes and neither changes the other's mode. A `path` that is itself gone
is scoped to its nearest existing ancestor, at admission and again when the
pass runs. Removal happens **after** the expected files are placed: the new
copy is indexed before the old one goes.

Each removed path is reported as one of:

| `state` | Meaning |
|---|---|
| `gone` | Absent; its file records (for a folder, every file catalogued below it, matched by whole path component) are removed. `item_id` is the first affected item, `item_ids` all of them; `item_removed` is true when every one of them was left with no files and went too |
| `unknown` | Absent, and plurx never had it |
| `present` | Nothing removed. `reason`: `exists` (still there), `symlink_in_chain` (a symlink stands in the path; plurx never follows one to decide absence), `case_variant` (the name exists only because the filesystem folds case: a case-only rename), `reappeared_before_removal` (absent at the first look, back at the second) |
| `retained` | Absent, but the record was kept: `code`, `retryable` and a `reason` for people |
| `skipped` | Outside the request path or the library; never looked at |

`retained` codes: `root_identity_unset` (retryable: any full scan records the
library's root identity), `root_identity_mismatch` (not retryable: this node's
roots are not the recorded ones; an operator matter), `root_identity_unavailable`
(retryable), `prune_bound` (not retryable as sent: more files than one removal
may delete; send fewer), `record_kept` (retryable: the record was still there
after the removal; the one exclusion the removal makes is a Dolby Vision
conversion holding the file, which plurx does not verify, so it does not claim
it), `expected_not_placed` (retryable: an expected file of the same request was
not placed, so nothing was removed; send a new attempt), `chain_unreadable`
(retryable: the path could not be re-read just before removal).

**How Curator reads it.** `gone`, `unknown` and `present` with `reason: exists`
or `case_variant` are settled and need no retry: `unknown` is **satisfied** (the
path is not in plurx, which is the goal), and so is `gone`. A `present` with
`symlink_in_chain` is a standing warning (the old path resolves elsewhere),
not a retry. `reappeared_before_removal` means someone put the file back;
Curator re-asks only if it still wants it gone. `retained` with `retryable:
true` is retried with a new attempt key; with `retryable: false` it is
surfaced. A replay of a finished request after a *new attempt* superseded it,
or after a crash between the removal committing and the report being written,
reports the removed paths as `unknown` (the records are already gone): Curator
treats `unknown` as satisfied, so the replay is harmless.

**Failure is not absence.** `ENOENT` and `ENOTDIR` are absence and never
`path_unreadable`. Any other error on the chain (`EACCES`, `EIO`, `ENOTCONN`,
`ESTALE`), a library root that cannot be listed, a root that lists nothing and
is not a mount point of its own (an unmounted share looks like that), or a
deepest existing folder that is on a different filesystem than the library root
(a nested or auto mount that may be away) fails the whole request as `503
path_unreadable` before anything is placed or removed. A root left empty by
its last file moving out is told from an unmounted one by its filesystem: a
live mount's device differs from its parent's. An empty root that is a plain
folder on its parent's filesystem stays `path_unreadable` until it holds a file
again (any placement or full scan), because nothing on this node can tell it
from an unmounted share.

Removal is the full scan's reconcile bounded to the named files: under the
library's mutation fence (a full scan holds it, so a removal waits as
`full_scan:<library>`), with the root identity checked, and in one
transaction.

**Mixed clusters, and the deploy order.** A removal request is stored with a
sidecar entry an older cinema build cannot decode. What an older node does with
it (verified): its worker reads the sidecar, the decode fails, and that ends
the library turn; the job settles as a charged retry (`library_execution_failed`,
5 s, 30 s, 2 min, then 10 min backoff) and the request stays pending for any
node that understands it. It is never walked as a plain targeted scan and never
fails the request outright until the job's attempt limit is spent. Probe-only
requests are their own job, so a row an older node cannot read delays only
other probe-only requests, not plain targeted scans. An older node's status
`GET` of such a row answers `500` until it is upgraded; this build's Jobs
listing tolerates a sidecar it cannot read (it omits that row's `waiting_on`). So: **upgrade every
cinema node before Curator sends `removed_paths`**, and send it only when the
connection test's `features` lists `removed_paths` (a mixed cluster can answer
either way, so the feature list is a hint, not a guarantee).

See [API.md](../API.md) §6.3.
