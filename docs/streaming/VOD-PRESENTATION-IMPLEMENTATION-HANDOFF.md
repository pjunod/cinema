# VOD presentation — implementation handoff

**Status:** ready to build · **Executes:**
[VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) v2 (review round 1
resolved — see [VOD-PRESENTATION-PLAN-REVIEW.md](VOD-PRESENTATION-PLAN-REVIEW.md)
and [VOD-PRESENTATION-PLAN-REVIEW-RESPONSE.md](VOD-PRESENTATION-PLAN-REVIEW-RESPONSE.md))
· **For:** the implementing agent · **Written:** 2026-08-23 against branch
`docs/vod-presentation-plan` (docs) and `origin/main` @ `9cace96e` (code) —
**re-verify every cited line before relying on it; the tree is the truth,
this doc is the map**

## 1. Orientation — read this, work like this

Read in this order before writing any code: plan §0–§2 (the contract — §2
is normative), plan §7 (guardrails), plan §9 (settled decisions), the
review, the response, then this document. The plan says *what*; this
document says *how to execute it in this repo without failing its gates*.
Where this document and the plan disagree, **stop and flag — do not pick
one silently.**

Work one milestone per PR, in order (M0 → M8), each off current `main`,
branch names `agent/vod-m0` … `agent/vod-m8` (split a milestone into
`agent/vod-m2a`-style stacked PRs if it grows, but land them in order).
**M0's P0 proof gates everything**: if P0 fails any of its four clauses,
stop after M0 and write up what failed — that outcome is a success of the
process, not a failure of yours. M0 may change ledger decisions D1, D4, D6
and nothing else; every other §9 decision is settled — reopening one is a
stop-and-flag, not a judgment call.

The first mechanical step before M0: get the plan docs onto `main`
(fast-forward merge of `docs/vod-presentation-plan`), so milestone PRs can
cite them at stable paths.

Standing instructions, non-negotiable:

