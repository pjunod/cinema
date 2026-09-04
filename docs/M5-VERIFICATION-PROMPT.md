# M5 verification — the first converted stream a browser ever plays

You are verifying, on the live fleet, that a Dolby Vision Profile 7 title
reaches a browser as **Profile 8.1** and plays. This is the only step that
proves the feature; everything merged in PR #869 removes a reason it could
not run, and none of it substitutes for this.

Do not change code. Do not fix raft membership. Do not retarget analysis
jobs to another node. If a gate below is shut, stop and report which one —
a shut gate is the answer, not an obstacle.

## 0. What you have

- SSH to the four nodes as `pjunod@{nynuc,m6,nuc4,nuc3}` with the key at
  `~/code/plurx-agent/.ssh-deploy-key` (copy it to `~/.ssh/id_ed25519`,
  chmod 600).
- The plurx API needs a bearer token. Get one the way a browser does, or
  ask Paul. `curl localhost:32400/api/v1/system` unauthenticated returns
  `{"error":"authentication required"}`.
- The subject is **file 70**, a Profile 7 dual-layer title.

## 1. Check you are testing the build you think you are

The check is not "does the fleet match a version written down here" — it
cannot be, because `main` moves and a version in a document is stale the day
after it is typed. It is **do the four nodes agree with each other and each
with its own checkout**, and is the build they agree on an ancestor of the
change you are testing.

The last two readings, both taken while writing this: 19:17 UTC all four on
`v0.3.0-568-gd4c67ff4`, 21:55 UTC all four on `v0.3.0-575-gaa486f4b`. Two
and a half hours, one whole generation. Read it yourself:

```bash
for h in nynuc m6 nuc4 nuc3; do
  printf '%-7s ' "$h"
  ssh pjunod@$h 'docker logs plurxd 2>&1 | grep -m1 "plurxd starting";
                 echo "checkout=$(cd /opt/noirr/plurx && git describe --tags --always)"' \
    | sed -E 's/\x1b\[[0-9;]*m//g; s/.*build="([^"]+)".*/\1/' | paste -sd' '
done
```

The escape-stripping `sed` is not decoration. `plurxd` logs through
`tracing`'s formatting layer with ANSI on, so the line reads
`…build<ESC>[0m<ESC>[2m=<ESC>[0m"v0.3.0-…"`, and a pattern expecting a
literal `build="` matches nothing and dumps the whole raw line instead.

`docker exec plurxd plurxd --version` reads the same stamp; the log line is
used here because it survives a container that has exited or is restart
looping, which is exactly the state worth catching. If the node you are about
to test runs a binary older than its own checkout, or older than the others,
have it redeployed first and say so in the report. A Safari play against a
stale node proves nothing about #869.

The clients are a separate hand-off: `docs/CLIENT-DEPLOY-PROMPT.md`. Nothing
in this document needs them.

## 2. The ops gate — do not proceed past a shut one

M5 is gated, in order:

1. **Raft membership.** All voters healthy, no tombstoned node answering.
   `GET /api/v1/system` → the cluster block. Guardrail: never repair this
   from code.
2. **The analysis queue draining.** `analysis_requests`: `queued` falling,
   `running` leases fresh (not the 9,915-lost-lease / 1,933-at-attempt-limit
   picture from 2026-08-31). This is PR #866's territory
   (`effort/fragment-index-queue-repair`), not yours.
3. **File 70's converting identity built.** A `fragment_indexes` row for the
   `dv_convert` argv fingerprint, and `dv_conversions` non-empty on file
   70's owning node.

Note the live store is **hiqlite**, not the stale `/var/lib/plurx/plurx.db`
(last written 2026-08-26). Read these through the API, not by opening a
file. There is no `sqlite3` in the container.

If gate 1 or 2 is shut, report that and stop. If gate 3 alone is shut,
say so — it may open on its own once 2 drains.

## 3. The reads that decide it

On file 70's **owning** node, with `T` a bearer token and `NODE` its base
URL. Follow `M5A-VERIFICATION-ON-NUC4.md` §3 for the shared steps, plus
these web-specific ones:

```bash
# the init segment the browser is handed: dvcC must say profile 8, not 7
curl -s -H "authorization: Bearer $T" "$NODE/api/v1/.../init.mp4" | xxd | grep -i 'dvcc\|dvvc'

# the master playlist
curl -s -H "authorization: Bearer $T" "$NODE/api/v1/.../master.m3u8" | grep -i 'CODECS'
```

Pass looks like:

- `CODECS="hvc1.2.4.L153.B0"`-shaped — `hvc1`, not `dvh1`;
- `SUPPLEMENTAL-CODECS="dvh1.08.06/db1p"` — **08**, the converted profile;
- the create response carries `delivered_dolby_vision_profile: 8`;
- Safari plays it, with the DV badge;
- Chrome plays the same title as HDR10 (Chrome's caps probe never answers
  yes to `dvh1.05.06` or `.08.07`, so `/decision` strips for it);
- `scripts/dv-timeline-check` passes from zero **and** from a seek.

## 4. The log lines that explain a failure

```bash
docker logs plurxd --since 10m 2>&1 | grep -E \
  'playback capability decision|create trusted|vod_index_pending|cannot convert Dolby Vision|copy-video HLS ffmpeg args|stream_rejected|plan_derivation'
```

What each tells you:

- `decision … method=Remux preserve_dolby_vision=true` with the "converted
  to Profile 8.1" reason — the ladder chose conversion.
- **`create trusted the client's plan echo` should now be absent** for a web
  create. PR #869's M1 makes the web player send its caps document on every
  create, so the create derives the plan rather than trusting the echo. If
  you still see it from a browser, M1 did not reach that node.
- `VOD prerequisite unavailable … refusal="vod_index_pending"` — gate 3 is
  shut; the converting identity has not been indexed yet.
- `this copy cannot convert Dolby Vision, so it strips to the HDR10 base` —
  this is the **live-HLS recovery** path, not the VOD path. Seeing it during
  an M5 run means you fell back to recovery and are not testing conversion.
- The argv line: for a **converted** stream expect no `-strict unofficial`,
  `-tag:v hvc1`, and a filter carrying `remove_types=…62-63` or
  `dovi_rpu=strip=1`. If you see `-strict unofficial … remove_types=32-34`
  you got the *preserve* argv — the node answered create with
  `render_caps().dolby_vision_convert == false`. Check `GET /api/v1/system`
  for `dovi_rpu` and `PLURX_DV_CONVERT` on that node before anything else.

## 5. Counters

`GET /api/v1/system` → `plan_derivation`. After a web play on a #869 build,
`rederived` should move and `legacy_trusted` should not. As of the check
above, nuc4 had logged **no** plan-derivation traffic in 12 hours — these
counters only move when someone actually plays something, so read them
immediately before and after your play, not in the abstract.

## 6. What to report

One page. In it: which node, which binary, which gates were open, the three
reads from §3 verbatim, and the verdict. If it failed, the §4 line that
explains why. If a gate was shut, name it and say nothing else was
attempted.
