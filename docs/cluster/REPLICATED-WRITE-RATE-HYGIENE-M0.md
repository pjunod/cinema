# Replicated write-rate hygiene — M0 baseline readout

**Status:** M0 complete (12-hour gate) · **Plan:**
[REPLICATED-WRITE-RATE-HYGIENE.md](REPLICATED-WRITE-RATE-HYGIENE.md) §5.1 ·
**Written:** 2026-09-25 · **Board:** K-03

This is the number every later K-03 milestone is judged against. It has two
parts: the plan's four series over one qualifying idle window, and an
attribution of the replicated log to the statements that write it, because
the four series say how much is written but not by whom.

## 1. The gate

The plan asked for 24 continuous idle hours. Deploys reset the sampler every
few hours, so none ever completed; on 2026-09-25 Paul set the gate to
**12 hours**. The sampler (`scripts/replicated-write-capture-sampler`, the
same script running on the Mac as launchd `com.plurx.k03-m0-20260921`) and
the evaluator (`scripts/replicated-write-capture evaluate`) both use 12 hours
now. A window counts only while all three voters answer, each reports itself a
voter with zero pending outbox rows and no transcode, Live TV or
protected-playback activity, the build set does not change, and no counter
goes backwards.

Replaying those rules over the whole capture (`samples.tsv`, 16,783 lines,
sha256 `c9a1bb9f…41bd812`, 2026-09-21T03:23:20Z to 2026-09-25T01:25:21Z)
finds 57 idle windows. **One meets the 12-hour gate:**

| Window | Length | Samples | Largest gap | Builds | Ended by |
|---|---|---|---|---|---|
| 2026-09-22T03:36:36Z – 19:39:03Z | **16.04 h** | 956 | 62 s | nuc4 `3052-g882862e88`; m6, nynuc `3225-g5c48ed5ab` | build change |
| 2026-09-23T05:45:09Z – 17:12:31Z | 11.46 h | 682 | 61 s | three builds | build change |
| 2026-09-24T05:01:04Z – 15:39:52Z | 10.65 h | 634 | 61 s | `3649-g99d4abf8c` | build change |

Every other window is shorter than 6.2 hours. The sampler's last sample is
2026-09-25T01:25:21Z; it had not written again by 09:15Z, and its launchd
state cannot be read from the agent workspace, so whether it is still running
is unknown. Nothing here depends on it running.

## 2. The four series, per voter

Over the qualifying window (57,747 s). Rates are the window's deltas scaled
to a day.

| Voter | Proposals (commit index) | Store writes | Authority reads | Snapshot builds |
|---|---|---|---|---|
| nuc4 | 603,211 → **902,513/day** (10.45/s) | 192,961 → 288,705/day (3.34/s) | 728,919 → 1,090,595/day (12.62/s) | 61 → 91/day |
| m6 | 603,210 → 902,512/day | 195,071 → 291,862/day (3.38/s) | 710,544 → 1,063,103/day (12.30/s) | 61 → 91/day |
| nynuc | 603,210 → 902,512/day | 196,516 → 294,024/day (3.40/s) | 713,348 → 1,067,298/day (12.35/s) | 61 → 91/day |

The commit index is the cluster's, so the three voters agree on it:
**902,512 proposals a day on an idle fleet**. The per-process write counters
sum to 874,591 a day, 96.9% of it; the rest is Raft's own entries. Snapshot
builds follow directly: 902,512 ÷ 10,000 logs per snapshot ≈ 90 a day per
voter, against the ~26 the plan attributed to the outbox alone.

## 3. Who writes the replicated log

