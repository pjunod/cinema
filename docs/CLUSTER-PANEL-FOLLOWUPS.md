# Cluster panel follow-ups — live status, a real module seam, one credential surface

**Status:** ready to build · **Depends on:** PR #651 (`agent/cluster-operations-rail`,
code head `7e417145`) — if it is not merged when you start, branch from its head ·
**Written:** 2026-08-30 by the session that built PRs #647 and #651 · **Ratified:**
Paul approved all three workstreams on 2026-08-30.

Read [docs/OPERATIONS.md](OPERATIONS.md) §"The Cluster tab" first — it describes
the panel this plan extends, and PR #651's description carries the measured
numbers behind the layout decisions. Work milestone by milestone (§6); each ends
with a runnable acceptance check. One adversarial review of the WHOLE deliverable
at the end, not one per workstream — that cadence is a standing instruction. If a
step seems to require changing a server endpoint, a payload shape, or anything
under `crates/plurx-core/`, stop and flag it instead: every workstream here is
deliberately client-only.

---

## 1. Objective

Three defects, in the order they bite an operator:

1. **The rail's verdicts are evaluated off a frozen sample.** `/cluster/status`
   is fetched once when the tab opens (`SETTINGS_MANIFEST.cluster.secondary`)
   and never again unless a button or the restart poll refreshes it. Every
   Ready/Blocked chip, the node cards' Operations groups, and the database
   ledger's direct readings age off that one fetch. The "Reading age" row
   admits it; nothing fixes it. A rail whose pitch is "read the refusal before
   you click" must not read it off a nine-minute-old snapshot. →&nbsp;§3
2. **The cluster panel has no module boundary.** ~2,700 lines of cluster logic
   live inline in `index.html`, and the test suite reaches them by slicing the
   file at `\nfunction name(` — which is why a bare `const` between functions
   silently vanishes from the sandbox and everything must sit at column zero.
   Those are conventions carrying load a file boundary would carry
   structurally. The repo already has the pattern: `playback-policy.js` is a
   separate served file, `require()`d directly by its tests. →&nbsp;§4
3. **The credential has two surfaces.** The rail's Add row routes to a Danger
   zone `<details>` that holds the join-token minting panel and the graceful
   leave — two places whose *decisions* now happen one card up, in the rail.
   The token panel should be the expansion of the rail's Add row: one fewer
   surface, and the credential still renders exactly once. →&nbsp;§5

Every workstream is `crates/plurxd/src/web/` plus tests and docs. No new
endpoint, no payload change, no Rust beyond one route + one `include_str!`
constant in §4.

## 2. Ground truth — where everything lives

Re-verify each of these against the tree at build time; line numbers drift.
All were checked against `7e417145` on 2026-08-30.

