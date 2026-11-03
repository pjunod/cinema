# Voter isolation

A Raft voter must keep answering heartbeats. On 2026-11-01 an uncapped agent
build container on a voter (six `clippy-driver` processes of 2.3–4.6 GB each)
filled its swap, plurxd's runtime stalled for 2.7 s at a time, and the cluster
lost its leader again and again. Every node then lost serving authority, and
playback failed. The cause and the rest of the fix are in
[PLAYBACK-QUORUM-GAPS-RCA.md](../../docs/cluster/PLAYBACK-QUORUM-GAPS-RCA.md)
§3.6 and §4.6.

The isolation is a cgroup boundary that every container on a voter falls under:

| Piece | What it does |
|---|---|
| [`plurx.slice`](plurx.slice) | plurxd's slice: `MemoryLow=3G`, `CPUWeight=1000`. |
| compose `cgroup_parent: plurx.slice`, `mem_reservation: 3g` | Puts the plurxd container in that slice and gives it its own `memory.low`; a slice's `MemoryLow` only reaches a container through that. |
| [`docker-other.slice`](docker-other.slice) | Every other container: `MemoryHigh=50%`, `MemoryMax=70%` of host RAM, `MemorySwapMax=2G`, `CPUWeight=50`. |
| [`daemon.json.example`](daemon.json.example) | Makes `docker-other.slice` dockerd's default parent, and turns on `live-restore`. |
| `scripts/voter-build` | Agent builds on a voter: `--memory 8g --memory-swap 8g --cpus 4`, cargo `-j4`. |
| `scripts/voter-isolation-check` | Prints what the kernel and systemd actually enforce, and exits 1 on any mismatch. |

The compose keys alone are mitigation only: CPU weights are relative and a
memory reservation is a soft limit. Without the dockerd default, only
containers started with `--cgroup-parent` are bounded, and an ad-hoc container
can still starve the voter.

## Installing it on a voter (Paul's call)

Setting the dockerd default needs a dockerd restart. Done in this order it
interrupts nothing:

1. Measure the steady RSS of the containers that will land in
   `docker-other.slice` (on `media1`, the sibling services too) and confirm
   50 % / 70 % of host RAM never binds on them.
2. Copy `plurx.slice` and `docker-other.slice` to `/etc/systemd/system/` and
   run `systemctl daemon-reload`.
3. Merge `daemon.json.example` into `/etc/docker/daemon.json`. Check
   `live-restore` first: with it on, restarting dockerd keeps running
   containers up.
4. Restart dockerd. Containers that are already running keep their old cgroup
   until they are recreated.
5. Recreate plurxd. That is a normal deploy.
6. Run `scripts/voter-isolation-check` on the voter. It must print
   `isolation OK`.

## What the check reads

All read-only: `docker ps`/`docker inspect` for every running container,
`/proc/<pid>/cgroup` and the cgroup's `memory.low`, `memory.max` and
`memory.swap.max`, `systemctl show` for both slices, and
`/etc/docker/daemon.json`. It reports each block device's IO scheduler and
judges nothing there: `IOWeight` needs BFQ or io.cost, which are not assumed.

It fails when plurxd sits outside `plurx.slice` or has `memory.low` 0, when any
other running container sits outside `docker-other.slice` (an explicit
`--cgroup-parent` escapes the default), when a slice is missing or its memory
limits are unbounded, or when dockerd's default parent is not
`docker-other.slice`.
