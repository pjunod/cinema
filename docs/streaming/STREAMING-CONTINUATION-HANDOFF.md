# Streaming reliability — handoff for the continuing session

**Status:** open — server work landed through 2026-09-08; the remaining §4 item is
blocked on a hardware measurement · **Repo:** `noirr/plurx` on Forgejo ·
**Branch:** `effort/streaming-reliability` · **Written:** 2026-09-08

Read [§1](#1-access-and-the-rules-that-are-not-negotiable) before touching
anything — two of those rules have cost real rework. Then [§3](#3-what-is-left)
for the queue. [§5](#5-the-machinery-that-will-bite-you) is the section you
will wish you had read first; every item in it broke a build during the work
that produced this document.

Work milestone by milestone. **If a step seems to require changing the
control protocol's wire format, stop and flag it** — `ControlRequestV1` and
`ControlResponseV1` are both `deny_unknown_fields` and both are re-parsed by
a relaying node, so "just add a field" is a rolling-upgrade break, not an
addition. That mistake has already been made once and caught in review.

---

## 1. Access and the rules that are not negotiable

**Forgejo is the sole write, PR, CI and merge authority.**
`http://forge.lan:3000/noirr/plurx` · SSH
`ssh://git@forge.lan:222/noirr/plurx.git`. GitHub is historical and
read-only; PR numbers below 900 in old docs are GitHub, current ones are
Forgejo.

**Credentials** live on Paul's machine at `~/code/plurx-agent/`:

| File | What it is |
|---|---|
| `forgejo_token` | The Forgejo API token. **This is the one you want.** |
| `.gh-token` | A GitHub token — wrong for this repo, fails with "token is malformed" |
| `.ssh-deploy-key` | SSH key for reaching other nodes |

**Never work in Paul's clones.** Make your own, from Forgejo, on your own
disk. Stated explicitly and more than once.

**The PR lifecycle Paul wants, in this order:**

1. Proper commits — real messages, the reasoning in the body.
2. The fast local lane only, until a PR is together. An early PR burns a full
   CI fan-out for nothing.
3. Open it as a **draft**. Forgejo has no draft flag — it is a `WIP:` title
   prefix, and Forgejo refuses the merge while it is there. Un-draft with a
   title edit: `PATCH /api/v1/repos/noirr/plurx/pulls/<n>` with
   `{"title": "..."}`.
4. **One** adversarial agent review, after you have a PR you want to merge —
   not one per commit. Paul was explicit about this.
5. Implement the findings.
6. Run the full suite **once**, after all the fixes.
7. Watch the unit tests, fix until green.
8. **Merge it yourself.** He does not want to click.

**Keep the status page updated** — `docs/streaming/STREAMING-RELIABILITY-STATUS.md`,
newest chronicle entry first — so progress can be determined without asking
him.

**Do not gate features in code.** Put an enable section in Settings →
Developer saying what is needed to turn it on safely and whether each part is
currently met — advisory only, never blocking the enable.

**Do not stop while there is buildable work.** A status question is not
permission to stop; answer it in a line and keep going. Waiting on a merge is
not a blocker — stack the next branch on the open one.

---

## 2. What landed, and what it means

Six PRs merged into `effort/streaming-reliability` on 2026-09-08, on top of
the previously merged #91 / #97 / #105 / #127 / #128 / #141 / #146.

| PR | What it did |
|---|---|
| [#148](http://forge.lan:3000/noirr/plurx/pulls/148) | §4 step 2 server half — every node accepts `switched` and releases the predecessor's drain on it |
| [#153](http://forge.lan:3000/noirr/plurx/pulls/153) | The last three hidden gates, plus takeover honouring the live-HLS switch |
| [#160](http://forge.lan:3000/noirr/plurx/pulls/160) / [#161](http://forge.lan:3000/noirr/plurx/pulls/161) | `docs/playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md`, the client adapters' brief |
| [#162](http://forge.lan:3000/noirr/plurx/pulls/162) | The finding that §4's remaining server work is blocked |
| [#163](http://forge.lan:3000/noirr/plurx/pulls/163) | Status page |

**§4 step 2, in one paragraph.** A client that reports its successor is on
screen (`switched`) releases the predecessor's ten-second drain immediately
instead of paying the whole window. The release lives in
`hls.rs::control_local` — the one function both public ingress *and* the
internal relay reach — runs **after** the exchange so the reporting packet is
answered rather than told `410`, and runs **only on success** so it sits
behind the client-instance and monotonic-sequence fences in
`ControlState::accept`. A `switched` naming a still-`Staged` slot aborts that
preparation rather than leaking an encoder permit until its deadline lapses.

**No client may send `switched` yet.** `AcknowledgementState` has no
unknown-value fallback, so one sent to an older owner is a deserialize
failure, an empty `400`, and a `503` the client retries forever — it wedges
rather than degrades. The negotiation that would let a client ask was built
and **withdrawn**: `ControlResponseV1` carries `deny_unknown_fields` and the
*relaying* node re-parses it, so the field broke every cross-node exchange
during a rolling upgrade. That negotiation needs `deny_unknown_fields`
relaxed, shipped alone, and rolled out fleet-wide first — three releases, not
two.

---

## 3. What is left

### 3.1 Blocked on hardware — the successor's producer

**This is the last piece of §4 and it is not a coding task.** The full
reasoning is in `docs/streaming/STREAMING-RELIABILITY-HANDOFF.md` §4's banner; the
short version is three facts that stack:

1. `PREPARED_AXIS_SETS` admits exactly two crossings: `{ResolutionOrBitrate}`
   alone, or that paired with `{DeliveryMethod}`.
2. `rendition_key` (`vodserve.rs`) hashes file id, source identity, audio
   index, AAC, the Dolby Vision flags, audio offset and cache key — **not
   height, not bitrate**. Two Copy recipes differing only in resolution share
   one rendition: same producer, same bytes. On a Copy source the delivered
   media *is* the source, so that crossing alone is not a transition at all.
3. The pair that does change the media — Copy→Transcode, what the 2026-09-03
   hardware run measured — is refused by immutable-VOD with
   `501 vod_transcode_unavailable`, *"transcode serving is gated on the D6
   device measurement"*.

So the worker can be built and, for every case VOD may legally serve, it
would produce the stream the viewer already has. **Transcode-rung VOD stays
unavailable until the open P2/D6 measurement establishes that AVPlayer and
Media3 tolerate the planned EXTINF timing** — an Apple TV and an Android TV
in a room. Do not spend a week on the plumbing expecting a demo at the end.

Two findings recorded for whoever builds it after D6 lifts, both in the
handoff banner and both worth re-reading there rather than rediscovering:
the **publication fork** (clearing the sentinel makes the route playable with
no new gate but hands the row to the 3-second lease loop, breaking the "one
clock" invariant), and the **fourth teardown path** (`SESSION_IDLE_TTL` is
300 s while `PREPARATION_DEADLINE_MS` is 330 s, and the idle reap is
deliberately tombstone-free, so nothing clears the slot).

### 3.2 The three client adapters

Separate handoffs, one per platform, built in parallel. They share
`docs/playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md` and nothing else. **Apple first** —
it is the only platform with a measured `true` behind the capability.

Coordination rules that matter across the three:

- **One session at a time owns `crates/plurxd/src/web/index.html`.** The web
  adapter shares that single large file with every settings panel.
- **No adapter changes the server.** If an adapter seems to need a change in
  `playback_control.rs`, that is either a wrong contract or a wrong document —
  raise it, do not work around it.
- Each adapter is its own PR into `effort/streaming-reliability`, its own
  review, its own qualification. They do not stack.

### 3.3 Not yours

- **The dirty VOD recipe task** in another author's worktree. The handoff's
  immediate queue says finish that work, not reimplement it.
- **The main-promotion qualification.** Real devices, two-hour 4K, seek and
  quality storms, capacity and node loss. Paul's.

---

## 4. Open decisions, recorded rather than taken

| Decision | State |
|---|---|
| `MEDIA_SESSION_DRAIN_MS = 10 s` against `DEFAULT_MAX_HW_SESSIONS = 2` | Chained drains want three encoders. Left at 10 s. Step 2 is what shortens it. |
| Seeding the two retired switch variables | Fleet-wide first-writer-wins via `put_setting_if_absent`. Documented in `main.rs::seed_switch_settings`. |
| Peer takeover and the live-HLS switch | **Settled by Paul, 2026-09-08: takeover refuses and names the switch.** Do not reopen — the reasoning, including the two framings that were wrong first, is in the handoff. |
| The publication fork (§3.1) | Recorded preference: leave the sentinel, teach `classify_durable_route` one more case, read only when the route would otherwise be refused. Not implemented. |

---

## 5. The machinery that will bite you

Every item here cost a CI failure or a broken build during the work that
produced this document.

### 5.1 The corrective-history audit

`make history-check` → `scripts/history-audit`. It scans **commit subjects and
bodies** for a large vocabulary of corrective words — `fix`, `correct`,
`stop`, `remove`, `withdraw`, `regression`, `wrong`, `failure`, `avoid`,
`keep`, `preserve`, `refuse`, `recover`, `bound`, `omit`, `restore`,
`prevent`, `invalid`, `compatible`, `fallback`, `truth`, and more — and every
match needs a `validation/regressions.d/*.toml` entry or the gate fails.

Three rules that are not obvious:

- **The file must be named after the *first* commit it maps.**
  `<sha8>-<slug>.toml`, where `<sha8>` is `commits[0]`. Naming it after the
  newest commit fails with a message telling you the name it wanted.
- **`points` and `checks` are validated against `validation/points.toml`.**
  An id that appears in another ledger file is not proof it is still valid —
  `cluster.store` and `cluster-store-check` are both stale and both rejected.
  The error names the points your commit's files actually map to; use those.
- **The ledger commit itself is scanned.** A subject like "file the takeover
  fallback ledger entry" fails because `fallback` is in the vocabulary. Word
  ledger commits without corrective vocabulary.

The audit needs `tomllib`, so Python 3.11+. On a 3.10 box, vendor `tomli` and
shim it:

```bash
pip3 download tomli -d /tmp/tw && unzip -q /tmp/tw/tomli-*.whl -d /tmp/tp
mkdir -p /tmp/shim && cp -r /tmp/tp/tomli /tmp/shim/
printf 'from tomli import *\nfrom tomli import load, loads\n' > /tmp/shim/tomllib.py
PYTHONPATH=/tmp/shim scripts/history-audit --report /tmp/hist.json
```

### 5.2 The module-wide sentinels

`tests/playback/rolling-producer-owners.toml` holds exact occurrence counts
for patterns across `crates/plurxd/src`. Adding a test that awaits a store
call, or writes `Duration::from_millis`, or names `publication_ready_at_ms`,
moves a count and fails `python3 -m unittest discover -s tests/validation`.

**Always grep for the summary line.** A truncated `tail` of that suite once
read as a pass and hid five failures:

```bash
python3 -m unittest discover -s tests/validation -p 'test_*.py' 2>&1 \
  | grep -E "^(OK|FAILED|Ran )|^FAIL:"
```

Each count carries a comment explaining what moved it. Add yours in the same
style — the allowlist exists to force a review, not to be silenced.

### 5.3 The hiqlite placeholder rule

Replicated SQL uses `$N` placeholders and they must **first appear** in
ascending order. `... SET a = $1, b = $3 WHERE c = $2` is rejected at
runtime with *"placeholders must first appear in order; expected $2, found
$3"*. Past incident `ce253a55`. Renumber so first appearances ascend.

### 5.4 Both store backends, always

`sqlite/sessions.rs` and `hiqlite_sessions.rs` are twins and must not
diverge. Shared SQL text lives in `plurx-core/src/store/mod.rs` as macros so
both hold the identical statement. A migration that is one statement for
SQLite is often two for hiqlite: the replicated backend submits prepared
statements, so a trailing `;` makes it unpreparable — hence the paired
`*_COLUMN` (no semicolon) and `*_SCHEMA` (with) constants.

Also: `"` inside a SQL comment inside a Rust string literal closes the
literal. Reword without quotes.

### 5.5 Two hand-wound schema fixtures

`sqlite_v43_guard_migration_refuses_an_unrecoverable_committed_claim`
(`sqlite/dv_conversion.rs`) and `populated_v14_import_fixture`
(`tests/store_contract.rs`) both wind a current database backwards by hand.
**Every migration you append must be undone in both**, and
`the_downgrade_fixture_undoes_every_migration_after_the_guard` asserts the
count so it fails on the commit that opens the gap. Drop a trigger before the
column it reads — SQLite refuses otherwise.

### 5.6 Test fixtures that reach the wrong engine

Every existing `stage_prepared_successor` test runs against the **rolling**
gate, not `VodPreparationGate`, because `HlsDeliveryFixture::publish`
registers into the rolling map. `VodServe::install_http_test_session`
publishes a producer-less attachment. Both will let an acceptance pass while
proving nothing — the exact failure `PreparationGate`'s own doc predicted
once already, and it happened.

### 5.7 Known flake

`transcode::tests::a_client_fetch_releases_a_held_session_and_restarts_progress`
failed once under a full-workspace run at `crates/plurxd/src/transcode.rs`'s
`assert!(!session.suspended…, "session was released")`, and passed in
isolation and on a re-run. Timing-sensitive, not in code this work touched.
Re-run before chasing it.

The CI runner's `effort Rust compile` job has also been observed flaky —
identical content passed, failed, passed.

---

## 6. Verification — what to actually run

```bash
cargo fmt --all                                    # before anything
cargo clippy -p plurxd --all-targets               # must be silent
cargo test -p plurxd --bin plurxd -- <filter>      # the fast lane
make web-check                                     # embedded UI + contrast
PYTHONPATH=/tmp/shim scripts/history-audit --report /tmp/hist.json
PYTHONPATH=/tmp/shim make validation-lint
python3 -m unittest discover -s tests/validation -p 'test_*.py'
make operations-check                              # deploy/CI/shipping contracts
```

The full suite, **once**, after all fixes are in:

```bash
CARGO_INCREMENTAL=0 cargo test --workspace --all-targets
```

It takes roughly 30 minutes and produces about 3,230 tests across 15 result
lines. Run it in the background and grep the summary — do not watch it:

```bash
nohup bash -c 'CARGO_INCREMENTAL=0 cargo test --workspace --all-targets \
  > /tmp/full.log 2>&1; echo "EXIT:$?" >> /tmp/full.log' &
grep -E "^test result" /tmp/full.log | awk '{p+=$4; f+=$6} END {print p,f}'
```

`plurxd` has no lib target — `cargo test -p plurxd --lib` fails. Use
`--bin plurxd`.

**Disk.** A debug `target/` reaches 24 GB and a full-workspace run will hit
"No space left on device" mid-link, reported as a linker signal rather than
as a disk error. `rm -rf target/debug/incremental` frees ~13 GB.

---

## 7. Non-goals — do not do these

- **Do not add a field to `ControlRequestV1` or `ControlResponseV1`.** Both
  are `deny_unknown_fields` and the relaying node re-parses the response. Any
  field is a rolling-upgrade break. §2 has the incident.
- **Do not teach any client to send `switched`.** Server-first, and the fleet
  is not there.
- **Do not gate a feature behind a compile flag or an environment variable.**
  The whole hidden-gate inventory was just closed; do not reopen it.
- **Do not make the cluster do what a setting forbids.** Settled with the
  takeover decision; it is now a rule, not a case.
- **Do not build the successor's producer expecting to demonstrate a switch.**
  §3.1.
- **Do not run more than one adversarial review per PR**, and do not run one
  before the PR is merge-ready.