| Thing | Where |
|---|---|
| The whole panel | `crates/plurxd/src/web/index.html` (~14.5k lines, one file) |
| Loaders + manifest | `SETTINGS_LOADERS.clusterOps: ()=>api("/cluster/status")` · `SETTINGS_MANIFEST.cluster:{required:["cluster"],secondary:["clusterOps"]}` |
| The poll loop | `settingsTick()` — runs every 2s via `setPageTimer(()=>settingsTick(generation,tab), 2000, generation)` in `viewSettings`; guarded by generation, route, tab, `visibilityState`, and a `SETTINGS_TICKING` re-entrancy latch |
| Tick's cluster branch | refetches `/cluster/nodes` only while `live` (a node in maintenance, or recovery required); diffs an `operational()` projection; calls `renderSettings()` only on change |
| Manual ops refresh | `refreshClusterOperations(btn)` — deletes the key, refetches, full `renderSettings()` |
| Restart-prep poll | `pollLocalRestart(nodeId, generation)` — 2s loop on `/cluster/status` while a preparation is active |
| Rail | `clusterRailBlocker` · `clusterRailRow` · `clusterOperationRows` · `clusterOperationsRail` · `openClusterDanger` |
| Database section | `clusterDatabaseStatus/Wal/Health/Summary/Rows/Panel` · `clusterSampleAge` · `clusterSqliteReplication` · `clusterCapacityText` · `clusterHealthPill` |
| Folds | `clusterFoldKey/Read/Write` · `clusterNodeFoldId/Later/Save` · `applyClusterFolds` · `setClusterDatabaseFold` · `toggleClusterDatabase` — one `localStorage` key `plurx.cluster.fold` = `{database, nodes_closed[], tab}` |
| Danger zone | `<details class="cldanger">` in `clusterPanel` wrapping `joinPanel(maintenanceActive)` + `leavePanel(cluster.local_node_id, maintenanceActive)` |
| Token lifecycle | `CLUSTER_TOKEN` module var; dropped by `forgetJoinToken()` on tab switch, route change, and logout — three call sites, find them all |
| The extraction precedent | `crates/plurxd/src/web/playback-policy.js` — UMD factory (`module.exports` + `root.PlurxPlaybackPolicy`), `include_str!` in `crates/plurxd/src/http/web.rs`, route `/assets/playback-policy.js` in `crates/plurxd/src/http/mod.rs`, `<script src>` before the inline script in `index.html`, `require()`d by `tests/playback/web-policy.test.js`, listed in `validation/points.toml` |
| Test harness | `tests/web/cluster-membership.test.js` — `shippedSource(name)` slices `index.html` at `\nfunction name(` → next top-level `function`; 89 tests at `7e417145` |
| Gates that read `index.html` directly | `scripts/js-check` (inline blocks) · `make web-check` incl. `scripts/contrast-check --from-index` · `scripts/ui-baseline` · `validation/ci_scope.py` (`crates/plurxd/src/web/**`) · `validation/points.toml` |

## 3. Workstream 1 — the direct status refreshes itself

### 3.1 The contract

While the Cluster tab is visible, `/cluster/status` is refetched at most every
**15 seconds**, inside the existing `settingsTick` loop. Not a new timer:
`settingsTick` already owns the cadence, the generation guards, the visibility
check, and the re-entrancy latch, and a second loop would have to reimplement
all four. The tick runs every 2s; the ops fetch gates itself on a timestamp:

```js
// inside settingsTick's `if(tab==="cluster")` branch, alongside the existing
// /cluster/nodes maintenance poll (which stays exactly as it is):
if(!CLUSTER_OPS_FETCHED_AT || Date.now()-CLUSTER_OPS_FETCHED_AT>=15000){
  CLUSTER_OPS_FETCHED_AT=Date.now();          // set BEFORE the await: a slow
  const ops=await api("/cluster/status");     // probe must not stack requests
  if(!settingsCurrent(generation,tab)) return;
  ...
}
```

`CLUSTER_OPS_FETCHED_AT` is module state next to `CLUSTER_LOADED`; reset it in
`loadCluster()` and wherever `SETTINGS_LOADED.delete("clusterOps")` already
happens, so a manual refresh restarts the clock. 15s because `/cluster/status`
is a bounded direct fan-out that probes every voter — it is not a cheap read,
and the restart-prep flow that needs 2s freshness already has its own poll.
Skip the tick fetch entirely while `pollLocalRestart` is active (cheapest:
have the poll stamp `CLUSTER_OPS_FETCHED_AT` on each of its own fetches).

### 3.2 Re-render discipline — change-driven, like the roster poll

A fetch is not a repaint. Follow the pattern the maintenance poll already
established: compute a projection of what the panel actually renders from
`clusterOps`, compare to the previous one, and call `renderSettings()` only on
change. The projection must cover what the rail and the cards read — per node:
`observation`, `process.live`, `serving.ready`+`reason`, the raft sample/
watermark validity and `apply_lag_entries`, WAL health inputs, `media`
drain/admissions, `build`; plus `verdict.safe_to_restart_one`,
`verdict.candidate_node_id`, blocker codes, and the `maintenance[]` verdicts.
Leave `observed_at_unix_ms` and the age fields OUT of the projection —
they change on every fetch, and including them turns "on change" into
"always".