The metrics have no per-statement label, so the attribution was taken from
the log itself: a read-only copy of the learner `nuc3`'s hiqlite WAL
(`/srv/plurx/hiqlite/logs/*.wal`), classified by
`scripts/replicated-write-capture attribute`. The sample is **25,170
contiguous log entries** (ids 20,730,865 – 20,756,034), about 07:07 – 07:53Z
on 2026-09-25, with the voters on `v0.3.0-3974`/`-4002` and idle (zero
pending outbox, transcode, Live TV and protected playback at both ends). Its
rate, ≈ 9.2–10.5 entries/s, matches the M0 window's 10.45/s. It is a later
build than the M0 window, so the shares are the mix *now*; the outbox's
share is fixed by code that has not changed since, and it agrees (28.7% of
the M0 window's proposals is exactly 3 voters × 86,400).

| Writer (first statement of the entry) | Entries | Share | ≈ per day at 902,512 |
|---|---|---|---|
| `metadata-classification` lease: acquire | 5,967 | 23.7% | 214,000 |
| `metadata-classification` lease: renew/release | 4,518 | 17.9% | 162,000 |
| **`UPDATE watched_outbox` claim (K-03 §2.1)** | **7,285** | **28.9%** | **261,000** |
| `UPDATE offline_packages` idle claim | 3,531 | 14.0% | 127,000 |
| node heartbeat transaction (23 statements) | 941 | 3.7% | 34,000 |
| `dvr_reminders` | 326 | 1.3% | 12,000 |
| `media:fragment-index:N` leases | 486 | 1.9% | 17,000 |
| `cluster_fragment_index_jobs` | 307 | 1.2% | 11,000 |
| everything else (≈ 40 writers, each < 1%) | 1,809 | 7.2% | 65,000 |

What dominates, then, is three idle loops that each propose on every voter
whether or not there is work:

1. **The `metadata-classification` lease cycle, 41.6%.**
   `library_search::worker` acquires and releases the lease around every
   32-item classification page, once a second, on every voter; a node that
   loses the race still pays an `acquire_lease` proposal. **Outside K-03's
   scope** — flagged for the coordinator below.
2. **The watched-outbox claim, 28.9%.** The one K-03 names. M1 and M2 remove
   it (§4).
3. **The offline-package claim, 14.0%.** `offline::run` calls
   `claim_next_offline_package` every 2 s on every voter; the claim is an
   `UPDATE … RETURNING` that proposes whether or not a package is queued. The
   same shape as the outbox, **outside K-03's scope**, flagged below.

Authority reads cannot be attributed from the log (reads are not in it). The
takeover loop's share is fixed by code: two consistent reads every 2 s is
86,400 a day per node, **8.0%** of the ~1.07 million a day each voter pays.

## 4. What the later milestones need

| Series | M0 (this readout) | After M1–M3, expected | M4 acceptance |
|---|---|---|---|
| Outbox proposals, cluster | ≈ 259,200/day (3 × 86,400) | ≤ 2,880 forced claims + 2,880 lease renewals ≈ 5,760/day | < 10,000/day per cluster |
| Takeover authority reads, per node | 86,400/day | ≤ 1,440/day (one pair per 60 s) | < 1,500/day per node |
| Proposals, cluster | 902,512/day | ≈ 649,000/day (−28%) | reported, not gated |
| Snapshot builds, per voter | 91/day | ≈ 65/day | reported, not gated |

The after-measurement is the same readout on a build carrying M1–M3, with the
same gate; the evaluator prints it (`--json`). For scale: the two
out-of-scope loops in §3 are ≈ 500,000 of the ≈ 649,000 that remain; if both
were reduced to a forced claim every 30 s the idle cluster would sit near
150,000 proposals a day.

## 5. Flagged for the coordinator

K-03 is scoped to the outbox and the takeover poll (§3 of the plan), and the
executing session kept to it. The readout shows that scope covers under a
third of the idle write rate. Two loops outside it are larger than, or close
to, the one the plan was written for, and each needs its own decision:

- `metadata-classification`: hold one lease across a whole pass (as the
  outbox now does), or read the lease row locally before contesting it.
- `offline_packages`: gate `claim_next_offline_package` on a local
  "anything queued?" hint, exactly as §3.1 does for the outbox.

Neither was built here.
