# Architecture review fleet evidence — the 2026-10-11 acceptance-pass build

**Status:** evidence-only snapshot · **Source:** [architecture-review workboard](ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) at `main` `05bf603815e6aad926fa561aeaffabf5e31b56d3` (#983) · **Observed:** 2026-10-11 00:42–00:55 UTC

#983 ("fix: root-cause fixes from the architecture acceptance pass") was
rolled out to the four nodes between 2026-10-10 23:45 and 2026-10-11
00:40 UTC, in the order media1, lab6, lab4, lab3. Every node reported
`plurx_build_info{version="0.3.0",build="attribution-repair-2026-10-06-1252-g05bf60381"} 1`.
This record holds the read-only server-side observations taken on that
build in the first quarter-hour after the rollout. Nothing was restarted,
no setting changed, nothing was written inside a container and no admin
token was used. Each section says what it does **not** establish; no plan
closes from this record.

lab4 was the Raft leader and lab3 the non-voting learner. Container uptime
at 00:42:50 UTC was 3,266 s (media1), 2,519 s (lab6), 1,452 s (lab4) and
244 s (lab3); uptime stayed monotonic and no scrape failed during the
window. A 24-hour one-minute collector of the clock, Live TV observer,
handler-deadline, telemetry, child-priority, transcode and takeover-read
families was started on lab3 at 00:42:50 UTC; its readout is not part of
this record.

| Plan | Observation | Result |
|---|---|---|
| K-06 | 10-minute clock-counter window and peer offsets | media1, lab4, lab3 flat; lab6 one early burst, flat after; offsets pass |
| L-02 | 2-minute settings-observer read window | observer Δ0 on all four (was about 0.9/s) |
| C-08 | scrape size/time, label values, ANSI, request id | pass; the §6.3 literal grep could not pass as written |
| C-01 | slow request header, static asset encoding | pass |
| P-02 | daemon and child priorities and limits | daemon pass; background child pass; realtime not observable; unapplied counter false positive |
| K-08 | embedding-pool threads | inconclusive: no embedding since the restart |
| K-05 | query plans on current snapshot copies | pass on lab6 and lab4 (plans only) |

## K-06 cluster clock

`plurx_cluster_clock_{unknown_rounds,refusals,advisory_refusals,discontinuities,authority_reads}_total`
were read at 00:43:11 and 00:53:41 UTC on all four nodes. Everything except
authority reads was flat on media1, lab4 and lab3. lab6 moved
`unknown_rounds_total` 113 → 114 and
`refusals_total{decision="expiry_scan",cause="unknown"}` 20 → 25; the
one-minute collector places all of it between 00:42:50 and 00:43:50 UTC,
four to five minutes after lab3's container came up at the end of the
rolling deploy, and nothing moved for the remaining ten minutes. No sample
on any node showed an `observation_state{state="unknown"}` series at 1, and
lab6's container log for 00:41–00:45 UTC has no clock or expiry line. The
cause is not determined; a link to the end of the rollout is possible and
unproven. Discontinuities stayed 0 everywhere; authority reads rose 180 per
node (0.30/s). Lifetime enforced unknown expiry-scan refusals were
media1 355, lab6 25, lab4 0, lab3 0.

Peer `|offset| + uncertainty` at 00:43:21 UTC was at most 0.003 s on every
observer/peer pair (bound 2 s); every node reported `enforced` 1 and zero
unobserved learners. On the plan's "flat" bar this window passes on media1,
lab4 and lab3 and fails, strictly, on lab6. It is a post-deploy point
window, not the 24-hour, loaded or drill acceptance.

## L-02 settings observer

Over 00:43:11–00:45:11 UTC `plurx_live_tv_settings_reads_total` for the
observer site moved Δ0 on all four nodes (failed Δ0, fence Δ0), against
about 0.9 reads/s before #983; the request site moved 2–4 (client requests,
not the observer). `plurx_live_tv_transports` was 0 and
`plurx_live_tv_enabled` 1 throughout. This verifies the idle parking; the
§6.3 leader-restart and warm-start observations are separate.

## C-08 observability

- **Scrape:** 494,033–494,435 bytes and 5,468–5,472 series per node, five
  scrapes each in 1.5–12.1 ms (bars: under 2 MB, under 500 ms).