When nothing changed, patch exactly one thing: the database ledger's "Reading
age" `<dd>` (give it an id). It is computed from `Date.now()` at render time
via `clusterSampleAge`, so without this it visibly freezes between repaints —
the row whose only job is freshness must not itself go stale.

### 3.3 What a repaint must not destroy

`renderSettings()` rewrites the whole tab. The folds and the troubleshooting
tab already survive it (`applyClusterFolds` restores them from
`plurx.cluster.fold`), and `CLUSTER_TOKEN` is module state, so the token panel
survives too. Three things do NOT survive today, and a 15s cadence turns them
from a papercut into unusable:

1. **`#cluster-node-list` scrollTop** — the roster is a fixed-height scroller
   since #651. Capture before `renderSettings()`, restore after.
2. **Open `<details class="clnodedetail">`** (the "WAL, snapshot, and protocol
   details" fold inside each node card). Capture the set of open ones keyed by
   the card's `data-node` (it is on `.clnodebody`), restore after. Do NOT
   persist these to `localStorage` — they are drill-downs, not layout.
3. **Text selection** — cannot be preserved across innerHTML; this is exactly
   why repaints must be change-driven rather than periodic.

Wrap 1+2 into one helper (`repaintClusterPreserving(fn)` or similar) and use
it from the tick, `refreshClusterOperations`, and `pollLocalRestart`, so every
repaint path preserves the same things. The helper belongs in the extracted
module's DOM-glue counterpart, not the model (§4.3).

### 3.4 Acceptance

- With the panel open on a fixture server, `network` shows `/cluster/status`
  at ~15s intervals, none while the tab is hidden, none stacked when the probe
  is slow, and none at 2s cadence.
- Headless: open a node's WAL details, scroll the roster halfway, force a
  changed projection → repaint happens, scrollTop and the open details
  survive. Force an unchanged fetch → no repaint, but the Reading age `<dd>`
  text advanced.
- Tests to write (in the suite, not just probes): the projection ignores
  `observed_at_unix_ms`; the tick-side fetch respects the 15s gate; the
  preserve-helper is called by all three repaint paths (pin the call sites —
  helpers with free call sites is how #647's review found six dead features).

## 4. Workstream 2 — `cluster-panel.js`, on the playback-policy pattern

### 4.1 Why this shape and not a build step

The earlier idea for this workstream was a build-time concat of `web/src/*.js`
into the embedded blob. Superseded: the repo already solves "testable JS out
of the shell, single binary intact" with **separately served files** —
`playback-policy.js`, `playback-control.js`, `reader.js` — each its own
`include_str!`, its own route, its own `<script src>`, `require()`d by Node
tests with no extraction regex. No generator, no committed-output drift gate,
no double-diff PRs. Follow the house pattern.

### 4.2 What moves: the model, not the templates

`cluster-panel.js` gets the **pure** layer — functions that read data and
return data (or HTML strings with no `document` access), with zero references
to `SETTINGS_DATA`, `ME`, `CLUSTER_*` module state, or the DOM:

- Preconditions and rail model: `clusterOperationRows` (see 4.4),
  `clusterRailBlocker`, `clusterRailRow`, `clusterQuorum`,
  `clusterRecoveryState`, `clusterMaintenanceEntryVerdict`,
  `clusterMaintenanceReady`, `clusterMaintenanceResumeReady`,
  `clusterDirectOperationRow`, `clusterDirectMaintenanceStatus`.
- Database model: `clusterDatabaseStatus`, `clusterDatabaseRows`,
  `clusterDatabaseSummary`, `clusterDatabaseWal`, `clusterDatabaseHealth`,
  `clusterCapacityText`, `clusterSampleAge`, `clusterSqliteReplication`.
- Words: `clusterStateView`, `membershipRefusalText`,
  `clusterOperationReason`, `clusterOperationAge`.
- Fold state (pure half): `clusterFoldKey`, plus `clusterFoldRead/Write`
  refactored to take a storage object so Node tests stop stubbing
  `global.localStorage` (browser call sites pass `localStorage`).

**Stays inline:** everything that touches the DOM or module state —
`clusterPanel`, `clusterNodeRow` and the card templates, `applyClusterFolds`,
`toggleClusterNodes`, `selectClusterTab`, the async actions (`promoteNode`,
`removeNode`, `setNodeMaintenance`, `mintJoinToken`, …), `renderSettings`
wiring. Do not chase a perfect split in one pass; the boundary is "no DOM, no
module state", applied honestly.

### 4.3 Wiring, exactly like playback-policy

```js
(function (root, factory) {
  const panel = factory();
  if (typeof module === "object" && module.exports) module.exports = panel;
  root.PlurxClusterPanel = panel;
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";
  // esc/fmtAgo/fmtBytes live in the shell; the factory takes no globals —
  // functions that need them receive them as arguments (see 4.4) so the
  // module stays loadable under Node with zero setup.
  ...
  return Object.freeze({ clusterOperationRows, clusterDatabaseRows, ... });
});
```

- `crates/plurxd/src/http/web.rs`: `const CLUSTER_PANEL_JS: &str =
  include_str!("../web/cluster-panel.js");` + a handler mirroring
  `playback_policy_js` (same cache-header treatment — copy it, don't improve
  it).
- `crates/plurxd/src/http/mod.rs`: route `/assets/cluster-panel.js`, and add
  it to the test at the bottom of that file that pins the asset route list
  (grep `/assets/playback-policy.js` there).
- `index.html`: `<script src="/assets/cluster-panel.js"></script>` beside the
  existing three, BEFORE the inline block; inline call sites become
  `PlurxClusterPanel.clusterOperationRows(...)` — or destructure once at the
  top of the inline script.
- `validation/points.toml`: add the file beside `playback-policy.js` in the
  audited-files list; `ci_scope` already globs `crates/plurxd/src/web/**`.
- Rust touched = one const, one handler, one route, one test row. Anything
  more is scope creep — stop and flag.

### 4.4 The helper problem, solved by parameter, not by global

The model functions call `esc`, `fmtAgo`, `fmtBytes` from the shell. Do not
copy them into the module (two `esc` implementations is an XSS review burden)
and do not have the module reach for `root.esc` (hidden coupling, untestable
under Node without setup). Pass them: the moved functions gain a leading
`env` parameter — `clusterDatabaseRows(env, cluster, replication, ops)` where
`env = {esc, fmtAgo, fmtBytes}` — and the inline shell builds one `CLENV`
object once. Tests construct their own tiny env (the suite already has these
stubs). Mechanical, greppable, and the module's dependency surface is written
in its signatures.

### 4.5 Test migration — delete the regex where the boundary makes it obsolete

For every function that moved, `cluster-membership.test.js` switches from
`shippedSource("name")` to `const panel =
require("../../crates/plurxd/src/web/cluster-panel.js")`. The sandbox eval
remains only for what stays inline (templates, fold DOM glue) — the regex
harness shrinks instead of growing. Keep the call-site assertions that pin
where the shell invokes the module (`PlurxClusterPanel.clusterOperationRows`
must appear in `clusterPanel`'s source — a moved-and-never-called module is
#647's dead-feature failure mode again). Also keep the existing behavioural
tests passing UNCHANGED in what they assert: this workstream must be a pure
refactor, and the 89 green tests before/after are the proof. Do it as its own
commit with no behaviour change mixed in.

### 4.6 Acceptance

```bash
node -e 'const p=require("./crates/plurxd/src/web/cluster-panel.js"); console.log(Object.keys(p).length)'
node tests/web/cluster-membership.test.js       # same count, 0 fail, no global stubs for the moved fns
scripts/js-check                                 # inline blocks still parse
grep -c 'shippedSource(' tests/web/cluster-membership.test.js   # strictly lower than before
```

Plus: the served page loads with the script tag (headless probe: no
`ReferenceError`, panel renders identically pixel-for-pixel at 1440×900
against a before-screenshot).

## 5. Workstream 3 — the Add row IS the credential surface

### 5.1 The change

Dissolve `<details class="cldanger">`. The rail's **Add a node** row gains an
inline expansion (collapsed by default) containing exactly what
`joinPanel(maintenanceActive)` renders today — TTL select, role select, mint
button, and the one-time token display. The **Leave this cluster** row gains
the same treatment wrapping today's `leavePanel(...)` content. The rail is
then the only membership surface, and `openClusterDanger` is deleted (its two
call sites become expand-toggles on the rows themselves).

### 5.2 Invariants that must survive — each is load-bearing

1. **The token renders exactly once, in exactly one place.** `CLUSTER_TOKEN`
   stays module state; `joinTokenHtml` is called from one mount. Grep for a
   second `mintJoinToken`/`joinTokenHtml` call site after the change — the
   #651 review specifically checked "two surfaces that mint one".
2. **The token's drop points are untouched.** `forgetJoinToken()` on tab
   switch, route change, logout — all three stay. Expanding/collapsing the
   Add row must NOT drop it (an accidental collapse eating a shown-once
   credential is worse than the old layout).
3. **The expansion state is never persisted.** It does not go into
   `plurx.cluster.fold` — a credential surface must not auto-reopen on the
   next visit. Fresh page = collapsed rows, always. Write the test.
4. **Lifecycle gating rides along:** `joinPanel(maintenanceActive)` and
   `leavePanel(localNodeId, maintenanceActive)` keep their arguments; when the
   Add row is Blocked (lifecycle in flight, recovery), the expansion is not
   reachable — the blocked reason is the row's whole content.
5. **Leave keeps its friction and its binding.** The graceful-leave text, the
   `CLUSTER_LEAVING` state, and the roster-bound `node_id` (the
   `leave_node_mismatch` protection) move as-is. The row is already styled
   `destructive`; the expansion does not soften the wording.
6. **A destructive control never sits where a repaint can move it under a
   click** — with §3's change-driven repaints this holds, but add the row
   expansion to §3.3's preserved-state list (an operator mid-mint must not
   have the form yanked shut by a repaint).

### 5.3 Acceptance

- Rail Add → expand → mint → token shows once with its warning text; switch
  tab and return → token gone, row collapsed. Reload → row collapsed.
- `plurx.cluster.fold` in localStorage never contains a key for the
  expansions (test).
- The `cldanger` CSS class, `openClusterDanger`, and the details element are
  gone from `index.html` (grep returns only CHANGELOG/docs mentions).
- `docs/OPERATIONS.md`'s Danger-zone paragraph rewritten in the same commit.

## 6. Order, milestones, gates

Build in this order — §4 first, because §3 and §5 then land in a codebase
where the model has a boundary, and their tests are `require()` tests instead
of more regex:

| M | Deliverable | Acceptance |
|---|---|---|
| M1 | `cluster-panel.js` extraction, pure refactor, no behaviour change | §4.6 commands; test count unchanged; screenshot diff clean |
| M2 | Live status: 15s gate, change projection, preserve helper, reading-age patch | §3.4; new tests pin the three call sites |
| M3 | Danger zone → rail expansions | §5.3; token invariant tests |
| M4 | Docs (`OPERATIONS.md`, CHANGELOG) + `regressions.d` ledger row | `make history-check` green (see §8) |
| M5 | ONE adversarial review of M1–M4 together, findings fixed, mutations proven | reviewer's mutation list all caught |

Per-commit gates, every commit:

```bash
scripts/js-check
node tests/web/cluster-membership.test.js   # 0 FAIL
node tests/web/page-read-budget.test.js     # 0 FAIL
make web-check                              # includes contrast-check
```

Do NOT run `make check` / `make test-full` during development — the standing
lane rule is fast lane while building, full suite only at the gate, and this
change has no Rust behaviour to cover anyway. Hosted CI runs the Rust gate on
the PR.

## 7. Non-goals — guardrails, each with its reason

- **No server changes** beyond §4.3's serving plumbing. The staleness fix is
  a client cadence, not a push channel; if you find yourself wanting SSE or a
  websocket, stop — that is a different plan with a different blast radius.
- **No shortening of the 15s cadence** without measuring the fan-out cost on
  a real 3-voter cluster first. The restart flow already has its 2s poll.
- **Do not extract the rendering templates** (`clusterNodeRow`,
  `clusterPanel`) in this pass. The module boundary is "no DOM, no module
  state"; templates entangled with fold state and event handlers go later or
  never.
- **Do not "improve" the rail's preconditions** while moving them. Every one
  was verified against the server in #651's review (removal is `voters >= 3`,
  not `!== 2`; the maintenance verdict's `blockers[0]` is reported verbatim;
  restart preparation is candidate-bound). Behaviour changes there need their
  own review trail.
- **Do not persist anything new to `localStorage`** beyond what
  `plurx.cluster.fold` already holds. In particular nothing credential-
  adjacent (§5.2.3), and the fold's whitelist reader stays a whitelist.
- **Do not touch the two half-baked bands**: 901–1099px still renders two
  uncapped columns, and the panel deliberately does not stream. Known,
  accepted, out of scope.

## 8. Traps this session already paid for — read before your first commit

- **Work in your own clone, never in `~/code/plurx`.** Token:
  `~/mnt/plurx-agent/.gh-token` (device VM). Clone from GitHub into the VM's
  `$HOME`, keep the token out of `.git/config`, push with it in the URL. The
  mount is read-only reconnaissance; a graft into Paul's repo cannot be fully
  undone (the mount forbids unlink — the last attempt left lock litter he had
  to clean).
- **Open the PR; do not merge it** unless Paul says to in his own words, in
  this task.
- **`history-check` will block the PR**: any commit whose subject matches
  `ISSUE_RE` (fix/correct/regression/stop/…) and touches `crates/` needs a
  `validation/regressions.d/<sha8>-web-experience.toml` row naming points
  `["web.experience"]`, checks `["web-membership","web-static"]`, and a reason
  pointing at the retained tests. The ledger commit's own subject must NOT
  match `ISSUE_RE` ("chore(validation): add …", not "fix …"). Device VM
  python is 3.10: `pip3 install --user tomli` + a one-line `tomllib.py` shim
  on `PYTHONPATH` to run it locally.
- **The regex harness eats free `const`s** — until M1 lands, any new
  top-level state next to the cluster functions must be a function or it
  silently misses the sandbox.
- **Chrome fires one `toggle` per `<details open>` during parse** — never
  persist anything from `ontoggle`; the fold saves hang off `onclick` +
  `setTimeout(...,0)` for that reason.
- **Grid rows inside a definite height compress to fit** and `.clnode` hides
  overflow — the capped roster must stay `display:flex` with `flex:0 0 auto`
  cards. There is a test on the exact declaration; leave it green.
- **Headless verification kit**: chromium at `/opt/pw-browsers/chromium`,
  playwright-core under `/home/claude/node_modules` (container). The probe
  pattern (extract functions, render `clusterPanel` over fixtures, measure
  overflow/clip/height across widths and node counts) is in #651's
  description; rebuild it — after M1 it gets simpler, because the model half
  is a plain `require()`.
- **Screenshot before you start.** M1's acceptance is "pixel-identical"; you
  need the before image from the same fixture to prove it.

## 9. What done looks like

An operator opens Settings → Cluster and leaves it open. The readings and the
rail track the cluster within 15 seconds without the page ever moving under
them or a repaint eating their scroll position, their open WAL details, or a
half-minted token. The rail is the only place membership changes start, the
token still exists exactly once for exactly as long as the panel shows it,
and the next person who has to test a precondition writes
`require("cluster-panel.js")` instead of a regex. The 89-test suite is larger,
not smaller, and every new helper has its call site pinned.