- If a step seems to require changing something plan §7 forbids (encoders,
  ladder numbers, DV pipelines, admission, offline packaging, recovery
  thresholds, the legacy live path's behavior), **stop and flag**.
- Nothing in M0–M7 may change behavior for a client that did not send
  `presentation: "vod"`. The proof is `playback-lab normalize`
  base-vs-candidate byte-identity (M2/M3 acceptance), not your reading of
  the diff.
- The deletion pass is M8 and only M8, gated on M7's evidence week.
  Resist every temptation to "clean up while in there."
- Telemetry parity is mandatory (plan §7): every beacon and server event
  that exists today still fires, or has a named successor, under the VOD
  presentation.

## 2. Ground rules of this repo — the gates and the traps

These are the reasons previous agents' green-looking branches failed CI or
review. All are current as of `9cace96e`; each names its source.

**Gates every PR must pass.**

```bash
make check              # fmt --check + clippy --workspace -D warnings + cargo test --workspace
make web-check          # any web/index.html or playback-policy.js change
make validation-lint    # catalog consistency
make history-check      # corrective-subject ledger (see below)
make operations-check   # doc/build-claim consistency (96 tests)
python3 -m validation.mobile_versions   # on any clients/** release-path change
```

- `cargo test` stops at the first failing TARGET — use `--no-fail-fast` or
  plurx-core failures hide behind plurxd ones. `plurxd` is a BIN:
  `cargo test -p plurxd --bin plurxd`, never `--lib`.
- Toolchain is pinned 1.97.1 (`rust-toolchain.toml`); the cloud sandbox
  honors it.
- **history-check:** `validation/history.py` `ISSUE_RE` matches corrective
  commit *subjects* (fix/harden/recover/stall-ish vocabulary — read the
  regex at `validation/history.py:21`). A matching commit touching
  `clients/**` must appear in a `tests/client-fixes.toml` anchor row, and
  a commit cannot anchor itself: land the corrective commit, then a second
  commit with a NON-matching subject ("test(apple): anchor …") adds the
  rows. Cheaper: phrase subjects outside the regex when the commit is not
  actually corrective (this plan's docs commits did exactly that).
- **mobile_versions + doc_versions:** any release-path change under
  `clients/apple/` or `clients/android/` requires the build counter to
  rise above the merge-base, and every doc claiming a build number
  (`clients/*/README.md`, `docs/*-CLIENT-PARITY.md`, `docs/STATUS.html`)
  must move in the same commit. Bumping `MARKETING_VERSION` alone FAILS.
- **Docs move with behavior, same commit:** PLAYBACK.md, STATUS.html,
  CHANGELOG. A PR that changes what the system does and not what the docs
  say is incomplete here, and `operations-check` often enforces it.
- Commit as `Claude <noreply@anthropic.com>` (the repo's verified-commit
  hook rejects other committers on unpushed work).

**Traps that have each cost a previous session real time.**

- The web player is `include_str!`-embedded: a JS syntax error compiles
  clean and breaks the whole UI at runtime. For ANY `index.html` change,
  extract the largest `<script>` block and `node --check` it; UI layout
  claims get measured in headless Chromium (`/opt/pw-browsers/chromium*`),
  not eyeballed.
- **Swift never compiles in the sandbox.** CI's macos lane
  (`make apple-test`) is the first real compile. Hand-screen deleted
  symbols, switch exhaustiveness, `@MainActor` placement; an adversarial
  review agent over the Swift diff is the working substitute and has
  earned its keep repeatedly. Kotlin DOES build here (SDK recipe:
  cmdline-tools → `sdkmanager 'platforms;android-37.0' 'build-tools;37.0.0'`
  → `local.properties` → `./gradlew --no-daemon :app:testDebugUnitTest
  :app:lintDebug`).
- **For anything about timestamps or muxer output, produce a stream and
  MEASURE it** — asserting on the argument list proves nothing (two P0s
  once shipped green for weeks that way). `ffmpeg -f lavfi -i testsrc2=…
  -g <fps·gop>` builds controlled-GOP fixtures; `ffprobe -show_entries
  packet=pts_time,dts_time,flags` reads the result. **Choose fixture
  parameters that make the two answers differ** — a GOP that divides the
  test's start offset hides an origin bug completely — and prove a test
  fails by reintroducing the bug before believing it. This rule is
  load-bearing for M0-P0 and M2's media-time work.
- macOS-vs-Linux test drift: any test comparing a `tempdir()` path against
  a canonicalized path passes here and fails on Paul's Mac. Reproduce
  with `mkdir /tmp/realtmp && ln -sfn /tmp/realtmp /tmp/linktmp;
  TMPDIR=/tmp/linktmp cargo test --workspace` before handing over test
  changes.
- Paused-clock tokio tests (`start_paused`): auto-advance leaps to the
  nearest timer whenever every task is in blocking IO — drive virtual time
  in small hops from the test body, or better, inject the bound through a
  settings seam. 2-core runners also surface machine-derived-budget
  failures a 10-core Mac never sees — inject budgets in any test that
  coexists two software sessions.
- A known pre-existing flake, not yours:
  `subtitles::cold_extraction_is_deduplicated_and_survives_waiter_cancellation`
  fails ~25–40% of runs. Re-run; don't debug it inside this arc.

**Store discipline (matters from M1 on).**

- SQLite migrations are the `MIGRATIONS` array in
  `crates/plurx-core/src/store/sqlite/mod.rs:41` — append, never edit;
  `SQLITE_SCHEMA_VERSION` derives from its length.
- Every store feature must keep **sqlite/hiqlite parity through the
  store_contract suite** — the clustering reviews' recurring scar is a
  mechanism shipped to one backend with the other inert.
- **Fragment indexes, rendition manifests, and dormant-session recipes are
  node-local facts** (plan D11: renditions are owner-bound). Keep them out
  of Raft, the way `hiqlite_catalog.rs` keeps derived-local items and the
  perf2 telemetry sidecar keeps playback rows. If the store fights this,
  stop and flag (plan §7).

**Deploy and device reality.**

- You cannot deploy or touch physical devices. Server deploys run through
  `scripts/ship` (which drives Paul's private Ansible repo); device
  installs and on-device acceptance are run by Paul's gpt agent. Your job
  at those milestones: write the exact protocol (commands, titles, pass
  criteria) into STATUS.html's operator checklist and hand Paul a
  ready-to-paste gpt prompt.
- Deploy trap: a bare `docker compose up -d --build` skips
  `PLURX_BUILD_REF` and the node reports `build: "unknown"` — use
  `make docker-up` / the playbook var, and verify landed builds via the
  unauthenticated `/api/v1/server`.
- Live-fleet debugging: plurx auth is a Bearer token in
  `localStorage['plurx_token']`; `/api/v1/activity/detail` and
  `/api/v1/system/playback-events?limit=400` are the live telemetry reads.

## 3. Cross-cutting additions (used by several milestones)

Name things exactly as the plan does; these are the shared surfaces.

**Settings keys** (add to `crates/plurx-core/src/store/mod.rs` `keys`
alongside `HLS_TYPELESS_SLIDING` at `:287`, same documentation style —
every key's comment says why it exists):

| Key | Default | First used |
|---|---|---|
| `playback.vod_presentation` | off | M3 |
| `playback.vod_block_secs` | 15 (P3 may revise) | M3 |
| `playback.vod_dormant_ttl_secs` | 21600 | M3 |

**Wire additions** (`CreateSession`, `crates/plurxd/src/http/hls.rs:105` —
follow its existing doc-comment style; both fields optional so every
shipped client is unaffected):

- `presentation: Option<String>` — `"vod"` opts in; anything else/absent =
  legacy. Honored only when `playback.vod_presentation` is on.
- `block_budget_secs: Option<u32>` — the client's declared segment-wait
  tolerance; server uses `min(playback.vod_block_secs, declared)`.

`StartResponse` gains nothing new for VOD beyond `vod: true` with
full-duration semantics (the cached path already models this — mirror it).

**Typed refusal vocabulary** (extend the `64a24854` table in
`http/hls.rs` / PLAYBACK.md §"when a stream cannot start"):

- `segment_pending` — 503 + `Retry-After`, on blocked-GET deadline expiry
  or wait-pool cap. Retryable by definition.
- `session_gone` — becomes **410** for terminal tombstones (B6); stays 404
  for genuinely unknown ids after tombstone pruning (24 h).
- `producer_failed` / `session_failed` — unchanged meanings, now also the
  answer for init-identity refusal and owner-death (D11).

**Where new server code lives** (plan §3): new modules —
`crates/plurx-core/src/segplan.rs` (index + plan types, pure),
`crates/plurxd/src/fragindex.rs` (indexer job),
`crates/plurxd/src/titlestore.rs` (store + eviction),
`crates/plurxd/src/vodserve.rs` or a clearly-bounded region of
`http/hls.rs` for serving. Resist adding to `transcode.rs` (17.3k lines);
integration points there should be calls out, not new subsystems in.

## 4. Milestone execution specs

Each spec: what to build, the interface shape (sketches — re-verify names
against the tree; if a sketch fights the code, flag it in the PR rather
than contorting either), the tests that make the acceptance runnable, and
the milestone's documentation/versioning obligations. Acceptance criteria
themselves live in plan §8 — restated here only where this document adds
mechanics.

### M0 — `agent/vod-m0` — feasibility proofs + probes (no product code)

Everything lands under `scripts/` + `docs/` + `/tmp`-style harnesses; zero
`crates/` changes. Deliverable is an appendix section **appended to
VOD-PRESENTATION-PLAN.md** (§12 "M0 results") recording P0's four proofs
and P1–P3's numbers, plus the D1/D4/D6 ledger entries marked resolved.

- **P0** — extend the `scripts/gop-census` machinery (or a sibling
  `scripts/vod-plan-probe`) to: run the production-shaped **video-only**
  copy pipe twice over each input and diff the fragment
  (DTS, duration, bytes, verdict) streams (determinism); compute the plan
  via `CutPolicy` over clean boundaries; then run the **full** production
  pipe (with audio) and prove every planned boundary lands on a clean
  fragment whose DTS matches, that discard-until-boundary-DTS converges
  from a `-noaccurate_seek -ss` start, and that real segment bytes stay
  under ceiling given the audio headroom; finally hash `init.mp4` across
  two separate process generations. Apply the measure-don't-assert rule:
  fixtures must be built so a wrong answer FAILS (offset ∤ GOP).
- **P2/P3 web halves** run through `scripts/playback-lab` (its
  token-bucket/loopback harness and `cases.json` criteria are the
  precedent); the **device halves are gpt runs** — write the protocol
  (exact stub-server invocation, titles, observation, pass criteria) into
  STATUS.html's operator checklist and give Paul the prompt.
- Gates: validation-lint · history-check · operations-check (docs+scripts
  commit) + `make check` if any script is Rust-adjacent.

### M1 — `agent/vod-m1` — `segplan` + fragment indexer (server, dark)

```rust
// crates/plurx-core/src/segplan.rs — sketch, re-verify tick/type choices
pub struct FragmentIndex { pub timescale: u32, pub rows: Vec<IndexRow>,
    pub init_sha256: [u8; 32], pub source: SourceIdentity }
pub struct IndexRow { pub dts: u64, pub duration: u64, pub bytes: u32,
    pub clean: bool }                        // fmp4::CutClass::is_clean()
pub enum PlanEntryKind { Video, AudioTail }
pub struct PlanEntry { pub index: u32, pub start_ticks: u64,
    pub duration_ticks: u64, pub est_bytes: u64, pub kind: PlanEntryKind }
pub struct SegmentPlan { pub entries: Vec<PlanEntry>,
    pub target_duration: u32, pub timescale: u32, pub version: u32 }
pub fn plan_copy(idx: &FragmentIndex, policy: &CutPolicy,
    tracks: &TrackDurations) -> SegmentPlan;   // pure; audio tail per c58a4307
pub fn plan_transcode(duration_ms: i64) -> SegmentPlan;  // 2 s grid
```

Daemon side: the indexer job (background at scan/analyze; reuse the scan
job plumbing) runs the video-only pipe through the existing fmp4 reader +
`classify`, persists `FragmentIndex` keyed by file identity recipe
(node-local table — sqlite migration appended to `MIGRATIONS`, hiqlite
kept in parity as derived-local, store_contract extended). Invalidation:
source (size, mtime) change drops the index.

Tests: P0's suite as fixtures (`plan == materialized boundaries` on the
corpus), determinism, invalidation, `plan_copy` property tests
(floor/ceiling/first-segment rules, audio-tail entries), <100 ms plan
build from a persisted index.

### M2 — `agent/vod-m2` — title store + producer scheduler (server, dark)

The big one. Two subsystems behind zero client-visible change:

```rust
// crates/plurxd/src/titlestore.rs — sketch
// Per-segment state, three facts (plan §2.4 / D12):
enum SegState { Planned, Materialized { bytes: u64, at_ms: i64 } }
struct Rendition { key: RenditionKey /* file identity + recipe + rung */,
    plan: SegmentPlan, init: StoredInit, states: Vec<SegState>,
    admitted: bool /* set only by atomic completion */ }
```

- Storage: cache-generation layout (reuse `produce.rs::assemble` /
  `publish_from` / `complete_cache_entry` shapes) with a bitmap-with-holes
  manifest. Reader guards via `cachekeep.rs`. Eviction clears
  `Materialized`, never inside any reader window, never on an admitted
  generation's members. Admission threshold from planned total size vs
  `cache.max_gb`; reservation before backfill.
- Scheduler: one producer per `RenditionKey`; demand = requested frontier
  + `AHEAD_HORIZON` (existing constant); suspend/resume reuses the
  SIGSTOP machinery (`apply_ahead_window`'s internals) with demand from
  *requests*; reposition (kill + `-ss` boundary + discard-until-DTS) when
  the gap exceeds `REPOSITION_GAP` (60 s); watchdog serialization per
  PR #244's transition-ownership pattern.
- Media time: the segmenter rebases `tfdt` to plan `start_ticks` and
  stamps `mfhd` = index+1 (extend `fmp4::merge`/`Segmenter` — this is
  plurx-core, property-test it hard); transcode restarts get
  `-output_ts_offset`; init identity enforced against
  `FragmentIndex.init_sha256` with typed refusal.

Tests are the acceptance list in plan §8 M2 — the noncontiguous
two-generation timestamp test and the over-budget synthetic title are the
two that must exist before review. Measure timestamps with ffprobe on real
produced bytes, never by asserting on args (§2's rule). Legacy invariance:
`playback-lab normalize` byte-identical, run and recorded in the PR.

### M3 — `agent/vod-m3` — VOD serving behind the opt-in (server)

- Create path: parse `presentation`/`block_budget_secs`; when honored,
  attach the session handle to a `Rendition` (creating/joining its
  producer) and answer `vod: true` + full duration; playlist rendered
  from the plan (immutable bytes — render once, store, serve).
- Segment GET: the three-outcome contract with the hard deadline, typed
  `segment_pending`, per-session (4) and global wait caps, coalescing,
  disconnect cancellation (drop the wait future on body drop — verify
  axum/hyper actually cancels; write the test before the impl).
- Lifecycle: dormant/terminal per the plan's state diagram; tombstone
  writes on DELETE/supersession/admin/revoke/file-invalidation; sliding
  TTL touch on authorized GETs; resurrection from the persisted recipe
  row (node-local table, migration + parity as in M1).
- The curl transcript for docs/PLAYBACK-TESTING.md is part of the PR.

### M4 — `agent/vod-m4` — web adoption

Send the flag + budget from `attachHls` config; treat every VOD session
like today's cached-VOD arm; raise `fragLoadingTimeOut`/retry per P3;
`node --check` the extracted script; playback-lab suites + the nynuc
protocol (2 h 4K remux, 20-seek storm with a server-log zero-create
assertion, sleep/wake) — the nynuc runs are gpt's, protocol into
STATUS.html. **No deletions** (M8).

### M5 — `agent/vod-m5` — Apple adoption

Flag + VOD path (the `hls.vod` arm generalizes); build bump + doc claims
+ STATUS device rows; Swift is review-gated here, CI-compiled — run the
adversarial-agent pass over the diff before handing to CI. Device matrix
(plan §8 M5: the wedge title file 5836, DV P5, Dexter S01E01 audio tail,
>60 s deep pause, autoplay boundary, 30-min scrub torture) is a gpt
protocol in STATUS.html with pass criteria phrased as observables (zero
terminal screens, zero resume-behind, stall beacons only under induced
starvation).

### M6 — `agent/vod-m6` — Android adoption (+S1/S2 fixes ride along)

Flag + VOD timeline (`sessionIsVod` arm generalizes; `MediaOrigin`'s
session arm bypassed under VOD). The plan's §6 stopgaps S1 (sliding-window
position — verify on device first) and S2 (stall tracker emits at
threshold while stalled + exhaustion beacon) land here if not already
landed as independent PRs — check `main` first. Kotlin gates run in-sandbox
(recipe in §2); build bump + doc claims; Google TV + phone gpt protocol.

### M7 — `agent/vod-m7` — fleet flip + burn-in

Deploy via `scripts/ship` (gpt runs it; verify `/api/v1/server` build on
all three nodes — nynuc, m6, nuc4); flip `playback.vod_presentation`; the
comparison week reads `playback_events` (stall rate, reopen rate,
`session_end` reasons, TTFF) against the prior week; the power-pull test
(plan §8 M7) needs Paul physically — protocol + rollback note ("one
setting") into STATUS.html. Record the week's numbers there; **M8 does not
start unless they show the failure categories gone** (plan §11).

### M8 — `agent/vod-m8` — the deletion pass

Per-client deletion lists are plan §4.1–§4.3; the version floor and the
typed "server needs updating" refusal are plan §8 M8. Verify the fleet
floor first (all nodes ≥ VOD build, recorded). Expect the largest
history-check interaction of the arc: deletions of recovery machinery are
corrective-looking commits touching `clients/**` — budget for anchor rows.
PLAYBACK.md gets its rewrite here, same commit as each client's deletion.

## 5. Definition of done, every PR

- All §2 gates green, run locally before handoff, `--no-fail-fast`.
- Docs moved in the same commit: PLAYBACK.md (when behavior), STATUS.html
  (always — the VOD row's state advances per milestone), CHANGELOG
  (`[Unreleased]`), build-claim docs on client bumps.
- New behavior has a test that FAILS when the behavior is reverted —
  prove it by reverting once (the mutation habit; reviews here run
  mutation passes and green-suite-under-mutation is the most common
  blocker in this repo's review history).
- Acceptance evidence quoted in the PR body: command + output for
  runnable checks; "protocol handed to operator" + STATUS.html row for
  device checks. Never claim device evidence you don't have — unclaimed
  acceptance is written down as unclaimed (the N0/N4 rows are the house
  precedent).
- Anything you could not verify, one clause in the PR body saying so.

## 6. When blocked

Stop-and-flag means: stop the milestone, write the conflict into the PR
body and a STATUS.html note (what you hit, which plan section it strains,
the options you see), and end the run there. Do not improvise around plan
§7, do not reopen §9 decisions, do not patch the legacy path "while
you're in there." A flagged blocker after M0–M2 is an expected outcome of
this plan's design, not a failure — P0 exists precisely to be allowed to
fail.