- **Label values:** no UUID other than node and peer ids, no token,
  `/mnt/` path or `file_id=` in any label value on any node; `route=` takes
  three values (`peer`, `plex`, `user`). The §6.3 literal grep over whole
  lines printed 178 lines per node, all metric *names* containing `session`
  or the fixed enum values `session_gone`, `session_busy`, `in_session` and
  `media_sessions`, so it can never print nothing on this build. The §6.4
  M5-families grep printed nothing on media1 and lab4.
  `plurx_start_outcomes_total` has 16 series and `_unpaired_total` 2 on
  every node.
- **ANSI:** zero escape bytes in the last hour of container logs on every
  node.
- **`X-Request-ID` on `GET /api/v1/server`** (lab4 and media1): a value of
  at most 64 bytes of `[A-Za-z0-9_-]` is adopted; a value with `/` or `.`,
  over 64 bytes, or with other punctuation is replaced by a minted id; a
  control byte is refused 400 before the application. Neither rejected
  value appears in the logs.

The token-gated RED sanity, refused-POST and attributed play/seek checks
were not run.

## C-01 slow header and assets

A connection that sent a partial request header and then nothing was closed
by the server after 15.02 s with no bytes, on lab4 and media1. Every one of
the 89 JS/CSS assets the shell references (lab4) served gzip for
`Accept-Encoding: gzip` and identity for `gzip;q=0`, with
`Vary: Accept-Encoding` on both, distinct ETags (`-gz` suffix), 304 for
each encoding's own `If-None-Match`, and a gunzipped body equal to the
identity body (1,152,623 versus 3,463,260 bytes in total). A weak gzip
ETag also answered 304 and `br` alone answered identity.
`plurx_http_handler_deadlines_total` stayed 0 on all four. This is not the
HAR/waterfall, reader or soak evidence.

## P-02 child priorities

The daemon (PID 1 in each container) ran at nice 0, `oom_score_adj` 0 and
524288/524288 open files on all four nodes. No FFmpeg or ffprobe child was
running anywhere (`plurx_transcode_sessions_active` 0), so the realtime
class (nice 5 / `oom_score_adj` 500) could not be observed. A 40-second
poll of media1's children caught five library-root reachability `find`
children, already exited and awaiting their reaper, at nice 15 /
`oom_score_adj` 800: the background policy.

`plurx_child_priority_unapplied_total{class="background"}` rose 51 → 65 on
media1 and 3 → 5 on lab3 (lab6 and lab4 flat; realtime 0 everywhere). This
is a false positive of the read-back: a child that exits before it is read
back still reports its nice and `oom_score_adj`, but `ioprio_get` on an
unreaped child answers class 0, which the read-back counted as unapplied.
It was reproduced on lab3 with the same pre-exec policy: alive, class 2
level 7, nice 15, oom 800; exited and unreaped, class 0, nice 15, oom 800.
media1's background spawns are dominated by the short-lived reachability
`find`. The read-back fix is the follow-up PR from `claude/arch-followups-2`.

## K-08 embedding threads

Every node's daemon had only `tokio-rt-worker`, `plurxd` and
`plurx-child-own` threads: no `plurx-embed-*` thread and no global rayon
pool. Semantic search is enabled (replicated setting), but the
`semantic_embedding` background jobs were 0 queued and 0 running, and the
embedding pool is built on the first embedding, which had not happened
since each container started. The two-thread bar needs one authenticated
semantic search on each node first.

## K-05 query plans

On lab6 and lab4 the current Raft snapshot (the file `current` names, at
least two minutes old) was copied, verified unchanged and equal by size,
mtime and SHA-256, opened read-only and immutable, and removed after use;
the live database was never opened. Both copies were schema marker 82 with
6,979 items, the three M5 indexes and one `sqlite_stat1` row.
`item_by_external_id` (bound to a real `tmdb_id`) planned as covering
searches on `idx_items_tmdb` and `idx_items_imdb` under a rowid lookup, and
the canonical top-level count, with a literal and with a bound library id,
planned as `SEARCH items USING INDEX idx_items_top_level_title (library_id=?)`
on both nodes, matching the earlier media1 readback. These are plans from
Python's SQLite 3.46.1, not the daemon's bundled engine, and carry no
timings.

## Owed token-gated measurements

These need an admin bearer token and were not run: K-04 per-route
authority reads with and without the Read-After echo; C-03 cold poster
grid; C-05 and K-05 detail, Home and Title-grid route timings and the
documented page-latency runner; K-09 subtitle GETs from a non-owning node;
C-08's refused POST and RED sanity; S-07 pipeline verdicts; and the one
semantic search that makes K-08's thread count decidable. Physical-client
bars are in the 2026-10-10 device-evidence prompt, not here.
