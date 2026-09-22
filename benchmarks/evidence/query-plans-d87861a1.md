# K-05 query-plan evidence — prerequisite boundary

**Status:** incomplete; blocks query changes · **Generator:**
`d87861a1afcc4cdedfdec91f3aa81c079b48cd6d` · **Schema/base:**
`main@9deb58a2e9c2e36f1753eb9f4ab01eb0854b40fe` · **Captured:** 2026-09-21

This receipt records what was actually measured. It is not the M0 acceptance
receipt: an actual Hiqlite state-machine fixture, the statement plans, the
cold/warm medians and the concurrent writer run remain absent. Sections 3.3
and 3.4 of the plan prohibit choosing a pool size, index, pagination rewrite,
pragma or row-layout change from this partial result.

## Standalone fixture

The production migration path created the schema; the fixture generator then
inserted the plan's full deterministic population in one transaction:

| Measure | Observed |
|---|---:|
| Libraries | 4 |
| Movies | 20,000 |
| Shows | 500 |
| Seasons | 4,000 |
| Episodes | 48,000 |
| Home folders / videos / photos | 100 / 1,000 / 1,000 |
| Recordings | 1,000 videos |
| Files | 100,000 |
| Probe JSON size | 5,252–30,852 bytes |
| Users | 5 |
| Watch rows | 35,000, playable leaves only |
| Home preview rails | 4 (Movies, Shows, Home, Recordings) |
| Database size after checkpoint/VACUUM | 1,870,753,792 bytes (1.74 GiB) |
| Warm release build plus generation wall time | 18.34 s |

Command (the output database was disposable and is not committed):

```sh
git switch --detach d87861a1afcc4cdedfdec91f3aa81c079b48cd6d
rustup run 1.97.1 cargo run -p plurx-core \
  --example catalogue_fixture --release -- /tmp/k05-catalogue/fixture.db
```

The generator runs its census before commit. It requires the exact
library/item-kind counts above, all four top-level Home preview rails, a
non-empty Recordings candidate set that the catalog-wide recently-added
predicate excludes, zero files on invalid library/item-kind pairs, and zero
watch rows on non-playable kinds. The observed post-build SQL census repeated
those counts. Photo rows use photo-shaped file/probe facts; file and watch
durations follow their movie, episode, home-video or recording owner rather
than being assigned to hierarchy containers.

`scripts/bench` is already the repository's tracked playback-benchmark
program, so the plan's proposed `scripts/bench/catalogue-fixture` pathname
cannot be created without replacing that program with a directory. The Cargo
example is the equivalent executable and refuses to overwrite an existing
database.

## Missing evidence and stop condition

No copied standalone file is labeled as Hiqlite evidence. M0 still requires:

1. population of the same shape through the replicated backend into an actual
   Hiqlite state-machine database;
2. verbatim `EXPLAIN QUERY PLAN` output and five-run cold/warm medians for every
   statement named in §3.4 on both databases; and
3. the 32 × 256 Home workload with a 50-files/s writer for read pools 2, 4 and
   8, including read p50/p95, write p99 and process RSS.

Until those measurements exist, M1–M7 remain deliberately unimplemented.
