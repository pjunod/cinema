# Architecture review 2026-09-20 — appendix: the nine area reports

Companion to ARCHITECTURE-REVIEW-2026-09-20.md (the consolidated, ranked verdict, now at revision 2). Each section below is one reviewer's full report against `main` @ a1414368, unedited except for heading depth. Finding ids (F-<area>-<n>) are the ones the main document and ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md cite. **This appendix is raw material and was not revised**: several remedies in it were withdrawn or corrected by the assessment (B-frames via negative CTS, the auth cache, heartbeat-derived skew, the `NOT EXISTS` search predicate, the `-hls_start_time_offset` option, the frame-rate parser claim, among others). Build from the main document's §0 and the assessment's per-finding verdicts, not from these reports alone.


---

## Change-history review — what 3,860 commits did to plurx (2026-08-20 → 2026-09-20)

Reviewer area: **change-history**. This report reads the month, not the code, except where a
commit body made a claim that had to be checked against the tree. Everything below is derived
from `git log` on `main` = `a1414368`, CHANGELOG.md, STATUS.md, docs/README.md and the two
status docs named in the brief. Where a finding needed a code path it is quoted with file:line.

Headline numbers (window 2026-08-20 00:00 → 2026-09-20, `main`):

| Measure | Start of window (`42f2c8d5`, 08-19) | Now (`a1414368`) | Δ |
|---|---|---|---|
| Non-merge commits in window | — | 3,860 | 1,199 merges; 627 PR merges into `main` |
| Rust lines under `crates/` | 126,265 | 539,804 | ×4.3 |
| …of which test code (after `#[cfg(test)]`, `tests/`, `*_tests.rs`) | 71,751 (56%) | 378,071 (70%) | ×5.3 |
| …product code | 54,514 | 161,733 | ×3.0 |
| `.rs` files | 112 | 237 | |
| `#[test]` + `#[tokio::test]` | 1,180 (664 + 516) | 3,949 (2,232 + 1,717) | +2,769 |
| `#[ignore]` tests | 5 | 12 | all 12 carry a reason string |
| Swift `func test…` / Kotlin `@Test` | 194 / 208 | 658 / 765 | |
| `tests/web/*.test.js` files / `test(` cases | 0 / 0 | 25 / 300 | |
| `validation/` files | 11 | 869 (853 in `regressions.d/`) | ×79 |
| `tests/` files | 22 | 127 | |
| `docs/` files | 115 | 460 | ×4 |
| `vendor/hiqlite*` commits in window | — | 68 (+15,456 / −1,302 lines) | |
| Server version tags | v0.2.8 (08-29), v0.3.0 (08-31) | none since | 2,213 commits on untagged `main` |
| Apple `CURRENT_PROJECT_VERSION` | 82 | 173 | 92 bump commits |
| Android `versionCode` | 44 | 111 | |

---

### 1. TIMELINE — week by week

Commit volume: W1 1,089 · W2 936 · W3 1,067 · W4 645 · W5 (4 days) 168. PR merges into `main`
peaked at **64 on 09-02** and 46 on 09-01. PR numbering restarts at #1 on 09-04/05 when the
repository moved from GitHub (#344–#925) to Forgejo (#1–#387).

#### W1 08-20 → 08-26 (1,089 commits, ~120 PRs; GitHub #344–#614)
- **Cluster performance & hardening** (`codex/cluster-*`): page-latency measurement/fix/UI
  (#510, #524, #528, #529), store/raft/passive metrics (#516–#521), job leases (#497), scheduler
  fences (#505), bounded catalogue reader/handlers/read-permit (#534–#536), write coalescing
  (#537), shared cache roots (#538), leader self-leave (#527), hiqlite WAL recovery / metadata
  atomicity / raft socket leak (#483, #486, #487), sparse roster/join (#475, #479),
  short/reverse hostnames (#494, #498, #500).
- **VOD index / fragment index** (`agent/vod-m0..m4`, #540, #541, #555, #556, #563, #566,
  #573, #594, #597, #603): clustered fragment-index catalogue shared across voters; four
  `fix(vod)` commits on 08-26 alone hardening queue eligibility.
- **Playback-control rewrite M1–M3** (#600 protocol, #602, #605, #611, #614, #615–#617).
- **Provenance-repair series** from the previous review cycle (`agent/builder/issue-*`, #447,
  #474, #476, #481, #488) and **ebook reader** M1/M2a (#427, #429, #432).
- **CI**: self-hosted macOS runner (#564), runner mode (#586, #587, #598), risk-routed CI
  (#595), fast unit tests (#592), main-CI regressions/badges (#601, #604, #610).

#### W2 08-27 → 09-02 (936 commits, ~200 PRs; #615–#859)
- **Playback-control M4** (watchdog removal #618, actor deadline #619, deadline cutoff #621,
  command sequencing #624, producer decision #626, prepublication/published/copy lifetime
  #636, #641, #642), **M5** (control hold/actions/telemetry/verdicts #690–#703, m5a–m5g
  client asks #706–#725), **M5.5** staged generations (#714, #726, #776, #781), **M7**
  burn-join / M4 remainder (#740, #760–#766, #775, #789, #830, #836).
- **Playback caps v2 + Dolby Vision P7→8.1 conversion** (`playback-caps-v2-m1..m6`, #659,
  #664, #669, #670, #675–#678, #683, #688, #694, #698, #710, #716; DV badges #770, #772).
- **M6 prepared quality handoff** begins: grade negotiation (#664) then **40 `agent/m6-*` PRs
  merged on 09-02 alone** (#793–#850).
- **Player input contract** (`agent/pic-*`, #795–#832, `effort/player-input-contract` #814).
- Releases **v0.2.8 (08-29, #661)** and **v0.3.0 (08-31, #736)**; mobile builds #645.
- Settings reorg (#790), content-analysis index (#700), CI execution acceleration / store
  sharding / native ARM shadow (#761, #767, #769, #773, #774, #833).

#### W3 09-03 → 09-09 (1,067 commits, ~130 PRs; GitHub #860–#925 then Forgejo #1–#221)
- **Fragment-index queue repair** (#856, #863, #866, #873, #881) after the lease-renew outage
  (F-hist-1); **DV P7 web delivery** (#854–#870, #869).
- **M8 owner-loss** (#880, #885, #887), preparation deadlines (#883, #890, #893).
- **Forgejo migration** (#2, #3, #8, #9, #18, #44, #46) — local CI authority, registry, image
  publication.
- **HDHomeRun Live TV** promoted (#52, 09-06) → activation guard "could never be satisfied"
  (#68, 09-07) → **Live TV guide/DVR foundation** (#154, #160–#172 handoffs, 09-08).
- **Streaming reliability** merged (#53, 09-06; #178 09-09).
- **Transport recovery** promoted (#71, 09-07) → campaign lane red on `main` and every PR →
  contract changed (#159, 09-08); raft media starvation (#918/#919); docs reorganised into
  subject folders (#102), API reference written (#103).
- **M6 clients** on `main` (#118–#148, 09-08), enabled by default (#193), server prime (#203).
- **Decoder selection & recovery** promoted (#189, 09-09) followed the same day by eight
  post-merge repair PRs (#196, #198, #199, #200, #205, #208–#212).

#### W4 09-10 → 09-16 (645 commits, ~90 PRs; #222–#343)
- **Library channels** (#231, #234, #238, #240, #251–#254) + resumable subjects (#313); Ripwire
  (#248); local library search later (#350).
- **Live TV native layouts / proportions** (#229, #265–#271), **DVR recording + reminders**
  (#294, #307, #314–#316), **Live TV reliability / start-stall** (#273, #281, #288, #301).
- **Playback lifecycle + rewrite remainder** (#255, #259, #263), playback surface M0–M5
  (#274–#291).
- **Windows port** (#309, #312) — the source of F-hist-3.
- **UI rewrite then rollback**: usability #317 (09-14) → calm library #330 (09-15) → restore
  home #331, native home #334, web item page #342 (09-16) → tvOS cinematic detail #367
  (09-19).
- Public-mirror sanitisation (#332, #333), schema-40 startup hotfix (#338), DVR attention
  placeholder order (#340), seek-control (#336), Free Fall freeze (#343).

#### W5 09-17 → 09-20 (168 commits, 56 merges; #344–#387)
- **Red-suite repair #356**: 20 failing tests on `main`, three of them live production
  defects (F-hist-2, F-hist-3). Held-source comparison #354, held-seek #361, Apple resume
  #360, frame-callback #358.
- **Quality-switch continuity** #348/#353 — "the prepared quality handoff, reachable at last".
- Content-analysis repair #352, Android memory #355, Apache-2.0 #357, one-command install
  #349, web shell split #369/#371, subtitle reliability #372/#376, MKV duration + sliding HLS
  #377, Android DV #381, watch-and-browse #386, scan identity #385, fleet builds 169/107 #365,
  nzbd enablement added (#373) and reverted next day (#374).

#### The ~12 efforts, sized

| # | Effort | Window | PRs into `main` (approx.) | Outcome as of 09-20 |
|---|---|---|---|---|
| 1 | Cluster performance, hardening, transport recovery, raft starvation | 08-20 → 09-17 | ~79 | merged; campaign lane never passed under its first contract (§7) |
| 2 | VOD / fragment index / content analysis | 08-23 → 09-17 | ~26 | merged; two silent outages (F-hist-1, F-hist-2) |
| 3 | Playback-control rewrite M1–M9 | 08-26 → 09-12 | ~97 | merged; M9 "final deletion and acceptance incomplete" (PLAYBACK-CONTROL-STATUS) |
| 4 | Playback caps v2 + DV P7 conversion | 08-29 → 09-20 | ~44 | merged; 12-commit fix chain (§2) |
| 5 | M6 prepared quality handoff | 08-29 → 09-17 | 67 | merged, enabled by default 09-09, found unreachable 09-16 (F-hist-4) |
| 6 | Player input contract + playback surface | 09-02 → 09-13 | ~30 | merged |
| 7 | Live TV: HDHomeRun → guide → DVR → layouts → reliability → ATSC audio | 09-04 → 09-19 | ~34 (224 commits) | merged; two "impossible on any cluster" defects reached the fleet (F-hist-5) |
| 8 | Decoder selection & recovery | 09-05 → 09-09 | ~10 | merged + 8 same-day post-merge repairs |
| 9 | Streaming reliability, MKV/sliding HLS | 09-04 → 09-20 | ~12 | merged |
| 10 | CI: runner fleet, Forgejo, fast-lane, Windows CI | 08-25 → 09-16 | ~52 | migrated; red lanes recurring (§7) |
| 11 | UI: settings reorg, usability, playback-info, web-shell split | 09-02 → 09-19 | ~25 | usability half reverted (F-hist-11) |
| 12 | New surfaces: ebook, library channels, Ripwire, search, Windows, DVR, watch-and-browse, scan identity, subtitles | 08-20 → 09-20 | ~25 | merged, most "physical acceptance pending" |
| — | Docs / status / validation-ledger PRs | throughout | ~65 PRs + 631 `chore(validation)` commits | see F-hist-8 |

---

### 2. FIX-OF-FIX CHAINS

Keyword counts on subjects in the window: `still` 438 · `regression` 213 · `actually` 204 ·
`again` 127 · `restore` 63 · `revert` 56 · `red test/lane/main` 44 · `post-merge` 6.
Chains of ≥3 commits on one subject, oldest first:

**C1 — hiqlite `$N` placeholder order (6 commits, 08-24 → 09-17).** hiqlite binds `$N` by
first appearance; rusqlite `?N` by number. `0ae2ffab` (08-24) "bind the replicated
media-session statements to their values" → `4da3bbde` (08-28) "preserve Hiqlite placeholder
order" → `120a3d29` (09-03) six statements incl. fragment-index renew/yield and four
`dv_conversions` writes (F-hist-1) → `e314a5bd` (09-14) DVR schedule page bound one value
against four placeholders → `896f6424` (09-16) `/api/v1/dvr/attention` 500 on every replicated
store → `cfba5ff6` (09-17) the *SQLite* twin `requeue_cluster_fragment_index` binds 12 against
`?13` (F-hist-2). The repo-wide census added on 09-03 is hiqlite-only; `cfba5ff6` says so:
"the placeholder-order validator is hiqlite-only … would be cheap to apply to the `?N` side;
that is worth doing and is not done here."

**C2 — hiqlite schema-admission list (4 startup hotfixes, 09-06 → 09-16).** `e26161a5`
(09-06) "an import plan named a version from the wrong number line" → `f34aecf5` (09-10)
"admit channel migration sources" → #304 (09-14) "admit the DVR schema predecessor" → #338
`8df09290` (09-16) "admit schema 39 to the v40 migration". Each is a hand-maintained match arm
in `crates/plurx-core/src/store/hiqlite.rs` that a new migration forgot to extend; each was a
daemon that would not start on the replicated store after deploy. The 09-16 fix finally adds
the loop `for schema_version in AUTH_SCHEMA_MIGRATION_SOURCE..AUTH_SCHEMA_VERSION { … "schema
v{schema_version} must be admitted to the migration chain" }` (F-hist-6).

**C3 — Dolby Vision conversion/delivery (≥12 commits, 08-30 → 09-20).** `7ebeee4a` P7→8.1 in
the copy pipe (08-30) → `d60ff663` route P7 (08-31) → `bcdb8f06` "paths that served a
conversion they could not make" → `c6892bf5` (09-02) derive conversion when no caps document
→ `1109dde6`, `4c1679dc` (09-03) restore/pin the tests → `ceec7f8c` "a blanket DV claim is not
a claim about dual-layer" → `31b0fdce` (09-08) switch out of environment → `ae392853`,
`3cdafd4c` (09-09) "repair Dolby Vision and tvOS Live TV", "sustain DV copy conversion" → #221
Safari DV → #222 (09-10) "keep DV P7→P8 playback at 4K" → `2a2ec42f` (09-15) exact shared DV
index identity → `a20c04b1`, `4f71e58c` (09-19) bind conversion to HLS / review findings →
#381 (09-20) "Repair Android Dolby Vision delivery". Plus F-hist-3's still-open Profile 5
refusal.

**C4 — Live TV start (≥15 commits, 09-04 → 09-19).** `3945a713`, `5ca7c9ad`, `b01b0d5c`
(09-04) → `cfbcd1f1` (09-05) "a Live TV join guard made every join unpreparable" (caught
before promotion) → `57e03a6b` #68 (09-06/07) "the activation guard could never be satisfied"
→ `90319fcf` #192 (09-09) AC-4 + speed starts → `7d0acd98` tvOS restart marker → #216/#221
(09-09) tvOS Live TV starts → `e44b5426`, `93beae93` #288 (09-13) "the owner refused fifteen
start ids in sixteen" → `7e400202`, `ce58fa10` review findings → #301 start-stall → #306
reveal focus trap → `554e1325` (09-16) VideoToolbox A53 captions → `bd31b6e0` (09-19) AAC
channel bound / AC-3 fMP4 → #370 (09-19) ATSC 3.0 audio startup on macOS.

**C5 — web/native Home and item page (6 commits, 09-14 → 09-18).** `41f6960c` #317
usability → `a767f5fe` #330 "Recompose Home and item pages" → `f41d73cd` #331 "Restore the
previous Classic, Catalog and Theater home pages" → `cb878f70` #334 "Restore original Home
screens on Apple and Android" → `4b109ebd` #342 "Restore the original web item page from
before PR #317" ("Paul compared real renders … and chose the page from before PR #317") →
`ab8541b5` #367 "Restore cinematic Apple TV item details". Two rewrites, four restores, five
days, an Apple build per step.

**C6 — fragment-index / content-analysis queue (≥10 commits).** `6dee9505`, `2561595e`,
`cbd4f7fa`, `f5238856` (four `fix(vod)` on 08-26) → `ce253a55` (08-31) "harden queue
lifecycle" — the commit that broke renew/yield → `120a3d29`, `11f13868`, `9a6530aa`,
`e80ba1e2` (09-03) → `b6890aa4` "the merge's six loose ends, two of them red" → `cfba5ff6`
(09-17) requeue + force-supersede → #352 content-analysis-repair.

**C7 — red tests on `main` (recurring).** `89ff94ec` #74 (09-07) "main's three red unit tests,
none of them a product bug" → #196, #198, #199, #200, #205, #210, #211, #212 (09-09, eight
post-merge repairs after #189) → `e314a5bd` (09-14) "the three replicated-store defects my
qualification never ran" → STATUS 09-13 "Two `live_tv` tests are deterministically red on
`main` … Reported, not changed, and still red" → #356 (09-17) "the twenty red tests, and the
three live defects behind them".

**C8 — CI badges (5 commits).** #601 `fix-ci-coverage-badges` (08-26) → `4bcfcdca` and
`6219c239` "restore private repository badges" (08-26, twice) → #644 `fix-ci-coverage-badges`
again (08-29) → #339 "restore portable status badges", #341 "publish truthful portable badges"
(09-16).

**C9 — HLS startup on web (5 PRs).** #840 `hls-startup-demand-deadlock`, #845
`status-startup-deadlock` (09-02) → #311 "Recover delayed web HLS startup" (09-14) → #335
"Fix Chrome playback startup with text subtitles" (09-16) → #343 "Repair Free Fall web
playback freeze recovery" (09-16) → `6fa0f9e2` (09-19) "restore player loading and subtitle
test baseline".

**C10 — held source / held seek (5 in three days).** #336 seek-control (09-16) → `23239199`
#354 "compare a held source on its media facts, not its reporter's schema" → `670fbc97`
"compare every projected source fact strictly" → `f35ddc67` #351 "admit the legacy E-AC-3
Atmos profile omission" (09-17) → #361 "Fix held-seek presentation stalls" (09-18). Root cause
per STATUS: "Every manual quality change in the web player was refused".

**C11 — M6 prepared handoff (67 PRs, 08-29 → 09-17).** See F-hist-4.

**C12 — cluster shutdown grace / transport recovery.** #627 and #630 (`codex/fix-cluster-
shutdown-grace`, same branch name, same day 08-27) → #900 raft snapshot retry (09-04) → #1
"recover cleanly after snapshot transport cancellation" (09-05) → **19 `fix(cluster)` commits
on 09-06 in `vendor/hiqlite`** → #71 promotion (09-07) → #147, #150 runner fixes → #159
contract change (09-08) → #918/#919 raft media starvation (09-08) → #220 "retry requests
proven undispatched" (09-09) → `ed9fbe5d` "anchor restored snapshots to Raft log" (09-10).

---

### 3. CHURN vs FIX DENSITY — top 40 source files (≥15 commits in window)

`fix` = subject starts with fix/revert or contains fix/repair/restore/revert. Size = lines
now. Files flagged `!` have fixes > 50% of commits.

| File | Commits | Fix | Fix% | +/− lines | Size now | Size 08-19 |
|---|---|---|---|---|---|---|
| crates/plurxd/src/web/index.html | 299 | 142 | 47% | +32,115/−30,326 | 99 (split 09-19) | 10,808 |
| ! crates/plurxd/src/transcode.rs | 294 | 155 | 52% | +62,933/−9,814 | 47,096 (27,573 product) | 14,366 |
| ! crates/plurxd/src/http/hls.rs | 204 | 109 | 53% | +35,518/−5,498 | 28,507 (14,193 product) | 3,129 |
| ! crates/plurx-core/tests/store_contract.rs | 199 | 101 | 50% | +39,170/−1,420 | 29,435 | 3,781 |
| crates/plurxd/src/playback_control.rs | 188 | 84 | 44% | +33,962/−3,986 | 29,986 (14,533 product) | (new) |
| crates/plurxd/src/http/mod.rs | 150 | 52 | 34% | +17,416/−809 | 15,028 | |
| crates/plurx-core/src/store/mod.rs | 124 | 47 | 37% | +6,837/−282 | 5,309 | |
| ! crates/plurx-cluster-check/src/lib.rs | 124 | 63 | 50% | +20,531/−1,269 | 13,416 | |
| crates/plurxd/src/http/system.rs | 115 | 39 | 33% | +8,142/−1,038 | 6,501 | |
| ! crates/plurxd/src/vodserve.rs | 110 | 67 | 60% | +20,910/−2,492 | 15,011 (8,091 product) | (new) |
| crates/plurxd/src/state.rs | 108 | 43 | 39% | +17,081/−2,139 | 12,752 | |
| ! crates/plurx-core/src/cluster/membership.rs | 106 | 80 | **75%** | +23,612/−2,317 | 17,169 (10,708 product) | 1,345 |
| crates/plurx-core/src/store/hiqlite.rs | 96 | 40 | 41% | +9,555/−876 | 6,505 | |
| crates/plurxd/src/main.rs | 95 | 26 | 27% | +7,356/−658 | 6,622 | |
| crates/plurx-core/src/store/sqlite/mod.rs | 84 | 26 | 30% | +5,470/−227 | 4,228 | |
| clients/apple/Tests/AppleClientTests.swift | 83 | 34 | 40% | +12,906/−578 | 11,528 | |
| clients/apple/Sources/PlayerController.swift | 72 | 36 | 50% | +10,957/−1,157 | 9,548 | |
| clients/android/…/player/Controller.kt | 66 | 29 | 43% | +6,845/−1,939 | 4,293 | |
| ! crates/plurxd/src/media_sessions.rs | 65 | 34 | 52% | +11,869/−1,317 | 8,264 | |
| ! crates/plurx-core/src/store/hiqlite_sessions.rs | 65 | 42 | **64%** | +7,646/−1,149 | 4,977 | |
| ! crates/plurx-core/src/cluster/migration.rs | 63 | 34 | 53% | +11,621/−553 | 8,905 | |
| ! crates/plurxd/src/live_tv.rs | 55 | 28 | 50% | +13,574/−1,419 | 11,778 (7,505 product) | |
| ! crates/plurxd/src/http/stream.rs | 52 | 27 | 51% | +6,981/−662 | 5,868 | |
| ! crates/plurx-core/src/store/sqlite/sessions.rs | 50 | 33 | **66%** | +5,747/−674 | 3,915 | |
| crates/plurx-core/src/domain.rs | 45 | 22 | 48% | +2,816/−34 | 2,216 | |
| ! crates/plurxd/src/http/internal_media_sessions.rs | 44 | 24 | 54% | +2,140/−272 | 1,635 | |
| ! crates/plurx-core/src/fmp4.rs | 42 | 22 | 52% | +10,387/−589 | 8,512 | |
| ! crates/plurxd/src/fragindex.rs | 41 | 21 | 51% | +4,650/−777 | 3,184 | |
| crates/plurx-core/src/store/hiqlite_import.rs | 41 | 14 | 34% | +4,040/−68 | 3,728 | |
| ! crates/plurxd/src/http/cluster.rs | 41 | 31 | **75%** | +1,195/−301 | 719 | |
| clients/android/…/player/PlayerScreen.kt | 40 | 17 | 42% | +5,627/−1,947 | 2,897 | |
| crates/plurx-core/src/transcode/mod.rs | 40 | 12 | 30% | +5,856/−842 | 4,735 | |
| ! crates/plurxd/src/http/cluster_operations.rs | 39 | 32 | **82%** | +6,042/−1,064 | 4,978 | |
| ! crates/plurxd/src/decode_facts.rs | 36 | 19 | 52% | +6,646/−1,080 | 5,566 | |
| ! crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs | 35 | 27 | **77%** | +5,310/−802 | 4,508 | |
| crates/plurx-core/src/store/replicated.rs | 35 | 9 | 25% | +1,428/−42 | 1,234 | |
| ! crates/plurxd/src/web/playback-policy.js | 34 | 22 | **64%** | +2,138/−141 | 1,997 | |
| clients/apple/Tests/LiveTvTests.swift | 34 | 12 | 35% | +2,594/−205 | 2,408 | |
| ! crates/plurxd/src/ffmpeg.rs | 32 | 20 | **62%** | +4,937/−387 | 4,502 | |
| ! crates/plurx-core/src/store/sqlite/fragment_index_cluster.rs | 31 | 22 | **70%** | +6,102/−686 | 5,416 | |

Also over 50%: `plurx-cluster-check/src/transport_recovery.rs` (29/15), `http/internal_activity.rs`
(28/17, 60%). 86 source files had ≥15 commits.

**Where the code reviewers should dig, ranked by fix density × size:** `cluster/membership.rs`
(75%, 10.7k product lines, 1.3k→17k in a month), `http/cluster_operations.rs` (82%),
`store/hiqlite_fragment_index_cluster.rs` + `store/sqlite/fragment_index_cluster.rs` (77%/70% —
the two backends drift independently, see C1), `store/hiqlite_sessions.rs` + `sqlite/sessions.rs`
(64%/66%, same twin problem), `vodserve.rs` (60%), `ffmpeg.rs` (62%), `transcode.rs` (52% of 294
commits; 27.5k lines of product code in one file).

---

### 4. TEST CHURN

- Rust test attributes 1,180 → 3,949 (+2,769). `git log -p` shows 5,165 `+#[test]` lines
  and only **79** `-#[test]` lines in the window (the excess over the net is branch/merge
  duplication) — tests are essentially never deleted or renamed.
- `#[ignore]` 5 → 12; every one carries a reason (`"writes ~600MB"`, `"run serially by the
  Rust gate; real FFmpeg restart fixture"`, `"requires macOS VideoToolbox"`, `"spawned by the
  backend-neutral contract factory"` …). Nothing is ignored to hide a failure. Good.
- Test code is now **70% of all Rust lines** (378k of 540k) and grew 5.3× while product code
  grew 3.0×. `store_contract.rs` alone is 29,435 lines (3,781 at start) and had 199 commits,
  101 of them fixes — the contract suite is itself a hot spot.
- Client tests: Swift 194 → 658, Kotlin 208 → 765, web 0 → 300 Node cases in 25 files.
- Per STATUS 09-19 the web-check Node lane "fail[s] on the same six tests, by name, as they
  do on `main`" — six web tests are known-red on `main` today.
- Feature-gated blind spot: `e314a5bd` (09-14): "`cargo test -p plurx-core --lib` does not
  enable [`hiqlite-store`] … therefore compiled out 282 tests, including every test of the
  code that decides whether a cluster can run the DVR at all. It was not a green run; it was a
  run that never asked the question."

---

### 5. VALIDATION GATE GROWTH — the cost-of-process number

`chore(validation)` = 631 commits (16.3% of all commits). Classified by the files they touch:

| Class | Count | What it is |
|---|---|---|
| Receipt only — one or two new 6–8-line TOML files in `validation/regressions.d/` | **502** | `[[coverage]] commits=["<sha>"] points=[…] checks=[…] reason="…"` mapping one earlier fix commit to a coverage point, or `ignore = true` for "non-runtime" |
| Receipt + tests/ci touched | 70 | ledger + a `tests/*.toml` or workflow tweak |
| Receipt + docs | 33 | ledger + STATUS/handoff prose |
| Validation framework code (`validation/*.py`) | 12 | real gate changes |
| Touches product code | 13 | mixed |

Sample of 30 (every 21st): 27 receipt/evidence rows, 2 gate repairs (`492f3528` M3 corrective
history, `c212547a`), 1 receipt rename batch (`ca5ca8bd` "re-map the M7 M4 evidence onto the
merged ancestry" — 8 files renamed because the SHAs changed on rebase). The ledger is keyed by
short SHA, so every rebase or squash invalidates rows and generates a re-mapping commit.

Self-referential receipts exist: `114bad7d` "mark the preceding ledger commit non-runtime";
`0022fc43-activity-ledger.toml` reads "The history-regressions check requires this
ledger-only correction to remain mapped to the validation framework". The gate
(`validation/history.py`, `ISSUE_RE`) classifies a commit as a fix by regex on its subject —
words like `keep`, `bound`, `remove`, `preserve`, `truth` all trigger it — so prose commits
need receipts too.

**Fraction of the month that touched no product code** (product = `crates/` or `clients/`
non-test files):

| | Commits | Share |
|---|---|---|
| Touch product code | 1,660 | 43% |
| Touch only `validation/`, `docs/`, `tests/` | 1,795 | **47%** |
| …only `docs/` (+ STATUS/CHANGELOG/README) | 577 | 15% |
| …only `validation/` | 757 | 20% |
| Touch no product code at all (incl. CI, test-only files) | 2,189 | **57%** |

Lines changed by class: product 924k · docs 224k · tests/ 115k · in-crate test files 110k ·
CI 52k · validation 18k.

---

### 6. DOC DRIFT — ARCHITECTURE.md and the index vs the code

`docs/ARCHITECTURE.md` was last edited 09-10 (`70979bc2`). The index `docs/README.md` says
"Accurate as of 2026-09-07".

| Claim | Where | Code / reality | Verdict |
|---|---|---|---|
| "**No DVR, and no scheduler.** Live TV (§3a) plays one tuner and keeps nothing. Recording would introduce a writer with a schedule, a retention policy, and a conflict resolver — a second product wearing this one's clothes" | ARCHITECTURE.md:533–537 (§8 non-goals) | `/api/v1/dvr/status`, `/dvr/schedule`, `/dvr/rules` routes (http/mod.rs:5654–5668 tests); `dvr_recordings`, `dvr_rules`, `dvr_reminders` in the replicated import set (store/hiqlite_import.rs:1349); `docs/features/LIVE-TV-DVR-STATUS.md`, `DVR-VISIBILITY-*`; #294 "feat(dvr): recording and reminders" merged 09-13 | **MISMATCH** — a non-goal shipped and §8 was not amended |
| "Single-node mode is the same code path with a 1-voter raft" and diagram "Hiqlite: 1 voter (M2) · 3+ future (M3)" | ARCHITECTURE.md:40, 88–90, 460–461 | OPERATIONS.md:556 documents a distinct `SQLite single-node` surface: "This boot is using unreplicated SQLite"; `main.rs:3315` opens `SqliteStore::open(&dir.join("plurx.db"))`; STATUS/PLAYBACK-CONTROL-STATUS describe a 3-voter + learner fleet; CLUSTERING-PLAN "M0 through M3 are complete" | **MISMATCH** — unreplicated SQLite single-node exists, and 3-voter is present tense |
| "watchdog: if the first HLS segment hasn't landed within a grace window (**8 s**), it kills the session … respawns on software x264" | ARCHITECTURE.md:243–246 | `transcode.rs:292` `ACTOR_HARDWARE_STARTUP_BUDGET = 12 s`, `:295` `ACTOR_SOFTWARE_STARTUP_BUDGET = 30 s`, `:306` `PROGRESS_STALL = 10 s`; #618 was literally "playback-control-m4-watchdog-removal" (08-27) — the actor's prepublication verdict replaced the watchdog | **MISMATCH** — number and mechanism |
| "segment duration `d` (**4 s**), forced keyframes at multiples of `d`" and diagram `seg=4s, keyframes@4s` | ARCHITECTURE.md:157, 162 | `plurx-core/src/transcode/mod.rs:72` `pub const SEGMENT_SECONDS: u32 = 2;` `:99` `COPY_SEGMENT_SECONDS = 6` `:151` `COPY_FIRST_SEGMENT_SECONDS = 2`; live TV `-hls_time 1` (live_tv.rs:9490) | **MISMATCH** — 2 s transcode, 6 s copy, 1 s live; never 4 |
| "Clients poll — there is no push channel." | ARCHITECTURE.md:400 | no `text/event-stream`, `WebSocketUpgrade`, `tungstenite` anywhere in `crates/plurxd/src` (only trakt.rs mentions "ws"); the playback-control plane is a bounded blocked-GET long poll (`vodserve.rs:123–129`, `playback.vod_blocked_get_cap`) | **TRUE**, with the nuance that the "poll" is now a server-held long poll — worth one sentence |
| "hiqlite 0.14 (spike) → else openraft 0.9 + redb + rusqlite" and risk row "hiqlite is a small project (bus factor) → `Store` trait isolation; openraft fallback is the same shape" | ARCHITECTURE.md:441, 545 | `Cargo.toml:149` `hiqlite = { path = "vendor/hiqlite" }`; `vendor/hiqlite/PLURX-PATCH.md` lists **15** patches (+3 in hiqlite-wal); 68 commits, +15,456 lines into `vendor/` in the window | **MISMATCH** — hiqlite is a maintained private fork, not a spike with a fallback (F-hist-12) |
| "Web app: embedded single-file SPA … one hand-written `index.html` with inline CSS and JS" | ARCHITECTURE.md:452, 455–456 | `index.html` is 99 lines; 62 served files under `crates/plurxd/src/web/` via `WEB_ASSETS` (#371, 09-19; `docs/clients/WEB-SHELL-LAYOUT.md`) | **MISMATCH** (9 days old; still no build step, so the *intent* holds) |
| "No transcode-by-default. The server will not 'optimize' a library into pre-baked renditions" | ARCHITECTURE.md:538–539 | Scheduled pre-transcode pass: `web/pages/activity-stream.js:258` "A pass runs on the schedule in Settings and picks from Continue Watching, Next Up and Recently Added"; distributed pretranscode queue (#520); `PretranscodeJob` in transcode.rs:19 | **SOFT MISMATCH** — opt-in, but the sentence reads as an absolute |
| "Every node is identical — there are no roles to configure" | ARCHITECTURE.md:18 | learners exist and are role-distinct (STATUS: "lab3 is a non-voting learner … serves only bounded catalogue reads and node-local routes"); `cluster_nodes.role` column (`57e03a6b`) | **STALE** — true for voters, not for the learner role added this month |
| Index row status "open" for `DECODER_SELECTION_RECOVERY_STATUS.md` | docs/README.md | the doc's own header: "**Status:** M0–M7 implementation complete and merged" | index drift |
| Index: 134 rows "open", 12 "live", 51 "built", 43 "done" | docs/README.md | 44 of the 134 "open" docs' own `**Status:**` headers say merged/complete/shipped/superseded; 51 `.md` files under `docs/` (all `apple-builds/*`, `archive/retro-*`, `evidence/*`) are not in the index at all | index drift |
| `PLAYBACK-CONTROL-STATUS.md` | docs/playback-control/ | opens with "**Reconciliation, 2026-09-12:** this historical ledger contains conflicting status snapshots" and defers to PLAYBACK-REWRITE-REMAINDER.md | self-declared unreliable; 178 `docs(playback)` commits produced a ledger that needed a reconciliation preface |

Also checked and **true**: ffmpeg spawned not linked; `/_internal/v1/activity-snapshot` RPC
exists; Argon2id/SHA-256 token scheme; no OpenAPI (API.md is the spec, #103).

---

### Findings

#### F-hist-1: The clustered fragment-index worker built nothing for 72 hours and no metric, gate or test noticed
- Severity: **P1** (production outage of a subsystem; silent) · Category: stability / ops
- Confidence: **CONFIRMED** (commit body + the census that now exists)
- Evidence: `120a3d29` (09-03): "`ce253a55` made `target_node_id` part of the jobs primary key
  and appended `AND target_node_id = $6` … in front of `owner_node_id = $4` … `validate_sql`
  refuses a statement whose placeholders are introduced out of order, and it refuses it *before
  any I/O*. Both statements have been refused on every voter since the 2026-08-31 19:00 UTC
  deploy. … **12,193 claims, 9,915 lost leases, 1,933 dead jobs over 617 cache keys, and not one
  artifact built in seventy-two hours. Store metrics showed zero write errors the whole time,
  because the statement never reached the store.**" Same commit: "This defect class already had
  four ledger rows … six weeks later the same mistake landed in two others."
- Why it matters: the outage window coincided with the v0.3.0 release (08-31) and was found by a
  human reading a heartbeat log, not by CI, the 631-commit validation ledger, `/metrics`, or the
  three-voter cluster lanes. The heartbeat treated the validator's `Err` as "lease lost" and the
  worker exited without logging at error level. A dead-job counter that climbs to 1,933 with
  zero write errors is exactly the signal an SRE dashboard should alarm on.
- Proposed change: (a) count `validate_sql` refusals as a store *error* metric distinct from
  I/O errors and alert on `jobs_dead_total` rising while `artifacts_built_total` is flat; (b)
  make the placeholder census run on **both** backends (C1; `cfba5ff6` says the `?N` side is
  still unchecked); (c) the store-contract suite must execute every replicated statement at
  least once through `dyn Store` on hiqlite in CI — the reason no test noticed is that the
  SQLite twin was correct. Reference: Jellyfin/Plex ship a single storage engine; if plurx keeps
  two, the contract suite is the only thing standing between them.
- Size: S–M · Risk: low

#### F-hist-2: `requeue_cluster_fragment_index` never worked on the SQLite backend, and its caller discarded the error
- Severity: **P1** · Category: stability · Confidence: **CONFIRMED**
- Evidence: `cfba5ff6` (09-17): "`requeue_cluster_fragment_index` binds twelve parameters and
  asked its active queue cap for the thirteenth, so rusqlite refused the whole statement every
  time. Its one production caller discards the error — `let _ = …` in the 'no verified holder
  could supply the v2 artifact' arm — so on the SQLite backend an artifact whose holders have
  all gone away has never been requeued for rebuild, silently, and the session answers 'no
  verified holder' forever." The statement was introduced with the clustered index in the 08-26
  `fix(vod)` series (`6dee9505`…`f5238856`; `git log -S requeue_cluster_fragment_index` oldest
  hit `6dee9505` 08-26) → **22 days** broken. Same commit: the admin **Force rebuild** button
  "answered 'analysis source changed or the active request queue is full' — a sentence
  describing neither condition — and did nothing" on every pre-upgrade row, on both backends.
- Why it matters: two user-facing dead ends (permanent "no verified holder", a Force button
  that lies) survived 22 days, four adversarial reviews and the validation ledger. The `let _ =`
  is the design smell: a store write whose failure is discarded cannot be observed.
- Proposed change: `rg 'let _ = .*store\.' crates/` and turn every discarded store `Result`
  into at least a `tracing::error!` with a counter; run the placeholder validator on `?N`
  statements (pure function over SQL, per the commit); make the "no verified holder" arm's
  requeue a tested path on both backends.
- Size: S · Risk: low

#### F-hist-3: The Windows port silently un-piped child stdout; one probe was fixed, two more are still broken on `main`
- Severity: **P1** (video-quality regression + a refused playback class) · Category:
  video-quality / stability · Confidence: **CONFIRMED** (read end to end)
- Evidence:
  - Origin: `2e3a3bb5` (09-13, "feat(windows): complete native server runtime", merged in #309
    09-14) replaced nine `.output()` calls with `crate::process_control::output_job_owned(&mut
    command)`. `process_control.rs:39–44`: `pub(crate) async fn output_job_owned(command) { let
    (child, _job) = spawn_job_owned(command)?; child.wait_with_output().await }` — nothing sets
    `Stdio::piped()`, and `wait_with_output` captures only what was piped, whereas
    `Command::output()` pipes both streams by default.
  - Fixed case: `6febf93e` (09-17): "no GPU tone-map graph has ever been validated on any node.
    Every HDR source that needs tone-mapping falls back to the CPU chain with software x264 …
    measured on `media1` as a 1080p rung presenting at 83% of realtime" → `pipeprobe.rs:208–222`
    now pipes stdout. So **every HDR transcode on the fleet ran software x264 from the 09-14
    deploy to 09-17.** (The commit body's "has ever been" is wrong — at the window start
    `pipeprobe.rs:207–213` used `.output()`, which piped; the regression is the Windows commit.)
  - **Still broken #1** — `ffmpeg.rs:2660–2690` `dovi_probe_output`: builds `Command::new(ffmpeg_bin())
    … .args(["-f", "framemd5", "-"])`, calls `output_job_owned`, then `let frames = String::from_utf8_lossy(&output.stdout).lines()…; (!frames.is_empty()).then_some(frames).ok_or_else(|| "Dolby Vision pixel probe produced no frame hashes")`. stdout is not piped → always `Err`.
    Consumer `transcode.rs:14322–14331`: `let proved = crate::ffmpeg::dovi_reshape_changes_pixels(file).await; … if !proved { return Err(unsupported_build_error("this source did not prove that Dolby Vision RPU application changes pixels through the production renderer")); }` and `needs_dovi_reshape` (`:14277–14295`) returns `Ok(true)` for every `dolby_vision` Profile 5 source without an HDR10/HLG-compatible base. **Every DV Profile 5 transcode has been refused since 09-14**, and the false result is cached per file in `dovi_proofs`.
  - **Still broken #2** — `transcode.rs:9264–9290` `probe_media_origin`: `Command::new(ffprobe_bin()).args(&args).stdin(null).kill_on_drop(true)` → `output_job_owned` → `parse_keyframe_origin(&String::from_utf8_lossy(&out.stdout))` → `.unwrap_or(start_seconds)`. stdout empty → origin always equals the requested start. Caller `transcode.rs:21274` "Freeze the achieved media origin before actor admission" for every copy-video HLS start with `start_seconds > 0`; the frozen presentation therefore claims the keyframe landed exactly at the request, which the log line itself says is the fallback ("subtitle cues fall back to the requested start").
  - Other callers (`ffmpeg.rs:2437`, `:2513`, `:2859`, `live_tv.rs:7403`) only read `status`/`stderr`; their stderr is now unpiped too, so the "live-TV FFmpeg graph failed: {stderr}" message (`live_tv.rs:7410`) will be empty.
- Why it matters: a platform-port refactor changed the semantics of a process helper used by
  quality-critical probes, and the test suite could not see it because the probes are mocked
  behind the `Spawn` trait (`pipeprobe.rs:1103` test double). One of three regressions was
  found by reading a container log three days later; two remain.
- Proposed change: make `output_job_owned` pipe stdout and stderr itself (match
  `std::process::Command::output` semantics — that is what every caller assumed), and add a unit
  test that spawns `/bin/echo` through it and asserts non-empty stdout. Invalidate the
  `dovi_proofs` cache on restart (it is in-memory, so a deploy clears it). Re-run the HDR
  tone-map reference on each node after deploy and record the encoder chosen.
- Size: S · Risk: low

#### F-hist-4: M6 "prepared quality handoff" — 67 PRs, enabled by default, unreachable by any client for a week
- Severity: **P2** (wasted effort + a misleading status record; no user harm because the old
  path kept working) · Category: architecture / ops · Confidence: **CONFIRMED** (docs +
  timeline)
- Evidence: 40 `agent/m6-*` PRs merged on 09-02; clients on `main` 09-08 (#118–#148); #193
  "Enable prepared handoff by default" and #203 "M6: prime prepared quality successors" 09-09;
  DECODER_SELECTION_RECOVERY_STATUS.md: "Settings → Developer now has a direct, default-on
  prepared-handoff checkbox". Then `docs/playback-control/QUALITY-SWITCH-CONTINUITY-PLAN.md`
  (09-16): "**the prepared handoff is built and deployed on every side, and no viewer action on
  any client can reach it.** The web and Android quality menus reopen before a `Prepare` can
  exist; Apple asks for one on the exchange that cannot yet carry it, gets `none`, and reopens
  1.5 s later … Every rung change the fleet has ever served has been the fallback path, whose
  interruption is measured at 271–2,246 ms on the web and 353–766 ms on Android." Fix: #348
  (09-17) "the prepared quality handoff, reachable at last".
- Why it matters: this is the clearest instance of the month's pattern — milestone-by-
  milestone merges each "proven" by unit/contract tests and receipts, no single end-to-end
  measurement ("did a real rung change on a real client take the new path?") until a human wrote
  an assessment. The same pattern produced F-hist-5 (Live TV) and the DVR replicated-store
  defects.
- Proposed change: every effort that changes a client-visible path must carry one
  *observed-in-production* counter as its exit criterion (here: `prepared_handoff_taken_total`
  vs `quality_switch_fallback_total` on `/metrics`, read off the fleet before the status doc
  says "merged"). Jellyfin's release process gates on a client smoke matrix for exactly this
  reason.
- Size: S per effort · Risk: low

#### F-hist-5: Live TV reached the fleet twice with a start path that could not succeed
- Severity: **P1** (feature unusable on every cluster after deploy) · Category: stability ·
  Confidence: **CONFIRMED**
- Evidence: `57e03a6b` #68 (09-07, one day after #52 promoted Live TV): "Turning Live TV on
  was impossible on any real cluster. Not difficult — impossible. …
  `live_tv_activation_guard_predicate`'s owner arm required `owner.role = 'voter'`.
  `cluster_nodes.role` is written in exactly one place, `promote_learner`, so a node that joined
  as a voter never receives that string and carries NULL." `93beae93` #288 (09-13, same day as
  the lane deployed): "Live TV starts have been failing on the fleet since this lane deployed …
  The owner's `validate_start_request` has always required the internal `request_id` to parse as
  a version-4 UUID … a client mints it from sixteen random bytes … so fifteen ids in sixteen are
  not version 4 and the owner threw the start away. … Nothing crossed that boundary in a test,
  which is why the suite stayed green." Also `cfbcd1f1` (09-05, caught pre-promotion): "No node
  could join a cluster running this build. Live TV did not have to be enabled for that to be
  true" — a trigger on a cluster table read `settings`, which a joining node does not yet have.
- Why it matters: 224 Live TV commits and four "adversarial review found N defects" passes
  (8, 14, 19, 20 defects on 09-08) still shipped two start-blocking bugs, both at a *boundary*
  (ingress vs owner; SQL NULL semantics). The reviews were reviewing code, not running a start.
- Proposed change: a per-deploy smoke that performs one real `start` on one real tuner from
  the web client and fails the deploy playbook if it does not produce a playable segment within
  the budget; a `cluster_nodes.role` backfill or a `role IS NULL → voter` invariant test.
- Size: S · Risk: low

#### F-hist-6: The hiqlite schema-admission list caused three startup hotfixes in six days
- Severity: **P1** (daemon does not start on replicated store after upgrade) · Category:
  stability · Confidence: **CONFIRMED**
- Evidence: chain C2. `8df09290` (09-16) diff adds `| VIDEO_CODEC_TAG_SCHEMA_MIGRATION_SOURCE`
  to the admitted-source match and the loop assertion "schema v{schema_version} must be
  admitted to the migration chain" over `AUTH_SCHEMA_MIGRATION_SOURCE..AUTH_SCHEMA_VERSION`
  (`crates/plurx-core/src/store/hiqlite.rs`). Before 09-16 there were 35 hand-written
  `assert_eq!(X_SOURCE + 1, X_VERSION)` pairs and no exhaustive check; the migration ladder is
  at v63 per STATUS ("the migration ladder at 63").
- Why it matters: 63 schema versions in a store whose migration path is a hand-maintained
  enumeration is a per-release outage risk. Three of the four misses were found by deploying.
- Proposed change: the loop test now exists — keep it, and additionally derive the admitted
  set from the migration table rather than a parallel list (single source of truth, the way
  `refinery`/`sqlx migrate` treat the ladder). Add a downgrade-fixture generator so
  `DROPPED_BY_THE_FIXTURE` is not edited by hand (`e314a5bd` shows both fixtures missed the DVR
  tables).
- Size: S · Risk: low

#### F-hist-7: `main` was red, repeatedly, and merging over red became "established practice"
- Severity: **P2** · Category: ops · Confidence: **CONFIRMED**
- Evidence: `docs/cluster/TRANSPORT-RECOVERY-RESOURCE-BASELINE.md` §1: "`ci / cluster
  transport recovery campaign` fails on `main` itself, and has since `14d0519a` [09-07] … PRs
  #67, #74, #93, #98 and #100 all merged with this red, **which is the established practice
  while it stays this way**." STATUS 09-13: "`web layout and accessibility` was red on `main`
  … `main` stayed red on that lane until … `37ce1e87`"; "Two `live_tv` tests are
  deterministically red on `main` … Reported, not changed, and still red." #74 (09-07) three
  red unit tests; eight post-merge repair PRs on 09-09 after #189; #356 (09-17) "`main` was
  carrying twenty failing tests across `plurxd` and `plurx-core`". STATUS 09-19: web-check
  "fail[s] on the same six tests, by name, as they do on `main`" — still six red today.
  `e314a5bd`: the fast lane compiles but does not run the 282 `hiqlite-store` tests, so the DVR
  merged with three replicated-store defects.
- Why it matters: a red baseline hides new red (the STATUS entry about `TargetClosedError`
  matching is a textbook example of that failure mode — it was reasoned to be pre-existing and
  was not). Twenty red tests carried three live production defects (F-hist-2, F-hist-3).
- Proposed change: no-merge-on-red for the fast lane as a hard rule (Forgejo required
  status), a required `--features hiqlite-store` run for any PR touching `store/` or
  `cluster/`, and a nightly "known-red inventory" that must be empty or explicitly quarantined
  with an issue — the `#[ignore = "reason"]` discipline already used for slow tests is the right
  shape.
- Size: S · Risk: low

#### F-hist-8: 57% of the month's commits changed no product code; 502 of them are 7-line ledger receipts
- Severity: **P2** (velocity and review-attention cost) · Category: ops / architecture ·
  Confidence: **CONFIRMED** (numbers in §5)
- Evidence: §5 tables. `validation/regressions.d/` 853 files; `validation/history.py`
  `ISSUE_RE` classifies commits as fixes by subject regex (`keep|bound|remove|preserve|truth|…`);
  receipts are keyed by short SHA (`ca5ca8bd` renames 8 after a rebase); self-referential rows
  exist (`114bad7d` "mark the preceding ledger commit non-runtime"). Docs grew 115 → 460 files
  and `docs(playback)` alone is 178 commits; the playback-control ledger needed a
  "Reconciliation" preface because its snapshots conflict.
- Why it matters: the ledger did not catch F-hist-1/2/3/4/5/6 — each was found by a human
  reading a log, a render, or the code — while consuming ~one commit in six and forcing every
  rebase to re-map. Process that does not move detection earlier is cost without return. The
  owner should see this number alongside the 3,860.
- Proposed change: keep the *coverage points* (`validation/points.toml`) and the fixture
  gates; retire per-commit SHA receipts in favour of a PR-level field (Forgejo PR template:
  "regression test: <path>::<name>") checked once at merge; move status prose out of STATUS.md
  (2,581 lines) into per-effort docs with a one-line index row; stop counting `docs`/`chore`
  commits toward "fix" classification by matching on Conventional-Commit type only.
- Size: M · Risk: low

#### F-hist-9: Server releases stopped at v0.3.0 while the fleet runs untagged `main` and mobile builds were bumped 92 times
- Severity: **P2** · Category: ops · Confidence: **CONFIRMED**
- Evidence: `git tag --sort=-creatordate` → `v0.3.0` (08-31), `v0.2.8` (08-29), nothing
  after; `git rev-list v0.3.0..HEAD` = 2,213 non-merge commits / 736 merges; STATUS and docs
  reference fleet versions `v0.3.0-64-g…`, `-260-`, `-568-`, `-639-`, `-700-gc661d387`;
  `Cargo.toml:21 version = "0.3.0"` unchanged; CHANGELOG `[Unreleased]` is 340 lines / 25
  headline entries vs 116 lines for 0.3.0 itself. Apple `CURRENT_PROJECT_VERSION` 82 → 173
  (92 bumping commits to `project.yml`), Android `versionCode` 44 → 111.
  `validation/mobile_versions.py` enforces that mobile counters advance with client changes —
  and they do — but nothing equivalent exists for the server.
- Why it matters: the project instructions require "proper versioning" and Ansible deploys to
  every node; a fleet on `v0.3.0-700-g…` cannot be rolled back to a named point, CHANGELOG
  entries cannot be matched to what a node runs, and 0.3.0→(unreleased) spans the fragment-index
  outage, the HDR x264 regression and the schema hotfixes. The mobile side shows the discipline
  is achievable — 92 client builds in a month is arguably *too* fine.
- Proposed change: tag `v0.3.x` at least weekly or on every Ansible fleet deploy (the deploy
  playbook can refuse an untagged SHA); make `[Unreleased]` → section rollover part of the tag
  script (RELEASING.md already passes the tag to CI); pair each mobile build with the server tag
  it was qualified against in `docs/apple-builds/`.
- Size: S · Risk: low

#### F-hist-10: ARCHITECTURE.md §8 non-goals and §3 numbers describe a system that no longer exists
- Severity: **P3** (docs) · Category: architecture · Confidence: **CONFIRMED** (table in §6)
- Evidence: §6 — "No DVR" vs shipped DVR (#294, routes, replicated tables); "watchdog 8 s" vs
  `ACTOR_HARDWARE_STARTUP_BUDGET = 12 s`/`PROGRESS_STALL = 10 s` and #618 "watchdog removal";
  "segment 4 s" vs `SEGMENT_SECONDS = 2`, `COPY_SEGMENT_SECONDS = 6`; "1-voter raft single node"
  vs `SQLite single-node` mode; "single-file SPA" vs 62 files; "hiqlite (spike) → openraft
  fallback" vs vendored fork; "no roles" vs learners.
- Why it matters: the brief tells every reviewer to read §1–§3 "so you know what is
  deliberate"; four of the numbers they will calibrate against are wrong, and one shipped
  feature is listed as a refusal. New agents are being briefed from this file (the project
  docs-style skill says so).
- Proposed change: one PR that rewrites §2.1, §3 (numbers → named constants with file links,
  as `transcode/mod.rs:68` already demands: "Nothing may hardcode the number"), §6 table, §8
  (DVR moves to a "decided 2026-09-13" entry with the retention/conflict answers), §9 risks.
  Add a `tests/docs` gate that greps ARCHITECTURE.md for the constants it quotes — the repo
  already has `validation/doc_versions.py` for versions; extend it to these constants.
- Size: S · Risk: low

#### F-hist-11: UI redesign shipped to three platforms and was rolled back by the owner comparing renders
- Severity: **P2** (four rollback PRs, five Apple builds, an unstable web item page for a
  week) · Category: architecture / ops · Confidence: **CONFIRMED**
- Evidence: chain C5. `4b109ebd`: "Paul compared real renders of the web item page before PR
  #317, after PR #317 and at current main (PR #330) and chose the page from before PR #317."
  `index.html` had 299 commits / ±62k lines in the window; 142 were fixes. `docs/README.md`
  still carries "Mobile UI work: [Apple build 159 notes] … **open**" and "Native Home
  restoration … **open**" side by side.
- Why it matters: layout is the one class of change whose correctness cannot be established
  by the unit/golden/receipt machinery the month leaned on — and it was the class that got
  reverted. There is no design-review gate *before* an agent builds a redesign; mockups exist
  (`docs/mockups/`) but #317/#330 were merged first and judged after.
- Proposed change: for any PR touching layout, require a before/after render pair (the
  ui-baseline fixture library already renders at 1280/390 px) attached to the PR and an owner
  ack before merge; keep the `tests/web/calm-library.test.js` pin that "fails against main's
  index.html".
- Size: S · Risk: low

#### F-hist-12: hiqlite is now a private fork carrying 18 patches and +15k lines; the documented mitigation ("openraft fallback") is gone
- Severity: **P2** · Category: architecture · Confidence: **CONFIRMED**
- Evidence: `Cargo.toml:149–150` `hiqlite = { path = "vendor/hiqlite" }`, `hiqlite-wal = {
  path = "vendor/hiqlite-wal" }`; `vendor/hiqlite/PLURX-PATCH.md`: "Plurx carries fifteen
  compatibility patches for clustered deployments" (+3 in `hiqlite-wal/PLURX-PATCH.md`),
  including an election trigger exposed "because that release has no dedicated leader-transfer
  operation", `local_db_raft_metrics`, `db_quorum_watermark`, snapshot-recovery retry/anchoring
  (`97075008`, `ed9fbe5d`), undispatched-request retry (`30b9d8e4`), cancelled-query redaction
  (`0b1e43f5`). 68 commits in the window; `git diff --stat` since the window root: 65 files,
  +15,456/−1,302 in `vendor/hiqlite*/src`. `ARCHITECTURE.md:545` still lists the mitigation as
  "`Store` trait isolation; openraft fallback is the same shape".
- Why it matters: the bus-factor risk the architecture named has been *absorbed* rather than
  mitigated — plurx now maintains ~40k lines of raft/SQLite replication code that upstream 0.14
  does not have, with 19 `fix(cluster)` commits landing in it on a single day (09-06). Upgrading
  to hiqlite 0.15+ or swapping to openraft directly is no longer "a backend swap".
- Proposed change: decide explicitly — either upstream the patches (several are generic:
  duplicate-id rejection, TLS key race, metrics wrappers) and pin to a release, or own the fork
  openly (rename the crate, own its CI, drop the "fallback" sentence). Either way, add the
  vendored diff size to the risk table and keep `PLURX-PATCH.md` as the authoritative list.
- Size: L (upstreaming) / S (documenting the decision) · Risk: med

#### F-hist-13: Four files hold 65k lines of product code and absorb 40% of all fix commits
- Severity: **P2** (maintainability; merge-conflict and review-blindness cost) · Category:
  architecture · Confidence: **CONFIRMED**
- Evidence: §3. `transcode.rs` 14,366 → 47,096 lines (27,573 before its last `#[cfg(test)]`),
  294 commits, 155 fixes; `http/hls.rs` 3,129 → 28,507 (14,193 product); `playback_control.rs`
  29,986 (14,533 product, new this month); `vodserve.rs` 15,011 (8,091 product);
  `cluster/membership.rs` 1,345 → 17,169 (10,708 product) at 75% fix density. Product LOC
  overall grew 3.0× (54k → 162k) in 31 days.
- Why it matters: every reviewer in this exercise will be asked to read `transcode.rs`; a
  27k-line module with `PROGRESS_STALL`, `FLOW_INTENTION_BUDGET`, `PLAYLIST_WAIT_SLACK`,
  actor, supervisor, DV pipe, media-origin probe and HLS presentation in one namespace is where
  F-hist-3's second regression hid unnoticed for six days. Fix density > 50% on the largest
  files is the strongest single stability signal the history offers.
- Proposed change: split along the seams the commit messages already name (prepublication
  actor, producer supervisor, copy pipeline, DV pipeline, presentation freezing) — the web
  shell split (#371) shows the team can do a byte-identical decomposition with a gate; do the
  same for `transcode.rs` and `hls.rs` before the next effort lands on them.
- Size: M · Risk: med

---

### Already good (do not undo)

1. **Commit bodies are RCA-grade.** `120a3d29`, `cfba5ff6`, `6febf93e`, `57e03a6b`,
   `93beae93`, `e314a5bd` each state the mechanism, the blast radius with numbers, why tests
   missed it, and what census now prevents recurrence. This is the best forensic record I have
   seen in a repo of this size; keep the standard.
2. **`#[ignore]` always carries a reason** and is used only for slow/hardware/opt-in tests
   (12 total); no failing test was silenced this way.
3. **Tests are added, not deleted** (79 removed attributes vs thousands added); the web suite
   went from 0 to 300 cases; Swift/Kotlin tests tripled.
4. **The web-shell split (#371)** was proven byte-identical with a one-shot reassembly gate,
   and the reviews caught three guards that had become vacuous. This is the model for splitting
   `transcode.rs`.
5. **Repo-wide placeholder census on hiqlite** (`120a3d29`: "run against the validator the
   store itself applies, with the module list checked against the directory so a new slice
   cannot opt out … no per-statement exemption marker").
6. **Migration-chain loop test** (`8df09290`) and the `DROPPED_BY_THE_FIXTURE` naming in
   downgrade fixtures.
7. **`vendor/*/PLURX-PATCH.md`** — every vendored patch is enumerated with its reason; the
   Apache-2.0 adoption (#357) shipped third-party notices.
8. **Fleet-observed numbers in docs** (`QUALITY-SWITCH-CONTINUITY-PLAN.md` 271–2,246 ms; the
   transport-recovery baseline's run table; `PLAYBACK-CONTROL-STATUS` fleet versions read off
   `/metrics`) — when measurement happened, it was written down precisely.
9. **Mobile version enforcement** (`validation/mobile_versions.py`) works: counters never
   went backwards across 92 bumps.
10. **`docs/README.md` three-tier structure** (reference set / subject folders / archive) is
    the right shape; only its status column has drifted.

### Hot spots (fix commits per path, window)

`git log --since=2026-08-20 --format=%s -- <path> | grep -c '^fix'` (strict prefix, so lower
than the §3 "fix" column):

| Path | `^fix` commits |
|---|---|
| crates/plurxd/src/transcode.rs | 155 (broad) / see §3 |
| crates/plurxd/src/http/hls.rs | 109 |
| crates/plurx-core/src/cluster/membership.rs | 80 |
| crates/plurxd/src/vodserve.rs | 67 |
| crates/plurx-cluster-check/src/lib.rs | 63 |
| crates/plurx-core/src/store/hiqlite_sessions.rs | 42 |
| crates/plurxd/src/http/cluster_operations.rs | 32 |
| crates/plurxd/src/http/cluster.rs | 31 |
| crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs | 27 |
| crates/plurxd/src/live_tv.rs | 28 |
| vendor/hiqlite (all) | 68 commits, majority `fix(cluster)` |

By scope over the whole tree: `fix(cluster)` 219 · `fix(playback)` 189 · `fix(ci)` 62 ·
`fix(web)` 51 · `fix(validation)` 40 · `fix(apple)` 37 · `fix(live-tv)` 32 · `fix(android)` 30 ·
`fix(store)` 28.

### Open questions I could not settle from history alone

1. Is `dovi_reshape_changes_pixels` exercised on the fleet today (any DV Profile 5 titles in
   the libraries)? If yes, F-hist-3's first residual has been refusing them since 09-14 and
   should show as `unsupported_build_error` in client logs.
2. How many of the 92 Apple builds were actually installed on a device? `docs/apple-builds/`
   has notes for ~35 of them; the rest may be counter bumps forced by `mobile_versions.py`.
3. Whether the `SQLite single-node` boot mode is a supported product configuration or only
   the "recovery boot after an interrupted activation" OPERATIONS.md describes — ARCHITECTURE
   says one thing, OPERATIONS another.
4. The transport-recovery campaign now asserts an envelope (Option A). Has it been green on
   `main` for a sustained run since 09-08, or only on the PR that changed it?
5. Whether the 631 receipt commits are ever *read*: is there any consumer of
   `validation/regressions.d/` other than `history.py`'s own pass/fail?
6. `PLAYBACK-CONTROL-STATUS.md` says M9 "final deletion and acceptance are incomplete" — is
   the legacy path (`retained live-HLS engine`, `58bd7b3f`) still reachable in production, and
   is anything measuring which path a session takes?

---

## plurx architecture review — streaming-pipeline (server side)

Reviewer scope: `crates/plurxd/src/{transcode.rs, ffmpeg.rs, http/hls.rs, http/stream.rs, vodserve.rs, vodencode.rs, vodgen.rs, dvpipe.rs, dv_disk.rs, decode_facts.rs, decoder_health.rs, progressive.rs, copyseg.rs, renditiondir.rs, pipeprobe.rs, bounded_process.rs, process_control.rs, cachekeep.rs, shared_cache.rs, prod*.rs}`, `crates/plurx-core/src/{transcode/, playback/, segplan.rs, fmp4.rs, mediafacts.rs, tracks.rs}`. Repo at `main` = a1414368, 2026-09-20. Read-only; nothing compiled or run.

Two engines are live. **Immutable encoded/copy VOD** (`vodserve.rs` + `vodgen.rs`/`copyseg.rs` + `plurx_core::transcode::vod::vod_pipe_args`, fMP4 pipe cut by plurx) is primary. **Rolling HLS** (`transcode.rs::Session` + `plurx_core::transcode::hls_args_inner`, ffmpeg's own HLS muxer, MPEG-TS) is the fallback for un-indexed / refused media. Both build their argv from the same `hls_args_inner`, which is good; most argument findings therefore apply to both.

---

### 1. Argument sets, as built (quoted), and the verdicts

#### (a) Hardware encode — `Encoder::encode_args_for`, `plurx-core/src/transcode/encoder.rs:384-479`

```
Software:      -c:v libx264 -preset veryfast [-crf Q] [-b:v Nk] -maxrate 1.5Nk -bufsize 2Nk -profile:v high [-threads T]
NVENC:         -c:v h264_nvenc -preset p4 [-rc vbr -cq Q] -b:v Nk -maxrate 1.5Nk -bufsize 2Nk [-forced-idr 1]
VideoToolbox:  -c:v h264_videotoolbox [-q:v Q] -b:v Nk -maxrate 1.5Nk -bufsize 2Nk
VA-API:        -c:v h264_vaapi [-rc_mode QVBR -global_quality Q] -b:v Nk -maxrate 1.5Nk -bufsize 2Nk
QSV:           -c:v h264_qsv [-global_quality Q] -b:v Nk -maxrate 1.5Nk -bufsize 2Nk [-forced_idr 1]
HDR10 rung:    -c:v {libx265|hevc_qsv} -preset veryfast {-x265-params hdr10=1:repeat-headers=1 | -profile:v main10}
               -b:v Nk -maxrate 1.5Nk -bufsize 2Nk -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -color_range tv
```
`N = bitrate_for_height()` (`transcode.rs:26619`): 2160→20000, 1080→8000, 720→4000, 480→2000, else 1200 kbps. `[-crf/-cq/-q:v/-global_quality]` appear **only** when `RateMode::Quality` is configured; `RateMode::default()` is `Bitrate` (`encoder.rs:106-110`), and `effective_for` (`transcode.rs:12552-12562`) yields `EffectiveRateControl::Vbr` otherwise.

Common to every rolling/HLS command (`plurx-core/src/transcode/mod.rs:1651-1706`):
```
-force_key_frames expr:gte(t,n_forced*2)  -c:a aac -ac 2 -b:a 160k  -muxdelay 0 -muxpreload 0
-f hls -hls_time 2 -hls_playlist_type event -hls_flags independent_segments+temp_file -hls_segment_type mpegts
```
VOD encoded pipe appends (`vod.rs:255-304`): `-ar 48000 -profile:a aac_low -bf 0 -flags +cgop -g F -keyint_min F -sc_threshold 0 -fps_mode:v passthrough -enc_time_base:v … -movflags +empty_moov+delay_moov+default_base_moof+frag_keyframe -f mp4 pipe:1`.

Verdict: the maxrate/bufsize model on every family is right (see "already good"). What is missing is listed as findings F-1 (ABR default), F-2 (`-bf 0`), F-10 (NVENC profile/tools), plus: no `-level`, no `-g/-sc_threshold` on the rolling path (only `-force_key_frames`, so x264 scenecut IDRs make segment lengths drift off the 2 s grid the failover contract assumes — P3), QSV/VAAPI/VT/NVENC have never been quality-swept except QSV (`encoder.rs:189-199`).

#### (b) Software fallback — see above; x264 `veryfast`, profile high, no tune, 8-bit via `format=yuv420p`.

#### (c) HDR→SDR tone-map

CPU chain (`mod.rs:1052-1056`):
```
scale=-2:'min(H,ih)',
zscale=tin=smpte2084:min=bt2020nc:pin=bt2020:t=linear:npl=100,format=gbrpf32le,
tonemap=tonemap=hable:desat=0,
zscale=p=bt709:t=bt709:m=bt709:r=tv,format=yuv420p
```
GPU chains (`pipeline.rs:327-412`): `vpp_qsv=w:h:tonemap=1:format=nv12`; `scale_vaapi=…:format=p010,tonemap_vaapi=format=nv12:matrix=bt709:transfer=bt709:primaries=bt709`; `hwupload,libplacebo=w:h:tonemapping=bt.2390:colorspace=bt709:…:format=nv12,hwdownload`; `scale,format=p010,hwupload,tonemap_opencl=tonemap=hable:…,hwdownload`; `tonemapx=tonemap=bt2390:…:apply_dovi=1,scale,format=yuv420p` (jellyfin-ffmpeg Dolby path). Verdict: F-3 (CPU chain has no `peak=`, wrong primaries order, no dither); `tonemap_opencl=hable` should be `bt2390` where the build has it; libplacebo chain is correct (peak detection and blue-noise dither are its defaults). No deinterlacer anywhere on the file path — F-4.

#### (d) Audio — every full transcode: `-c:a aac -ac 2 -b:a 160k` (`TranscodeOptions::default`, `mod.rs:895-913`, reached via `..Default::default()` at `transcode.rs:14663`). Copy/remux with audio conversion (`mod.rs:2045-2055`): `-c:a aac -b:a 320k -channel_layout:a 5.1` for 6-ch, else `-b:a 256k` with the source channel count. No `-ar` on the rolling path, no downmix matrix, no loudness handling, no EAC3/AC3 anywhere — F-5.

#### (e) Remux — `copy_video_args` (`mod.rs:1787-1878`): `-c:v copy -tag:v hvc1|dvh1 [-strict unofficial] -bsf:v dovi_rpu=strip=1,hevc_mp4toannexb,extract_extradata,filter_units=remove_types=…`, `-map_chapters -1`, `-sn`, `-noaccurate_seek`, `-avoid_negative_ts make_zero`. Verdict: good (see "already good").

---

### Findings

#### F-stream-1: Default video rate control is 1-pass ABR (`-b:v` alone) on every family; the measured QVBR/CRF mode is opt-in
- Severity: P1 · Category: video-quality · Confidence: CONFIRMED
- Evidence: `encoder.rs:106-110` `enum RateMode { #[default] Bitrate, Quality }`; `transcode.rs:12552-12562` `effective_for`: quality only `if self.requested_mode == RateMode::Quality && …`, else `EffectiveRateControl::Vbr`; `encoder.rs:416-424` software arm adds `-crf` only in the `Qvbr` case, so the shipped x264 line is `-c:v libx264 -preset veryfast -b:v 8000k -maxrate 12000k -bufsize 16000k -profile:v high`. `bitrate_for_height` (`transcode.rs:26619-26627`) never consults `file.bitrate`. CHANGELOG (Unreleased, "media1 D5 sweep selected QSV quality 22 … cleanup restored idle legacy bitrate mode") confirms the default stayed Bitrate.
- Why it matters: x264 1-pass ABR is its weakest mode (no lookahead-informed allocation, visible QP pumping after scene cuts; at `veryfast` rc-lookahead is 10 frames). A 3 Mb/s 1080p web source is re-encoded to an 8 Mb/s *target* (ABR spends bits to hit the target) while a 40 Mb/s grainy 1080p disc is crushed to the same 8 Mb/s. Every HDR tone-map session on an SDR TV, every burned-subtitle session and every "reduce quality" rung goes through this.
- Proposed change: make `Quality` the default for Software (`-crf 23 -maxrate -bufsize`, which is Jellyfin's default shape and what x264 documents as "CRF with VBV") and for QSV (already swept, 22); cap the VBV `maxrate` at `min(ladder, 1.2 × source bitrate)` as Jellyfin/Plex do. Keep Bitrate mode for the un-swept families until their sweep runs (`scripts/bench rate-control` already exists).
- Size: S · Risk: low (cache recipe hash changes — new entries, no invalidation of correctness)

#### F-stream-2: Encoded VOD disables B-frames (`-bf 0`) on every encoder
- Severity: P1 · Category: video-quality · Confidence: CONFIRMED
- Evidence: `plurx-core/src/transcode/vod.rs:270-275`:
  ```
  // No reorder delay: the plan addresses presented frame boundaries,
  // not a decoder preroll hidden before the URI's declared start.
  "-bf".to_owned(), "0".to_owned(), "-flags".to_owned(), "+cgop".to_owned(),
  ```
  together with `-use_editlist 0` and `-avoid_negative_ts disabled` (`vod.rs:290-293`).
- Why it matters: this is the *primary* transcode path. x264 at equal CRF loses roughly 10–20 % efficiency without B-frames (more on animation/static content); for hardware encoders the loss is larger (NVENC/QSV rely on B-pyramids for most of their quality at a given bitrate). At the fixed ABR targets of F-1 it becomes visible quality loss rather than bitrate growth. Closed GOP is fine; `-bf 0` is not required for it.
- Proposed change: keep B-frames and handle the reorder delay the way Apple's and Bento4's fMP4 packagers do: `-movflags +negative_cts_offsets` (ctts v1, no edit list needed) with `-bf 3` (x264) / `-bf 3 -b_ref_mode middle` (NVENC) / default (QSV). The segmenter (`plurx_core::fmp4`) already reads `tfdt`/`trun`; the plan's frame grid is unchanged because IDR positions are still `-force_key_frames expr:eq(mod(n,F),0)` (`vod.rs:252-254`). Validate with the existing `establish_or_verify` (`vodserve.rs:7084`) physical-grid check.
- Size: M · Risk: med (needs the AVPlayer/ExoPlayer/hls.js matrix run once; negative CTS is universally supported since iOS 11)

#### F-stream-3: CPU zscale tone-map has no explicit `peak`, converts primaries after the tone curve, and outputs 8-bit without dither
- Severity: P1 · Category: video-quality · Confidence: LIKELY (chain read end to end; the peak-inference path is FFmpeg behaviour I could not run here)
- Evidence: `mod.rs:1040-1056` (quoted in §1c). The comment above it says exactly why side data is unreliable: "Hardware decode (QSV/VAAPI) frequently drops color metadata across the hwdownload" — and then fixes only zscale's *input* tags, not `tonemap`'s `peak`. `vf_tonemap` with `peak=0` calls `ff_determine_signal_peak()`, which reads MaxCLL / mastering max-luminance side data, and when that is absent falls back on `frame->color_trc` (100 for PQ, **1.0 when trc is unspecified**). After an `hwdownload` that stripped both, `hable(sig)/hable(1.0)` clips every highlight; when only the SEI is lost, `peak=100` maps 100-nit white to ~0.2 (the classic "dark tone-map"). The scanner stores no MaxCLL/max-luminance to pin it (`rg -i 'max_luminance|maxcll' crates/plurx-core/src` → nothing). Primaries: FFmpeg's documented chain is `zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=…,zscale=t=bt709:m=bt709:r=tv` — gamut map *before* the curve; plurx maps after, so out-of-709 values are clipped post-compression (hue shift on saturated highlights). `format=yuv420p` after a float chain with no `dither=` produces banding in tone-mapped gradients.
- Why it matters: the CPU chain is "the floor and the fallback" (ARCHITECTURE §3) and the only chain on any node without a proven GPU graph or jellyfin-ffmpeg. The picture varies with whether the decoder kept side data — a hardware node and a software node produce different brightness for the same file, and the cache digest treats them as one recipe.
- Proposed change: (1) store `max_content_light_level`/`max_luminance` from `ffprobe -show_streams` side data at scan (already the ffprobe-is-ground-truth design) and emit `tonemap=…:peak={MaxCLL or max_lum}/100`, defaulting to `peak=10` (1000 nits, the mastering norm) when unknown — never let it infer; (2) move `zscale=p=bt709` before `tonemap` as the ffmpeg docs and Jellyfin do; (3) `zscale=…:dither=error_diffusion` on the final 8-bit conversion; (4) prefer `tonemap=bt2390`/`mobius` if the build has it (`tonemapx` already uses bt2390). Reference: FFmpeg filters doc §tonemap; Jellyfin `EncodingHelper.GetSwTonemapFilter`.
- Size: S–M · Risk: low

#### F-stream-4: No deinterlacing on the file-transcode path, and the scanner does not record field order
- Severity: P1 (for interlaced libraries) · Category: video-quality · Confidence: CONFIRMED
- Evidence: `rg -n 'yadif|bwdif|field_order|interlac' crates/plurx-core/src crates/plurxd/src` → only `live_tv.rs:6135` (`bwdif=mode=send_field:parity=auto:deint=interlaced`) and PGS overlay. `decode_facts.rs:3343` show_entries list has no `field_order`; `plurx-core/src/domain.rs`/`mediafacts.rs` have no interlace field. `video_filters_for_contract` (`mod.rs:981-1065`) goes straight to `scale`.
- Why it matters: 1080i broadcast captures, DVD remuxes and older TV rips (a large share of TV libraries) are scaled and encoded with combing baked in — visibly worse than the source on every transcode rung, and worse still after `scale` smears the fields.
- Proposed change: capture `field_order` at scan; in the CPU chain insert `bwdif=mode=send_frame:parity=auto:deint=interlaced` (or `yadif`) before `scale` when `field_order != progressive`; for VPP/VAAPI graphs use `vpp_qsv=deinterlace=2` / `deinterlace_vaapi`. Jellyfin and Plex both do this by default.
- Size: M · Risk: low

#### F-stream-5: Every full transcode downmixes to stereo AAC 160 k; no 5.1 AAC, no EAC3/AC3, no sample-rate control
- Severity: P1 · Category: video-quality (audio) · Confidence: CONFIRMED
- Evidence: `TranscodeOptions::default()` → `audio_channels: 2, audio_bitrate_kbps: 160` (`mod.rs:895-913`); `options_for_tone_map` fills the rest and ends with `..Default::default()` (`transcode.rs:14591-14664`); no other construction site sets `audio_channels` (`rg -n 'audio_channels' crates/plurxd/src` shows only tests and live_tv). `hls_args_inner` then emits `-c:a aac -ac {2} -b:a {160}k` (`mod.rs:1661-1666`) with no `-ar`, no `pan=`. ARCHITECTURE §3 promises "else transcode to EAC3/AC3/AAC with correct downmix"; only AAC exists in the tree. The copy path is better (`-b:a 320k -channel_layout:a 5.1` for 6-ch, `mod.rs:2048-2052`) but passes 7.1/8-ch sources straight to the AAC encoder with no `-ac`, and keeps the source sample rate on the rolling path (a 96 kHz TrueHD concert → 96 kHz AAC-LC, which several Android MediaCodec decoders and TVs refuse).
- Why it matters: an Apple TV or Shield on an AVR gets stereo whenever the video needs work (HDR→SDR for an SDR TV, a subtitle burn, a bitrate rung) even though the client advertised `eac3`/`ac3` (`playback/profiles.toml:23`). swresample's default downmix normalises so dialogue lands several dB lower than the source — the "quiet dialogue when transcoding" complaint.
- Proposed change: carry `max_audio_channels`/codec from the device profile into `TranscodeOptions` (`audio_channels: min(source, profile)`), emit `-c:a eac3 -b:a 640k` (or `ac3 640k`) when the profile lists it, else `aac -ac 6 -b:a 320k -channel_layout 5.1` (Apple authoring spec §7); always `-ar 48000` on lossy output; for stereo downmix use an explicit matrix with centre at −3 dB and no LFE (Jellyfin's `pan=stereo|c0=0.5*c2+0.707*c0+0.707*c4+0.5*c3|c1=…` or `-ac 2 -af loudnorm` is not appropriate live). Record the choice in the recipe hash.
- Size: M · Risk: low

#### F-stream-6: Encoded VOD kills and respawns ffmpeg every few seconds in steady state (ahead-window hold = SIGKILL, no hysteresis)
- Severity: P1 · Category: performance / stability · Confidence: CONFIRMED (from code; production log rate not measured here)
- Evidence: `vodserve.rs:6316-6322`:
  ```
  let step = if rendition.recipe.encoding.is_some() && matches!(step, Step::Stop) {
      // A full ahead window is no reason to reserve scarce encoder capacity …
      Step::Terminate { why: Termination::IndefiniteHold }
  ```
  `prodsched.rs:467-475` `stop()` suspends when `through >= furthest + horizon`; `prodsched.rs:372-386` resumes as soon as `next_gap(furthest) <= furthest + horizon`, i.e. one segment after the frontier advances — there is no low-water mark (the hysteresis at `prodsched.rs:214-237` covers only the working-set budget). `prodexec.rs:151-164` even documents the opposite intent: "`Hold::Ahead` clears when a reader advances … resuming costs nothing where restarting costs a reposition." Frontier = playhead from control (`vodserve.rs:706-717`), control cadence 5 s (`docs/playback-control/PLAYBACK-LIFECYCLE-COVERAGE.md:277`). Each respawn: `spawn_generation` (`vodserve.rs:6735`) → `recipe_engine_is_current` (stat of the whole ffmpeg `.so` closure, `ffmpeg.rs:1693-1699`, plus `fc-list`+`fc-conflist` for text burns, `ffmpeg.rs:1612-1621` → `:1806`), a store read for admission policy (`vodencode.rs:97-101`), then an ffmpeg that seeks to `target − 2 s` and decodes/discards two seconds of preroll (`vod.rs:137-140`). VOD-ENCODING.md's own measurement: "7.629 s preparation/reap" per restart.
- Why it matters: after the first 180 s the producer sits on the horizon line; every control beat opens a 2–3 segment gap, spawns a process that produces 2–3 segments (≈5 s) and is SIGKILLed again. A two-hour film ≈ 1,400 process launches; each pays decoder init + 2 s preroll decode (≈50 frames of 4K HEVC — several seconds on a CPU decode) + encoder warm-up (x264 ABR restarts its rate-control history at every generation boundary, so F-1's pumping recurs every few seconds). With a text burn it also pays two `fc-list` invocations per launch (see F-7). A single viewer on an otherwise idle box pays this; the "another viewer waits" case the comment optimises for is handled by admission anyway.
- Proposed change: (1) treat `Hold::Ahead` for encoded renditions like copy — `Step::Stop` (SIGSTOP), with a release only when a *different* rendition is actually queued in `Admissions` (`vodencode.rs` already exposes `is_waiting`); (2) add a low-water mark to `stop()`/resume (e.g. resume at `furthest + horizon/2`) so a running producer works in ≥90 s bursts; (3) cap preroll to what the decoder needs (`-ss` to the previous IDR is enough for video; audio preroll of 2 AAC frames, not 2 s). Jellyfin keeps one ffmpeg per session for the whole session and throttles with `-readrate`; plurx already has the `Pacing` machinery for that.
- Size: S for (1)/(2), M for (3) · Risk: med (touches the scheduler contract; `prodsched` is pure and well-tested, so add the cases there first)

#### F-stream-7: Text-subtitle burn sessions spawn `fc-list` and `fc-conflist` on every producer launch **and every materialised segment**
- Severity: P1 · Category: performance · Confidence: CONFIRMED
- Evidence: `vodserve.rs:7483` `if !recipe_engine_is_current(&self.rendition.recipe).await` inside `materialize()` (called per segment); `vodserve.rs:6741` same check in `spawn_generation`; `recipe_engine_is_current` → `encoding.engine.is_current()` (`ffmpeg.rs:1612-1621`) → `font_render_engine_inner().await` (`ffmpeg.rs:1806-1850`) which runs `Command::new("fc-list")` and `Command::new("fc-conflist")` and then `std::fs::metadata` on every font file (`engine_path_version`, `ffmpeg.rs:2041-2046`, synchronous, on the runtime). Nothing memoises the font closure ("Probe it for every recipe capture", `ffmpeg.rs:1585`).
- Why it matters: a producer running 3× realtime materialises a 2 s segment every ~0.7 s; two process spawns plus thousands of stats per segment is easily 100–500 ms on a Debian box with `fonts-noto`, i.e. it can *become* the producer's bottleneck and it competes with the encode for CPU. Combined with F-6 it also runs on every respawn. All of it sits in `async fn`s on tokio workers (the stats block the runtime; the process spawns are async but their cost is real).
- Proposed change: attest the font closure once per recipe capture (already done) and re-check at most on a timer (e.g. 60 s, or inotify on the fontconfig dirs), never per segment; move `engine_objects_are_current` stats to `spawn_blocking` or cache them with a 1 s TTL. The library closure check per segment (150–250 `stat`s) can also drop to per-launch.
- Size: S · Risk: low

#### F-stream-8: Media bodies are streamed in 4 KiB chunks through a blocking-pool hop, an mpsc(1) and a oneshot ack per chunk
- Severity: P2 · Category: performance · Confidence: CONFIRMED
- Evidence: `http/hls.rs:13402` `ReaderStream::new(tokio::io::AsyncReadExt::take(ready.file, len))` and `:14012` (tokio-util 0.7.18 `DEFAULT_CAPACITY = 4096`, verified in the vendored source); the pump at `hls.rs:13404-13460` sends each chunk over `mpsc::channel(LOCAL_MEDIA_BODY_CHANNEL_CAPACITY = 1)` (`:9900`) and waits for a `oneshot` ack per chunk (`:9902-9905`, `:10006`). Direct play is worse: `http/stream.rs:2940` and `:2957` `ReaderStream::new(fh…)` with no pump at all but the same 4 KiB reads. `internal_media.rs:116` and `offline.rs:1192` already use `with_capacity(256 * 1024)`, so the fix pattern exists.
- Why it matters: `tokio::fs` reads are `spawn_blocking` round trips. An 80 Mb/s UHD direct play = 10 MB/s = ~2,500 blocking-pool hops per second per viewer; a 5 MB segment = 1,280 iterations of read + channel send + ack + two `Instant::now()` + a fresh `sleep_until` future. tower-http `ServeFile` uses 64 KiB; nginx `sendfile`. This is the "goal state" path (ARCHITECTURE §3) and it is the most expensive way to serve a file the daemon has.
- Proposed change: `ReaderStream::with_capacity(_, 256 * 1024)` in all four sites (S); consider ack-per-N-chunks or a `bytes`-budget in the pump rather than per chunk. Longer term: `tokio_uring`/`sendfile` is not worth it here; 256 KiB chunks recover >90 % of the cost.
- Size: S · Risk: low

#### F-stream-9: Every encoded/burn session create runs a full `ffprobe -show_format -show_streams -show_chapters` on the source, plus a second decode-facts probe, before ffmpeg can start
- Severity: P2 · Category: performance (TTFF) · Confidence: CONFIRMED
- Evidence: `transcode.rs:18227` `held_source_probe_json(&source.handle)` → `ffmpeg.rs:358` → args at `ffmpeg.rs:603-609`; compared with the stored probe at `:18237`. Then `resolve_held_movie_plan` → `decode_facts.get_or_probe` (`decode_facts.rs:2905`) which is LRU-cached in memory only (cold after every restart) and gated by `probe_gate` + `offset_gate` semaphores. Then `EncodedEngine::capture` (ldd closure + `fc-list` for burns, `ffmpeg.rs:1570-1606`), `ensure_burn_file` (`subtitles.rs:979`), then spawn with a 2 s preroll (`vod.rs:140`), then the client's init + first-segment fetch. Rolling path budget constants show the scale: `ACTOR_HARDWARE_STARTUP_BUDGET = 12 s`, `ACTOR_SOFTWARE_STARTUP_BUDGET = 30 s`, `PLAYLIST_WAIT_BUDGET = 55 s` (`transcode.rs:292-345`).
- Why it matters: on NAS media a cold MKV probe is 0.3–2 s (ffprobe seeks to Cues/SeekHead and reads the tail for duration); the drift guard costs that on every play/seek-to-new-session. The comparison exists to detect a replaced file; the fence already holds the inode and compares dev/ino/size/mtime/ctime (`fragment_index_cluster.rs:177-210`).
- Proposed change: skip the ffprobe re-run when the held fence's object version (dev:ino:size:mtime_ns:ctime_ns) equals the one recorded at scan; keep the full comparison only when it differs (rare) or the stored probe came from a different ffprobe build. Persist decode-facts across restarts keyed by that same object version (SQLite, not raft). Expected saving: 0.3–2 s per session create, more on cold NFS.
- Size: S · Risk: low

#### F-stream-10: NVENC arguments are unmeasured defaults: Main profile, no B-frames/AQ/lookahead, decode never zero-copy; VideoToolbox similar
- Severity: P2 · Category: video-quality / performance · Confidence: LIKELY (arg builder read; `h264_nvenc` default `-profile main` is from `ffmpeg -h encoder=h264_nvenc`, not re-verified on a box here)
- Evidence: `encoder.rs:436-444` `-c:v h264_nvenc -preset p4 -b:v -maxrate -bufsize` only; `pipeline.rs:244-270` — there is no CUDA pipeline (`scale_cuda`/`tonemap_cuda`), so an NVENC node decodes with `-hwaccel cuda` (`mod.rs:1461`) **without** `-hwaccel_output_format cuda`, downloads every frame, scales with swscale, and re-uploads inside `h264_nvenc`. `Pipeline::keeps_frames_off_the_cpu` (`pipeline.rs:431-433`) is honest that only VppQsv/TonemapVaapi are zero-copy. VideoToolbox: `-q:v` is Apple-Silicon-only (guarded by the startup probe, good), no `-allow_sw 0`, no `-realtime`, decode→`hwdownload`→CPU scale (`mod.rs:1458-1460`).
- Why it matters: NVENC in `main` profile disables 8×8 transform; without `-bf 3 -b_ref_mode middle -spatial-aq 1 -temporal-aq 1 -rc-lookahead 20` NVENC is far below its known quality at a given bitrate (Jellyfin's `GetNvencArgs` sets all of these). A 4K HDR → 1080p on an NVIDIA box is CPU-bound in the tone-map and the scale, which is the exact 0.71× case the QSV work fixed.
- Proposed change: `-profile:v high -bf 3 -b_ref_mode middle -spatial-aq 1 -temporal-aq 1 -rc-lookahead 20 -tune hq` (all accepted by every driver ≥ 470); add `Pipeline::Cuda` = `-hwaccel cuda -hwaccel_output_format cuda` + `scale_cuda=w:h:format=nv12` (+ `tonemap_cuda` on jellyfin-ffmpeg) behind the same boot probe the other graphs use. Same pattern for VideoToolbox: `-hwaccel_output_format videotoolbox_vld` + `scale_vt` on macOS 13+.
- Size: M · Risk: low (probe-gated like VppQsv)

#### F-stream-11: Rolling-HLS publication clock does a `read_dir` + `stat` of every file in the session directory and re-parses the whole playlist every 250 ms
- Severity: P2 · Category: performance · Confidence: CONFIRMED
- Evidence: `transcode.rs:5939` `ROLLING_PUBLICATION_POLL = 250 ms`; `publication_cycle` (`:6625-6660`) starts with `self.refresh_scratch_bytes().await` (`:6566-6591`, `read_dir` + `entry.metadata()` for every file) then `read_bounded_playlist` + `parse_playlist` of the full EVENT playlist; `refresh_segments` (`:7773-7842`) does the read again plus a `metadata` per advertised segment with unknown size.
- Why it matters: with `RETENTION_SECS = 180` (`:368`) ≈ 90–100 files per session → ~400 `tokio::fs` stats/s per rolling session (each a blocking-pool hop), 4×/s playlist parse of up to 3,600 lines for a 2 h film; on NFS-backed scratch the stats are RPCs. The byte total is already known per segment from the index; the walk re-derives it.
- Proposed change: maintain `live_bytes` incrementally from the segment index deltas and retention deletions; walk the directory only on attempt start and after a rewrite. Poll ffmpeg's playlist with inotify or on the `-progress` `out_time` tick (already parsed) instead of a flat 250 ms.
- Size: S · Risk: low

#### F-stream-12: `is_progress_line` drifted between the transcode and progressive paths — the loose copy swallows ffmpeg error lines
- Severity: P2 · Category: stability / architecture (duplication) · Confidence: CONFIRMED
- Evidence: `transcode.rs:2392-2410` matches the closed key set with the comment "an error message about `filter_units=remove_types=32-34` contains plenty of those, and swallowing it would hide exactly the failure a copy session is most likely to have"; `http/stream.rs:3409-3420` (progressive remux, used at `:3297`) is `key.chars().all(|c| lowercase || digit || '_')` — precisely the version the other file calls out as wrong. Same family of drift: frame-rate parsing exists three times (`decode.rs:1016` filters `attached_pic`; `hls.rs:10142` takes the first `codec_type=video` stream, so an MKV with an MJPEG cover art reports the cover's `FRAME-RATE`; `vodencode.rs:236`).
- Why it matters: a progressive remux that fails with a bsf error logs nothing useful; the master playlist can advertise a wrong `FRAME-RATE`.
- Proposed change: one `plurx_core::ffmpeg_io::{is_progress_line, parse_frame_rate}` used by all callers.
- Size: S · Risk: low

#### F-stream-13: The VOD producer spawn bypasses `configure_ffmpeg_runtime`/`spawn_job_owned`, so the fontconfig-cache fix and the Windows job object do not apply to the primary path
- Severity: P2 · Category: stability (drift) · Confidence: CONFIRMED
- Evidence: `transcode.rs:2362-2383` `configure_ffmpeg_runtime` sets `XDG_CACHE_HOME` because "fontconfig has no user cache while libass initializes a text-subtitle burn. A transcode can then spend the entire producer startup budget rebuilding font metadata", plus `AV_LOG_FORCE_NOCOLOR`; both spawn paths in transcode.rs call it (`:2164`, `:2300`) and use `spawn_job_owned`. `vodserve.rs:6778-6794` builds `tokio::process::Command::new(recipe_program(recipe))` and calls `.spawn()` directly — `rg 'configure_ffmpeg_runtime|spawn_job_owned' crates/plurxd/src/vodserve.rs` → no matches. The VOD path also uses the 8 KiB `drain_diagnostics` ring (`ffmpeg.rs:123-136`) instead of the `decoder_health` grammar/fault sink the rolling path has, so decode-fault classification differs by path.
- Why it matters: the encoded VOD path is where subtitle burns now run (VOD-CUTOVER table), so the documented failure ("player with no HLS resource") is live again for the Docker-UID case; on Windows a killed daemon can strand a VOD ffmpeg.
- Proposed change: one `producer::spawn(command, ProducerIo)` used by all three spawn sites (see §4), which owns env, job, stderr reader and progress parsing.
- Size: S · Risk: low

#### F-stream-14: Master playlist advertises the *source* BANDWIDTH/RESOLUTION for transcoded variants and omits CODECS for SDR variants
- Severity: P2 · Category: video-quality (client behaviour) · Confidence: CONFIRMED
- Evidence: `http/hls.rs:12413-12421` `let bandwidth = file.bitrate.unwrap_or(25_000_000)…; RESOLUTION={file.width}x{file.height}` regardless of `SessionKind::Transcode { height }`; `CODECS` only inside `if let Some(video_range)` (`:12446-12461`), i.e. only HEVC/HDR.
- Why it matters: RFC 8216 §4.3.4.2 BANDWIDTH "MUST be the peak segment bit rate of the Variant Stream"; AVPlayer applies `preferredPeakBitRate`/cellular caps to the advertised number, so an 8 Mb/s 1080p transcode of an 80 Mb/s UHD source can be refused or throttled as an "80 Mb/s" stream; ExoPlayer uses it for initial buffer sizing. Apple's authoring spec requires CODECS on every variant; without it AVPlayer sniffs the first segment (slower start, and it will not pre-select an audio decoder).
- Proposed change: for transcode sessions emit `BANDWIDTH = 1.5×video + audio` from the recipe (`maxrate` is already computed), `RESOLUTION` from `output_size`, `CODECS="avc1.640028,mp4a.40.2"` (profile/level from the init for VOD; static for the rolling path). The `HlsContext.codecs` plumbing exists.
- Size: S · Risk: low

#### F-stream-15: `transcode.rs`/`hls.rs`/`vodserve.rs` are three files that each hold half of the same lifecycle; 10 "close … races" fixes in 4 weeks is the cost
- Severity: P2 · Category: architecture · Confidence: CONFIRMED (structure) / LIKELY (causal)
- Evidence: `transcode.rs` 47,096 lines, 1,011 fns, 142 types, one `impl TranscodeManager` spanning `12942–26776` (13.8 k lines); tests from line 27574 (41 % of the file). `hls.rs` 28,507 lines (14,194 production); `vodserve.rs` 15,011 (8,092). Per-segment serving (`hls.rs:13571-14183`) calls into `TranscodeManager::authorize_response_publication`/`commit_authorized_media` (`transcode.rs:23957-24050`, three `self.sessions.lock()` acquisitions per commit) which delegates to `VodServe` for VOD ids — the same request crosses three files and two session registries. Git: `transcode.rs` 329 commits / 154 `fix`, `hls.rs` 225 / 105, `vodserve.rs` 122 / 67 since 2026-08-20; subjects include "close producer event race windows", "make producer supervision race free", "close takeover publication races", "close terminal settlement races", "settle prepared lifecycle races", "exempting the demand hold alone left the deadlock wearing another name".
- Why it matters: every fix in this month touched the same three files; the module boundary is "which file the author was in", not "which state machine". Duplicated helpers have already drifted (F-12, F-13). Compile-and-test iteration on a 2 MB file is slow, and `rustfmt`/IDE tooling degrade.
- Proposed change: see §4 below (named modules and what moves). Do it as file moves with `pub(crate)` re-exports first (no behaviour change), then collapse the duplicate registries.
- Size: L · Risk: med (mechanical moves are low risk; unifying the two session registries is the medium-risk part and should be a separate step)

#### F-stream-16 (bundle, P3): small argument/serving items
- Rolling path has no `-g`/`-keyint_min`/`-sc_threshold 0` (`mod.rs:1651-1653` only `-force_key_frames`), so x264 scene-cut IDRs shift cut points off the 2 s grid the cluster failover story (`ARCHITECTURE §2.3`) assumes. VOD has them (`vod.rs:276-281`). Add `-sc_threshold 0` (or `-x264-params scenecut=0`) to the rolling path.
- Rolling path packages HEVC Main10 (HDR10 rung) into MPEG-TS (`-hls_segment_type mpegts`, `mod.rs:1696-1697`); Apple's HLS authoring spec wants HEVC in fMP4. The VOD path is fMP4, so this only bites when the rolling fallback serves an `hdr10` request — switch the rolling path to `-hls_segment_type fmp4` for the HEVC grade.
- `tonemap_opencl=tonemap=hable` (`pipeline.rs:367`) → `bt2390` on jellyfin-ffmpeg, matching `libplacebo`/`tonemapx`.
- Direct play responses (`stream.rs:2941-2968`) carry no `ETag`/`Last-Modified`/`Cache-Control`; ARCHITECTURE §3 promises "correct caching headers". Add `ETag: "{size}-{mtime}"`, `Last-Modified`, `Cache-Control: private, max-age=…`, and honour `If-Range`.
- `SourceFence::drift()` (`fragment_index_cluster.rs:196-210`) is a synchronous `fstat` on the per-segment hot path (`vodserve.rs:5195`, `:7461`); cheap on local disk, an RPC on `noac` NFS. Move it to `spawn_blocking` or rate-limit to once per second.
- VOD segments are assembled as whole `Vec<u8>` up to the 64 MiB cut cap (`materialize(&self, entry, bytes: Vec<u8>)`, `vodserve.rs:7450`); eight concurrent 4K copy sessions can hold ~0.5 GB transient. Consider writing through to the temp file as the cutter emits `moof/mdat`.

---

### Question 2 summary — time to first frame (encoded VOD)

Before the first segment can exist: session-request claim → `open_source_fence` (open + fstat) → `get_file_probe_json` (store) → **ffprobe #1** (`held_source_probe_json`) + `reporter_identity_of` (cached) → `compare_probe_documents` → `frame_grid` → `encoder_and_grade_for` (may probe DoVi reshape availability) → `ensure_burn_file` (one-off full-file extract for text subs, cached) → `resolve_held_movie_plan` → **ffprobe #2** (`decode_facts`, in-memory LRU) → `EncodedEngine::capture` (ldd closure stats; `fc-list`+`fc-conflist` for text burns) → recipe hash → rendition dir create → `Admissions.try_permit` (store read of two settings) → `spawn_generation` (`recipe_engine_is_current` again) → ffmpeg with `-ss target−2` → 2 s of discarded preroll decode → init verified (`establish_or_verify`) → first 2 s segment materialised → wait-pool wake → response. Hardware decode↔encode is zero-copy only on `VppQsv`/`TonemapVaapi` and only for `heavy_source()` (HEVC ∧ (HDR ∨ ≥2160p)); everything else bounces through system memory by design (`pipeline.rs:431-437`, `mod.rs:1222-1241`). GPU concurrency: `DEFAULT_MAX_HW_SESSIONS = 2` (`admission.rs:41`), software budget in threads, both replicated settings. Segment durations: 2 s transcode (`SEGMENT_SECONDS`), copy cuts at 6 s target / 15 s max / 64 MiB / 2 s first segment with a 12 s publish gate (`mod.rs:72-254`). Playlists: rolling = EVENT (full history, `RETENTION_SECS = 180` behind the download frontier), VOD = full VOD playlist. Cache headers: VOD segments `ETag` + `private, max-age=3600, immutable` (good); direct play none (F-16).

### Question 3 summary — stability

Process lifecycle is the strongest part of this area: children are `kill_on_drop`, owned by one supervisor task with a `biased` `select!` over `Child::wait` so a pid is never signalled after reap (`transcode.rs:5393-5560`, `signal_owned_pid` `:5828`), stdout/stderr readers are owned tasks (not detached) with a bounded ring (`ffmpeg.rs:123-136`) or the grammar reader; SIGSTOP/SIGCONT pacing is fenced through the same supervisor; disk pressure uses `statvfs` with a 512 MiB emergency margin (`transcode.rs:12432-12455`), per-session/global ahead-byte budgets with hysteresis, and eviction never touches a reader window. Watchdogs: 12 s hardware / 30 s software startup budgets, `PROGRESS_STALL = 10 s` with a motion clock that ignores flow-control suspends. Weak points found: F-6 (respawn churn is itself a stability risk — every respawn re-enters the race-prone attach/close window the comments at `vodserve.rs:6820-6835` describe), F-7 (per-segment process spawns), F-13 (VOD spawn outside the hardened spawner), and the sheer number of lifecycle races fixed this month (F-15). No unbounded queues in the hot path (`unbounded_channel` only for supervisor commands, `transcode.rs:5375`, which is bounded by callers); retries are budgeted (`PREPUBLICATION_REAP_RETRY = 5 s`, `QUEUE_WAIT = 5 s`).

### Question 4 — decomposition

`transcode.rs` (47 k) → `plurxd::media::…`:
- `producer/spawn.rs` — `spawn_ffmpeg`, `spawn_ffmpeg_pipe`, `FfmpegDescriptors`, `WindowsPathHandoff`, `FfmpegProgressObserver`, `DiagnosticObservation`, `configure_ffmpeg_runtime`, `is_progress_line` (lines ~1755–2410). Make `vodserve::spawn_generation` and `stream.rs` progressive use it (fixes F-12/F-13).
- `producer/attempt_child.rs` — `AttemptChild`, `AttemptChildTerminal/Command`, `signal_owned_pid` (5287–5843).
- `rolling/segment_index.rs` — `SegmentVisibility`, `SegmentMeta`, `SegmentIndex`, `ReadyCoverage`, `parse_playlist` (723–1330).
- `rolling/flow.rs` — `Ahead*`, `FlowEvaluation`, `FlowInputs`, `SuspendedAt`, `RollingScratchReservation`, `apply_ahead_window` (1331–1755).
- `rolling/prepublication.rs` — `Prepublication*Retry`, `PrepublicationStartSettlement`, `OfflineRecoveryState` (2457–3050).
- `rolling/retirement.rs` — `RollingRetirement*`, `RetiredPresentation`, published-failure cleanup (3047–4540).
- `rolling/publication.rs` — `RollingPublicationClock`, `ServedPlaylistSnapshot`, `RollingTerminal*`, `publication_cycle` (5844–6020 + Session methods).
- `rolling/session.rs` — `Session` + impl (6019–8426).
- `delivery.rs` — `SegmentFile`, `MediaResponseOwner/Publication/Authorization`, `SegmentDelivery`, `authorize_response_publication`, `commit_authorized_media` (8426–9110, 23400–24100).
- `session_request.rs` — `SessionRequest`, `SessionKind`, `Presentation`, `StartInfo`, `SessionInfo`, `HlsSessionInfo`, `DeliveryCandidate` (9540–10410).
- `cluster/adoption.rs` — `ClusterSessionStart`, `SessionAdoptionToken`, `SessionReleaseGate*`, `ClusterReplacementGuard` (9581–9745).
- `requests.rs` — `RequestEntry/RequestClaim/Claimed` singleflight (10409–10560).
- `pretranscode.rs`, `offline.rs` — `PretranscodeFence`, `BoundPretranscodeSource`, `PortableProduction`, `ResumedParts`, `ValidatedPart`, `ArtifactQualificationReadiness`, `GenerationObservation` (10563–12380).
- `cache/offer.rs` — `CachedLocationIdentity`, `CacheOfferVerdict/Verification`, `verify_cache_offer_location*`, `PreparedSharedCacheRead`.
- `rate_control.rs` — `RateControlSnapshot`, `normalize_rate_control_request`, probes (12485–12565 + manager methods).
- `manager.rs` — what remains of `TranscodeManager`: construction, `encoder_and_grade_for`, `options_for_*`, `create_session`, `playlist`, `segment_for_publication`.
- `tests/` — move the 20 k lines of `mod tests` and `pretranscode_renewal_tests` into `transcode/tests/*.rs` via `#[cfg(test)] #[path]`.

`http/hls.rs` (28.5 k) → `http/hls/{create.rs (CreateSession, create, plan_review_for, admit_restart, activation replay 566–4050), release.rs (4051–4610), status.rs, control.rs (control_inner 5870–6700), preparation.rs (prepared successor / quality switch 4798–9350), relay.rs, playlist.rs (master_playlist*, video_playlist, exact_hls_context 10167–10600, 11509–12500), subtitles.rs (subtitle_playlist/vtt, slice_webvtt, timeline 10598–11470, 12501–12680 — this belongs beside plurxd::subtitles), segment.rs (segment, vod_segment_response*, driven_local_body, range/etag 9589–10065, 12683–14183)}`.

`vodserve.rs` (15 k) → `vod/{discovery.rs (Discover, markers, reconcile 161–300, 5236–5310), session.rs (Session, Reader, ResponseOwner, preparation guards 500–2060), marker_prewarm.rs (~950–1300, 6532–6735, 7657–7770), admission.rs (try_admit, purge_if_dormant, working set 5968–6165), driver.rs (spawn_driver, driver_pass, spawn_generation, run_generation, establish_or_verify, on_generation_end 6166–7400), serve.rs (serve_init/segment, blocked_wait, open_materialized 4953–5235), control.rs (4151–4630), init.rs (regenerate_init_head, read_muxer_init* 7846–8090)}`.

Duplicated/drifted logic between transcode, VOD and DV paths: ffmpeg spawn (3 implementations, F-13); progress-line classification (2, F-12); frame-rate parsing (3, F-12); segment naming/`is_safe_segment`/etag formats (rolling `rolling_etag` vs VOD `key-index-at_ms`); two session registries (`TranscodeManager.sessions` and `VodServe.shared.sessions`) with parallel `response_owner_is_live`, `promote_prepared_session`, `record_desired_persisted`, `segment_window` on both (`rg` list in my notes); DV conversion is correctly a post-mux rewrite in one place (`dvpipe.rs`/`plurx_core::transcode::dvconvert`), no drift found there.

---

### Already good (do not undo)
1. `-maxrate 1.5×/-bufsize 2×` on every family, with the measured rationale (`encoder.rs:328-346`).
2. `-forced_idr`/`-forced-idr` for QSV/NVENC so `-force_key_frames` produces cuttable IDRs (`encoder.rs:301-326`) — a real, measured 10 s→2 s startup fix.
3. `-muxdelay 0 -muxpreload 0` and the WebVTT `X-TIMESTAMP-MAP` reasoning (`mod.rs:1668-1683`); `independent_segments+temp_file`.
4. `OutputGrade` typing + `assert_no_pq_at_8_bit` (`mod.rs:1133-1159`) making the SIGABRT pairing unspellable; explicit `zscale tin/pin/min` for metadata-stripped hardware frames.
5. Copy path bitstream hygiene: `hvc1`/`dvh1` tagging, `dovi_rpu=strip`, `filter_units`, parameter-set promotion, `-map_chapters -1`, `-noaccurate_seek` + `avoid_negative_ts make_zero` (`mod.rs:1787-1878`, `1952-2061`).
6. 6-channel AAC forced to `channel_layout 5.1` at 320 k (`mod.rs:2037-2052`) — the AVPlayer −12848 fix; keep.
7. Process supervision: owned readers, biased reap-before-signal select, `kill_on_drop`, Windows job objects, bounded stderr (`transcode.rs:2150-2278`, `5333-5560`; `process_control.rs`).
8. VOD frame-grid design: rational output cadence, `-enc_time_base`, film-global AAC lattice, `establish_or_verify` physical checks; no per-segment fsync, one `make_durable` at admission (`renditiondir.rs:466-510`).
9. Blocked-GET wait pool with admission caps and deadlines, manifest-locked open (`vodserve.rs:5045-5225`); scheduler is a pure function with exhaustive tests (`prodsched.rs`).
10. `-readrate`/`-readrate_initial_burst` pacing with build-probed capability, SIGSTOP flow control, statvfs emergency margin, working-set hysteresis.
11. Fragment index keyed by executable+dependency closure digest (prevents cross-build cache reuse); `hls_time` floor vs copy segment cap reasoning (`mod.rs:60-254`).

### Hot spots (fix commits since 2026-08-20)
| file | commits | `fix` |
|---|---|---|
| `crates/plurxd/src/transcode.rs` | 329 | 154 |
| `crates/plurxd/src/http/hls.rs` | 225 | 105 |
| `crates/plurxd/src/vodserve.rs` | 122 | 67 |
| `crates/plurx-core/src/transcode/` | 65 | 21 |
| `crates/plurxd/src/http/stream.rs` | 45 | 19 |
| `crates/plurxd/src/decode_facts.rs` | 36 | 18 |
| `crates/plurxd/src/ffmpeg.rs` | 32 | 18 |
| `crates/plurx-core/src/fmp4.rs` | 34 | 17 |
| `crates/plurxd/src/copyseg.rs` | 22 | 13 |
| `crates/plurxd/src/dv_disk.rs` | 13 | 8 |
Within the big three, the fix subjects cluster on session lifecycle races (10 commits with "race", plus "deadlock", "hang", "stall"), not on encoding. `prodsched.rs`/`prodexec.rs`/`process_control.rs`/`renditiondir.rs` are quiet (≤3 fixes) — the pure/small modules are stable; the giant ones are not.

### Open questions I could not settle from code
1. Production frequency of `"spawned a producer generation"` per encoded session (F-6): the code says every control beat in steady state; a `journalctl | grep -c` on media1 over one film would confirm.
2. Whether media1's QSV/VAAPI `hwdownload` actually strips MDCV/CLL side data and `color_trc` (F-3). `ffmpeg -hwaccel qsv … -vf hwdownload,format=p010,showinfo` on an HDR10 fixture answers it in one run.
3. Whether AVPlayer refuses/throttles the over-advertised BANDWIDTH on cellular/`preferredPeakBitRate` (F-14) — needs an iPhone on LTE.
4. Whether the encoded VOD `-bf 0` decision (F-2) was forced by a client bug with negative CTS/`ctts` v1 or only by the plan arithmetic; the commit message (`44abf337`) does not say.
5. Real TTFF split on media1 (probe vs attest vs spawn vs first segment) for a cold NFS 4K title — `PLAYBACK-TESTING.md` has no such breakdown; the budgets (12/30/55 s) were sized by symptom.
6. Whether any deployed node is NVENC/VideoToolbox in production; if not, F-10 is a correctness-when-it-happens item rather than a live regression.

---

## plurx architecture review — server-core (2026-09-20)

Scope: `crates/plurxd/src/{main.rs, state.rs, http/{mod,system,cluster_operations,extract,images,auth,browse,plex,stream}.rs, media_sessions.rs, playback_control.rs, media_pool.rs, admission.rs, job_lease.rs, schedule.rs, progress.rs, watched.rs, telemetry.rs, meter.rs, logbuf.rs, manifest_cache.rs, titlestore.rs, waitpool.rs, serving_fence.rs, delivery.rs, playstart.rs, subtitles.rs, offline.rs, trakt.rs}`, `crates/plurx-core/src/{auth,config,fs_secure,secrets}.rs`, `store/{sqlite,hiqlite*}.rs`, `scan/`, `metadata/`, `crates/plurx-compat-plex`, `docs/SECURITY.md`, `docs/API.md`, plus the vendored `hiqlite` client and the `hyper`/`axum`/`tokio`/`tokio-util` versions pinned in `Cargo.lock` (axum 0.8.9, hyper 1.10.1, hyper-util 0.1.20, tokio 1.53.1, tokio-util 0.7.18).

Read-only; nothing compiled. Every claim below has a file:line.

---

### Executive summary

The server core is unusually careful about the things that are hard to retrofit: every Store call is async behind `spawn_blocking` (SQLite) or a Raft client (Hiqlite); no `std::sync` guard is held across an `.await` anywhere in scope (heuristic scan over ~30 files plus manual reading of the hot paths found zero); every per-session/per-user in-memory map is TTL-pruned or capped; the auth model is a pair of extractors that the router applies uniformly (the route table at the end confirms every admin route is `AdminUser`); file serving is `openat`+`O_NOFOLLOW` based; range parsing is overflow-safe; browse endpoints are paginated and batched (no N+1 in `/api/v1`).

The problems are in the *defaults* and the *plumbing*, not in the correctness engineering:

1. **Direct play and HLS segment bodies are streamed in 4 KiB chunks**, each chunk a `spawn_blocking` hop (F-core-1). This is the “goal state” path of the whole architecture and it is doing ~2,500 blocking-pool round trips per second per 80 Mbps viewer.
2. **The HTTP listener has no timeouts at all** — hyper's 30 s header-read default is silently disabled because `axum::serve` sets no timer, and there is no request, idle or concurrency limit layer (F-core-2).
3. **The library scanner walks the whole tree with synchronous `WalkDir`/`std::fs::metadata` on a tokio worker thread** (F-core-3). On NFS/SMB this pins a runtime worker for the duration of the walk.
4. **Every authenticated request is a leader-consistent Raft read over a WebSocket**, and, because `bounded_replica_reads` defaults to `false`, so is every catalogue read (F-core-4). The team's own CLUSTER-PERFORMANCE-PLAN names this; the default has not moved.
5. **The Plex façade `section_all` is an N+1 over up to 5,000 items with no pagination** (F-core-5) — the one real N+1 in the server, and it sits on the Kodi path.
6. TMDB/AniList clients have **no HTTP timeout** (F-core-6); image serving **SHA-256s the whole file on the async worker per request and 503s local hits past 8 in flight** (F-core-7); **logout is a cluster-wide two-phase operation behind a process-wide `try_lock`** (F-core-8); **login has no lockout and tokens never expire** (F-core-9).
7. `playback_control.rs` at 30 k lines (14.5 k non-test, 100 structs, 66 enums, 82 `fix:` commits in a month) is the single biggest stability signal in the repo (F-core-10).

---

### Findings

#### F-core-1: Direct-play and HLS-segment bodies stream in 4 KiB chunks, one `spawn_blocking` per chunk
- Severity: **P1** · Category: performance · Confidence: **CONFIRMED**
- Evidence:
  - `crates/plurxd/src/http/stream.rs:2940` — `let stream = tokio_util::io::ReaderStream::new(fh.take(count));` and `:2957` — `Body::from_stream(tokio_util::io::ReaderStream::new(fh))` (direct play, download, book content, and the Plex `/library/parts` route all go through `serve_file_range`).
  - `crates/plurxd/src/http/hls.rs:13402` — `let reader = tokio_util::io::ReaderStream::new(tokio::io::AsyncReadExt::take(ready.file, len));` and `:14012`; the chunks are then handed through `tokio::sync::mpsc::channel::<DrivenLocalChunk>(LOCAL_MEDIA_BODY_CHANNEL_CAPACITY)` with `hls.rs:9900 const LOCAL_MEDIA_BODY_CHANNEL_CAPACITY: usize = 1;`.
  - `tokio-util-0.7.18/src/io/reader_stream.rs:8` — `const DEFAULT_CAPACITY: usize = 4096;`.
  - `tokio-1.53.1/src/fs/file.rs:620` — `let max_buf_size = cmp::min(dst.remaining(), me.max_buf_size);` → each `poll_read` reads at most the destination's remaining capacity (4096) and does so via `spawn_blocking_read` (`file.rs:1003`, the non-io-uring fallback that this build uses).
  - The team already knows the right idiom: `crates/plurxd/src/http/internal_media.rs:116` — `ReaderStream::with_capacity(file, 256 * 1024)` and `crates/plurxd/src/http/offline.rs:33/1192` — `TRANSFER_STREAM_BUFFER = 256 * 1024`.
- Why it matters: an 80 Mbps direct play = 10 MB/s ÷ 4 KiB = **~2,440 blocking-pool hops and ~2,440 HTTP body frames per second per viewer**; a 4 MB HLS segment is ~1,000 read→channel(cap 1)→hyper handoffs. Each hop is a thread wake-up plus a `read(2)` of 4 KiB — on NFS/SMB that defeats client-side readahead heuristics and shows up as CPU and as jittery throughput on the one path the architecture calls "the goal state". Four seeks in a row on Apple TV = four fresh 4 KiB pipelines racing.
- Proposed change: one shared helper (`plurxd::delivery::file_body(file, len)`) that uses `ReaderStream::with_capacity(file, 256 * 1024)` (the value `internal_media.rs` and `offline.rs` already picked), and for the segment pump either raise `LOCAL_MEDIA_BODY_CHANNEL_CAPACITY` to 2–4 or drop the per-chunk channel in favour of `Body::from_stream` on the bounded reader. Reference: Jellyfin serves files with 64 KiB–1 MiB buffers; nginx/Caddy default 32–64 KiB `sendfile`/`output_buffers`; tokio's own `File::set_max_buf_size` docs recommend large buffers for sequential reads. Also consider `File::set_max_buf_size` + `tokio_uring`-free `io_uring` later, but the buffer size alone is the 100× win.
- Size: S · Risk: low (throughput meter `meter.rs` will show the before/after).

#### F-core-2: The public listener has no header-read, request, idle or concurrency limits; hyper's 30 s default is silently disabled
- Severity: **P1** · Category: stability · Confidence: **CONFIRMED**
- Evidence:
  - `crates/plurxd/src/main.rs:2418-2430` — `axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(...)`. No builder configuration is possible through this API.
  - `axum-0.8.9/src/serve/mod.rs:391` — `let mut builder = Builder::new(TokioExecutor::new());` — no `.timer(TokioTimer::new())`, no `.http1().header_read_timeout(...)`, no `.http2().keep_alive_interval(...)`.
  - `hyper-1.10.1/src/server/conn/http1.rs:249` — `h1_header_read_timeout: Dur::Default(Some(Duration::from_secs(30)))` and `hyper-1.10.1/src/common/time.rs:72-76` — `Dur::Default(Some(dur)) => match self { Time::Empty => { warn!("timeout `{}` has default, but no timer set", name); None }`. So the one timeout hyper ships is turned off by the absence of a timer.
  - `crates/plurxd/src/http/mod.rs:629-643` — the only global layers are `TraceLayer` and `cluster_capacity_gate`. `rg 'TimeoutLayer|ConcurrencyLimitLayer|LoadShed|CompressionLayer|RequestIdLayer' crates/` → no matches.
  - `docs/SECURITY.md:569-573` hands "HTTPS and request rate limiting" to a reverse proxy, but the shipped `deploy/` compose runs the daemon bare on the LAN and the SECURITY doc explicitly says LAN exposure without a proxy is the normal deployment.
- Why it matters: a client that opens a TCP connection and never sends a request line (a sleeping TV, a half-open NAT mapping, a misbehaving smart-TV app) holds a hyper connection task **forever**. Each is a tokio task plus a socket buffer; there is no reaper. A slow-body POST to `/api/v1/files/{id}/hls/sessions` (64 KiB limit, no time limit) does the same. No layer bounds concurrent in-flight requests, so a seek storm from one misbehaving hls.js instance is bounded only by the per-route caps that individual subsystems built (waitpool, artwork permits, password permits) — good engineering that a global cap would make unnecessary to get right everywhere. Graceful shutdown bounds the drain (5 s) but not steady-state connections.
- Proposed change: replace `axum::serve` with the same `hyper_util::server::conn::auto::Builder` axum uses, but configured: `.timer(TokioTimer::new())`, `.http1().header_read_timeout(15s)`, `.http1().timer(...)`, `.http2().keep_alive_interval(20s).keep_alive_timeout(20s)`; add `tower_http::timeout::TimeoutLayer` per group (e.g. 30 s for `/api/v1` JSON routes, none for streaming routes — HLS blocking GETs already own their own deadline in `waitpool.rs`), and `tower::limit::ConcurrencyLimitLayer` (or `LoadShedLayer` + `GlobalConcurrencyLimitLayer`) around the JSON API. Reference: hyper's own `header_read_timeout` default (30 s) and Jellyfin/Kestrel `RequestHeadersTimeout` 30 s / `KeepAliveTimeout` 130 s.
- Size: S–M (axum's `serve` is ~100 lines to copy) · Risk: low if the streaming routes are excluded from the request timeout.

#### F-core-3: The library scanner runs a synchronous `WalkDir` and per-file `std::fs::metadata` on a tokio worker thread
- Severity: **P1** · Category: stability/performance · Confidence: **CONFIRMED**
- Evidence:
  - `crates/plurx-core/src/scan/mod.rs:494` — `for entry in WalkDir::new(root).follow_links(true) {` inside `pub async fn scan_library_with_progress...` (`:377`, `:394`, `:437`); comment at `:473` says "Collect candidate files first (cheap, synchronous)".
  - `scan/mod.rs:358-368` — `fn file_stat(path) { let meta = std::fs::metadata(path)?; ... }` called per candidate from the async `record_candidates` (`:947` area); `scan/nfo.rs:190` `std::fs::read(path)`, `scan/exif.rs:18` `std::fs::File::open(path)`, `scan/recordings.rs:87` `std::fs::read(sidecar_for(media))` — all synchronous, all reached from the async scan.
  - `crates/plurxd/src/state.rs:5272-5278` — `scan::scan_library_with_publication_and_prune_percent(&publisher, &library, Some(&progress), self.scan_prune_percent).await` — awaited directly on the runtime from a `tokio::spawn`ed job task (`state.rs:10926`, `:11823`), never `spawn_blocking`/`block_in_place`.
  - `rg 'spawn_blocking|block_in_place' crates/plurx-core/src/scan/*.rs` → no matches.
- Why it matters: `WalkDir` over a 30 k-file library on NFS is tens of thousands of `getdents`/`lstat` round trips (`follow_links(true)` adds a `stat` per entry), each 0.2–5 ms over the network and up to seconds if the NAS spun down. For the entire walk one runtime worker is unavailable to poll HLS playlist/segment futures; on a 4-core box that is 25 % of the runtime, and tokio's cooperative scheduler cannot pre-empt it. The docs (`ARCHITECTURE.md §2.3`) say "Scanner … leader-scheduled singletons, so three nodes don't … thrash shared storage" — the thrash concern is addressed, the *thread* concern is not. It is not visible today because scans are rare, which is exactly when it surprises someone (playback stalls "randomly" during a 3 a.m. scheduled scan).
- Proposed change: move the walk into `tokio::task::spawn_blocking` (or `tokio::fs::read_dir` with a bounded depth-first loop) and return the `Vec<PathBuf>`; wrap `file_stat`/`nfo::read`/`exif` in `spawn_blocking` or use `tokio::fs::metadata`. Better: batch the walk into pages of ~256 candidates so memory is bounded and progress is visible earlier. Reference: tokio docs "CPU-bound tasks and blocking code" — anything that can block > 10–100 µs belongs off the runtime; Jellyfin's `LibraryMonitor`/scan run on the threadpool, never on the request pool.
- Size: S · Risk: low.

#### F-core-4: Every authenticated request, and by default every catalogue read, is a leader-consistent Raft read over a WebSocket
- Severity: **P2** (P1 on a 3-node cluster) · Category: architecture/performance · Confidence: **CONFIRMED**
- Evidence:
  - `crates/plurxd/src/http/extract.rs:627-636` — `AuthUser::from_request_parts` → `state.store.user_for_token(&hash).await?` on every request that names `AuthUser`/`AdminUser` (122 of the 205 route registrations; see table).
  - `crates/plurx-core/src/store/hiqlite.rs:3980-3989` — `user_for_token` → `self.client().query_consistent_map::<TokenUserRow, _>(sql, params!(token_hash))`.
  - `vendor/hiqlite/src/client/query.rs:29-33` — `// A consistent query it not a write operation, but it needs to travel to the leader, and the raft needs to be blocked.` and `:36-45` — "This query is very expensive compared to the other ones. It needs network round-trips, pauses the raft and allocates a lot more memory … You should only use it, if you really need to." `query_remote` (`:286-291`) always goes through `tx_client_db` → the WebSocket stream manager (`client/stream.rs:526`, `:1645`), even when this node is the leader; server side `network/api.rs:783-789` → `query_consistent_local` → `query/mod.rs:29` `raft.ensure_linearizable().await?` (an openraft ReadIndex round) then a `spawn_blocking` SQLite read.
  - Call-site census (`rg -c query_consistent` over `store/hiqlite*.rs`): **225 consistent** vs **18 local** query sites; `hiqlite_media.rs` alone has 61 consistent / 0 local.
  - `crates/plurx-core/src/config.rs:170` — `bounded_replica_reads: false` (default), so `CatalogueReader::bounded` (`store/mod.rs:5035-5043`) returns `None` and every `catalogue.*` call falls through to `self.authority.*` — i.e. the consistent path. `plurx.example.toml:47` confirms `# bounded_replica_reads = false`.
  - The auth design is deliberate: `auth.rs:52-54` "Authentication still performs an authority-consistent credential lookup on every request." and `docs/cluster/CLUSTER-PERFORMANCE-PLAN.md §3.2` classes token validation as `Authority`.
- Why it matters: a library grid = 1 auth read + ~7 catalogue reads (`browse.rs:190-261`) + 60 poster requests × (1 auth read each) ≈ **70 consistent reads per page view**. On a single voter each is a loopback TLS-WebSocket round trip + bincode + ReadIndex (~0.2–0.5 ms); on a 3-node cluster with the request landing on a follower it is a LAN round trip *plus* the leader's heartbeat quorum round (~1–3 ms) — 70 of them serially per page is 100–200 ms of pure consensus latency before rendering, and, per hiqlite's own comment, each one pauses Raft application on the leader. The per-request auth read is also the thing that couples HTTP latency to apply lag during a scan (thousands of Raft commits) — `ensure_linearizable` waits for the state machine to reach the read index.
- Proposed change: (a) flip `bounded_replica_reads` to `true` by default once the fleet is on the protocol (the code, gate, metrics and kill switch already exist — `store/mod.rs:4958-5107`); (b) add a **process-local positive auth cache** keyed by token digest with a short TTL (2–5 s) and *explicit invalidation on the paths that already exist*: `delete_token`, `users::update/delete`, and the cluster revocation fan-out in `internal_auth_revocation.rs` (which was built for exactly this problem but currently guards only the six recovery routes). A 5 s window on a self-hosted LAN server is within the revocation latency the product already accepts (the coalesced `last_seen_at` write is 60 s; the cluster exclusion is 15 s). Reference: Jellyfin caches `AuthenticationInfo` per token in memory; Plex validates tokens against an in-process map. Keep `Authority` for anything that mutates.
- Size: M · Risk: med (needs the invalidation tests; the `CacheOnlyAdminProofCache` machinery in `extract.rs:60-430` is a template).

#### F-core-5: Plex façade `GET /library/sections/{id}/all` is an N+1 over up to 5,000 items and has no container paging
- Severity: **P1** (Kodi/Plex clients) · Category: performance · Confidence: **CONFIRMED**
- Evidence:
  - `crates/plurxd/src/http/plex.rs:226-236`:
    ```rust
    let page = state.catalogue.list_top_items_in_genre(id, Default::default(), 0, 5000, None).await?;
    let views = views(&state, user.id, &page.items).await?;
    let mut elements = Vec::with_capacity(page.items.len());
    for item in &page.items {
        let view = views.get(&item.id).copied().unwrap_or_default();
        elements.push(element_for(&state, item, view).await?);
    }
    ```
  - `plex.rs:191-204` — `element_for` does `state.catalogue.files_for_item(item.id).await?` for every movie/episode and `state.catalogue.get_item_children(item.id).await?` for every show/season — one Store round trip per element, sequential.
  - `plex.rs:253-267` — `children` has the same loop.
  - `rg 'X-Plex-Container' crates/` → no matches: `X-Plex-Container-Start/Size` are not honoured.
  - `docs/API.md:2672` documents "Up to 5000 top-level items" as the contract.
- Why it matters: with F-core-4 in effect each `files_for_item` is a consistent read; a 2,000-movie section is **2,001 sequential consistent reads** ≈ 1–2 s on one node, 5–10 s on a 3-node cluster, *per Kodi library refresh*, holding one request future and generating a multi-MB XML body in memory. The native API has the batched `item_media_facts`/`child_counts` helpers (`browse.rs:236`, `:247`) — the façade simply does not use them.
- Proposed change: build `section_all`/`children` from two batched queries (`files_for_items(&ids)` — add it next to `item_media_facts` — and `child_counts(&ids)`), and honour `X-Plex-Container-Start`/`X-Plex-Container-Size` with `totalSize` in the `MediaContainer`, which Plex clients already send. Reference: Plex Media Server pages every `/library/sections/*/all` at the client's requested container size; Kodi's PlexKodiConnect requests 200-item pages.
- Size: S–M · Risk: low.

#### F-core-6: TMDB and AniList HTTP clients have no timeout; a stalled provider hangs the enrichment job indefinitely
- Severity: **P2** · Category: stability · Confidence: **CONFIRMED** (absence) / **LIKELY** (hang)
- Evidence:
  - `crates/plurx-core/src/metadata/tmdb.rs:140-144` — `http: reqwest::Client::builder().user_agent(...).build().unwrap_or_default()`; no `.timeout`, no `.connect_timeout`.
  - `crates/plurx-core/src/metadata/anilist.rs:59-62` — same.
  - `rg -n 'timeout' crates/plurx-core/src/metadata/*.rs` → only `book.rs:735 .timeout(Duration::from_secs(15))` and `genres.rs:480`; nothing wraps the TMDB/AniList calls. Contrast `trakt.rs:311-312` (25 s) and `http/images.rs:414-416` (750 ms connect / 3 s total).
  - `crates/plurx-core/src/cluster/migration.rs:873` — `reqwest::Client::new().post(...)` in `post_join_request`, also untimed (join path, low frequency).
  - The enrichment runs under a cluster job lease with a heartbeat task (`state.rs:9221`, `lease_heartbeat(15_000, ...)`), so a hung request keeps the lease alive and blocks the singleton for every node.
- Why it matters: reqwest's default is *no* total timeout; a black-holed egress (firewall drop, a VPN that swallows SYNs, TMDB CDN stall) leaves the enrichment future parked forever, the library shows "enriching" until restart, and, because the job is leader-scheduled and leased, no other voter picks it up. The artwork retry and genre backfill passes share the client.
- Proposed change: `.connect_timeout(5s).timeout(30s)` on both builders plus `tokio::time::timeout` around `download_image` (posters are ≤ 15 MiB, so 60 s), and a per-item deadline in `enrich` so one bad title cannot stall a pass. Reference: Jellyfin's `HttpClientFactory` default 30 s; the repo's own Trakt client.
- Size: S · Risk: low.

#### F-core-7: Image serving hashes the whole file on the runtime per request and refuses local hits with 503 above 8 in flight
- Severity: **P2** · Category: performance/stability · Confidence: **CONFIRMED**
- Evidence:
  - `crates/plurxd/src/http/images.rs:255-263` — `serve_local_artwork` takes `coordinator.permit()` (`:87-89` → `try_acquire_owned()`, non-blocking) **before** reading a local file; `:33 const MATERIALIZE_CONCURRENCY: usize = 8;`. A ninth concurrent local hit is `Err(artwork_capacity_error())` → 503 `artwork_response_capacity`.
  - `images.rs:265-266` → `artwork_bytes_match_name(filename, &bytes)` → `:838-847` — `let actual = hex::encode(Sha256::digest(bytes));` over the full file, executed after `spawn_blocking` has returned, i.e. **on the async worker**.
  - `images.rs:395-411` — response carries `Cache-Control: private, max-age=604800, immutable` but no `ETag`/`Last-Modified`; the whole file is read into a `Vec` (`:296`) rather than streamed.
  - `crates/plurx-core/src/metadata/mod.rs:43-44` — `BACKDROP_SIZE: &str = "original"; STILL_SIZE: &str = "original";` → 4K backdrops (1–5 MB) are stored and served unchanged; `docs/API.md:784` "there are **no sizing parameters** — the stored bytes are what you get".
  - `docs/API.md:786-789` describes the cap as "8 concurrent materializations" — the code applies it to local hits too.
  - Web client has no retry: `crates/plurxd/src/web/player/measurements.js:676-677` renders `<img loading="lazy" src=...>` with no `onerror`.
- Why it matters: a home screen of 20 backdrops at `original` is 20 × ~3 MB = 60 MB read into memory, SHA-256'd on runtime workers (~5–10 ms each, ≈150 ms of worker time), and served to a phone that will downscale to 400 px. Behind an HTTP/2 reverse proxy (Caddy default) or with two clients browsing at once, more than 8 image requests overlap easily and the ninth poster is a permanent blank until the user navigates away (the response is `immutable`-cached for a week only on success; a 503 is simply a broken image with no retry).
- Proposed change: (a) validate the content address once at materialization (already done: `install_artwork` is atomic) and on request compare only size+mtime+inode (`artwork_file_identity`, already computed at `:295`) — or hash inside the same `spawn_blocking` closure if the per-request check must stay; (b) separate the permit for peer fetches (memory-bounded network buffers) from local reads (stream them with a 256 KiB reader and let hyper back-pressure); (c) add `ETag` = the content digest already in the filename so proxies/CDNs can revalidate; (d) store backdrops at `w1280` and serve `?size=` variants (or at minimum cap `original` to 1920 wide at ingest). Reference: Jellyfin `/Items/{id}/Images/Backdrop?fillWidth=` with an on-disk resize cache; Plex `/photo/:/transcode` — the façade already routes `photo_transcode` (`plex.rs:460`), so the resize primitive is on the roadmap anyway.
- Size: S (a–c) / M (d) · Risk: low.

#### F-core-8: Logout and user mutations are a cluster-wide two-phase protocol serialized behind a process-wide `try_lock`; the second concurrent one gets 503
- Severity: **P2** · Category: architecture/stability · Confidence: **CONFIRMED**
- Evidence:
  - `crates/plurxd/src/http/auth.rs:100-109` — `logout` → `ClusterCacheRevocation::begin_digest(&state, &hash).await?` then `delete_token_with_cache_admin_claim`, returning `503 "logout lost its cache-revocation exclusion; retry the request"` on a lost claim.
  - `crates/plurxd/src/http/internal_auth_revocation.rs:265-274` — `begin_digest` → `state.cache_only_admin_proofs.try_acquire_revocation_operation().map_err(|_| propagation_error())?` → `extract.rs:221-228` — `self.revocation_operation_gate.clone().try_lock_owned().map_err(|_| "cache-admin revocation already in progress")`; `propagation_error()` (`:625-631`) is **503 `admin_revocation_propagation_failed`**.
  - `internal_auth_revocation.rs:285-354` — `begin` commits a replicated exclusion lease (`membership_exclusion.commit()` = a Raft write), busy-waits for local apply (`wait_for_exact_local_claim_with`, poll every 10 ms up to 1.5 s, `:31-32`), reads the peer directory, fans out `Begin` to every member with a 2 s deadline, requires two consecutive identical roster reads, and only then admits the Store mutation; `finish` repeats for `End` and releases the lease (another Raft write). `MEMBERSHIP_EXCLUSION_DURATION = 15 s` (`:29`).
  - Same gate on `users.rs:89` and `:183` (admin user update/delete).
  - The cache being protected (`CacheOnlyAdminProofCache`) serves exactly the cluster-recovery reads (`extract.rs:673-735`); ordinary routes never read it.
- Why it matters: two people pressing Sign Out within the same ~100–300 ms window (a family switching profiles on two TVs), or an admin editing a user while anyone logs out, yields a 503 and — per `SECURITY.md:63-69` — the client clears its local bearer anyway, leaving a **valid token in the database that the user believes is revoked**. On a single voter every logout still costs ≥ 3 Raft commits and a 1.5 s worst-case apply poll to invalidate a cache that six admin diagnostic routes read. The engineering weight (a 750-line module with roster-stability proofs) is out of proportion to the asset.
- Proposed change: make the ordinary path `DELETE FROM tokens` + a best-effort, non-blocking peer `POST` (the fan-out already exists) and let the cache-only routes accept the 15 s staleness they already accept when a peer is unreachable; or at least replace `try_lock_owned` with `lock_owned` under a short timeout so concurrent logouts queue instead of failing. Reference: Jellyfin/Plex revoke a session by deleting the row; caches converge on TTL.
- Size: S–M · Risk: med (touches a security invariant the docs describe; needs the recovery-route tests re-run).

#### F-core-9: Login has no per-account/per-IP throttle and login tokens never expire
- Severity: **P2** (LAN posture) · Category: security · Confidence: **CONFIRMED**
- Evidence:
  - `crates/plurxd/src/http/auth.rs:45-91` — `login` does size check → `get_user_by_username` → Argon2 verify (dummy hash on unknown user, good) → mint token. No failure counter, no backoff, no per-IP bucket; the only limiter is the global Argon2 admission (`:21-23`: 2 active, 16 queued, 2 s wait), which is a *capacity* guard, not a brute-force guard, and it is shared by every user (one attacker saturating it locks everyone out of login with 503s).
  - `crates/plurx-core/src/auth.rs:20` — `Argon2::default()` = m=19 MiB, t=2, p=1 (OWASP minimum); a verify is ~40–60 ms, so 2 workers ≈ 30–50 guesses/s sustained.
  - Token table has no expiry column: `store/sqlite/mod.rs:72-78` and `store/hiqlite.rs:212-218` — `tokens(token_hash, user_id, device, created_at, last_seen_at)`; `user_for_token` (`hiqlite.rs:3980`) has no `WHERE last_seen_at > …`. `docs/SECURITY.md:21-74` describes tokens without any lifetime.
  - `token_from_parts` (`extract.rs:547-577`) accepts `?token=` on every route, and the TraceLayer span omits the query (`mod.rs:981-983`, good) — but a reverse proxy's access log will not.
- Why it matters: on a LAN this is defensible and the docs say so, but a lost phone's token is valid forever, and a "Sign out everywhere" has no server-side notion of idle expiry to lean on. Brute force at 30–50 guesses/s against an 8-character minimum password (`auth.rs:129-131`) is a weekend for a wordlist. Plex/Jellyfin both expire idle device tokens and Jellyfin has per-IP login throttling.
- Proposed change: (a) an in-memory `(username, peer_ip) → failures` map with exponential backoff (5 failures → 30 s, doubling; `ConnectInfo<SocketAddr>` is already wired at `main.rs:2424`); (b) idle expiry via the existing `last_seen_at` — refuse tokens with `last_seen_at < now - 90 days` in `user_for_token` and prune them in the daily housekeeping tick; (c) surface "devices" (tokens) to the user for revocation. Reference: OWASP ASVS 2.2.1 (anti-automation), Jellyfin `LoginAttemptsBeforeLockout`.
- Size: S · Risk: low.

#### F-core-10: `playback_control.rs` is a 30 k-line single module mixing wire types, fences, a preparation state machine and a producer actor; 82 `fix:` commits in 30 days
- Severity: **P2** · Category: architecture/stability · Confidence: **CONFIRMED**
- Evidence:
  - `wc -l crates/plurxd/src/playback_control.rs` → 29,986; `mod tests` starts at `:14534`; 100 `struct`s and 66 `enum`s in the non-test half; `RollingControlActor` alone spans `:8461-13564` (~5,100 lines).
  - `git log --since=2026-08-20 --no-merges --format=%s -- crates/plurxd/src/playback_control.rs | grep -c '^fix'` → **82 of 188** commits are fixes; titles include "fence preparation admission and takeover cleanup", "retain settlement across owner races", "close the ways a window owner outlives its playback", "four defects an adversarial review found in tonight's merges".
  - The fence is ad hoc imperative code, not a table: `ControlState::accept_at` (`:3381-3620`) is a 240-line function with 14 early-return sites checking generation → owner epoch → sequence==1 → platform → client instance → sequence monotonicity → replay fingerprint → vocabulary → rate limit → rollover → terminal directive → prepared-successor binding, each mutating some of 17 fields on `ControlState` (`:3125-3219`), while `ControlAcceptance` carries a second copy of the request's intent because "observe runs after accept" (`:3252-3261`).
  - Replay/idempotency: only the *last* sequence is retained (`:3493-3517`); a resend of N-1 after N is `StaleSequence`, and a replay of N with a different body is also `StaleSequence` (not 409/400), so the client cannot distinguish "you moved on" from "your retry was corrupted".
  - Cadence and reordering: `NEXT_EXCHANGE_MS = 5_000` (`:26`), `MIN_CONTROL_INTERVAL = 250 ms` (`:70`) — 0.2 exchanges/s/client, fine. Server-side fencing is `Instant`/monotonic-sequence based (no client wall clock), so clock skew cannot break acceptance; the one wall-clock crossing is `inherited_exchange_budget` (`:99-108`) which is *capped* at 4 s, so skew can only shorten a relayed budget (safe). Reordering is only possible across concurrent HTTP requests from one client, which the 250 ms floor rejects.
- Why it matters: the volume of fix commits is the stability signal the brief asked for. Every fix in the list is a fence-ordering bug, which is what you get when the state machine is a sequence of `if`s over 17 mutable fields rather than a typed `(State, Event) → (State, Effects)` table. New agents will keep adding early returns. Compile time and reviewability also suffer: this one file is 10 % of the daemon.
- Proposed change: split into `playback_control/{wire.rs (ControlRequestV1/ResponseV1 + validate), fence.rs (ControlState as an explicit enum of phases with a pure `step()` and a table test), preparation.rs (PreparationSlot transitions), producer/ (RollingControlActor, ingress, executor), metrics.rs}` and move the 15 k lines of tests beside their units. Make `accept_at` a pure function of `(ControlState, Request) → Result<(ControlState, Disposition), ControlStateError>` so the 14 rejection paths become one exhaustive `match` and the tests can enumerate them. Reference: the repo's own `schedule.rs` ("The deciding is pure — `due_jobs` takes a clock reading and some rows") and `decision engine is a pure function` (ARCHITECTURE §3) — apply the same discipline here.
- Size: L · Risk: med (behaviour-preserving refactor; the existing 15 k lines of tests are the safety net).

#### F-core-11: Route cache TTL of 1 s makes every active HLS session cost ≥ 1 consistent Store read per second
- Severity: **P2** · Category: performance · Confidence: **LIKELY** (route lookup confirmed; exact per-request call count on the segment path is in the streaming reviewer's area)
- Evidence:
  - `crates/plurxd/src/media_sessions.rs:107-108` — `const ROUTE_CACHE_TTL: Duration = Duration::from_secs(1); const MAX_ROUTE_CACHE_ENTRIES: usize = 4_096;`; `:2237-2246` `cached_route` evicts on expiry; misses go to `self.store.media_session_route(session_id)` (`:1721`) which is a consistent read (`hiqlite_sessions.rs`: 18 consistent / 0 local).
  - `:1485` — `routes: Arc<tokio::sync::Mutex<HashMap<String, CachedRoute>>>` — one global async mutex; every insert does `routes.retain(|_, cached| cached.expires_at > now)` (`:2210`, `:2251`, `:2269`), an O(n) scan under the lock on every cache write.
  - `route_queries: Arc<Vec<tokio::sync::Mutex<()>>>` (`:1496`) shards the *store* single-flight but not the cache lock.
- Why it matters: 20 viewers × (playlist every 2–4 s + segment every 4 s + control every 5 s) → each session's route expires between most requests, so ~20 consistent reads/s go to the leader just to re-learn "this session is still mine". The `LEASE_TTL_MS = 12_000` (`:90`) says the durable truth changes at most every 6–12 s; a 1 s cache is 6–12× too eager. The global `retain` under the mutex is O(sessions) per write; at 4,096 entries it is a few hundred µs per insert on the hot path.
- Proposed change: cache positive routes until `min(lease_expires_at_ms, now + LEASE_TTL/2)` and keep the 1 s TTL only for negatives; replace the global `Mutex<HashMap>` with the same sharding already used for `route_queries` (or `dashmap`), and prune on a timer instead of on every insert. Reference: Plex keeps transcode session ownership in process memory and only consults the DB on miss.
- Size: S · Risk: low.

#### F-core-12: No RED metrics or request ids on the HTTP layer; access log is off at the default level
- Severity: **P3** · Category: ops · Confidence: **CONFIRMED**
- Evidence:
  - `crates/plurxd/src/http/mod.rs:629-638` — `TraceLayer::new_for_http().make_span_with(...)` with the default `on_response` (tower-http logs it at DEBUG); `main.rs:1542` `EnvFilter::new("info")` → no per-request line is emitted in production.
  - Metric census (`rg -o 'plurx_[a-z0-9_]+'`): ~200 series covering playback, offline, Live TV, Raft, membership and Store latency (`plurx_store_operation_seconds{class,outcome}` — `hiqlite.rs:678-715`, good), but **no** `http_requests_total`, no per-route duration histogram, no in-flight gauge, no 4xx/5xx counter. `rg 'x-request-id|RequestId' crates/` → nothing.
  - Logs: `main.rs:1541-1558` — stdout `fmt` layer (human format, not JSON) + two 2,000-entry ring buffers (`logbuf.rs:115`). So the ring is *not* the only sink (docker logs has everything), but there is no structured field for request correlation.
- Why it matters: the owner is an SRE; the first question after "playback stalled" is "what was p99 on `/hls/{s}/{segment}` at 21:14 and which node" and today the answer requires the client's telemetry. `plurx_store_operation_seconds` exists precisely because the plan noticed this gap for the Store; HTTP never got the same.
- Proposed change: a 40-line `axum::middleware::from_fn` that records `plurx_http_requests_total{route,method,status}` and `plurx_http_request_seconds_bucket{route}` using the *matched* route template (`axum::extract::MatchedPath`, so capability ids are not label values), plus `tower_http::request_id::SetRequestIdLayer` + `PropagateRequestIdLayer` with the id added to the span. Switch the fmt layer to JSON when `!isatty`. Reference: RED method (Weaveworks), `tower-http` request-id docs.
- Size: S · Risk: low.

#### F-core-13: Direct-play responses carry no validators and ignore `If-Range`; per-request Store hit before the file is opened
- Severity: **P3** · Category: performance · Confidence: **CONFIRMED**
- Evidence: `stream.rs:2828-2832` — `if headers.contains_key(header::IF_RANGE) { return Ok(None); }` (falls back to a full 200); `stream.rs:2941-2968` — no `ETag`, `Last-Modified` or `Cache-Control` on 200/206; `stream.rs:2458` `load_file` (a consistent read) precedes every range request, and AVPlayer/ExoPlayer issue dozens of range requests per minute on a direct play.
- Why it matters: `ARCHITECTURE.md §3` promises "HTTP range serving of the file with correct caching headers"; a proxy in front cannot cache or revalidate, and Safari's `If-Range` (sent after a pause/resume) turns a 206 into a full-body 200 that the player then aborts.
- Proposed change: emit `ETag: "<file.id>-<size>-<mtime>"` and `Last-Modified`, honour `If-Range` against that strong validator, add `Cache-Control: private, max-age=0, must-revalidate`; fold the `file.size` check into the validator so the per-range Store read can be skipped when `If-Range` matches. Reference: RFC 9110 §13.1.5, nginx `open_file_cache`.
- Size: S · Risk: low.

---

### `/api/v1` route table (from `http/mod.rs:82-644`, extractors read from each handler signature)

Auth legend: **B** bearer `AuthUser` · **A** `AdminUser` · **K** `ScopedKey::require` in body · **C** capability (session id / token in path, no user) · **P** peer HMAC · **N** none. Pagination: **P** offset/limit clamped · **F** fixed cap · **–** n/a. Cache headers as emitted by the handler.

| Route group | Handler file | Auth | Pagination | Cache headers |
|---|---|---|---|---|
| `/server`, `/setup`, `/auth/login` | system.rs, auth.rs | N | – | none |
| `/auth/logout`, `/me` | auth.rs | B | – | none |
| `/settings` GET/PUT, `/developer/readiness`, `/system*`, `/system/logs`, `/system/playback-events`, `/system/storage`, `/system/search-index/rebuild`, `/system/library-shape` | system.rs, developer.rs | A | logs ≤2000, events ≤2000 | none |
| `/scan/status`, `/activity`, `/activity/detail`, `/client-log` | system.rs | B (client-log rate-limited) | – | none |
| `/activity/sessions/{id}`, `/activity/offline/{id}`, `/activity/producer` DELETE | system.rs | A | – | none |
| `/trakt/*` | trakt.rs | A | – | none |
| `/cluster/*` (tokens, nodes, promote, maintenance, election, leave, protocol, remove) | cluster.rs | A | – | none |
| `/cluster/status`, `/cluster/support-bundle` | cluster_operations.rs | A or cache-only-admin proof | – | none |
| `/cluster/ingress` | cluster.rs | B | – | none |
| `/cluster/media`, `/cluster/media/offers` | internal_media.rs | A | – | none |
| `/cluster/join/*`, `/cluster/learner/join/*` | cluster.rs | N (single-use join-token digest in body) | – | none |
| `/cluster/artwork/{filename}` | images.rs | P (filename-bound HMAC + live membership) | – | private, 7d, immutable |
| `/users`, `/users/{id}`, `/keys`, `/keys/{id}` | users.rs, keys.rs | A | – | none |
| `/scan`, `/scan/requests/{id}` | scan.rs | K (`scan:trigger` / `status:read`) | – | none |
| `/coming-soon` / `/monarr/status` | comingsoon.rs | B / A | – | none |
| `/libraries` GET | libraries.rs | B | – (all libraries) | none |
| `/libraries` POST, `/libraries/{id}` PUT/DELETE, `/schedule`, `/scan`, `/refresh`, `/identity-repairs/*`, `/dv-conversion(s)`, `/root-identity/reset` | libraries.rs, scan_identity.rs, dv_disk.rs | A | – | none |
| `/libraries/{id}/items` | browse.rs | B | P (default 60, max 200) | none |
| `/items/{id}` GET | browse.rs | B | children unpaged | none |
| `/items/{id}` PATCH, `/reanalyze`, `/refresh-artwork` | items.rs | A | – | none |
| `/files/{id}/analysis`, `/timeline-annotations/*`, `/analysis/*`, `/dv-conversions`, `/files/{id}/dv-conversion` | analysis.rs, dv_disk.rs | A | jobs F | none |
| `/hubs`, `/home/previews`, `/search`, `/search/related` | browse.rs, library_search.rs | B | F (20/row; search ≤200) | none |
| `/search/settings` GET / PUT, `/items/{id}/classification` GET / PUT | library_search.rs | B / A | – | none |
| `/items/{id}/photo` | photos.rs | B | – | unknown (not read) |
| `/items/{id}/progress`, `/scrobble`, `/unscrobble`, `/reading-state` | watch.rs, reading.rs | B | – | none |
| `/files/{id}/decision` GET/POST (64 KiB), `/audio-offset` | stream.rs | B | – | none |
| `/files/{id}/direct`, `/download`, `/content` | stream.rs | B | – | **none** (F-core-13) |
| `/files/{id}/stream.mp4`, `/stream/{id}/status`, `/files/{id}/subs/{subtitle}` | stream.rs | B | – | none / unknown |
| `/files/{id}/publication` POST, `/publication/{s}` DELETE | publication.rs | B | – | none |
| `/publication/{s}/{*resource}` | publication.rs | C | – | unknown |
| `/files/{id}/offline-options`, `/offline-packages`, `/offline/packages/{id}*` | offline.rs | B | – | none |
| `/offline/media/{token}/*` | offline.rs | C (lease token) | – | unknown |
| `/files/{id}/subs/{i}/overlay*` | pgs_overlay.rs | B | – | unknown |
| `/files/{id}/hls/sessions` POST (64 KiB), `/hls/start` | hls.rs | B | – | none |
| `/hls/{s}/master.m3u8`, `index.m3u8`, `video.m3u8`, `subs/*`, `status` | hls.rs | C | – | playlists: unknown; segments: private, 1h, immutable + ETag (`hls.rs:13303-13311`) |
| `/hls/{s}/control` POST (16 KiB), `/hls/{s}` DELETE, `/hls/{s}/{segment}` | hls.rs | C | – | segment: as above |
| `/images/{filename}` | images.rs | B | – | private, 7d, immutable, no ETag |
| `/live-tv/readiness*`, `/guide/refresh`, `/guide/readiness` | live_tv.rs | A | – | none |
| `/live-tv/channels`, `/guide`, `/channels/{c}/sessions` POST, `/starts/{id}*` | live_tv.rs | B | – | none |
| `/live-tv/sessions/{cap}/*` | live_tv.rs | C | – | unknown |
| `/library-channels/*`, `/dvr/*` (nested routers) | library_channels.rs, dvr.rs | not read (nested) | unknown | unknown |
| Plex façade `/identity`, `/library/*`, `/photo/:/transcode`, `/:/timeline|scrobble|unscrobble`, `/search`, `/hubs/search` | plex.rs | `PlexUser` (X-Plex-Token header/query → same token table); `/identity` N | **F 5000, no container paging (F-core-5)** | none |
| `/healthz`, `/readyz`, `/metrics` | mod.rs, system.rs | N (metrics documented unauthenticated) | – | none |
| `/internal/*` (activity, cluster ops, auth revocation, media pool, shared-cache canary, live-tv, media-sessions, fragment-index) | internal_*.rs | P (HMAC peer transport, per-route body caps 1 KiB–20 KiB) | – | none |

Every route that mutates server state is `A`, `B`, `K` or `P`; no admin route falls through to `B` (checked by script over all 205 registrations). Global middleware order (outermost first): `cluster_capacity_gate` → `TraceLayer` → router; `mutable_media_serving_gate` wraps the `/api/v1` and Plex sub-routers. Default body limit is axum's 2 MiB except where a `DefaultBodyLimit::max` is shown above. No compression layer; HTTP/1.1 + h2c only (no TLS in-process; browsers will be on HTTP/1.1 with 6 connections/host unless proxied).

---

### Already good (do not "fix" these)

1. **Store access discipline.** SQLite backend is `spawn_blocking` around one writer + a 2-connection READ_ONLY pool with WAL/`synchronous=NORMAL`/`busy_timeout` (`store/sqlite/mod.rs:1302-1362`, `:1575-1683`); Hiqlite reads are `spawn_blocking` inside the vendored client. No rusqlite call on a runtime thread anywhere in scope.
2. **No lock-across-await.** Zero `std::sync::Mutex`/`RwLock` guards held over an `.await` in ~30 scanned files; `std` mutexes are used only for tiny fences (`playback_control.rs:2347`, `:6686`, `serving_fence.rs:64`) and `tokio::sync::Mutex` where a hold spans I/O.
3. **Bounded in-memory state everywhere it was checked:** `playstart.rs` (TTL + retain at 1,024), `delivery.rs` (idle timeout + `MAX_PER_USER = 8`), `progress.rs` (retain + `MAX_FLUSH_ATTEMPTS`), `media_sessions.rs` route cache (4,096) and control-rate map (4,096, 8/s per session, 512/s global), `images.rs` singleflight map (weak-pruned), `waitpool.rs` (per-session + global caps, `Drop`-based deregistration), `logbuf.rs` (2,000 ring).
4. **The auth extractor design** (`extract.rs:620-671`): access level is the handler's argument list; the router audit above found no gaps. Timing-uniform login with a real dummy Argon2 hash and Argon2 admission that survives request cancellation (`auth.rs:147-168`) is better than most.
5. **Path handling**: `fs_secure.rs` is an `openat`/`O_NOFOLLOW` capability API; `safe_artwork_name` (`images.rs:366-373`) requires `file_name() == input`; `parse_range` (`stream.rs:2824-2896`) saturates decimals, rejects reversed ranges by decimal comparison, caps 16 members, and treats unknown units as absent. Access-log target redaction (`mod.rs:965-984`) strips capability ids and the whole query string.
6. **Browse endpoints are paginated and batched**: `list_items` does exactly seven page-wide queries with no per-item fan-out (`browse.rs:184-285`); `hubs` uses `try_join!` to overlap independent reads (`:642`).
7. **Graceful shutdown** is bounded and explained (`main.rs:2400-2480`: 5 s connection drain + 3 s progress flush inside Docker's 10 s), signals are installed before store activation, and the progress coalescer's durability boundary is documented rather than hidden.
8. **Per-route body limits** on every internal and client-driven POST (`mod.rs:113-621`), the 64 KiB decision/create caps with the reasoning in comments, and the `client-log` rate limiter.
9. **Background jobs are single-flighted** with atomics or cluster leases (`state.rs:5635-5639`, `:3051`, `:6391`, `:8489`) and long passes are spawned off the scheduler task so a slow artwork retry cannot delay a scan.
10. **Store latency is already instrumented by consistency class** (`plurx_store_operation_seconds{class,outcome}`), and the `BoundedReplica` reader with a monotonic quorum-watermark lease is exactly the right primitive — it only needs to be turned on.

### Hot spots (fix commits / total commits since 2026-08-20, `git log --since=2026-08-20 --no-merges --format=%s -- <path> | grep -ci '^fix'`)

| File | fix / total |
|---|---|
| `crates/plurxd/src/playback_control.rs` | **82 / 188** |
| `crates/plurxd/src/http/mod.rs` | 43 / 116 |
| `crates/plurxd/src/state.rs` | 39 / 105 |
| `crates/plurxd/src/http/system.rs` | 38 / 115 |
| `crates/plurxd/src/http/cluster_operations.rs` | **32 / 39** (82 % fixes) |
| `crates/plurx-core/src/store/hiqlite.rs` | 29 / 72 |
| `crates/plurx-core/src/store/hiqlite_sessions.rs` | **26 / 46** |
| `crates/plurxd/src/main.rs` | 24 / 83 |
| `crates/plurxd/src/media_sessions.rs` | 18 / 40 |
| `crates/plurxd/src/serving_fence.rs` | 10 / 17 |
| `crates/plurxd/src/http/extract.rs` | **9 / 10** |
| `crates/plurx-core/src/fs_secure.rs` | 8 / 18 |

Interpretation: the fix density clusters on the two hand-rolled distributed protocols (playback control fencing, media-session ownership) and on the cluster operations page. Nothing in the pure/bounded modules (`schedule.rs`, `progress.rs`, `auth.rs`, `waitpool.rs`) needed more than a handful.

### Open questions (not settleable from code)

1. **Measured cost of a consistent read on the fleet.** `plurx_store_operation_seconds{class="authority_read"}` exists; what are p50/p99 on the 3-node lab vs the single-node box? That number decides whether F-core-4 is P1 or P2 and whether the auth cache is worth the invalidation complexity.
2. **Is `bounded_replica_reads=true` deployed anywhere?** `OPERATIONS.md:2272-2298` describes the rollout; STATUS.md does not say whether the lab runs it.
3. **What sits in front of the daemon in production** (Caddy/nginx with h2? bare?). F-core-2 and F-core-7's severity both depend on it; the `deploy/` compose suggests bare LAN.
4. **Tokio runtime shape on the target hardware.** `#[tokio::main]` defaults (`main.rs:255`) = one worker per core; on a 4-core N100-class box F-core-3 removes 25 % of the runtime during a scan — is that box the target?
5. **Backdrop `original` policy** — deliberate for tvOS 4K wallpaper, or an oversight? Changing it is a re-fetch of every backdrop.
6. **Plex façade audience** — if the only Plex clients are Kodi PlexKodiConnect, container paging is mandatory (it always sends `X-Plex-Container-Size`); if it is only for discovery, F-core-5 can wait.

---

## Review: store-and-cluster (2026-09-20, main @ a1414368)

Scope read: `crates/plurx-core/src/store/{sqlite/,hiqlite*.rs,mod.rs}`, `crates/plurx-core/src/cluster/{membership,migration,coordination}.rs`, `crates/plurxd/src/{progress,media_sessions,watched,storeprobe,wal_cli,fragindex,fragment_index_cluster}.rs`, `crates/plurx-cluster-check/`, `vendor/hiqlite/` (0.14.0 + 15 patches), openraft 0.9.25 from the registry, `docs/ARCHITECTURE.md` §2, `docs/cluster/*`, `docs/OPERATIONS.md`.

Sizes for orientation: `sqlite/` 1.16 MB across 24 files, `hiqlite*.rs` 1.63 MB across 17 files, `store/mod.rs` 232 KB, `cluster/membership.rs` 736 KB / 17,169 lines, `cluster/migration.rs` 376 KB, `tests/store_contract.rs` 29,435 lines.

---

### Answers to the six questions (evidence inline; findings follow)

#### Q1. SQLite: connection strategy and pragmas

**Strategy.** One writer `Connection` behind `Arc<Mutex<Connection>>`, plus a `ReadPool` of exactly two `READ_ONLY` connections picked round-robin. Every call is `tokio::task::spawn_blocking` → lock std mutex → run closure (`sqlite/mod.rs:1275-1300, 1575-1589, 1666-1683`):

```rust
pub struct SqliteStore { conn: Arc<Mutex<Connection>>, reads: Option<Arc<ReadPool>> }
const READ_CONNS: usize = 2;
async fn with_conn(...) { tokio::task::spawn_blocking(move || { let guard = conn.lock().map_err(|_| StoreError::Task("sqlite connection mutex poisoned"))?; f(&guard) }) }
```

So rusqlite never runs on tokio workers (good), but a poisoned mutex is permanent until restart (`.map_err(...poisoned)` rather than `into_inner`).

**Pragmas actually set** (`sqlite/mod.rs:1352-1355` writer, `:1316` readers):

```rust
conn.pragma_update(None, "journal_mode", "WAL")?;
conn.pragma_update(None, "foreign_keys", "ON")?;
conn.pragma_update(None, "busy_timeout", 5000)?;
conn.pragma_update(None, "synchronous", "NORMAL")?;
```

Not set anywhere on the SQLite backend: `cache_size` (default −2000 = 2 MiB), `mmap_size` (0), `temp_store` (file), `journal_size_limit` (none → the `-wal` file never shrinks after a large scan), `wal_autocheckpoint` (default 1000 pages, PASSIVE). No `PRAGMA optimize`, no `ANALYZE` ever (`rg optimize|ANALYZE crates/plurx-core/src/store/sqlite` → nothing), no `VACUUM`, no `wal_checkpoint`, no `integrity_check`/`quick_check` at boot (only in the import path `hiqlite_import.rs:2094` and the pre-import backup `cluster/migration.rs:3479`).

**Reads vs writes share a connection.** Despite the ReadPool comment ("the hottest control paths … were paying writer-lock latency"), ~70 read-only methods still run through `with_conn` on the writer mutex (counted with a script over non-test code; see F-sc-6). `with_read` is used by only 6 of 24 files. In particular **every authenticated HTTP request** does `user_for_token` on the writer (`sqlite/users.rs:273-299`), and it is a read+conditional UPDATE, so it takes the WAL write lock at statement start even when the UPDATE matches nothing.

**Long scans holding writes.** The only multi-second write transactions are admin/scan-scoped: `rebuild_search_index` (FTS rebuild of `items_fts` + `classification_fts`, `sqlite/media.rs:2317-2326`, admin-triggered) and `reconcile_library` (`:2197-2305`, once per scan, `NOT IN (SELECT …)` over all files/items). The scanner itself does not batch: per new file it issues `get_file_by_path` (read on writer), `find_movie`/`find_show`… (read on writer), `insert_item`, `upsert_file`, `apply_metadata` as separate autocommit statements (`scan/mod.rs:977, 1479, 1559`). In WAL+NORMAL each commit is a WAL append without fsync, so locally cheap; in cluster mode each is a separate raft entry (3–4 entries per new file).

**Progress coalescer**: 1 commit per 10 s per (user,item), first beat and 95 % crossing synchronous, map hard-capped at 1024 entries (`plurxd/progress.rs:23-27, 112-140`). Good.

**Prepared-statement caching**: `prepare_cached` appears twice in the whole SQLite backend (`media.rs`, `publication.rs`); everything else re-parses SQL per call (`rg -c 'prepare\(' sqlite/media.rs` = 34).

#### Q2. Schema & hot queries

63 migrations, append-only, `PRAGMA user_version`-tracked, FK off per migration then `pragma_foreign_key_check` after each (`sqlite/mod.rs:1467-1545`). Replay guards for v41/45/46/47/51 (`:1501-1506`) and a refusal to open a newer schema (`:1470-1474`). Only v6 rewrites big tables (`items` + `items_fts` rebuild, `:199-268`); nothing since v6 rewrites `items`/`files`; recent migrations are `ADD COLUMN` / new tables. Upgrade cost on a large library is therefore dominated by the one-time `backfill_hdr_format` (`:1368-1409`, reads every `probe_json` once, gated by a settings flag) — acceptable.

Indexes on the catalogue (`sqlite/mod.rs:108-110, 132, 143` + later): `items(library_id, kind)`, `items(parent_id)`, `items(added_at DESC)`, `items(book_work_id)`, `items(artwork_attempted_at)`, `files(item_id)`, `files.path UNIQUE`, `watch_state(user_id, updated_at DESC)`, PK `(user_id,item_id)`. **Missing** for the actual hot queries: `items(library_id, sort_title)` / `(library_id, year)` / `(library_id, added_at)` (library page sorted, `media.rs:856-896` sorts the whole library per page with `OFFSET`), `items(library_id, kind, title COLLATE NOCASE)` (scanner `find_movie`/`find_show`, `media.rs:451-470, 535-558`), `items(tmdb_id)` (`item_by_external_id`, `:415-450`, scans all rows of a kind), `items(kind)` alone (used by `next_up`, `recently_added`).

Row shapes: `files.probe_json` stores the **entire** `ffprobe -show_format -show_streams -show_chapters` document (`scan/probe.rs:199-211`, `parse_probe_json` keeps `raw_json`), so `files` is ~5–30 KB/row and dominates database size. `FILE_COLS` correctly excludes it (`sqlite/mod.rs:1218-1223`) but the columns added *after* it (`hdr_format`, `audio_offset_ms`, `dv_*`, `video_codec_tag`) sit past the blob in the record, so `get_file`/`files_for_item` still walk the overflow pages. Items store `tags`/`genres` as JSON text re-parsed on every row (`item_from_row`, `:1200-1206`) and genre filtering is `json_each` per row (`media.rs:877-878`) — acceptable at 20k, not at 1M.

No `SELECT *`. No N+1 in the item/episode list mappers (`child_counts`, `item_media_facts`, `watch_map` are single statements over `json_each(?)`, tests pin "one statement whatever the page size"). Unbounded `IN (...)` is avoided via `json_each` everywhere I looked. `LIKE`-prefix: `find_*_by_directory` uses a path range on the UNIQUE index (test `scan_identity_directory_lookup_uses_the_path_range_index`). Search is FTS5 external-content over `items` plus `classification_fts`, but with one full FTS scan per query (F-sc-8).

#### Q3. Raft / hiqlite: what goes through consensus

hiqlite is built with `features = ["auto-heal", "macros", "sqlite"]` (`Cargo.toml:38`) — **no `cache` feature, no `backup` feature**. There is no Raft KV/TTL tier; every "ephemeral" row is a durable SQLite state-machine write through the log.

Steady-state writes (3 voters, P active players), per second:

| Source | Cadence | Entries/s | Evidence |
|---|---|---|---|
| Watched-outbox claim (`UPDATE … RETURNING`) | 1 Hz **per voter**, unconditional | 3.0 | `watched.rs:192-199`, `hiqlite_durable.rs:702-722`, `main.rs:2375-2377`, authority = every committed voter `membership.rs:5034-5042` |
| Media-session lease renewals (batched txn ≤256) | every 3 s per node with sessions | ≤1.0 | `media_sessions.rs:89-90,131-134,3912-3919` |
| Membership heartbeat | 10 s per node | 0.3 | `membership.rs:72` |
| Playback progress | 10 s per stream | P/10 | `progress.rs:23` |
| Token `last_seen_at` | 60 s per token | P/60 | `hiqlite.rs:2963-3001` |
| Cache touches | 5 s window per recipe | ≤P/5 | `hiqlite.rs:364` |

Idle ≈ 3.3 entries/s (≈285k/day); 10 players ≈ 6–8/s. Snapshot policy is `LogsSinceLast(10_000)` with `max_in_snapshot_log_to_keep: 1` (`vendor/hiqlite/src/config.rs:395-398`, `cluster/migration.rs:2327`), so an **idle** cluster snapshots every ~50 min and purges its log right after; a scan (3 entries/file) snapshots every ~3,300 files.

Steady-state **consistent reads** (each = WebSocket to leader + openraft `ensure_linearizable()` heartbeat round, `vendor/hiqlite/src/query/mod.rs:19-30`, `client/query.rs:11-34`): one per authenticated request (`extract.rs:620-641` → `hiqlite.rs:3980-3997`), route lookup per session per second past the 1 s route cache (`media_sessions.rs:107, 1838`), `owned_media_sessions` every 3 s per node, `get_setting` ×2 every 2 s per node in the takeover loop even when the feature is off (`media_sessions.rs:4353-4377`), `get_setting_pair` every 2 s per node for rate control (`transcode.rs:12498, 19683-19690`, `hiqlite.rs:3476-3486`), item + watch-state per heartbeat. **225** `query_consistent` call sites vs ~40 local `query_map` sites in `hiqlite*.rs` today; the performance plan's baseline was 85 vs 13 (`CLUSTER-PERFORMANCE-PLAN.md:99-105`).

Read path: local reads exist only behind `CatalogueReader` (9 methods, `store/mod.rs:4949-5199`) and only when `cluster.bounded_replica_reads = true`, **default `false`** (`config.rs:170`). `get_item`/`get_file`/`watch_state` via `state.store` (`http/items.rs:162-218`, `http/watch.rs:33-130`, `http/hls.rs:4691`) bypass it regardless.

Quorum loss: authority reads retry within a 5 s budget then fail (`hiqlite.rs:180, 903-972`), so **every authenticated endpoint returns 503 after ~5 s**, including login and direct-play starts. The serving fence (`serving_fence.rs`) drops readiness, refuses new remux/transcode (`stream.rs:3136-3143`) and tears down admitted sessions on the loss generation; segment requests fall off the 1 s route cache into a failing consistent read. Nothing degrades; direct-play bodies already streaming continue (`serve_file_range` has no whole-body deadline). Design choice, but see F-sc-1 for the auth part.

#### Q4. Dual backend drift

`store_contract.rs` runs SQLite (file + memory) in `make unit`; the hiqlite three-voter run needs `--features hiqlite-contract-tests` and only runs in the self-hosted `cluster_store_*` lanes when `scope.cluster_auth` is true (`ci.yml:293-299`, `validation/ci_scope.py:395-397`). The PR-lane path list for that scope (`validation/points.toml:448-494, 550-561`) omits `hiqlite_{classification,dv_conversion,dvr,fragment_index_cluster,library_channels,shared_cache,timeline_annotations}.rs` and most of `sqlite/*.rs`; those get three-voter coverage only on the push-to-main `all_scope()` run.

Spot-check of 10 methods (SQL compared side by side): `user_for_token` (60 s activity window both; hiqlite splits into read + gated write), `due_watched` (identical `UPDATE…RETURNING`), `put_progress_at` (same semantics; SQLite uses `WATCHED_THRESHOLD` const, hiqlite hard-codes `0.95` twice — `hiqlite_media.rs:3629, 3692`), `put_progress_if_current`, `apply_remote_watch`, `watch_map`, `continue_watching`, `next_up`, `get_item_children` ordering, `files_for_item` ordering — all matched. Error mapping differs in text (`StoreError::Database(String)` from rusqlite vs `database_error`), fine. Structural drift risk is the real issue: ~50 hot statements exist as two hand-maintained copies (positional `row.get(n)` in SQLite vs named columns in hiqlite), and the SQLite copy has no consistency class at all while the hiqlite copy picks one per call site.

#### Q5. membership.rs

17,169 lines: 10,708 lines of implementation (54 inline `*_SQL` constants, ~350 fns/consts) + 6,460 lines of tests from `mod tests` at `:10709`. Contents: join tokens & redemption, heartbeat/reachability, protocol/capability activation (`REMOVAL_ATTEMPT_CAPABILITY`, `NODE_MAINTENANCE_CAPABILITY`, `LEARNER_PROTOCOL_CAPABILITY`, `CACHE_ADMIN_REVOCATION_CAPABILITY`, `:114-128`), voter/learner add–promote–remove, maintenance fence, force-election, artwork-repair leadership, activity-peer auth, storage headroom probe, metrics.

State machine: **not explicit**. Node lifecycle is encoded as rows in `cluster_nodes`, `cluster_node_maintenance`, removal-intent rows, capability strings inside heartbeat transactions, and predicates like `capability_ready_predicate`, `node_is_tombstoned`, `local_node_is_committed_voter`. Roles exist as enums (`ClusterRole:184`, `LocalServingRole:249`, `NodeRole:1524`) but transitions are procedural.

Membership-change safety: delegated to openraft's `change_membership` (joint consensus) via hiqlite's management API; plurx adds one-at-a-time-ness through a replicated removal-attempt fence (`begin_node_removal` → `request_voter_removal` → `reconcile_membership_change` on ambiguous HTTP, `:7926-7969`), refuses leader removal and `voters.len() < 3` (`:7900-7905`), settles offline work before the proposal, and rolls the fence back on definite failure.

Failure detector: two layers. Raft: heartbeat 800 ms, election 2.4–4.0 s (`migration.rs:104-108`). Application: heartbeat row every 10 s, coalesced within 250 ms; "reachable" = heartbeat committed within 30 s; protocol-change absence window 120 s (`membership.rs:60-75`). No suspicion/phi accrual; binary thresholds.

"Fence" means three related things: (a) the monotone `fence: u64` on a `Lease` row that a takeover increments and every fenced publication CAS-checks in the same transaction (`coordination.rs:27-33`, `sqlite/mod.rs:1593-1660 with_fenced_conn`); (b) the removal/maintenance intent rows that block admission before a membership change commits; (c) the process-local *serving fence* — a monotonic loss generation derived from the quorum-watermark lease, which tears down media and fails readiness without a Store call (`serving_fence.rs:1-7`).

#### Q6. Stability

Backup/restore: SQLite mode — Ansible copies the closed `plurx.db` before each deploy, three kept (`OPERATIONS.md:386-390`). Activated cluster — **none** ("There is no automated quorum-aware backup, restore … Capturing current replicated state as a portable backup does not exist", `OPERATIONS.md:351-355, 398-401`). `plurxd wal backup` copies WAL evidence (≤512 MiB) for diagnostics, not the database (`wal_cli.rs:1-20`).
Corruption detection: `quick_check(1)` only on import/pre-import backup; nothing at boot or on a schedule in either backend.
Disk full: SQLite backend surfaces `SQLITE_FULL` as a 500 per call (no handling); cluster — voter readiness requires 512 MiB headroom (`membership.rs:144, 4318-4320`) but snapshot needs a whole extra database copy and a failed snapshot build is fatal to the raft core (F-sc-5). hiqlite's state machine runs `synchronous=OFF` by design and rebuilds from log+snapshot after a crash (`vendor/hiqlite/src/store/state_machine/sqlite/state_machine.rs:574-591`).
Clock jump: lease expiry, takeover and reachability compare Unix timestamps from different nodes; self-fencing uses the monotonic runtime clock (`state.rs:2300-2345`) so a step can never *extend* a local lease, but a forward step on one node makes every peer's 12 s lease look expired to it. Only a documentation requirement exists (`OPERATIONS.md:2140-2147`, "offset above 250 ms … go/no-go").

---

### Findings

#### F-sc-1: In cluster mode the default read path is "every read is a leader round-trip", and the count is growing
- Severity: P1 · Category: performance / architecture · Confidence: CONFIRMED
- Evidence:
  - `vendor/hiqlite/src/client/query.rs:11-20` (hiqlite's own doc on the primitive plurx uses for 225 call sites): "This query is very expensive compared to the other ones. It needs network round-trips, pauses the raft and allocates a lot more memory … You should only use it, if you really need to." `query_consistent` always goes through `query_remote` (`:29-33`) — even on the leader it is a loopback WebSocket — and the server side runs `raft.ensure_linearizable().await?` per call (`vendor/hiqlite/src/query/mod.rs:19-30`).
  - Hot paths on it: `user_for_token` (`hiqlite.rs:3980-3997`) called by `AuthUser` for every request (`http/extract.rs:627-636`); `get_item` (`hiqlite_media.rs:1890-1900`), `get_file` (`:2847-2856`), `files_for_item` (`:2915-2928`), `watch_state`/`watch_map` (`:3560-3599`), `get_setting` (`hiqlite.rs:3453-3461`), `media_session_route` (`hiqlite_sessions.rs:1142-1151`), `get_file_by_path` used per scanned file (`hiqlite_media.rs:2764-2774`, `scan/mod.rs:977`).
  - Count: `rg -c query_consistent crates/plurx-core/src/store/hiqlite*.rs` = 225; local `query_map` = ~40. Plan baseline was "about 85 … and 13" (`docs/cluster/CLUSTER-PERFORMANCE-PLAN.md:101-102`).
  - The mitigation exists but is off and narrow: `CatalogueReader` covers 9 methods (`store/mod.rs:5067-5199`) and `bounded_replica_reads: false` by default (`config.rs:170`); `http/items.rs:162,165,212,218`, `http/watch.rs:33,107,130`, `http/hls.rs:4691,8688` call `state.store.get_item/get_file` directly, bypassing it.
  - Background noise: takeover loop does 2 consistent `get_setting` every 2 s per node with the feature disabled (`media_sessions.rs:4353-4377`); rate-control refresh 1 consistent read every 2 s per node (`transcode.rs:12498, 19683-19690`).
- Why it matters: with N nodes, adding nodes adds zero read capacity and adds followers to every read-index round; every API call pays ≥1 leader RTT + quorum heartbeat before the handler starts, and the `CLUSTER_PAGE_LATENCY_REVIEW.md` incident (multi-second Home/Settings) was exactly this path stuck behind a broken voter. During quorum loss every authenticated request, including login and direct play, 503s after the 5 s retry budget (`hiqlite.rs:180`). The doc promise "every node serves reads" (`ARCHITECTURE.md:18-20`) is not what ships by default.
- Proposed change: (1) Flip `bounded_replica_reads` default to true once the P3 proof is trusted, and route the remaining catalogue reads (`items.rs`, `watch.rs`, `hls.rs` `get_item/get_file`) through `CatalogueReader`. (2) Give `AuthUser` the same treatment already built for `CacheOnlyAdminUser` (`extract.rs:30-105`): a per-node token→user cache keyed by the replicated credential generation with the existing begin/end revocation protocol, TTL ≤ 5 min, revocation-fenced — this is the Kubernetes API-server token-cache / Vault lease-cache pattern, not a "time-only cache" (the plan's own objection at `CLUSTER-PERFORMANCE-PLAN.md:197-200` is about time-only). (3) Settings that gate loops (`CLUSTER_SESSION_TAKEOVER_ENABLED`, rate-control mode) should be read once and refreshed by hiqlite listen/notify or a 30 s local read, not a consistent read every 2 s. (4) Add a lint/test that fails when `query_consistent` sites grow without a `// Authority:` justification comment, like the existing `replicated_store_modules_cannot_bypass_the_timed_client_accessor` test.
- Size: M · Risk: med (auth cache needs the revocation proof wired for non-admin tokens)

#### F-sc-2: Every 10,000 log entries the state-machine writer runs `VACUUM INTO` inline, stalling all writes for the length of a full-database copy — long enough to expire 12 s session leases on a large library
- Severity: P1 · Category: stability · Confidence: LIKELY (mechanism CONFIRMED; duration on your hardware/library unmeasured)
- Evidence:
  - Policy: `NodeConfig::default_raft_config(10_000)` (`cluster/migration.rs:2327`) → `snapshot_policy: SnapshotPolicy::LogsSinceLast(logs_until_snapshot)` (`vendor/hiqlite/src/config.rs:395`).
  - Snapshot runs on the single writer thread, in the same `while let Ok(req) = rx.recv()` loop as every `Query::Execute` (`vendor/hiqlite/src/store/state_machine/sqlite/writer.rs:176, 503-535`) and is `VACUUM main INTO '{path}'` (`:763-767`). openraft's comment says it did not intend this serialization: "BuildSnapshot is a read operation that does not have to be serialized by sm::Worker" (`openraft-0.9.25/src/core/raft_core.rs:1384-1385`).
  - Database size scales with `probe_json` (whole ffprobe document per file, `scan/probe.rs:199-211`): ~10 KB × files. The lab snapshot was already 88,559,616 bytes (`LAB3_SNAPSHOT_CATCHUP_DIAGNOSIS.md:39`).
  - Lease arithmetic: `LEASE_INTERVAL 3 s`, `LEASE_TTL_MS 12_000`, `LEASE_RENEWAL_DEADLINE 4 s`, and renewals are *skipped* once `lease_expires_at_ms <= now + 4_000` (`media_sessions.rs:89-90,133-134,3896-3899`). A write stall of ≥ ~8 s therefore guarantees every session on every node self-fences; the CLUSTER-PERFORMANCE-PLAN lists "snapshot duration dominates write p99" only as a future guardrail (`:1088`).
  - Cadence amplifiers: F-sc-3 (3 entries/s idle) and scans (3 entries per file → a snapshot every ~3,300 new files, ~30 snapshots in a 100k-file import).
- Why it matters: on a 100k-file library (~1 GB) `VACUUM INTO` to the same disk is tens of seconds on spinning/NAS storage and several seconds on SSD; every occurrence is a cluster-wide write outage, and each occurrence also purges the log (`max_in_snapshot_log_to_keep: 1`), so any voter that was restarting at that moment must take a full snapshot install instead of log catch-up (the 15-minute lab3 incident shape).
- Proposed change: (1) Measure: `plurx_raft_snapshot_seconds{operation="build"}` already exists (P2e) — pull it from the lab against a production-sized import before anything else. (2) Raise `logs_until_snapshot` to O(100k–500k) (the raft log costs ~10 KB/entry at worst = a few GB of WAL segments, cheap next to a per-snapshot stall) — this does not touch `max_in_snapshot_log_to_keep`, which the plan says to leave alone. (3) Vendor patch #16: build the snapshot from a *read* connection (`VACUUM INTO` works from any connection and reads a consistent WAL snapshot) so the writer keeps applying — this is what openraft's design expects, and what rqlite does (snapshot from a read txn / backup API off the apply path). (4) Move `probe_json` to a side table or compress it (zstd of ffprobe JSON is ~10×), which shrinks every snapshot, install and backup.
- Size: S for (1)(2), M for (3)(4) · Risk: low for (2); med for (3) (touches vendored writer)

#### F-sc-3: The watched-outbox drain is an unconditional 1 Hz raft write on every voter, forever
- Severity: P2 · Category: performance · Confidence: CONFIRMED
- Evidence: `watched.rs:192-199` — `interval(Duration::from_secs(1))` → `deliver_due()` → `self.store.due_watched(BATCH)` *before* reading `MONARR_URL` (`:206-225`); `hiqlite_durable.rs:702-722` implements `due_watched` as `UPDATE watched_outbox SET claim_until = $1 WHERE id IN (…) RETURNING …` via `execute_returning_map`, i.e. a raft proposal (`vendor/hiqlite/src/client/execute.rs:120-143`) whether or not any row matches; spawned unconditionally at `main.rs:2375-2377`; the gate `may_run_cluster_jobs` is true for **every committed voter** (`membership.rs:5034-5042` → `local_node_is_committed_voter`).
- Why it matters: 3 voters × 86,400 = 259,200 log entries/day of no-ops, each fsynced by hiqlite-wal on all voters, each applied by the single writer, each advancing the 10,000-entry snapshot trigger (≈ one `VACUUM INTO` every 55 minutes on an idle cluster — F-sc-2). Same shape as the P1 "no-op auth activity writes" the plan already removed.
- Proposed change: read-before-write (`SELECT 1 FROM watched_outbox WHERE status='pending' AND next_at <= ? LIMIT 1` — local read is fine, a stale miss only delays delivery one tick), skip entirely when Monarr is unconfigured, back the poll off to 5–10 s when idle, and make it a leader-singleton via the existing job-lease helper instead of every voter. Same audit for `takeover_loop`'s two consistent settings reads per 2 s.
- Size: S · Risk: low

#### F-sc-4: There is no backup or restore for an activated cluster
- Severity: P1 · Category: ops / stability · Confidence: CONFIRMED (documented gap)
- Evidence: `OPERATIONS.md:351-355` "There is no automated quorum-aware backup, restore, or permanent-majority recovery path for an activated cluster. No active milestone owns one."; `:392-401` "those snapshots are not restore points … Capturing current replicated state as a portable backup does not exist." hiqlite's `backup` feature (VACUUM INTO + metadata reset + optional S3, `vendor/hiqlite/src/store/state_machine/sqlite/writer.rs:769-800`) is not in the feature list (`Cargo.toml:38`). The Ansible pre-deploy copy is of the frozen pre-activation `plurx.db`.
- Why it matters: users, tokens, settings, watch state, DVR rules and the whole catalogue live only in three `hiqlite/` directories on the same LAN; ransomware, a bad migration, an operator `rm`, or a three-node power event has no recovery. Every comparable system ships this on day one (`etcdctl snapshot save/restore`, Plex/Jellyfin "copy the DB", rqlite `/db/backup`).
- Proposed change: enable hiqlite `backup`, expose `plurxd backup --output` (leader `Client::backup()` → checksummed `.sqlite` with raft metadata reset, run off the writer per F-sc-2), document restore as "start one voter with `HQL_BACKUP_RESTORE`/import, then re-join the others", and schedule it nightly from the same job-lease singleton. Include the node-local sidecars (`telemetry.rs`) as regenerable.
- Size: M · Risk: low

#### F-sc-5: A failed snapshot build kills the raft node, and the storage floor that is supposed to prevent it is a fixed 512 MiB regardless of database size
- Severity: P2 · Category: stability · Confidence: CONFIRMED (openraft behaviour) / LIKELY (that a real fleet hits it)
- Evidence: `snapshot_builder.rs:107` `let resp = rx.await.expect(…)?;` propagates the `VACUUM INTO` failure as `StorageError::IO`; openraft `raft_core.rs:1381` `let res = command_result.result?;` — a `StorageError` from a state-machine command returns out of `RaftCore`'s main loop (fatal; the node stops participating until restart). `membership.rs:144` `MIN_VOTER_STORAGE_HEADROOM_BYTES = 512 MiB`, used at `:4318-4320`; `VACUUM INTO` needs ≥ the live database size plus WAL.
- Why it matters: a 1–2 GB catalogue with 600 MiB free passes readiness, then dies at the next 10,000th entry; with all three voters on similar disks (shared media pool docs) this can take the whole quorum within an hour.
- Proposed change: make the floor `max(512 MiB, 1.5 × current state-machine DB size)`; refuse/skip the snapshot proactively (openraft lets you defer by returning a retryable path only if you own the builder — another argument for vendor patch #16 in F-sc-2); alert on `plurx_raft_snapshot_seconds{outcome="error"}`.
- Size: S · Risk: low

#### F-sc-6: SQLite backend: ~70 read-only methods (including per-request auth and every Home rail) still serialize on the single writer mutex
- Severity: P2 · Category: performance · Confidence: CONFIRMED
- Evidence: `sqlite/mod.rs:1277-1287` documents the intent (readers off the writer lock) but only 6/24 files use `with_read`. Read-only methods on `with_conn` include: `users.rs` `user_for_token:273`, `get_user:51`, `count_admins:133`; `watch.rs` `watch_state:53`, `watch_map:71`, `watch_rollups:388`, `continue_watching:445`, `next_up:482`; `media.rs` `list_top_items_in_genre:846`, `recently_added:956`, `search_items:1015`, `get_file_by_path:1631`, `files_for_item:1985`, `get_item_children:734`, `child_counts:2027`, `item_media_facts:2067`, `library_file_paths:2142`; `library.rs` `get_library:131`, `list_libraries:144`; `apikeys.rs` `api_key_for_hash`; `cache.rs` `cache_hit`; `offline.rs`, `pretranscode.rs`, `shared_cache.rs` lookups. `user_for_token` additionally issues an `UPDATE … WHERE last_seen_at < unixepoch()-60` (`users.rs:290-294`) so it takes the WAL writer lock at statement start even on the 59 no-op calls per minute. `READ_CONNS = 2` (`:1300`).
- Why it matters: WAL exists so readers never wait for the writer; here a scan (`upsert_file` × N), a `reconcile_library`, a `set_watched_tree`, or an admin FTS rebuild (`rebuild_search_index:2317`) makes every login and every Home load queue behind it, and the std mutex is unfair so tail latency is unbounded during scans. This is the single-node path most users run.
- Proposed change: default `with_read` for every closure with no DML (a clippy-style test can grep the SQL literals as I did); split `user_for_token` into `with_read` lookup + a separate rate-gated activity write (the hiqlite side already does this, `hiqlite.rs:2963-3001`); raise `READ_CONNS` to `min(8, cores)`; open readers with `PRAGMA query_only` as belt-and-braces. Reference: SQLite WAL docs ("readers do not block writers and a writer does not block readers"), Jellyfin's move to a read pool in 10.9.
- Size: S–M · Risk: low

#### F-sc-7: Home/Next-Up/library-page queries are O(catalogue) per request and the planner has never seen statistics
- Severity: P2 · Category: performance · Confidence: CONFIRMED (shape) · LIKELY (magnitude at 1M items)
- Evidence:
  - `recently_added` (`sqlite/media.rs:970-1000`): `ROW_NUMBER() OVER (PARTITION BY CASE … END ORDER BY i.added_at DESC …)` over **all** playable items with two `LEFT JOIN items` and the full 28-column `item_cols("i")` materialized *before* the `LIMIT` — SQLite cannot push the limit through a window; every Home load sorts the whole library (wide rows) into a temp b-tree.
  - `home_preview_pages` (`:914-931`): `COUNT(*) OVER (PARTITION BY library_id)` + `ROW_NUMBER()` over every top-level item, per Home load.
  - `next_up` (`sqlite/watch.rs:488-520`): drives from `items e WHERE e.kind = 'episode'` (no usable index — `idx_items_library_kind` leads with `library_id`, and without `ANALYZE` SQLite will not skip-scan), with a correlated `SELECT COALESCE(MAX(…)) … WHERE se.parent_id = show.id` evaluated per candidate episode and two more `NOT IN`/`IN` subqueries over the user's watch history: O(episodes × watched).
  - `list_top_items_in_genre` (`:884-896`): `COUNT(*)` + `ORDER BY sort_title … LIMIT ?3 OFFSET ?2` with no `(library_id, sort_title)` index → full sort per page, plus `json_each(items.genres)` per row when filtering.
  - Scanner `find_movie` (`:451-470`) `WHERE library_id=? AND kind='movie' AND title = ? COLLATE NOCASE` → scans every movie in the library per new file.
  - No `ANALYZE`/`PRAGMA optimize` in the SQLite backend (`rg` empty); hiqlite's state machine runs `PRAGMA optimize` (`writer.rs:496, 520`) but the plurx SQLite backend never does.
- Why it matters: at 20k items these are ~10–50 ms each; at 200k–1M items (photos/home libraries make this easy) Home becomes seconds and it runs on the writer connection (F-sc-6). All of it is concurrent with per-request auth on the same mutex.
- Proposed change: (a) indexes: `items(library_id, kind, sort_title)`, `items(library_id, kind, added_at DESC)`, `items(library_id, kind, year)`, `items(kind, parent_id)`, `items(library_id, kind, title COLLATE NOCASE)`, `items(tmdb_id) WHERE tmdb_id IS NOT NULL`; (b) rewrite `recently_added` to rank only `(id, added_at, group key)` via `idx_items_added` with a bounded candidate window (e.g. newest 8×limit playable rows, dedupe by show in SQL or Rust, widen on underflow), join `ITEM_COLS` after the `LIMIT` — Jellyfin's "latest" does exactly this; (c) `next_up`: compute the per-show "last watched ordinal" once in a CTE from `watch_state` (user-scoped, indexed) and join episodes to it, instead of the correlated subquery; (d) run `PRAGMA optimize` at close and every few hours and once `ANALYZE` after large scans (SQLite guidance: "applications should run PRAGMA optimize … periodically, e.g. once per day"); (e) keyset pagination for the library page.
- Size: M · Risk: low (all behind the contract suite)

#### F-sc-8: Search scans the whole `classification_fts` table on every query
- Severity: P2 · Category: performance · Confidence: CONFIRMED
- Evidence: `sqlite/media.rs:1021` (mirrored in hiqlite): `SELECT rowid,rank … FROM items_fts WHERE items_fts MATCH ?1 AND rowid NOT IN (SELECT rowid FROM classification_fts)`. `classification_fts` is a plain FTS5 table (`store/classification.rs:19`); `SELECT rowid FROM classification_fts` with no `MATCH` is a full virtual-table scan and the result is materialized per query.
- Why it matters: cost grows with the number of classified items, not with the number of hits; at 100k classified items that is ~100k row decodes per keystroke-search.
- Proposed change: `AND NOT EXISTS (SELECT 1 FROM media_classifications c WHERE c.item_id = items_fts.rowid)` (PK lookup), or keep a boolean column on `items`.
- Size: S · Risk: low

#### F-sc-9: The architecture's "Replicated-ephemeral / Raft KV with TTL" tier does not exist; sessions, heartbeats and leases are durable log entries
- Severity: P2 · Category: architecture · Confidence: CONFIRMED
- Evidence: `ARCHITECTURE.md:97-101` table row "Replicated-ephemeral | Playback sessions …, node membership/health | Raft KV/cache with TTL"; `Cargo.toml:38` `features = ["auto-heal", "macros", "sqlite"]` (no `cache`); `grep -rn "Cache::\|dlock" store/hiqlite*.rs cluster/*.rs` → nothing; renewals write `media_sessions` rows (`hiqlite_sessions.rs:3522-3530`), heartbeats `cluster_nodes` (`membership.rs:83-91`).
- Why it matters: every 3 s renewal and 10 s heartbeat is fsynced on all voters, replayed on restart, included in every snapshot and counts toward F-sc-2's trigger; the doc promises a cheaper class that readers of the design will assume exists. It also means the "seconds of staleness OK" data is served by the most expensive read primitive (F-sc-1).
- Proposed change: either update §2.2 to say "all replicated state is SQLite-durable; ephemeral rows are bounded by retention triggers", or actually adopt the tier (hiqlite `cache` feature is in-tree; openraft memory state machine with TTL) for session routes/heartbeats and keep only ownership fences durable. The former is cheaper and honest; the latter is the design the doc sells.
- Size: S (doc) / L (tier) · Risk: low / high

#### F-sc-10: Cross-node clock skew has no runtime guard although lease expiry and takeover are decided on wall clocks
- Severity: P2 · Category: stability · Confidence: CONFIRMED (absence)
- Evidence: takeover eligibility and renewal compare `unix_ms()` from the acting node against `lease_expires_at_ms` written by the owner (`media_sessions.rs:3888-3899`, `expired_media_sessions(now_ms…)` at `:4384-4388`); `with_fenced_conn` compares against `SystemTime::now()` on the SQLite side (`sqlite/mod.rs:1607-1619`) and against the caller-bound `self.now()` on hiqlite (test `replicated_auth_schema_and_writes_bind_every_clock_value`); reachability = `last_seen_at` vs local now. The only defence is prose: `OPERATIONS.md:2140-2147` "Treat an absolute offset above 250 ms … as a go/no-go failure." `rg -i "skew" cluster/membership.rs store/hiqlite.rs` → none.
- Why it matters: one node stepping +15 s (VM resume, NTP slew-to-step) sees every peer's 12 s session lease as expired and, with takeover enabled, steals every session; owners then fail their CAS renewal and self-fence — a fleet-wide playback restart caused by one clock. A −15 s step delays expiry-based recovery by the same amount.
- Proposed change: each heartbeat already carries `last_seen_at`; compute `skew = local_now − peer.last_seen_at − observed_apply_delay` on read and (a) export `plurx_cluster_clock_skew_ms`, (b) refuse takeover and set readiness=false when |skew| > 2 s (CockroachDB `--max-offset` behaviour: a node exceeding the offset self-terminates). Or timestamp leases with the leader's clock in the state machine (bind `now` on the leader via a SQL `unixepoch('subsec')` default rather than the client's parameter) so all comparisons use one clock.
- Size: S–M · Risk: low

#### F-sc-11: The vendored hiqlite is now a hard fork
- Severity: P2 · Category: architecture / maintainability · Confidence: CONFIRMED
- Evidence: `vendor/hiqlite/PLURX-PATCH.md` lists 15 substantive patches (transport supervisors, snapshot file-ownership boundary, proxy trust boundary, election trigger, quorum-watermark API, metrics…); `git log --since=2026-08-18 --shortstat -- vendor/hiqlite vendor/hiqlite-wal` = **+54,813 / −3,983 lines** against a 35,072-line crate; 51 of 66 commits touching it in the last month are `fix`. Pinned `=0.14.0` and openraft `0.9.25`; the patch file's own exit criterion ("Remove this vendor when an upstream Hiqlite release contains all fifteen patches") is unreachable unless they are upstreamed.
- Why it matters: security fixes and the openraft 0.10 line cannot be taken; `cargo-audit` skips path sources (the doc knows: `scripts/vendor-audit-lock`); every raft-transport bug is now plurx's to find (the fix-commit rate says that is happening). This is the most expensive dependency decision in the repo and it is invisible from `Cargo.toml`.
- Proposed change: pick one explicitly — (a) upstream: open PRs for the generic patches (snapshot RemoteError preservation, WS flush/backpressure, supervisor ownership) and track upstream; or (b) own it: rename the crate (`plurx-raft`), drop the "temporary vendor" framing, add its tests to the fast lane, and budget it. Either way record the decision in ARCHITECTURE §7.
- Size: L · Risk: med

#### F-sc-12: `membership.rs` is a 17k-line procedural lifecycle with no explicit state machine
- Severity: P2 · Category: architecture · Confidence: CONFIRMED
- Evidence: 10,708 implementation lines + 6,460 test lines in one file; 54 inline `*_SQL` constants; node state is inferred from rows + capability strings (`:114-128`) via predicates (`capability_ready_predicate:2372`, `node_is_tombstoned`, `local_node_is_committed_voter:5073`) inside multi-step procedures (`remove_voter:7857-7979` alone does 2 reads, an offline-work settlement, a fence write, a second settlement, an HTTP proposal, an ambiguity reconciliation and a finalize). 45 `fix` commits in 30 days (`git log`), several titled "close the four gaps an adversarial review found", "a finished removal fenced the exclusions it finished with".
- Why it matters: the repeated-fix rate on this file is the clearest stability signal in the area; each new capability string multiplies the implicit state space and nothing enumerates the legal transitions.
- Proposed change: extract a pure `NodeLifecycle { Joining, Learner{ready}, Voter, Maintenance, PendingRemoval{attempt}, Tombstoned }` with a `transition(state, event) -> Result<(state, effects)>` function that is unit-tested exhaustively (the etcd `membership` package / Raft "configuration change" state pattern), and have the manager only persist and execute effects; split the file by concern (join, heartbeat/readiness, change, maintenance, auth) — the SQL constants already cluster that way.
- Size: L · Risk: med

#### F-sc-13: Two hand-maintained SQL copies, and the PR lane only exercises one of them for most store files
- Severity: P3 · Category: architecture / ops · Confidence: CONFIRMED
- Evidence: ~50 hot statements exist twice (e.g. `next_up` `sqlite/watch.rs:488-520` vs `hiqlite_media.rs:3904-3931`); constants diverge in form (`WATCHED_THRESHOLD` vs literal `0.95` at `hiqlite_media.rs:3629, 3692`); positional `row.get(base + 23)` (`sqlite/mod.rs:1174-1211`, with the `ITEM_COL_COUNT` hazard the comment describes) vs named columns. PR-lane routing for the three-voter contract (`validation/points.toml:448-494, 550-561`, `ci_scope.py:395-397`) omits `hiqlite_{classification,dv_conversion,dvr,fragment_index_cluster (224 KB),library_channels,shared_cache,timeline_annotations}.rs` and 20 of 24 `sqlite/*.rs`; they are covered only by the push-to-main `all_scope()`.
- Why it matters: a semantic change to one backend merges green on the fast lane and is caught after merge, which is where 60 `fix` commits on `store_contract.rs` came from.
- Proposed change: add the missing files to `cluster.auth`/`cluster.page-reads` paths (one-line change); longer term generate both backends from one SQL source per method (a `const SQL` + `?n`↔`$n` rewrite already exists in spirit via `validate_sql`).
- Size: S / M · Risk: low

#### F-sc-14: SQLite housekeeping defaults left at SQLite's 2006-era values
- Severity: P3 · Category: performance / stability · Confidence: CONFIRMED
- Evidence: only four pragmas set (`sqlite/mod.rs:1352-1355`); no `cache_size`, `temp_store`, `mmap_size`, `journal_size_limit`; `prepare_cached` used twice; mutex poison is permanent (`:1583-1584`); no `quick_check` at boot; `files` trailing columns after `probe_json` (`:1218-1223` vs schema order `:112-131` + `ADD COLUMN`s at `:168, :171`, v38, v60) so `get_file` decodes past a 10 KB blob (SQLite docs, "Internal Versus External BLOBs": put large columns last, or in a separate table).
- Proposed change: `cache_size=-65536` (64 MiB), `temp_store=MEMORY`, `mmap_size=268435456`, `journal_size_limit=67108864`, `wal_autocheckpoint` unchanged; `prepare_cached` everywhere; `unwrap_or_else(PoisonError::into_inner)`; `PRAGMA quick_check` on boot behind a config flag and weekly; move `probe_json` to `file_probes(file_id PRIMARY KEY, probe_json)` in a new migration (also helps F-sc-2/F-sc-4).
- Size: S · Risk: low

---

### Already good — do not undo

1. rusqlite strictly on `spawn_blocking`, WAL + `synchronous=NORMAL` + `busy_timeout` + `foreign_keys=ON`, STRICT tables (`sqlite/mod.rs:1352-1356`, schema throughout).
2. Migration ladder discipline: append-only, per-migration FK check, idempotency guards for crash-between-commit-and-version (`:1501-1506`), refusal to open newer schemas (`:1470-1474`), the v45/v46 renumbering repair (`:1476-1486`).
3. Progress coalescer (1 durable write / 10 s / stream, bounded map, synchronous first beat and 95 % crossing) and the physical-commit budget test in cluster-check (`progress.rs`, `points.toml cluster.auth` contract).
4. Lease renewals batched into one raft transaction per node per tick; frontier carried in the renewal instead of separate writes (`media_sessions.rs:3870-3927`, `hiqlite_sessions.rs:3522`).
5. Fenced publication: lease CAS and the payload mutation in one SQLite/raft transaction (`sqlite/mod.rs:1593-1660`, `Lease::publication_successor`); monotonic self-fence that a wall-clock step cannot extend (`state.rs:2300-2345`).
6. `json_each(?)` for every ID list (bounded parameter count), no `SELECT *`, `FILE_COLS` excludes `probe_json` and derives `probed` (`:1218-1223`), single-statement page mappers with tests pinning statement counts.
7. Node-local sidecar SQLite for telemetry, network priors and fragment indexes so none of it enters consensus (`store/telemetry.rs`, `hiqlite.rs:3082-3269`).
8. `CatalogueReader` design: bounded-replica reads only under a fresh quorum watermark, revalidated after the query, result discarded on proof loss, kill switch (`store/mod.rs:4949-5065`).
9. `TimedClient` as the only path to hiqlite with a hard 3 s bound, per-class metrics, idempotent-write retry inside the 16 s leader-recovery budget, and the test that forbids bypassing it (`hiqlite.rs:1015-1140, 5741`).
10. The store contract as a backend-neutral `Arc<dyn Store>` suite, and `validate_sql` catching placeholder ordering mistakes at call time.
11. Replay-safe activation/import path (`select_daemon_store`, fsynced markers, `quick_check` on the pre-import backup) and the sole-voter ungraceful-shutdown handling (`cluster/migration.rs:1125-1145`).

### Hot spots (fix commits since 2026-08-20)

| Path | fix / total commits |
|---|---|
| `crates/plurx-core/src/store/sqlite/` (sessions.rs 22, fragment_index_cluster.rs 19, mod.rs 17) | 63 / 131 |
| `crates/plurx-core/tests/store_contract.rs` | 60 / 118 |
| `vendor/hiqlite/` | 51 / 66 |
| `crates/plurx-core/src/cluster/membership.rs` | 45 / 67 |
| `crates/plurx-core/src/store/mod.rs` | 43 / 124 |
| `crates/plurx-cluster-check/` | 34 / 82 |
| `crates/plurx-core/src/cluster/migration.rs` | 32 / 61 |
| `crates/plurx-core/src/store/hiqlite.rs` | 29 / 72 |
| `crates/plurx-core/src/store/hiqlite_sessions.rs` | 26 / 46 |
| `crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs` | 24 / 35 |
| `crates/plurxd/src/media_sessions.rs` | 18 / 40 |
| `crates/plurxd/src/fragindex.rs` | 18 / 37 |

Pattern: the vendored raft transport, membership lifecycle, and the media-session ownership tables (`sqlite/sessions.rs`, `hiqlite_sessions.rs`, `media_sessions.rs`) absorb ~70 % of the fixes — the same surfaces F-sc-2, F-sc-9, F-sc-11 and F-sc-12 point at.

### Open questions (not settleable from code)

1. Actual `plurx_raft_snapshot_seconds{operation="build"}` p50/p99 on the lab voters, and the current state-machine DB size — this decides whether F-sc-2 is a P1 today or a P1 at 5× the library.
2. Is `cluster.bounded_replica_reads = true` set in the Ansible-deployed configs? (Not in the repo; default is false.)
3. Was `may_run_cluster_jobs == every voter` intended for the outbox drain, or should it be leader-singleton like scans?
4. Upstream hiqlite/openraft status: are any of the 15 patches in flight upstream, and what release is upstream on now (verified 0.14 in 2026-07)?
5. Real p99 of `authority_read` vs `local_read` from `prometheus_store_operations()` in production — the metrics exist; the numbers would size F-sc-1 precisely.
6. What `plurx-cluster-check` does **not** prove: a snapshot build during active playback, snapshot build under ENOSPC, clock skew between voters, and any scenario with a >500 MB state machine. Worth adding as named drills before trusting F-sc-2/5/10 are theoretical.

---

## Review — live-tv-and-channels (2026-09-20, main = a1414368)

Scope read end to end: `crates/plurxd/src/live_tv.rs` (11,778 lines; tests start at 7506, so
≈7,500 lines of production code), `live_tv_delivery.rs`, `live_tv/{guide,dvr}.rs`,
`http/live_tv.rs`, `http/internal_live_tv.rs`, `http/library_channels.rs`,
`plurx-core/src/library_channels.rs`, `web/pages/live-tv-controls.js`, the Apple/Android live
players (offset settings only), `docs/ARCHITECTURE.md` §3a/§3b/§8 and the LIVE-TV-*/DVR docs.

Short answers to the six questions first, then the findings.

### Answers to the brief's questions

**Q1 Live latency & start.** The producer is `-f hls -hls_time 1 -hls_list_size 24
-hls_delete_threshold 4` with **no** `-hls_init_time`, no LL-HLS parts, no
`EXT-X-PROGRAM-DATE-TIME`, `independent_segments` only on encode routes
(`live_tv.rs:148-157`, `6044-6049`). The "plays a few seconds then pauses" fix from
`LIVE-TV-START-STALL-AND-TVOS-PLAYBACK-SURFACE.md` **has landed**: uniform 1 s cadence, publish at
≥2 listed segments and ≥2 target durations (`161-163`, `6632-6636`); the constants' doc comment
explains the 4 s cadence jump it removed. Clients choose their own edge: hls.js
`liveSyncDurationCount:2` / `liveMaxLatencyDurationCount:4` (`live-tv-controls.js:97`) → 2 s
behind; AVPlayer default 3×TARGETDURATION with `preferredForwardBufferDuration = 12`
(`LiveTvView.swift:172`) → 3 s; Media3 default 3×target, `setMaxOffsetMs(8_000)`
(`LiveTvPlayer.kt:196`) → 3 s. So the design is now *consistent* but sits below Apple's
authoring guidance (6 s target, ≥3 segments of buffer): every client runs on 2–3 s of buffer.
A second viewer never shares: every start is a new tuner GET + a new ffmpeg
(`start_local_inner`, `3146-3192`); when `held() >= max_sessions` (default **2**, `267-269`)
the answer is 503 `tuner_capacity` "all N plurx Live TV session slots are in use" (`3181`), and
if the HDHomeRun itself is out of tuners it is 503 `tuner_unavailable` (`5587`).

**Q2 Transcode vs passthrough.** Decided by the pure planner `resolve_live_delivery`
(`live_tv_delivery.rs:394-718`): copy only when the client *claims* the exact codec/profile/
size/frame-rate/interlace tuple and HLS packaging; otherwise H.264 encode (hardware first via the
transcode admission, `-hwaccel none` so decode is always software, `live_tv.rs:5962`).
Interlaced: `bwdif=mode=send_field:parity=auto:deint=interlaced` in software (`6135`), never a
hardware deinterlacer. AC-3 copies in MPEG-TS when claimed; AC-3 in fMP4 is forced to AAC to dodge
an hlsenc init-segment race (`live_tv_delivery.rs:613-641`); AAC at 192k/384k ≤5.1 (`6033-6041`).
Captions: `-sn -dn` drops subtitle *streams*; A/53 CC ride as frame side data and the docs
call live captions "unsupported / no end-to-end proof" (`HDHOMERUN-LIVE-TV-STATUS.md:184`);
VideoToolbox gets `-a53cc 0` (`6170-6179`). Guide: HDHomeRun bulk + ≤64 extension pages per
20-min tick, XMLTV ≤4 MiB, durable `guide.json`, 6 h stale TTL (`guide.rs:31-54`).

**Q3 Ownership/fencing.** The owner is configuration; the session polls
`serving.is_current(owner_serving_generation)` every 25 ms (`5478-5485`) and re-reads settings
every second (`ensure_session_fence`, `5469-5475`). Serving authority is a lease with a retained
proof that can only *continue*, never re-grant (`serving_fence.rs:693-716`). I found no path where
two ffmpeg processes read the same tuner GET; the pump task owns the response body and is
aborted+joined before the child is reaped (`5497-5499`, `6306-6360`). Two *nodes* both opening
the device is prevented by `validate_start_config` (owner id + generation + `admission_ready`,
`4996-5026`) and the drain protocol. Owner restart mid-session ends the session; the client gets
410 `capability_expired` "live-TV capability expired" (`4784-4786`, `http/live_tv.rs:1650`) and
the resume path answers `unknown` for a request id the restarted owner never saw. Relay: streaming,
bounded, deadline-driven (`media_sessions.rs:2955-3055`) — but see F-3 (a new HTTP client and a
membership SQL read per relayed segment).

**Q4 Library channels.** Resolution is `div_euclid/rem_euclid` over UTC ms plus a
`partition_point` (`plurx-core/library_channels.rs:984-1022`) — DST-immune, NTP-step-sensitive
only in the obvious way (a backward step replays; a forward step skips). Replacement while a
viewer is mid-item is safe: the viewer holds an ordinary VOD session on the resolved file; the
pending generation activates at the next programme/rotation boundary with a 30 s lead
(`http/library_channels.rs:1914-1963`). Builder claim/renew (120 s / 30 s, renewed between
200-entry batches, publication CASes revision+claim+digest, `1632-1755`) is sound. The
64-generation/32 MiB LRU is sized fine; what is not fine is what each *hit* costs — see F-5.

**Q5 Stability.** Both root causes documented in `LIVE-TV-GUIDE-AND-START-RELIABILITY.md` are
fixed at the root, not the symptom: the guide is persisted (`GuideCache::with_store`,
`2054-2075`), the loop wakes on serving-fence transitions and on settings saves
(`4625-4645`), a cold lineup is fetched from the loop (`4584-4586`), and clients poll on the
owner's `next_refresh_at`. The "wait 90 seconds" client guess is replaced by
`/starts/{id}/resume` and `/starts/{id}/start-state` (`http/live_tv.rs:567-610`).

**Q6 Architecture.** `live_tv.rs` is nine modules in one file (F-9). The Live TV scratch
namespace boundary *is* clean in code: `LIVE_TV_WORK_DIR_NAME = "live-tv"` under the transcode
work root and a test that the VOD orphan sweeper never enters it (`transcode.rs:49`,
`27973-28001`). The duplication with VOD is real but shallow (three HLS playlist parsers, two
bounded-file streamers).

**DVR vs §8.** `docs/ARCHITECTURE.md:533-537` still says "**No DVR, and no scheduler.** Live TV
plays one tuner and keeps nothing"; `LIVE-TV-DVR-STATUS.md` says recording and reminders are
**merged to `main` at `aba14096`** with a 15 s scheduler tick (`live_tv/dvr.rs:45`), a writer
under a DVR root and a `recordings` library. None of the DVR docs reconciles with §8; the
architecture doc also still describes a "6-segment live window" (`ARCHITECTURE.md:277`, `300`)
against 24 in code. See F-10.

---

### Findings

#### F-ltv-1: DVR rule expansion reads the guide through the 2 MiB *response* clipper, so long horizons are silently truncated and the whole guide is cloned + JSON-serialised every 15 s
- Severity: P1 · Category: stability (correctness of recordings) + performance
- Confidence: CONFIRMED
- Evidence:
  - `live_tv/dvr.rs:753-760` (`dvr_expand`) and `848-857` (reconcile) call `self.local_guide(live_tv, GuideWindow { start: now, end: now + DVR_SCHEDULE_DAYS_MAX * 86_400 })` on every `DVR_TICK` (`dvr.rs:45`, 15 s).
  - `local_guide` → `GuideCache::read` → `cached.guide.clipped(&window)` (`live_tv.rs:2278`).
  - `guide.rs:222-279`: `clipped` does `let mut out = self.clone();` (the **entire** cached horizon, all channels), then `for _ in 0..8 { let encoded = serde_json::to_vec(&out)…; if encoded <= MAX_GUIDE_RESPONSE_BYTES { break; } … out.channels[index].programmes.pop() … }` — it **drops the furthest-out rows until the JSON is ≤ 2 MiB** (`guide.rs:54`).
  - The guide is multi-megabyte in practice: `live_tv.rs:2362-2366` "Parsing the document here would have meant a multi-megabyte blocking read"; `GUIDE_MAX_PROGRAMMES_PER_CHANNEL = 800`, synopsis ≤1024 B, image URL ≤512 B per row (`guide.rs:51,57,64`); `MAX_GUIDE_HOURS = 336` (`live_tv.rs:88`).
- Why it matters: with a fortnight horizon (the whole point of `MAX_GUIDE_HOURS = 336` — "what a series recording rule needs", `live_tv.rs:80-88`) and a normal 40–60 channel lineup, the JSON is 10–20 MB, so `clipped` throws away everything past roughly the first 2 MiB **by start time across all channels** before the scheduler ever sees it. A series rule therefore only ever matches the first day or two, and the reconcile pass (`848-874`, "does the guide still carry this exact airing") sees a *populated* guide missing the far rows — exactly the case its own comment says would "withdraw every rule row". Separately, the tick pays: clone of tens of thousands of rows + one full `serde_json::to_vec` of the clone + up to 8 more, twice per tick, while holding the `tokio::sync::Mutex` in `read()` (`2258`) that every public guide request also waits on. Tens of ms of CPU every 15 s on the owner for nothing, and a guide-read stall behind it.
- Proposed change: split the API shape from the scheduler's view. Add `GuideCache::for_each_programme(generation, window, f)` (or return an `Arc<LiveTvGuide>` and iterate by reference) for `dvr_expand`/reconcile; apply the 2 MiB bound only in the HTTP handler. While there, make `clipped` clip per channel by binary search on the sorted `start` (rows are sorted by `normalise_programmes`) instead of clone-then-retain, and measure size with a running byte estimate rather than repeated full serialisation. Add a regression test: 60 channels × 14 days × 48 rows with 512-byte synopses → a rule matching an airing on day 13 must be materialised. Reference: Jellyfin/Emby schedule against the full EPG store, never a paginated API view.
- Size: S–M · Risk: low

#### F-ltv-2: Every live session does a leader-routed (`query_consistent`) read of the whole settings table once per second, and a 6 s raft hiccup ends the viewer's stream
- Severity: P1 · Category: stability + performance
- Confidence: LIKELY (code path confirmed; hiqlite leader-failover duration not measured here)
- Evidence:
  - `live_tv.rs:5469-5475`: `if ticks.is_multiple_of(4) { … ensure_session_fence(&owner, session).await?; }` with `SESSION_TICK = 250 ms` (`134`) → once per second.
  - `ensure_session_fence` (`5554-5565`) calls `manager.config().await?` → `self.store.settings_snapshot().await.map_err(|error| LiveTvError::DeviceUnavailable(format!("reading live-TV settings: {error}")))` (`2740-2747`).
  - hiqlite: `settings_snapshot` = `self.client().query_consistent_map(…"SELECT key, value FROM settings ORDER BY key"…)` (`plurx-core/src/store/hiqlite.rs:3499-3509`), i.e. a linearizable read forwarded to the raft leader, wrapped in `time_authority_read_with_retry` with `STORE_TIMEOUT = 3 s`, `AUTHORITY_READ_TIMEOUT_MAX_ATTEMPTS = 2`, `AUTHORITY_QUORUM_RECOVERY_BUDGET = 5 s` (`hiqlite.rs:173-180`).
  - In the observe loop the `?` at `5474` breaks the `select!` with `Err`, which becomes the session's terminal error (`5488-5506`, `5052-5061`).
  - The same `config()` is also called per public start (`http/live_tv.rs:469`, `539`), per DVR tick, and in the guide loop.
- Why it matters: (a) on a 3-node cluster with the owner not the leader, two viewers cost two cross-node consistent reads per second, forever — the most expensive read class the store has, for a value that changes on a settings save; (b) if the raft leader is unreachable for more than ~5–6 s (leader restart during the fleet's rolling deploy, GC pause, a partition that does **not** involve the owner), every live session on a perfectly healthy owner with a healthy tuner dies with `device_unavailable` "reading live-TV settings: …" — and the user-facing text blames the HDHomeRun. The serving fence already has a separate, better-designed continuation rule for exactly this (`serving_fence.rs:705-709`); the per-second settings read undercuts it.
- Proposed change: the fence needs three facts — `enabled`, `owner_node_id`, `generation`, `admission_ready()`. Publish `LiveTvConfig` through a `tokio::sync::watch` fed by the settings write path / the store's apply hook (the drain protocol already bumps `generation` on every relevant edit), and have `ensure_session_fence` compare against the watched value with no I/O. If a store read must stay, read it from the locally applied snapshot (`query_map`, not `query_consistent_map`) and treat a *read failure* as "unknown, keep going until the serving fence says otherwise" rather than as a terminal error. Reference: tokio guidance on watch channels for config; Jellyfin's live streams are not torn down by a config-store outage.
- Size: M · Risk: med (fencing semantics must be re-argued in the doc)

#### F-ltv-3: The ingress builds a brand-new `reqwest::Client` and runs a membership SQL query for **every** relayed playlist/segment request (2 per second per remote viewer)
- Severity: P2 · Category: performance
- Confidence: CONFIRMED
- Evidence:
  - `http/live_tv.rs:1093` and `1121`: `PeerTransport::new(state.membership.clone()).request(...)` / `.request_stream(...)` inside `owner_resource`, which is the handler for `playlist`, `segment`, `session_status` and `keepalive` (`622-677`).
  - `http/peer_transport.rs:50-58`: `PeerTransport::new` does `reqwest::Client::builder()…build()` — a fresh connection pool each time; nine call sites in `http/live_tv.rs` do this.
  - `owner_resource` → `owner_peer` (`1356-1385`) → `state.membership.activity_peers().await` → `metrics_db()` + a `cluster_nodes ⋈ cluster_node_http` query (`plurx-core/src/cluster/membership.rs:5972-6010`) on every call.
  - Contrast: the VOD relay keeps one transport for the coordinator's lifetime (`media_sessions.rs:1599-1604`: `transport: PeerTransport::new(membership.clone())` stored in `MediaSessionCoordinator`).
- Why it matters: with 1 s segments a remote viewer issues ~1 playlist + ~1 segment per second; each becomes a new TCP (and TLS, if the cluster base is https) handshake plus an SQL read. Three viewers on two nodes ≈ 4 new connections/s and 4 membership queries/s on the ingress for the life of the session, and the per-segment latency the hls.js 2 s buffer has to absorb includes a cold handshake every time. No amplification (one upstream request per downstream request, bodies streamed with bounded channels), but it is the single largest avoidable per-segment cost on the relay path and it is exactly the jitter the tight live buffer cannot afford.
- Proposed change: hold one `PeerTransport` in `AppState` (or in `LiveTvManager`) and reuse it; cache `owner_peer`'s answer per `(owner_node_id, membership epoch)` for a few seconds (the serving fence + `validate_capability_owner` already guard correctness). Reference: reqwest docs ("create one Client and reuse it"), hls.js `fragLoadingTimeOut` budget.
- Size: S · Risk: low

#### F-ltv-4: One tuner and one ffmpeg per *viewer*; two viewers on the same channel cost two tuners and two encodes, and the default ceiling is 2
- Severity: P2 · Category: architecture (capacity) + performance
- Confidence: CONFIRMED
- Evidence:
  - `live_tv.rs:3146-3192`: sessions are keyed by `LiveTvRequestKey { source_node_id, source_serving_generation, user_id, request_id }` (`1253-1268`); the only reuse is a replay of the *same* request id. Every other start inserts a new session and spawns `run_live_session` (`3205-3209`) → new tuner GET (`5201`) → new ffmpeg (`5296`).
  - `max_sessions` defaults to `2` (`267-269`) and is capped at the device's tuner count ≤4 (`2899`).
  - The DVR already has the fan-out primitive: `DvrTransport { sinks: Mutex<Vec<Arc<DvrSink>>> }` — "One entry per channel being recorded, however many recordings share it. A transport is one tuner GET" (`1402-1405`, `live_tv/dvr.rs:65-100`). Viewers do not use it, and a viewer on a channel being recorded takes a *second* tuner for the same mux.
- Why it matters: a household of three watching the same game = three tuners and three H.264 encodes (or a refusal at the third), and a recording of the channel you are watching costs a second tuner. Jellyfin/Emby share one HDHomeRun stream per channel among all clients (`LiveStream.ConsumerCount`, stream sharing on by default for HDHomeRun); Channels DVR likewise. The scarce resource on a FLEX Duo is the tuner, not CPU.
- Proposed change: two steps, both bounded. (1) Share the *transport*: key a `LiveTransport` by `(device_id, channel_id)` exactly like `DvrTransport`, fan the TS out to N ffmpeg stdin pumps, count occupancy per transport (as `held()` already does for recordings) and let a viewer join a channel's existing recording transport. (2) Optionally share the *ffmpeg* per `(transport, LiveDeliveryPlan)` so identical clients also share the encode; capabilities stay per-viewer (the registry already distinguishes capability from session). Keep the "a viewer's own stray goes first" rule per capability.
- Size: M (step 1), L (step 2) · Risk: med

#### F-ltv-5: Library-channel guide and resolve clone the whole generation on every LRU hit and re-validate it O(n); a guide page can do that 1,000 times
- Severity: P2 · Category: performance
- Confidence: CONFIRMED
- Evidence:
  - `http/library_channels.rs:1299-1311`: on a cache hit `let generation = entry.generation.clone(); … return Ok(generation);` — `LibraryChannelGeneration.entries: Vec<ChannelGenerationEntry>` (`plurx-core/library_channels.rs:307-320`), each entry 72 bytes (`823-832`), pool up to `CHANNEL_ELIGIBLE_POOL_MAX = 10_000` (`19`) → ≈720 KB per clone.
  - `resolve_occurrence` calls `validate_entries(entries, loop_duration)?` (`993`, `1024-1047`) — a full O(n) pass — before the `partition_point`.
  - The guide handler's merge loop calls `programme_at → occurrence_at → cached_generation` **once per emitted programme**: `while programmes.len() < CHANNEL_GUIDE_OCCURRENCES_MAX { … streams[index].1 = programme_at(&state, &user, &streams[index].0, next_at_ms).await? }` (`941-963`, cap 1,000 at `library_channels.rs:25`), plus per-channel catch-up loops (`926-937`), each under the global `std::sync::Mutex` (`1296`).
- Why it matters: a 24 h window over 20 channels of 22-minute episodes hits the 1,000 cap; that request memcpy's ≈720 MB and runs ≈10 M `validate_entries` iterations, serialised through one process-wide mutex that every other library-channel request also takes. The ARCHITECTURE §3b claim "a bounded merge over only the requested channel/window" is true of the *output*, not the work. `resolve_now`/`start_session` pay one clone+validate each, which is fine; the guide is the multiplier.
- Proposed change: store `Arc<LibraryChannelGeneration>` in the cache and return the `Arc`; validate once at insert (cache only validated generations) and give `resolve_occurrence` an `unchecked` sibling; in `guide`, resolve the first occurrence per channel and then *advance* by `ordinal + 1` / `cycle + 1` arithmetic instead of re-resolving from `ends_at_ms`. Move the cache into `AppState` rather than a `OnceLock` static so tests can size it.
- Size: S · Risk: low

#### F-ltv-6: Every start spends a fixed 3 s collecting a prefix and ~0.5–2 s ffprobing it before ffmpeg is even spawned, although the channel's source format is already cached for 20 minutes
- Severity: P2 · Category: performance (time-to-first-frame)
- Confidence: CONFIRMED
- Evidence:
  - `live_tv.rs:5201-5204`: `open_tuner_stream` → `collect_live_prefix(response, …)` → `probe_live_source(…)` → only then `resolve_live_delivery` (`5241`) and `spawn_live_ffmpeg` (`5296`). Strictly serial.
  - `collect_live_prefix` runs until `SOURCE_PREFIX_BYTES = 8 MiB` **or** `SOURCE_PREFIX_TIME = 3 s` (`136-137`, loop at `5632-5660`); an ATSC 1.0 mux at ~7 Mbps yields ~2.6 MB in 3 s, so the *time* bound always governs.
  - `probe_live_source` writes the prefix to disk and runs `ffprobe -probesize 8388608 -analyzeduration 2000000` with a 2 s budget (`5791-5856`); then ffmpeg re-analyses the same bytes with `-probesize 524288 -analyzeduration 1000000` (`6106-6108`).
  - The per-channel `source_format` cache exists (`SOURCE_FORMAT_TTL = 20 min`, `61`; `record_source_format`, `2849`) but is used only for the UI (`5205-5228`, `channel_with_source_format`), never to skip the probe.
  - The docs' own numbers: first segment 5.5–7 s on ATSC 1.0 (`6087-6105`), of which 3 s is this prefix.
- Why it matters: the largest single component of channel-change latency is a fixed wait, and it is paid again on every zap even to a channel probed a minute ago. Plex/Jellyfin/Channels start ffmpeg on the tuner URL immediately and let `-analyzeduration` do the work once.
- Proposed change: (a) when a fresh `source_format`/`LiveSourceFacts` for `(generation, device, channel)` exists, spawn ffmpeg immediately with that plan and let the existing stderr watcher (`capture_live_stderr`, `source_format_changed`, `5356-5360`) demote to a re-plan if the mux disagrees; (b) otherwise stop the prefix at "enough": PAT/PMT + one video sequence header/SPS + one audio frame (≈0.5–1 s of a broadcast mux), not a fixed 3 s; (c) cache the full `LiveSourceFacts`, not just the UI subset. Expected win: ~3 s off every start on a warm channel.
- Size: M · Risk: med (a wrong cached plan must fall back cleanly — the machinery exists)

#### F-ltv-7: Encode bitrate is derived from the *source* frame rate, but `bwdif=send_field` doubles the output rate; 1080i sports land at half the intended bits-per-pixel-per-second
- Severity: P2 · Category: video-quality
- Confidence: LIKELY (arithmetic confirmed; ffprobe `r_frame_rate` for 1080i is the frame rate, 30000/1001)
- Evidence:
  - `live_tv.rs:6135`: `"bwdif=mode=send_field:parity=auto:deint=interlaced"` — send_field emits one frame per field, i.e. 59.94 fps from a 1080i29.97 source.
  - `live_video_bitrate_kbps` (`6110-6125`): `let fps = u64::from(rate.num) / u64::from(rate.den).max(1); base.saturating_mul(fps.clamp(24, 60)) / 30` with `rate = plan.source.frame_rate` — 30000/1001 integer-divides to **29**, so 1080p output gets 8000·29/30 ≈ 7,733 kbps and the doubled 59.94 fps is never seen. The same source at a true 60p would get 16,000 kbps.
  - `LiveDeliveryOutput.frame_rate: source.frame_rate` (`live_tv_delivery.rs:705`) — the plan also *reports* the wrong output rate to clients.
- Why it matters: 1080i60 is the dominant ATSC 1.0 sports format; at ≈7.7 Mbps for 1080p59.94 H.264 with a 1 s IDR interval (`6012-6019`, itself a ~10 % efficiency tax) the encode is at the edge where fast motion blocks. Jellyfin's default is send_frame (29.97 p) precisely to avoid doubling the encode cost; if plurx wants the (better) send_field, the ladder must know it.
- Proposed change: compute `output_fps = source_fps × (2 if deinterlace && send_field)` in the planner, use it for both the bitrate ladder and `LiveDeliveryOutput.frame_rate`, and make `fps` a rounded rational (30 for 30000/1001). Consider `send_frame` when `output_height ≤ 720` or when the admitted encoder is software, and `send_field` only for hardware encoders. Reference: Apple HLS authoring §4 bitrate ladder (1080p60 ≈ 2× 1080p30), Jellyfin `DeinterlaceDoubleRate` default false.
- Size: S · Risk: low

#### F-ltv-8: Closed captions are dropped or unverified on the routes that matter, and the fix is mostly flags plus a master playlist
- Severity: P2 · Category: video-quality (accessibility; FCC-relevant for broadcast content)
- Confidence: LIKELY
- Evidence:
  - `live_tv.rs:5992`: `-sn -dn` (subtitle streams dropped — correct for ATSC, captions are not a stream).
  - `6170-6179`: only VideoToolbox is touched (`-a53cc 0`, disabling A/53 SEI insertion); nothing enables `-sei +a53_cc` on `h264_vaapi`, whose default SEI set does not carry captions in every FFmpeg version; libx264/h264_qsv/h264_nvenc default `a53cc=1`, so captions *probably* survive there — unproved.
  - No master playlist: `activate_local` returns a media playlist URL directly (`3294-3301`), so there is no `EXT-X-MEDIA TYPE=CLOSED-CAPTIONS` / `CLOSED-CAPTIONS="cc"` attribute for AVPlayer/Media3 to surface a track.
  - Docs: "DRM and captions unsupported | captions lack end-to-end proof" (`HDHOMERUN-LIVE-TV-STATUS.md:184`).
- Why it matters: every ATSC 1.0 programme carries 608/708; on the encode route (which is *every* MPEG-2 channel on every client, since no HLS client decodes MPEG-2) plurx is one flag away from parity with Plex/Jellyfin/Channels, all of which pass A/53 through.
- Proposed change: add `-sei +a53_cc` for VAAPI and assert `a53cc` on QSV/NVENC/x264 in the graph probe; emit a two-line master playlist with `#EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID="cc",INSTREAM-ID="CC1"` (and `SERVICE1` for 708) and `CLOSED-CAPTIONS="cc"` on the variant; verify hls.js (`enableCEA708Captions` default true), AVPlayer and Media3 (default CEA-608 track from `DefaultTsPayloadReaderFactory`) with one captioned fixture. Revisit the VideoToolbox `-a53cc 0` once the SEI bug is understood.
- Size: S–M · Risk: low

#### F-ltv-9: `live_tv.rs` is nine modules in one 468 KB file; the seams already exist
- Severity: P2 · Category: architecture (maintainability)
- Confidence: CONFIRMED
- Evidence: one file holds config parsing (`201-500`), HDHomeRun documents and URL pinning (`7030-7384`), the lineup `SnapshotCache` (`1770-1936`), the `GuideCache` with persistence (`1937-2395`), the session registry and tombstones (`1398-1620`), session lifecycle (`5028-5507`), ffmpeg command construction (`5914-6194`), scratch inspection and an HLS playlist parser (`6362-6650`), bounded resource streaming (`6653-6950`), capability encoding (`6951-7015`), metrics (`1621-1760`, `4681-4763`) and 4,272 lines of tests (`7506-11778`). 28 of 63 commits touching it since 2026-08-20 are `fix:`; the DVR plan itself warns "`live_tv.rs` moves every week". VOD has its own playlist parser (`transcode.rs:1107`) and segment-name parser (`renditiondir.rs:162`) alongside `parse_playlist_bytes` (`6487`) and `parse_segment_name` (`6638`).
- Why it matters: every reviewer and every agent re-reads 11 k lines to change a constant; conflicts between concurrent branches land here; and the three independent HLS parsers will drift (they already accept different tag sets).
- Proposed change: mechanical split under `live_tv/`: `config.rs`, `device.rs` (discover/lineup/tuner status/pinning), `lineup_cache.rs`, `guide_cache.rs` (+ existing `guide.rs`), `registry.rs`, `session.rs` (`run_live_session*`, timeouts), `ffmpeg.rs` (command + filter + caption args + probe), `scratch.rs` (inventory + playlist parse), `resources.rs` (bounded body, capability), `metrics.rs`, and move tests beside their module. Extract one `hls_playlist` parser into `plurx-core` used by `transcode.rs`, `live_tv` and `renditiondir`. Keep Live TV's *own* reaper/namespace (the doc's reasoning is right; the boundary is clean) — do not merge it into the finite-media sweeper.
- Size: M (split), S (parser) · Risk: low if done as pure moves with `pub(super)` re-exports

#### F-ltv-10: The architecture document contradicts the shipped system on DVR (§8) and on the live window (§3a)
- Severity: P2 · Category: ops / architecture governance
- Confidence: CONFIRMED
- Evidence: `docs/ARCHITECTURE.md:533-537` "**No DVR, and no scheduler.** … Recording would introduce a writer with a schedule, a retention policy, and a conflict resolver … it would put plurx in the business of mutating storage on a timer, which §8's read-only rule exists to prevent." vs `docs/features/LIVE-TV-DVR-STATUS.md:3-5` "merged to `main` at `aba14096`", `live_tv/dvr.rs:45` `DVR_TICK = 15 s`, `LiveTvRegistry.transports` (`1402-1405`). `ARCHITECTURE.md:277` "6-segment live window" and `:300` "Six listed segments" vs `MAX_LISTED_SEGMENTS = 24` / `-hls_list_size 24` (`154`, `167`). None of `LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md`, `-IMPLEMENTATION.md` or `-STATUS.md` mentions §8 or the read-only non-goal.
- Why it matters: §8 is the document agents are told to read "so you know what is deliberate"; it now says the opposite of what the code does, and the second exception to the read-only rule (after DV P7→P8.1) is undocumented as a *decision*. That is how the next agent "fixes" the DVR back out, or adds a third writer without anyone deciding.
- Proposed change: rewrite the §8 bullet as a reversed decision with its date and constraints (DVR root is the only writer; `.part`-per-attempt; no play-while-recording; retention policy owner), add a §7 decision entry, and fix the window numbers in §3a to point at the constants rather than restating them.
- Size: S · Risk: none

#### F-ltv-11: A live session whose scratch cleanup fails is never retired from the registry
- Severity: P3 · Category: stability (slow leak, bounded)
- Confidence: CONFIRMED
- Evidence: `run_live_session` (`5071-5080`): `if let Err(error) = cleanup { tracing::error!(…); return; } manager.retire_session(&session);` — on a `remove_dir_all` failure past `SESSION_DRAIN_TIMEOUT` (`5105-5122`) the `Arc<LiveTvSession>` stays in `registry.sessions` forever. It is excluded from occupancy (`live_sessions()` filters cancelled, `1433-1438`) but still counts toward `registry.sessions.len() + registry.terminals.len() >= MAX_TERMINAL_TOMBSTONES` (`3161`), which refuses *all* starts with "request recovery history is full".
- Why it matters: a slow or briefly read-only scratch filesystem (the deploy habit rebuilds nodes; `/tmp` on tmpfs is fine, a NAS-backed work dir is not) accumulates zombies until the 256 cap refuses every viewer; only a restart clears it.
- Proposed change: always `retire_session`; hand the failed directory to `sweep_orphan_scratch` (`3903`) which already exists to retry.
- Size: S · Risk: low

#### F-ltv-12: The producer watches its scratch with a 250 ms `read_dir` + per-file `symlink_metadata` scan through `tokio::fs`
- Severity: P3 · Category: performance
- Confidence: CONFIRMED
- Evidence: `inspect_scratch` (`6362-6477`) is called every `SESSION_TICK` (`5387`); each `tokio::fs::read_dir`/`next_entry`/`symlink_metadata` is a `spawn_blocking` hop, ~30 files in the window (24 listed + 4 lag + playlist + tmp) → ≈130 blocking-pool dispatches per second per session, plus a playlist read and parse.
- Why it matters: it competes for the same blocking pool as SQLite reads (`with_read`) and is 4× more often than anything can change (segments land once per second). Cheap to fix, and it is per session.
- Proposed change: do the scan in one `spawn_blocking` with `std::fs`, and gate it on the playlist's mtime/len changing (or `notify`/inotify on `index.m3u8`).
- Size: S · Risk: low

#### F-ltv-13: Live playlists carry no `EXT-X-PROGRAM-DATE-TIME` and no `EXT-X-START`, so three clients settle at three different edges and none can report wall-clock latency
- Severity: P3 · Category: video-quality (latency consistency / diagnosability)
- Confidence: CONFIRMED
- Evidence: `LIVE_HLS_OUTPUT_ARGS` (`148-157`) and `hls_flags` (`6044-6049`) contain neither `program_date_time` nor `-hls_start_time_offset`; hls.js at 2×, AVPlayer and Media3 at 3× target (see Q1 evidence).
- Why it matters: Apple's HLS authoring spec recommends PDT for live; it is what lets a client (and the Developer tab) measure "seconds behind air" instead of inferring it, and `#EXT-X-START:TIME-OFFSET=-3` would make the three players agree. Also a prerequisite if LL-HLS parts are ever adopted to get the 2 s latency with a safer 6 s window.
- Proposed change: add `+program_date_time` to `hls_flags` and `-hls_start_time_offset -3` (hlsenc supports both); keep `independent_segments` copy-route omission (open-GOP MPEG-2 makes it unsafe there — correct as is).
- Size: S · Risk: low

#### F-ltv-14: Error text after an owner restart and after a clock-skewed channel publish is misleading
- Severity: P3 · Category: ops (honesty of client messages)
- Confidence: CONFIRMED
- Evidence: an unknown capability answers `CapabilityExpired("live-TV capability expired")` (`4784-4786`) → 410 with `retry: now`; a restarted owner has *no* record of the session, and the web page shows that text verbatim (`live-tv-controls.js:80-91`). Library channels: `ScheduleError::BeforeEpoch` (`library_channels.rs:989-991`, raised when an ingress clock is behind the publishing node's `activation_epoch = now_ms`, `1921-1922`) is mapped to 409 `catalogue_changed` (`http/library_channels.rs:2208-2210`).
- Proposed change: distinguish `unknown` (owner has never seen this capability → "the tuner owner restarted; press Watch again") from `expired`; map `BeforeEpoch` to a retry-after-1 s typed answer.
- Size: S · Risk: none

---

### Already good (do not touch)

1. **Ownership without elections and without timeout takeovers** — `validate_start_config` + `admission_ready` + signed drain acks (`4996-5026`, `1066`, `3492`); the serving fence's "continue on a retained proof, never re-grant" rule (`serving_fence.rs:693-716`) is the right shape.
2. **Atomic playlist+inventory publication** and the deletion-lag accounting (`ScratchInventory`, `6455-6476`, `MAX_DELETION_LAG_SEGMENTS`, `168-171`): a reader never sees a playlist naming a missing segment, and a just-rotated segment is not a false 410.
3. **Startup budget split by evidence** (`startup_overdue`, `5517-5537`): zero tuner bytes → short budget, bytes flowing → producer-progress budget. Measured, documented, and shared by producer and waiter so they cannot disagree.
4. **The 1 s uniform cadence fix** (`140-157`, `161-163`) — the stall analysis in `LIVE-TV-START-STALL…` is correct and landed; do not reintroduce `-hls_init_time`.
5. **Durable guide cache with discard epoch and persist gate** (`2054-2208`), fence-transition wake (`4625-4645`), `next_refresh_at` polling — the "no guide after deploy" root cause is fixed at the root.
6. **Pure, exhaustively-reasoned delivery planner** (`live_tv_delivery.rs`) with client *claims* rather than server guesses; the AC-3-in-fMP4 and AC-4 channel-count workarounds are documented with their cause.
7. **Process hygiene**: `env_clear()` with an explicit allow-list for the GPU stack (`5955`, `6069-6083`), `kill_on_drop`, job objects, pinned device URLs and SSRF-safe guide hosts (`pinned_url`, `approved_guide_url`), every document bounded.
8. **Relay bodies** are streamed through a bounded channel with lifetime and no-progress deadlines, and downstream drop cancels upstream (`media_sessions.rs:2990-3055`).
9. **Bounded VBR everywhere** (`-maxrate 1.5×`, `-bufsize 2×`, `encoder.rs:328-414`) and per-family forced-IDR handling (`forced_idr_flag`, `301-326`) — the "10 s segments from a 2 s request" lesson is encoded.
10. **The Live TV scratch namespace is genuinely separate** from the finite-media sweeper and tested (`transcode.rs:49`, `27973-28001`); Live TV should keep its own reaper.
11. **Same-viewer stray eviction only** (`stray_to_evict`, `1544-1572`) and occupancy that counts recording transports with the holders named in the refusal (`capacity_error`, `http/live_tv.rs:1667-1684`).
12. **Library-channel publication** — claim/renew/stage/fence/CAS (`1632-1755`) with activation at a programme or rotation boundary (`1914-1963`); an interrupted build is never visible.

### Hot spots (fix commits since 2026-08-20)

| Path | commits | `fix` |
|---|---|---|
| `crates/plurxd/src/live_tv.rs` | 63 | **28** |
| `clients/apple/Sources/LiveTvView.swift` | 31 | **12** |
| `crates/plurxd/src/http/live_tv.rs` | 20 | 8 |
| `clients/android/.../livetv/LiveTvPlayer.kt` | 12 | 4 |
| `crates/plurxd/src/live_tv_delivery.rs` | 4 | 3 |
| `crates/plurxd/src/live_tv/guide.rs` | 5 | 3 |
| `crates/plurxd/src/live_tv/dvr.rs` | 7 | 2 |
| `crates/plurxd/src/http/library_channels.rs` | 8 | 2 |
| `crates/plurxd/src/channel_subjects.rs` | 4 | 2 |

The fix titles cluster on start/recovery semantics ("the owner refused fifteen start ids in
sixteen", "a viewer's own stray never refuses them a tuner", "publish from listed media",
"keep one-second HLS cadence") and on audio/codec edge cases (AC-4, 10-bit, AC-3 fMP4, VideoToolbox
A/53). The start protocol has been reworked four times in five weeks (v1 → v2 caps → v3 request
ids → resume/start-state); it is now coherent but is the area to freeze.

### Open questions (not settleable from code)

1. How long does a hiqlite leader failover actually take on the fleet? F-ltv-2's severity is P1 only if it exceeds the ~5–6 s authority-read budget during a rolling deploy.
2. What is the size of `guide.json` on media1 today (`Developer → guide store`)? If it is >2 MiB the DVR truncation in F-ltv-1 is live now, not theoretical.
3. Does ffprobe report `field_order` (not `unknown`) for every ATSC 1.0 mux on the antenna? An `unknown` disables deinterlacing (`live_tv_delivery.rs:213-217`) and would ship combed frames through the encoder.
4. Measured post-fix latency and stall counts on Apple/Android (the `numberOfStalls` pass the stall doc §8 describes) — the 2–3 s buffers are a hypothesis until measured over Wi-Fi.
5. Whether the household actually wants shared viewing (F-ltv-4) or the two-tuner default is a deliberate personal-use ceiling.
6. Whether VAAPI's default SEI set on the fleet's jellyfin-ffmpeg includes `a53_cc` (F-ltv-8) — one `ffmpeg -h encoder=h264_vaapi` on media1 answers it.

---

## plurx architecture review — web-client (2026-09-20, main = a1414368)

Scope read: all 62 `WEB_ASSETS` rows + 8 sidecars under `crates/plurxd/src/web/`
(21,137 lines in the rows, 5,215 in the sidecars, 3,302 in `app.css`, hls.js
1.6.16 vendored), the serving side in `crates/plurxd/src/http/web.rs` and
`http/mod.rs`, `docs/clients/WEB-SHELL-LAYOUT.md`, `docs/PLAYBACK.md`
("Web delivery" and the seek notes), and the last month of `git log` for the
tree (427 commits, 166 of them `fix`).

Headline: the player is far more carefully engineered than a typical
self-hosted media web UI (capability probing, buffer budgeting, ownership
generations, a unit-tested policy sidecar). The weak spots are almost all
*around* it: how the 2.6 MB of assets reach the browser, one library that is
not cache-busted, main-thread demuxing, a seek path that restarts ffmpeg for a
±10 s nudge, and a structure whose only compile-time check is `node --check`.

---

### Findings

#### F-web-1: The whole app ships uncompressed over HTTP/1.1 — 2.6 MB across 71 requests before `boot()` can run
- Severity: P1 · Category: performance · Confidence: CONFIRMED
- Evidence:
  - `crates/plurxd/src/http/web.rs:207-217` — `asset()` sets only
    `CONTENT_TYPE` and `CACHE_CONTROL: public, max-age=31536000, immutable`.
    No `Content-Encoding`, no `Vary`.
  - `crates/plurxd/src/http/mod.rs:629-636` — the only global layer is
    `tower_http::trace::TraceLayer`; `crates/plurxd/Cargo.toml:43`
    `tower-http = { features = ["trace", "fs", "cors"] }` — no `compression-*`
    feature.
  - `crates/plurxd/src/main.rs:2418` — `axum::serve(listener, app.into_make_service_with_connect_info…)`
    on a plain `TcpListener`: cleartext HTTP/1.1. Browsers never negotiate h2
    without TLS, so direct-to-daemon installs get 6 connections × ~12
    sequential requests each.
  - `index.html` — 1 shell + 2 stylesheets + 1 head script + 7 script sidecars
    + 60 body `<script src>` = 71 fetches, all synchronous, `router.js` (which
    calls `boot()`) last. No `<link rel=preload>`, no `defer`, no `modulepreload`.
  - Measured bytes (this tree): body rows 1,339,857; all JS 2,157,852
    (hls.min.js 543,002; playback-policy.js 111,322; decode-tiers.js 98,564);
    app.css 442,759; **total 2,604,589 B raw, 865,197 B gzip -9** (3.0×).
- Why it matters: on a LAN this is invisible; on the WAN/remote case the
  project explicitly supports (`docs/OPERATIONS.md` proxy contract, PWA
  install, phones) a 20 Mbit link spends ~1.1 s on transfer alone before the
  first `/me` request, and every hard reload repeats the non-immutable part
  (see F-web-3). First paint waits for 443 KB of CSS + 21 KB of `core/theme.js`
  (the head script is parser-blocking behind the stylesheet). Caddy and nginx
  do **not** gzip JS by default, so "the proxy will do it" is not the default
  outcome either.
- Proposed change: compress at startup, once, with `flate2` (already a direct
  dep, `crates/plurxd/Cargo.toml:56`): a second `LazyLock<Vec<Vec<u8>>>` beside
  `ASSET_HASHES`, and `asset()` picks the gzip body when `Accept-Encoding`
  contains `gzip`, adding `Content-Encoding: gzip` and `Vary: Accept-Encoding`.
  Same for the sidecars. Do **not** wrap the router in `CompressionLayer` —
  it would touch `/hls/*` and `/stream.mp4` bodies. Add
  `<link rel="preload" as="script">` for the eight largest rows, or (better)
  fold this into F-web-10's module plan. Reference: every mainstream server
  UI (Jellyfin-web, Plex Web) ships gzip/brotli + immutable hashes.
- Size: S · Risk: low

#### F-web-2: `hls.min.js` is the one script that is neither hashed nor `no-cache` — a 7-day version-skew window against app code that subclasses its internals
- Severity: P1 · Category: stability · Confidence: CONFIRMED
- Evidence:
  - `web.rs:228-238` — `hls_js()` returns `Cache-Control: public, max-age=604800`,
    URL `/assets/hls.min.js` with no `?v=` (it is not a `WEB_ASSETS` row, so
    `SHELL`'s tag rewrite at `web.rs:139-149` never touches it). No ETag.
  - `player/player.js:558-564` — `const StockLoader=Hls.DefaultConfig&&Hls.DefaultConfig.loader; … loader:createHlsStartupLoader(StockLoader,startup)`
    subclasses the vendored loader and overrides `readystatechange()`
    (`player.js:440-466`).
  - `player/player.js:52-55` — "verified in the vendored build: `response:{url,data:void 0,code:o.status,…}`";
    `player/directed-change.js:304` — "the bundled hls.js 1.6.16, `subtitleTrack = -1` then back…".
  - `index.html` — every other row is `?v=<sha256[..8]>` + `immutable`.
- Why it matters: the day hls.js is bumped, every browser that visited in the
  previous week runs *new* app rows against the *old* hls.js until its cache
  expires — exactly the mixed-version state the hash scheme exists to
  prevent, on the one library whose private API the app depends on. It is
  also 543 KB re-downloaded weekly by every client for no reason.
- Proposed change: add `hls.min.js` to `WEB_ASSETS` (its `js-check` exemption
  is by name and survives; `asset-order` needs a one-line "vendored, skip
  graph" exemption), so it gets the hash + `immutable`. Keep the bare
  `/assets/hls.min.js` route for the tests that `require()` it by path.
- Size: S · Risk: low

#### F-web-3: The seven sidecars (266 KB) are `no-cache` with no validator — re-downloaded in full on every page load
- Severity: P2 · Category: performance · Confidence: CONFIRMED
- Evidence: `web.rs:241-328` — `cluster_panel_js`, `playback_policy_js`,
  `playback_control_js`, `live_tv_js`, `library_channels_js`, `reader_js`,
  `reader_css` all return `(CACHE_CONTROL, "no-cache")` and nothing else.
  `no-cache` means "revalidate", but with no `ETag`/`Last-Modified` the
  browser has nothing to send, so it issues an unconditional GET and gets
  a 200 with the full body: 111,322 + 57,141 + 35,275 + 33,169 + 19,561 +
  6,307 + 3,978 = **266,753 B per navigation/reload**, uncompressed.
- Why it matters: `playback-policy.js` is the *hot* policy module — it is the
  one file the shell cannot start without, and it is the largest non-vendored
  file in the tree. The comment at `WEB-SHELL-LAYOUT.md §4` explains why the
  URLs must stay (tests, Xcode/Gradle inputs); it does not require the cache
  policy to stay.
- Proposed change: keep the routes, add the same hash-and-`immutable`
  treatment (the shell tag rewrite already handles `"/assets/<path>"`
  patterns; only the `WEB_ASSETS` iteration needs to include them or a second
  table). Minimum viable: emit an `ETag: "<hash>"` and honour
  `If-None-Match` → 304.
- Size: S · Risk: low

#### F-web-4: 196 KB of base64 fonts live inside the render-blocking stylesheet
- Severity: P2 · Category: performance · Confidence: CONFIRMED
- Evidence: `app.css:2` `@font-face{font-family:"JetBrains Mono";…font-display:swap;src:url(data:font/woff2;base64,…)`
  — three data URIs of 51,376 / 51,932 / 92,560 chars (JetBrains Mono ×2,
  Inter) = 195,868 B of the 442,759 B file, i.e. **44 % of the critical CSS**;
  base64 inflates woff2 by 33 % and, being already compressed, gains nothing
  from F-web-1's gzip (app.css gz = 211 KB, most of it the fonts).
  `font-display:swap` is meaningless here: the font bytes must arrive before
  *any* CSS applies.
- Why it matters: first paint of every page load is gated on these bytes;
  the actual style rules are 247 KB raw / ~45 KB gzip.
- Proposed change: serve the three `.woff2` as `WEB_ASSETS` rows
  (`asset_content_type` needs a `font/woff2` arm), reference them by URL, and
  add `<link rel="preload" as="font" type="font/woff2" crossorigin>` in
  `<head>`. Reference: web.dev font best practice; Jellyfin-web ships fonts as
  separate hashed files.
- Size: S · Risk: low

#### F-web-5: Every non-VOD seek re-opens the server session — a ±10 s nudge on a transcode is a new ffmpeg
- Severity: P1 · Category: video-quality / performance · Confidence: CONFIRMED (and acknowledged as open work in the docs)
- Evidence:
  - `player/transport.js:733-737` — the only local-seek branch:
    `if(!forceReopen && (PLAYER.method==='direct_play' || PLAYER.vod)){ v.currentTime=…; return; }`
  - `player/transport.js:762-773` — "Every non-direct seek restarts the server
    stream … `return requestPlaybackMediaChange(PLAYER,{…reason:'seek'…})`".
  - `player/decode-tiers.js:1122-1165` (`executePlaybackMediaChange`) —
    `openSessionRetryingNotYet(...)` → `teardownHls(); resetMediaSource(v); … attachSession(v,p,info,pos)`;
    no check of `v.buffered` or the loaded level's fragment range before
    the reopen.
  - `docs/PLAYBACK.md:1573-1588` — "Apple seeks route by the advertised
    window first … **Web and Android still reopen for every non-VOD seek**;
    adopting the same window routing there is open work."
  - The buffer the reopen throws away is the one the app worked hard to
    build: `bufferTargets()` defaults to `{fwd:60, back:30}` seconds
    (`player/decode-margin.js:265`).
- Why it matters: per the transcode design a session restart is an ffmpeg
  spawn + first-segment latency (the app arms a 20 s
  `HLS_STARTUP.seek_deadline_ms`), and on a GPU it is a session slot churn.
  A viewer tapping "back 10 s" twice — the most common seek there is — pays
  two restarts and drops 30 s of already-buffered picture. Plex, Jellyfin
  (`seekable` range check) and the project's own Apple client all seek inside
  the buffered/published window locally.
- Proposed change: in `seekTo`, before `requestPlaybackMediaChange`, map the
  film-time target through `PLAYER.offset` and seek locally when it lands
  inside (a) any `v.buffered` range or (b) `hls.levels[hls.currentLevel].details.fragments`
  [start, end] minus a 1.5 s live holdback (the same rule as
  `PlayerController.seekRoute` on Apple). Keep the 100 ms coalescing. The
  policy half belongs in `playback-policy.js` so it gets the Node matrix.
- Size: M · Risk: med (interacts with the ownership/generation machinery —
  add it to the `seek-control.test.js` cases)

#### F-web-6: hls.js runs with `enableWorker:false`, so every MPEG-TS transcode segment is demuxed/remuxed on the main thread
- Severity: P2 · Category: video-quality / performance · Confidence: CONFIRMED
- Evidence:
  - `player/player.js:559-560` — `const hls=new Hls({ enableWorker:false, …` and again for the prepared successor at `player/prepared-replacement.js:274-275`.
  - `player/player.js:189-192` — the rationale: "Worker off: a wallet
    extension's injected CSP can silently kill a blob worker; main-thread
    demux of one 720p stream is nothing."
  - `crates/plurx-core/src/transcode/mod.rs:1696-1699` — transcode HLS is
    `-hls_segment_type mpegts`, `seg%05d.ts`: hls.js must run its TS demuxer +
    MP4 remuxer per segment (fMP4 copy segments are a passthrough).
  - hls.js default is `enableWorker:!0` (vendored `hls.min.js`, DefaultConfig),
    and the vendored build already handles the failure the comment fears:
    `try{ … new self.Worker(i) … }` then
    `onWorkerError=function(e){ … a.hls.config.enableWorker=!1, a.hls.logger.warn('Error in "'+a.id+'" Web Worker, fallback to inline') …}`.
- Why it matters: the same main thread runs `playbackProgressTick` every
  500 ms, `updateStats` every 1 s, `pollSessionHealth` every 2 s, the hitch
  detector's `requestVideoFrameCallback` accounting, and any DOM painting
  from watch-and-browse. A 1080p/2160p H.264 TS transcode at 20–40 Mbit is
  not "one 720p stream"; on TV/low-end browsers this is measurable jank and
  it lands squarely in the hitch detector's own numbers.
- Proposed change: drop the two `enableWorker:false` lines (hls.js's own
  fallback covers the CSP case). If a CSP is adopted (F-web-8), serve
  `hls.worker.js` as a row and pass `workerPath`, which also removes the
  blob-URL dependency. Longer term, `-hls_segment_type fmp4` for transcodes
  too (Safari native and hls.js both prefer it and the copy path already is)
  — but that is a server decision outside this area.
- Size: S · Risk: low

#### F-web-7: A single fatal media error skips hls.js's recovery ladder and permanently downgrades the item to a transcode
- Severity: P2 · Category: video-quality · Confidence: CONFIRMED
- Evidence:
  - `player/player.js:756-795` — on any `d.fatal && isMedia` the handler goes
    straight to `PlaybackPolicy.fallbackAction(...)` → `startTranscodeFallback("stream-rejected")`,
    with `PLAYER.triedFallback=true` ("once per item").
  - `player/player.js:762-764` — "hls.js retries nothing further unless the
    app calls recoverMediaError, and a decoder that refused these samples
    refuses them again" — but `grep recoverMediaError|swapAudioCodec` finds
    no call anywhere in the tree; the only occurrence is this comment.
  - hls.js's documented contract (API.md "Fatal Error Recovery"): on
    `MEDIA_ERROR` call `hls.recoverMediaError()` once, then
    `swapAudioCodec(); recoverMediaError()` — because `bufferAppendError`,
    `bufferAppendingError`, `fragParsingError` after a quota purge, and the
    `MEDIA_ERR_DECODE` a tab receives after a GPU process restart are
    recoverable and are *not* codec refusals.
- Why it matters: a transient append error on a 4K HEVC copy-HLS session
  becomes "re-encode this film at the Auto rung for the rest of it", which
  is precisely the silent quality loss the whole capability-probing effort
  exists to avoid. The distinction the code needs already exists in hls.js:
  `bufferIncompatibleCodecsError` / `manifestIncompatibleCodecsError` are
  the genuine "decoder refuses these samples" details.
- Proposed change: before the transcode rescue, if
  `d.details ∉ {bufferIncompatibleCodecsError, manifestIncompatibleCodecsError}`
  and `!p.hlsMediaRecovered`, set the flag and call
  `hls.recoverMediaError()` (second occurrence → `swapAudioCodec()` first),
  bounded to one attempt per attach so the existing "once per item" budget is
  untouched. Log it as `player_event` so the stall-diagnosis ledger sees it.
- Size: S · Risk: low

#### F-web-8: No CSP or security headers on the shell, 398 inline event handlers, bearer token in `localStorage`
- Severity: P2 · Category: security · Confidence: CONFIRMED
- Evidence:
  - `web.rs:197-199` — `index()` sets only `Cache-Control: no-cache`. The
    only CSP in the daemon is on `connect.svg` (`web.rs:347`) and the EPUB
    publication frames (`http/publication.rs:47,390-394`, which do it well).
    `grep nosniff|X_FRAME|frame-ancestors|Referrer-Policy` in `http/` finds
    nothing for the app.
  - Inline handlers: `grep -o 'on(click|change|input|…)='` over the rows and
    `index.html` = **398** (357 `onclick=`), e.g. `index.html` `onclick="toggleStats()"`,
    `core/cards.js` cards emitted as `<div onclick>` (see `core/keyboard-reach.js:3-6`).
    Any `script-src` without `'unsafe-inline'` breaks the app.
  - `core/app.js:12` — `let TOKEN = … localStorage.getItem("plurx_token")`;
    `core/auth.js:119,135` — `localStorage.setItem("plurx_token",TOKEN)`.
  - `core/api.js:70` — `const tok = u => u + … "token=" + encodeURIComponent(TOKEN)`
    puts the bearer in every `<img src>` / `background-image:url()` — 15+
    call sites (`core/lightbox.js:7`, `layouts/theater.js:180`, …).
  - Mitigations that *are* present: `esc()` at `player/measurements.js:670`
    is used consistently — an AST-assisted scan of every HTML template
    literal for interpolated `.title/.name/.message/...` without `esc()`
    found none reaching `innerHTML` (the hits were all `textContent`, `toast`,
    or already-escaped helpers such as `cluster-panel.js:480 const name=n=>esc(…)`).
    `safe_trace_target` (`mod.rs:625-636`) keeps `?token=` out of spans.
- Why it matters: 124 `innerHTML` sites and 398 inline handlers mean the
  app's XSS safety rests entirely on nobody ever forgetting `esc()` — and
  `ae948000 fix(web): accept numeric settings in HTML escaping` shows the
  helper itself has needed fixing. Without a CSP a single miss yields the
  bearer (and, via the `?token=` images, it can be exfiltrated with a
  `<img>`, no script needed). There is also no `frame-ancestors`, so the
  app can be framed.
- Proposed change (in order): (1) today, at zero risk: add
  `X-Content-Type-Options: nosniff`, `Referrer-Policy: same-origin`,
  `Content-Security-Policy: frame-ancestors 'none'; base-uri 'none'; object-src 'none'`
  to `index()` — that CSP is valid without touching script-src. (2) Move the
  398 inline handlers to one delegated listener keyed on `data-action`
  (the `nav-keyboard-adapter` already proves the pattern) and then ship
  `script-src 'self'` (+ `worker-src 'self'` with F-web-6's `workerPath`).
  (3) Consider an HttpOnly, SameSite=Strict cookie for browser sessions,
  keeping the bearer path for native/API clients; the `?token=` image URLs
  then become plain `/images/…` and the poster cache stops being per-login.
- Size: S for (1), M for (2), L for (3) · Risk: low / med / med

#### F-web-9: No global error reporting — after the split, a load-time throw is silent and nothing reaches `/client-log`
- Severity: P2 · Category: stability · Confidence: CONFIRMED
- Evidence:
  - `grep 'window.onerror|addEventListener("error"|unhandledrejection'` over
    the tree → only `player/transport.js:527` (`v.addEventListener("error"…)`,
    the `<video>` element).
  - `docs/clients/WEB-SHELL-LAYOUT.md §1.5` — "**A load-time throw is no
    longer fatal.** … the app boots with one row's `let`/`const` permanently
    in the temporal dead zone: a half-working UI where there used to be a
    loud failure. `tests/web/asset-load.test.js` rethrows, so it sees the
    throw; a browser will not."
  - `core/api.js:114-130` — `clientLog()` exists and is fire-and-forget, but
    only playback code calls it.
  - `router.js:81-109` — `render()` catches its own route errors and paints
    `e.message`, but a rejection from a timer callback (`setPageTimer`,
    `ACT_TIMER`, `pollSessionHealth`) or from a `MutationObserver` never
    reaches it.
- Why it matters: the owner reads Settings → Logs on a headless box; a
  ReferenceError from a browser he does not have (Firefox TV, an old iPad)
  is invisible unless someone opens devtools. It is also the cheapest
  possible detector for the exact failure class the split introduced.
- Proposed change: in `core/api.js`, `window.addEventListener("error")` and
  `("unhandledrejection")` → `clientLog({level:"error", event:"js_error", message, stack.slice(0,2000), route:location.hash})`
  with a per-session cap (e.g. 20) and dedupe by message. Plus a boot
  sentinel: `router.js` sets `window.__plurxBooted=true`; `index.html`
  carries a 5 s `setTimeout` that, if it is not set, shows a plain-HTML
  banner ("The app failed to start — reload") and posts the last
  `window.onerror`. Reference: what every SPA framework's default error
  boundary does; Jellyfin-web's `window.onerror` → server log.
- Size: S · Risk: low

#### F-web-10: The structure's real cost is measurable — and the cheapest fix is a type checker, not a bundler
- Severity: P2 · Category: architecture · Confidence: CONFIRMED
- Evidence (what the structure is):
  - 62 rows + 8 sidecars, one global scope, load-order dependence enforced by
    `tests/web/asset-order.test.js` (vendored acorn, `tests/web/asset-graph.js:29`),
    `tests/web/asset-load.test.js` (vm stub), and a `web.rs` unit test. No
    `package.json`, no ESLint, no TypeScript, no JSDoc types; the only
    compile-time gate is `scripts/js-check` (= `node --check`, syntax only).
  - 1,190 top-level functions; the ten longest include `play()` at
    `player/decode-tiers.js:606` (**479 lines**), `attachHls` at
    `player/player.js:499` (**378**), `armHitchDetector` (223),
    `persistentWait` at `player/measurements.js:358` (213).
  - Cross-file `typeof` guards that changed meaning with the split
    (`WEB-SHELL-LAYOUT.md §1.5`): `detail/dynamic-range.js`'s `typeof PLAYER`,
    `player/decode-tiers.js`'s `typeof LIVE_TV_LEASE`,
    `pages/settings-live-tv.js`'s `typeof PAGE_RENDER_GENERATION`.
- Evidence (what it costs — the last month's `fix` commits under the tree):
  166 of 427 commits are fixes. Mapping fix hunks to the function they land
  in (pre-split `index.html`, `git show -U0 | grep '^@@'`):
  `play` 62, `persistentWait` 47, `attachHls` 44, `seekTo` 23,
  `commitPreparedReplacement` 23, `watchLiveTv` 21, `startPlaybackControl` 21,
  `paintAnalysis` 21, `wirePlayer` 20, `stallDiagnose` 20, `clusterNodeRow` 19.
  The dominant recurring class is **ownership / generation races** — 35
  fix subjects match `race|generation|owner|supersed|stale|bind|scope|fence|intent`
  (`9a5652e6 bind held seek recovery to current playback`,
  `e4761dde scope playback terminal ownership`, `78223cef fence presentation
  and control generations`, `ca0913c1 preserve composed playback and
  attachment intent`, `0a70d6fb bound preparation and isolate retained media
  observers`, `b217c615 bind frame callbacks to media load and seek lifecycle`,
  `893ec4fd call the default timers on the global, not the reporter`, …).
  These are exactly the bugs a shared mutable `PLAYER` bag with ~40 ad-hoc
  fields (`_seekToken`, `controlIntentGeneration`, `mediaAttachment`,
  `hlsStartup`, `prepared`, `pendingMediaChange`, `terminalStop`, …) and no
  type on any of them produces: every guard is a hand-written conjunction
  (`player.js:555-557`, `measurements.js:360-364`) that must be re-derived
  at each new site. The second class is **wrong-name / wrong-arity at a
  distance** (`893ec4fd`, the `typeof` traps, `ae948000`), which is what a
  checker catches for free.
- Why it matters: 45 of the 427 commits are literally titled "address
  adversarial review findings" — the review loop is doing the job a static
  check would do before the PR opens.
- Recommendation (one path, with trade-offs), keeping "no build step in the
  release binary":
  1. **Now (S): `tsc --noEmit --allowJs --checkJs` in CI, no source edits.**
     A `jsconfig.json` listing the 62 rows in served order with
     `"lib": ["dom", "es2022"]`, `"module": "none"`. Because the files are
     *scripts* (no import/export), TypeScript already models them as one
     shared global scope — exactly the runtime semantics — so cross-file
     names resolve and misspelt properties on `PLAYER` are errors. Start
     with `strict:false, noImplicitAny:false` and a `// @ts-nocheck` on the
     worst three files, then ratchet. Add a JSDoc `@typedef Player` for the
     `PLAYER` bag and the generation fields; that single typedef is where
     most of the race-class bugs become visible. Trade-off: it needs a
     `typescript` download in CI (or vendor it like acorn, ~9 MB); it does
     not change what ships. ESLint flat config with `no-undef`,
     `no-unused-vars`, `no-implicit-globals` in the same job.
  2. **Not now: ES modules / importmap / esbuild.** Converting 1,190
     functions to explicit imports is L, the docs' own warning
     (`WEB-SHELL-LAYOUT.md §3`: "Do not add export/import") is correct while
     the ordering gates are the safety net, and a bundle would undo the
     per-file hashing that already works. The only ESM-shaped benefit the app
     lacks — parallel dependency discovery — is answered by F-web-1's
     compression + preload for a fraction of the risk.
  3. **Next (M), targeted:** split `play()` and `attachHls()` along the
     seams their own comments draw (route decision / session open / attach /
     event wiring), because those two functions took 106 fix hunks between
     them; and make the ownership predicate one function
     (`isCurrent(player, attachment, intentGeneration)`) instead of the
     five-clause conjunction copied at each site.
- Size: S (1) + M (3) · Risk: low

#### F-web-11: Library grids repaint the whole visible page per 200-item batch and have no size variants for posters
- Severity: P3 · Category: performance · Confidence: CONFIRMED
- Evidence: `layouts/library-grids.js:24-49` fetches *every* page
  sequentially (`for(let offset=0;; offset+=LIB_PAGE)`), and `draw()` at
  `:123-159` does `body.innerHTML=layoutRegion("library","items",m)` on each
  arrival; with `LIB_PER="all"` (`layouts/renderers.js:120-123`) a
  3,000-title library rebuilds a growing grid 15 times (≈22k `card()`
  strings). Filtering (`LIB_FILTER`, `LIB_FIND`) is client-side over the
  full fetched set. Cards are `<img loading="lazy">`
  (`player/measurements.js:677`) — good — but there is one poster URL for
  every `--poster` size 120–226 px (`core/theme.js:162`), no `srcset`,
  no `decoding="async"`, no `content-visibility`.
- Why it matters: only "All" is bad, and it is opt-in; the default (100 per
  page) is fine. The image side matters more on phones: the 4K-poster PNG
  the scanner cached is what a 120 px card downloads.
- Proposed change: only re-render the appended batch (`insertAdjacentHTML`)
  while `LIB_PER==="all"`; add `decoding="async"`; let `/images/{filename}`
  take `?w=` (server-side resize is in the images area) and emit `srcset`.
- Size: S–M · Risk: low

#### F-web-12: Progress heartbeat every 5 s continues while paused; every signed-in tab polls `/activity` every 4 s
- Severity: P3 · Category: performance · Confidence: CONFIRMED
- Evidence: `player/decode-margin.js:517-521` — `p.timer=setInterval(()=>{ … reportProgress(p.fileId); … }, PlaybackPolicy.AUTO_DEFAULTS.sampleMs)` with `sampleMs: 5_000`
  (`playback-policy.js:15`); `player/stats.js:463-480` `reportProgress` has
  no paused check — a film paused overnight POSTs the same position ~17,000
  times (the server coalesces, `crates/plurxd/src/progress.rs:1`, so it is
  request cost not write cost). `router.js:61` `ACT_TIMER=setInterval(pollActivity,4000)`
  on every route; it *does* skip when hidden (`core/activity-indicator.js:24`) — good.
  No SSE/WebSocket anywhere (`grep EventSource|WebSocket` → none); all
  freshness is polling (Activity page 3 s, Settings 2 s, analysis 3 s, DVR
  reminders 30 s, live-tv stats 1 s).
- Proposed change: skip `reportProgress` when `v.paused && !ended` and the
  position has not changed since the last send (send once on pause); 10 s
  cadence matches Plex/Jellyfin. Polling → SSE is a larger change and not
  worth it for a household-scale server; leave it.
- Size: S · Risk: low

#### F-web-13: No MediaSession, no hardware media keys, no TV back-key codes, no Remote Playback API
- Severity: P3 · Category: UX (TV/keyboard) · Confidence: CONFIRMED
- Evidence: `grep navigator.mediaSession|MediaPlayPause|MediaTrackNext|remote.prompt|RemotePlayback` → none.
  `player/stats.js:586-613` maps Escape/arrows/Enter/Space/j/k/l/i/f/c/Home/End
  only; the Tizen (`keyCode 10009`) and webOS (`461`) Back keys are unmapped
  (`grep 10009|461` → none), so on those TV browsers Back does nothing and
  the platform exits the app. AirPlay is wired only through
  `webkitShowPlaybackTargetPicker` (`player/decode-tiers.js:1322`) and only
  for direct play by design (`player/player.js:25-32`); PiP is present
  (`player/transport.js:344,377`).
- Why it matters: Bluetooth remotes, keyboard media keys, Android Chrome's
  notification controls and macOS Now Playing all come from
  `navigator.mediaSession` — it is ~20 lines and covers every "10-foot"
  browser the project is likely to meet.
- Proposed change: set `mediaSession.metadata` from `PLAYER.meta` and
  `setActionHandler` for play/pause/seekbackward/seekforward/seekto/nexttrack
  onto the existing `togglePlay/nudge/seekTo/playNextEpisode`; add
  `keyCode 10009|461|"BrowserBack"|"GoBack"` to the player and Live TV
  input adapters as `back`. `video.remote.prompt()` for Cast is optional.
- Size: S · Risk: low

#### F-web-14: Capability probe over-claims on two axes: unconditional H.264 with no ceiling, and a hard-coded container list
- Severity: P3 · Category: video-quality · Confidence: LIKELY
- Evidence: `player/decode-tiers.js:183` `const vc=["h264"];` and `:221`
  `const containers=["mp4","webm","mov","m4a","m4b","mp3","aac"];` are
  asserted, not probed; `maxheight` is HEVC-only by design (`:227-231`).
  Everything else (`hevc/hevc10` tiers via `decodingInfo` with width/height/
  bitrate/`transferFunction:'pq'`, DV `dvh1/dvhe` per profile, `ac-3/ec-3`
  via `canPlayType`) is probed carefully and does not over-claim.
- Why it matters: a TV browser or old tablet with a 1080p-only H.264
  decoder is offered 4K H.264 direct play; Safari < 14.1 and some embedded
  WebKits are told `webm` is fine.
- Proposed change: one `decodingInfo({video:{contentType:'video/mp4; codecs="avc1.640033"',width:3840,height:2160,bitrate:25e6,framerate:24}})`
  for H.264 to set a per-codec ceiling (the caps document already supports
  per-codec `max_height`, `:298-320`), and `canPlayType('video/webm')` for
  the container list.
- Size: S · Risk: low

#### F-web-15: Classic layout's poster hover animates `transform` regardless of `prefers-reduced-motion`
- Severity: P3 · Category: accessibility · Confidence: CONFIRMED
- Evidence: `app.css:147` `.poster{…transition…}` (unconditional);
  `app.css:2174-2176` and `:3076-3078` — catalog and theater explicitly opt
  out ("The shipped .poster rule animates `transform` unconditionally and
  theater cannot edit it, so it opts out here"). The base rule is the
  bug those two are working around.
- Proposed change: wrap the base `.poster` transition in
  `@media (prefers-reduced-motion: no-preference)` and delete the two
  overrides.
- Size: S · Risk: low

---

### Answers to the brief's questions (evidence summary)

**1. Player.** hls.js **1.6.16** (`hls.min.js` `var ca="1.6.16"`). Config
actually passed (`player/player.js:559-593`): `enableWorker:false`,
`maxBufferLength:tgt.fwd` (60 s unbudgeted; a byte budget against a
144 MB assumed MSE quota for copy-HLS, `decode-margin.js:211,257-305`),
`backBufferLength:tgt.back` (30 s), `maxBufferSize:tgt.fwdBytes` when
budgeted, a subclassed `loader`, `manifestLoadPolicy` (TTFB 10 s, load 12 s,
1 timeout retry, 7 error retries 1→4 s; `playback-policy.js:927-947`),
`fragLoadPolicy` (TTFB 10 s, 120 s load, 4 timeout / 7 error retries 1→8 s;
`player.js:194-213`), `startPosition`, `xhrSetup` (Bearer header),
`hls.bandwidthEstimate` seeded across instances. Everything else is hls.js
default (`maxMaxBufferLength` 600, `liveSyncDurationCount` 3, `abrEwma*`,
`startLevel`, `capLevelToPlayerSize` false, `lowLatencyMode` true,
`progressive` false). Live TV (`pages/live-tv-controls.js:97`):
`{liveSyncDurationCount:2, liveMaxLatencyDurationCount:4, maxBufferLength:12, maxMaxBufferLength:18, backBufferLength:0}`.
**ABR is not hls.js's**: the master has one `EXT-X-STREAM-INF`
(`http/hls.rs:12415`) plus subtitle renditions; "Auto" is an app-level
controller that restarts one transcode (`docs/PLAYBACK.md:1589-1593`) and,
for viewer-directed switches, builds a *prepared successor* on a second
hidden `<video id="video-prepared">` and commits it
(`player/prepared-replacement.js:165-313`) — no `nextLevel/loadLevel`.
MSE vs native: `useNativeHls()` gates on
`WebKitPlaybackTargetAvailabilityEvent`, not `canPlayType` (`player.js:11-24`);
native HLS is reserved for copied HEVC on Safari (`player.js:25-49`).
Capability probing: `navigator.mediaCapabilities.decodingInfo` per HEVC tier
and PQ (`decode-tiers.js:150-178`), sync `canPlayType/isTypeSupported`
ladder fallback, DV `dvh1.05.06`/`dvh1.08.07`, `ac-3`/`ec-3`/`opus`/`flac`
via `canPlayType`; `maxheight` is the HEVC min-of-profiles ceiling, and
`playerPixelHeight()` (`decode-tiers.js:530-538`) caps the Auto rung to the
layout height × DPR (not the screen). Stalls: `waiting` → `armWait` →
`persistentWait` at 8 s (`measurements.js:238-240,358`), classified by
`v.error.code` / runway < 1.5 s; hls.js `bufferStalledError` counted at
`player.js:727-742`; recovery is a session reopen through
`requestPlaybackMediaChange`, never `recoverMediaError` (F-web-7). Seek:
local only for direct play / VOD, otherwise a server reopen (F-web-5).
Subtitles: native `TextTrack` (browser renders): `<track src=/subs/N>` when
offset = 0, else the whole VTT is fetched, parsed (`vttParse`) and re-added
shifted into one reusable script track (`audio-sync.js:140-173`); HLS
sessions use hls.js `subtitleTrack` renditions; bitmap subs → burn-in
transcode. Audio switch = session reopen with `audio=<index>`. PiP yes,
AirPlay direct-play only, no Cast, no MediaSession (F-web-13). Keyboard: a
routed input table (`player-input-contract`) with arrows/space/j/k/l/i/f/c;
no TV back-key codes.

**2. Performance.** 71 synchronous requests, 2.6 MB raw / 0.87 MB gzip,
served uncompressed over HTTP/1.1 (F-web-1); hashed+immutable for 62 rows,
`no-cache`-no-validator for 7 sidecars (F-web-3), 7-day unhashed hls.js
(F-web-2); fonts inline in CSS (F-web-4); no preload hints. Libraries: paged
200/request, all pages fetched, page-sized DOM (default 100), `loading=lazy`
posters, no `srcset`/`decoding=async`/virtualisation (F-web-11). Pollers:
`/activity` 4 s global (hidden-aware), DVR reminders 30 s, Activity page 3 s,
Settings 2 s, analysis 3 s, library-channels 30 s, live-tv stats 1 s, player
progress tick 500 ms (local), sampling/heartbeat 5 s (POSTs progress,
F-web-12), stats overlay 1 s + health 2 s only while open, session-health
poll 2 s idle-guarded; one `PAGE_TIMER` at a time, cleared on route
(`router.js:47-56`) — good. Leaks: document/window listeners are wired once
behind flags (`NAV_KEYS_WIRED`, `LIVE_TV.hostWired/slotWired`,
`CATALOG_WIRED`); I found no per-render global listener. `localStorage`:
17 small keys (token, theme, layout, sizes, decode limits) — negligible.

**3. Architecture.** See F-web-10.

**4. Stability.** No global error handler (F-web-9). Router is hash-based,
`hashchange → render()`, with a `PAGE_RENDER_GENERATION` counter and
`location.hash` re-checks after every await (`router.js:59-109`,
`detail/preplay-selection.js:596-605`); scroll restore is consume-once
(`router.js:104-106`); deep links cold-boot through `boot()`. Overlapping
fetches are *not* aborted (only `pages/analysis.js`, `library-channels.js`
and `playback-control.js` use `AbortController`) but stale results are
discarded by generation; the server still does the work. Token in
`localStorage`, `Authorization` header for `fetch` and hls.js XHR,
`?token=` for `<img>`/`<video>`/native HLS (F-web-8). 124 `innerHTML`
sites; escaping is consistent (my scan found no unescaped server string
reaching HTML). No CSP on the shell (F-web-8).

**5. Accessibility / TV.** Custom transport has `role=slider` + `aria-value*`
on the scrubber, `aria-haspopup/expanded` on menus, `aria-live` status
regions (`index.html`), a focus trap and focus return in the player
(`player/stats.js:621-626,738-750`), `focus-visible` rings across 60 rules,
a WCAG contrast gate (`scripts/contrast-check`), `forced-colors` handling in
catalog/theater, and reduced-motion opt-outs in two of three layouts
(F-web-15). Cards are `<div onclick>` promoted to `role=link tabindex=0` by
a MutationObserver (`core/keyboard-reach.js`) — works, but real `<a href>`
would give TV spatial navigation and middle-click for free.

---

### Already good (do not undo)

1. Content-hashed, `immutable` asset rows with a `no-cache` shell and a
   hard 404 for unknown `/assets/*` (`web.rs:124-225`).
2. The three ordering gates — `asset-order` (real parser), `asset-load`
   (vm), `web_assets_match_the_shell` — and `WEB-SHELL-LAYOUT.md` as the
   map. This is why a 24k-line split shipped without a regression.
3. `useNativeHls()` keyed on the WebKit AirPlay API, not `canPlayType`
   (`player.js:11-24`) — the Chrome "maybe" trap is real.
4. MediaCapabilities-based HEVC tiering with `transferFunction:'pq'` and the
   rule "`hdr10t=0` is not proven, never proven false"
   (`decode-tiers.js:138-178,260-270`).
5. `startPosition` passed at construction, `bandwidthEstimate` carried
   across instances, `teardownHls()` before every re-attach so no orphaned
   instance keeps polling a playlist (`player.js:504-519,576,594-604`).
6. Buffer budget expressed in seconds with `maxBufferSize` moved in lock-step
   and `segBytes` measured from `FRAG_LOADED` (`decode-margin.js:257-323`).
7. The typed-refusal plumbing: reading the 503 body at the loader because
   hls.js discards it (`player.js:50-131,440-466`), stamped and per-attach.
8. Generation-guarded rendering and hidden-aware pollers; one page timer.
9. `esc()` discipline, `safe_trace_target` redaction, Bearer header (not
   query) for hls.js.
10. Player focus trap / focus return, `aria-*` on the custom transport, the
    contrast gate, and the policy sidecars (`playback-policy.js`,
    `playback-control.js`) being pure and Node-tested.

### Hot spots (last month, `fix` commits / hunks)

Files (`git log --since=2026-08-20 --format=%s -- <path> | grep -c '^fix'`;
per-file history mostly predates the 2026-09-19 split):
`index.html` 138 of 299 commits · `playback-policy.js` 22/34 ·
`cluster-panel.js` 22/28 · `playback-control.js` 7/14 · `live-tv.js` 7/14.

Functions (fix hunks landing inside them, pre-split `index.html`; current
location in brackets):
`play` **62** [`player/decode-tiers.js:606`, 479 lines] ·
`persistentWait` **47** [`player/measurements.js:358`] ·
`attachHls` **44** [`player/player.js:499`, 378 lines] ·
`seekTo` 23 [`player/transport.js:690`] · `commitPreparedReplacement` 23
[`player/prepared-replacement.js`] · `watchLiveTv` 21 · `startPlaybackControl`
21 [`player/directed-change.js:371`] · `paintAnalysis` 21 · `wirePlayer` 20 ·
`stallDiagnose` 20 [`player/stall-diagnosis.js`] · `clusterNodeRow` 19
[`pages/cluster.js`].

### Open questions (not settled from code)

1. Is production reached through a TLS proxy (h2 + gzip) or directly over
   cleartext on the LAN? F-web-1's numbers assume the latter, which is what
   `axum::serve` gives; the proxy contract in `OPERATIONS.md` does not
   mandate compression.
2. Is 1.6.16 the current hls.js release? The project tracks specific
   internals (loader subclass, `subtitleTrack=-1` bounce), so any bump needs
   F-web-2 first.
3. `MSE_QUOTA_BYTES=144e6` (`decode-margin.js:211`) is a Chrome-desktop
   figure; what does the budget do on Android Chrome / Safari iPadOS where
   the SourceBuffer quota is lower? hls.js's `BUFFER_FULL_ERROR` shrink is
   the backstop, but the log line at `retuneBuffer` will state a peak the
   device cannot hold.
4. Which TV browsers are actually targeted (Tizen, webOS, Fire TV Silk,
   Android TV Chrome)? F-web-13's key codes and F-web-14's H.264 ceiling
   depend on the answer.
5. Whether the `?token=` image URLs are intended to survive (F-web-8 step 3)
   or the plan is signed/short-lived image URLs; the per-login poster cache
   invalidation is a side effect either way.

---

## plurx architecture review — apple-client (clients/apple, iOS + tvOS)

Reviewer scope: `clients/apple/` (63,682 lines Swift; 44k app + 20k tests) plus the
server-side HLS authoring the Apple player consumes (`crates/plurxd/src/http/hls.rs`
`master_playlist_with_shape`). Repo at `main` = a1414368, 2026-09-20. Read-only.

Design record consulted: `docs/ARCHITECTURE.md` §1–3, `docs/PLAYBACK.md` (native
delivery, copy-HLS, `apple.*` rows), `docs/clients/APPLE-*.md`,
`PLAYBACK-SURFACE-CONTRACT.md`, `PLAYER-INPUT-CONTRACT.md`,
`docs/streaming/MKV-DURATION-AND-APPLE-SLIDING-HLS-RCA-AND-FIX.md` (the -11866/-12888
story is already owned server-side and is NOT re-reported here).

Summary verdict: the Apple client is far more carefully engineered than a
month-old agent-written codebase has any right to be — capability probing,
HDR/DV signalling, image pipeline, token handling and test coverage are
genuinely good. The two things that matter most are (1) **tvOS never asks the
display to match frame rate or dynamic range**, so a custom-`AVPlayerLayer`
Apple TV app that ships HDR10/DV bitstreams is quite likely being tone-mapped
to SDR at 60 Hz by the box unless the user has hard-set the output format, and
(2) **audio-session interruptions and route changes are not observed at all**,
so the stall detector treats every system-initiated pause as a stall and
"recovers" it. Everything after that is architecture debt concentrated in one
9.5k-line file.

---

### Findings

#### F-apple-1: tvOS never sets `AVDisplayCriteria`, so Match Frame Rate / Match Dynamic Range cannot engage for the custom player
- Severity: **P1** · Category: video-quality · Confidence: **CONFIRMED** (code) / LIKELY (user-visible outcome depends on the Apple TV's Video and Audio settings)
- Evidence:
  - The player is a bare layer, not `AVPlayerViewController`:
    `clients/apple/Sources/PlayerSurface.swift:310-311`
    ```swift
    final class PlayerSurfaceView: UIView {
        let playerLayer = AVPlayerLayer()
    ```
    and the doc comment at `PlayerSurface.swift:17-20` says this was chosen "to
    avoid reintroducing AVPlayerViewController's LIVE treatment".
  - There is no display-criteria code anywhere in the client:
    `rg 'AVDisplayCriteria|preferredDisplayCriteria|avDisplayManager|AVDisplayManager' clients/apple` → no matches.
    Nothing in `docs/` mentions match content either (`rg -i 'match content|match frame|displaycriteria' docs clients/apple` → 0 hits).
  - The HDR capability claim is read from `AVPlayer.eligibleForHDRPlayback`
    (`Caps.swift:226-228`), which on tvOS is true whenever the *display*
    can take HDR and Match Dynamic Range is on — i.e. exactly the configuration
    in which the OS relies on the app to request the mode switch. The server
    then preserves HDR10/DV (`PLAYBACK.md` `apple.transport-and-dv`) and the
    master advertises `VIDEO-RANGE=PQ` (`hls.rs:12446-12448`).
- Why it matters: Apple's documented contract for `AVDisplayManager` is that
  `AVPlayerViewController` applies `asset.preferredDisplayCriteria`
  automatically and an app using `AVPlayerLayer` must set
  `UIWindow.avDisplayManager.preferredDisplayCriteria` itself. Without it, an
  Apple TV whose default output is "4K SDR 60 Hz" with Match Content enabled
  (Apple's own recommended setting, and the default on new units) will keep
  the HDMI link at SDR/60 for every plurx title: 24 fps film gets 3:2
  pulldown judder and the HDR10/DV bitstream the server took pains to preserve
  is tone-mapped to SDR *inside the Apple TV*. The playback-info badge will say
  "HDR10 delivered" while the panel shows SDR. This silently negates the
  entire DV/HDR preservation effort on the one device class it was built for.
  Infuse, Plex and Jellyfin's Swiftfin all set display criteria (Swiftfin:
  `displayManager.preferredDisplayCriteria = item.asset.preferredDisplayCriteria`).
- Proposed change (S, low risk): on tvOS, when an item reaches `.readyToPlay`
  (there is already a single choke point — `reconcileNativeMediaSelections(to:)`
  after `open()` at `PlayerController.swift:4695`), do
  ```swift
  #if os(tvOS)
  if let dm = UIApplication.shared.connectedScenes
        .compactMap({ ($0 as? UIWindowScene)?.keyWindow }).first?.avDisplayManager,
     dm.isDisplayCriteriaMatchingEnabled {
      dm.preferredDisplayCriteria = item.asset.preferredDisplayCriteria   // tvOS 11.2+
  }
  #endif
  ```
  and reset to `nil` in `stop()`. Prefer `AVDisplayCriteria(refreshRate:videoDynamicRange:)`
  (tvOS 17) built from the decision's `frame_rate` and `deliveredRange` so the
  switch happens *before* the first frame rather than after AVFoundation
  parses the asset (which is what AVPlayerViewController does with its
  `appliesPreferredDisplayCriteriaAutomatically`). Add the same for the
  `LibraryChannels` and `LiveTv` players. Verify on the Bedroom Apple TV in
  "4K SDR + Match Dynamic Range + Match Frame Rate" — that is the
  configuration that exposes the bug; a box hard-set to "4K Dolby Vision"
  hides it.

#### F-apple-2: No `AVAudioSession` interruption or route-change handling; the stall detector treats every system pause as a stall and auto-resumes / reopens / fails
- Severity: **P1** · Category: stability · Confidence: **LIKELY** (code path traced end to end; not reproduced on a device)
- Evidence:
  - The audio session is configured and nothing else:
    `PlayerController.swift:2610-2612`
    ```swift
    try? AVAudioSession.sharedInstance().setCategory(.playback)
    try? AVAudioSession.sharedInstance().setActive(true)
    installRemoteCommands()
    ```
    `rg 'interruptionNotification|routeChangeNotification' clients/apple` → **no matches** in any of the three player stacks.
  - The recovery monitor deliberately ignores `timeControlStatus == .paused` and
    keys only on `wantsPlayback` and a stationary clock:
    `PlayerController.swift:5120-5127`
    ```swift
    let shouldMonitor = self.started && self.wantsPlayback && !self.finished
        && !self.isPlaybackBlocked && !self.isChangingStream
        && self.seekState.allowsStallRecovery && self.player.currentItem != nil
    ```
    and `PlaybackStallDetector.sample` (`PlayerController.swift:1317-1400`)
    fires `.nudge` after `establishedNudgeChecks = 3` two-second samples and
    `.reopen` after `establishedReopenChecks = 6`. The nudge is an unconditional
    `player.play()` / `playImmediately` (`applyStallRecoveryNudge`, 5179-5193).
  - `sameDeliveryStallRecovery` allows one reopen, then `.stop(terminal)` raises
    `owner_exhausted` → "Playback stopped." (5284-5299).
- Why it matters (three concrete user-visible failures, all standard iOS
  behaviours AVPlayer performs on its own):
  1. **Headphones unplugged / AirPods removed** → the system pauses (the
     `.oldDeviceUnavailable` route change). Six seconds later the nudge calls
     `play()` and the film resumes **out of the iPhone speaker** — exactly the
     behaviour Apple's route-change guidance exists to prevent.
  2. **Phone call / Siri / alarm** → the session is interrupted and AVPlayer
     pauses. t+6 s: nudge (fails, session still interrupted). t+12 s: same-delivery
     *reopen* — a full server `POST /hls` create, new `AVPlayerItem`, TTFF
     paid again. The replacement is "unestablished" so it gets 15 more checks,
     then the ladder is spent and the viewer comes back from a one-minute call
     to a terminal "Playback stopped." screen — and the server has burnt an
     encoder slot on a second session for nobody.
  3. **Another app starts audio** (user opens Music while plurx is paused by the
     interruption) → the nudge re-activates plurx's non-mixable `.playback`
     session and silences the other app after 6 s.
  On tvOS the same nudge runs after a Siri interruption; less harmful but the
  same defect.
- Proposed change (S–M, low risk): observe `AVAudioSession.interruptionNotification`
  and `routeChangeNotification` in `PlayerController` (and the other two
  players), and (a) on `.began` set an explicit `systemPaused` flag that gates
  `shouldMonitor` (and pauses the `PlaybackStallDetector`, which already has a
  `shouldMonitor → clearSample()` path); (b) on `.ended` resume only when
  `AVAudioSession.InterruptionOptions.shouldResume` is set and `wantsPlayback`;
  (c) on `.oldDeviceUnavailable` set `wantsPlayback = false` (Apple TN2136 /
  "Responding to audio session interruptions"). While at it, pass
  `.moviePlayback` as the mode (`setCategory(.playback, mode: .moviePlayback)`)
  — the default mode is tuned for music. The same flag should also cover
  `UIApplication.didEnterBackground` for video items (iOS pauses a
  layer-attached video player; today that too reads as a stall on return).

#### F-apple-3: `PlayerController.swift` is a 9,548-line, 250-method god object fenced by nine independent generation counters — the single largest stability risk in the client
- Severity: **P2** · Category: architecture · Confidence: **CONFIRMED**
- Evidence:
  - `wc -l clients/apple/Sources/PlayerController.swift` → 9,548; `grep -c func` → 250; 46 top-level types in one file; `@MainActor final class PlayerController` at line 1619.
  - Generation/epoch state that every continuation must re-check (lines
    2031, 2082, 2125-2126, 2137, 2233, 2294, 2342-2343 plus `seekState.generation`
    and `recipeRevision`): `initialDecisionGeneration`, `preparedAlignmentGeneration`,
    `createRetryEpoch`, `lifecycleGeneration`, `viewerActionEpoch`, `openGeneration`,
    `pgsOverlaySelectionGeneration`, `pgsOverlayItemGeneration`, `seekState.generation`.
    58 hand-written fence expressions (`isSuperseded(|isCurrentLifecycle(|viewerActionEpoch ==|openGeneration ==`).
    A typical one (`PlayerController.swift:2936-2944`) checks six of them at once:
    ```swift
    guard let self, self.lifecycleGeneration == lifecycle,
          self.viewerActionEpoch == actionEpoch, self.wantsPlayback == requested,
          self.openGeneration == attachmentGeneration,
          self.player.currentItem.map(ObjectIdentifier.init) == itemIdentity,
          self.sessionId == currentSession else { return }
    ```
  - 39 unstructured `Task {` launches and 13 `Task.sleep` loops in the one file.
  - Hot-spot data: **32 `fix` commits to this file since 2026-08-20** (of 77 across
    the whole Apple tree), including 12 on 2026-09-13 alone, with subjects like
    "a settled create sequence cannot be answered by its own watchdog",
    "fence title and native selection continuations", "fence presentation and
    control generations", "the switch is a critical section" — every one of
    them is a race between two of these counters.
- Why it matters: each new feature (pause/resume repair, prepared handoff,
  PGS overlay, surface contract) added another epoch and another set of
  guards; the failure mode is always the same — a continuation that checked
  five fences but not the sixth. The `docs/clients/APPLE-PAUSE-RESUME-*`
  and `seek-scratch-reservations` RCAs are that pattern. Test coverage (658
  XCTests) is impressive but the tests are written against seams that
  themselves encode the fence order, so they pin today's races rather than
  making new ones impossible.
- Proposed change (L, medium risk, staged): make the "current attempt" a
  single value type — `struct Attempt { lifecycle, open, viewerAction, item: ObjectIdentifier, session }` —
  held in one `private(set) var current: Attempt`, and have every async
  continuation capture one `Attempt` and compare once (`guard current == captured`).
  That collapses the nine counters into one equality. Then split the file by
  the lines it already draws for itself: `PlaybackStallDetector`/
  `PlaybackRecoveryMonitor`/`PlayerAttachmentRecoveryState` (pure, ~600 lines),
  the PGS overlay client (~400), prepared replacement (~700), Now Playing/remote
  commands, and the open/reopen state machine. The reference shape is
  Media3's `ExoPlayerImplInternal` (one message loop, one `playbackInfo`
  snapshot) or the web client's `playback-control.js` reducer which this file
  claims to port. Do not attempt this until F-apple-1/2 are in, and land it
  behind the existing ownership tests.

#### F-apple-4: Three independent AVPlayer stacks with divergent observation; the main one relies on `item.status` KVO + clock polling and never observes `AVPlayerItemFailedToPlayToEndTime`, `AVPlayerItemNewErrorLogEntry` or `timeControlStatus`
- Severity: **P2** · Category: stability / architecture · Confidence: **CONFIRMED** (divergence) / SUSPECTED (missed failures)
- Evidence:
  - Main player: only end + status are observed —
    `PlayerController.swift:6908-6977` (`observeEnd` → `.AVPlayerItemDidPlayToEndTime`;
    `observeStatus` → `item.observe(\.status)` acting on `.failed` only).
    `rg 'FailedToPlayToEndTime|NewErrorLogEntry|PlaybackStalled' PlayerController.swift` → 0.
    `timeControlStatus` is *sampled* every 2 s / 0.5 s (5127, 6770), never observed.
  - Library-channel player observes `AVPlayerItemFailedToPlayToEndTime`
    (`LibraryChannels.swift:643-648`) and the Live TV player observes
    `timeControlStatus` via KVO (`LiveTvView.swift:187-197`). Each of the three
    manages the audio session independently (`LiveTvView.swift:67-80`,
    `PlayerController.swift:2610`, `LibraryChannels.swift` its own `AVPlayer`).
  - Error classification reads the *last* error-log event regardless of
    relevance: `PlayerController.swift:6990` `let event = item.errorLog()?.events.last`
    (same at 7258 and `LibraryChannels.swift:654`). An HLS item accumulates
    benign entries (a subtitle segment that answered an empty `WEBVTT`, a
    playlist refresh that 404'd during supersession — both documented as
    expected in `PLAYBACK.md`), so `events.last` at the moment of failure can be
    an unrelated entry and `isCompatibilityPlaybackFailure` / `isTransportPlaybackFailure`
    (7561-7683) then classify on the wrong domain/status. There is no
    correlation to `item.error`'s timestamp.
- Why it matters: `AVPlayerItemFailedToPlayToEndTime` carries the fatal
  `NSError` in `userInfo` at the instant of failure and is the notification
  Apple's own sample code keys recovery on; `NewErrorLogEntry` is how you see
  the -12645/-12938 segment failures *before* the item dies. Relying on
  `status == .failed` plus a 2-second clock means the fastest the main player
  can notice a mid-stream fatal error whose status transition is delayed is
  one poll tick, and the three stacks will keep drifting (this review found
  one already: only `LibraryChannels` classifies the notification's error).
  Three copies of "how do I know AVPlayer stopped" is three places for the
  next bug.
- Proposed change (M, low risk): extract one `AVPlayerItemObserver` (KVO on
  `status`, `isPlaybackBufferEmpty`, `isPlaybackLikelyToKeepUp`, player
  `timeControlStatus`+`reasonForWaitingToPlay`, notifications
  `FailedToPlayToEndTime`, `NewErrorLogEntry`, `PlaybackStalled`,
  `DidPlayToEndTime`) that emits a typed `AsyncStream<PlayerEvent>`, and have
  all three players consume it. Classify on the error delivered *with* the
  event, falling back to `errorLog()` only when none is attached. This is the
  shape of Media3's `AnalyticsListener` and what the Android client already
  gets for free.

#### F-apple-5: `LibraryView` eagerly pages the entire library, re-sorts the merged array on the main actor after every page, and filters the whole list three times per render
- Severity: **P2** · Category: performance · Confidence: **CONFIRMED**
- Evidence:
  - `AppModel.swift:611-644` (`AppModel` is `@MainActor`, line 16):
    ```swift
    let limit = 200
    while true {
        let page = try await requireAPI().libraryItems(library.id, sort: sort, offset: offset, limit: limit)
        ...
        merged.append(contentsOf: batch)
        if !batch.isEmpty { publish(merged.sorted { Self.compare($0, $1, sort: sort) }) }
        if batch.isEmpty || offset >= total || batch.count < limit { break }
    }
    ```
    — no scroll-driven paging; every page is followed by a full sort of
    everything received so far, on the main actor, and a `publish` that
    replaces the `@State items` array.
  - `LibraryView.swift:14-16`:
    ```swift
    private var visibleItems: [Item] {
        items.filter { AppModel.matches($0, filter: filter) && (query.isEmpty || $0.title.localizedCaseInsensitiveContains(query)) }
    }
    ```
    is a computed property evaluated in `summary` (line 51, twice), `stateContent`
    (line 85) and the `ForEach` (line 94) — four full passes per body
    evaluation, and body is re-evaluated on every keystroke of the search
    field and every page arrival.
- Why it matters: for a 6,000-title movie library this is 30 sequential
  round-trips before the grid is complete, ~30 main-thread sorts (the last of
  6,000 items, comparator does locale-aware title compares), ~30 SwiftUI diffs
  of a 6,000-row `LazyVGrid` identity set, and each search keystroke does
  24,000 `localizedCaseInsensitiveContains` calls on the main thread before the
  first character renders. On an Apple TV HD that is visible focus lag. Plex,
  Infuse and Swiftfin all page on demand (Swiftfin: `PagingLibraryViewModel`,
  100 per page, next page requested when the last row appears) and let the
  server sort.
- Proposed change (M, low risk): (1) page on demand — request the next 200
  when the `ForEach` renders an item within ~40 of the end (`.onAppear` on the
  row or `scrollTargetBehavior`); (2) drop the client-side re-sort (the server
  already sorted the page; pages arrive in order, so `append` suffices —
  the only case that needs a merge is a multi-library collection, and that
  can k-way merge instead of `sorted`); (3) memoise `visibleItems` into a
  `@State` recomputed in `.onChange(of: query/filter/items)` with a 150 ms
  debounce on `query`; (4) move the filter off the main actor for libraries
  >1k. Keep the existing "paint as it arrives, never blank on refresh"
  behaviour, which is right.

#### F-apple-6: Polling loops where an event would do — 25 ms, 50 ms and 100 ms sleep loops on the main actor
- Severity: **P2** · Category: architecture / performance · Confidence: **CONFIRMED**
- Evidence:
  - `PlaybackControlSession.swift:568` `static let askPollNanoseconds: UInt64 = 25_000_000`,
    used in `askForAction` (426-462) and `awaitPreparedOffer` (498-553) — the
    latter runs for up to 12 s ("twelve is a picture that is still playing
    waiting for a better one", 473) → ~480 main-actor wakeups per quality change.
  - `PlayerController.swift:7990-7997` `awaitItemReady` polls `item.status` every
    100 ms for up to 15 s instead of observing it.
  - `PlayerController.swift:6637-6713` `beginSeekPresentationMonitor` polls
    `AVPlayerItemVideoOutput.hasNewPixelBuffer` every 50 ms after every seek.
  - `PlayerController.swift:9182-9210` prepared-readiness monitor "runs every 100 ms".
  - `PlayerController.swift:5011-5047` status poll every 2 s for the life of
    every HLS session, plus `sampleThePreparedSwitch()` reading the access log
    each tick — regardless of whether the info panel is open or the app is
    backgrounded playing an audiobook.
- Why it matters: none of these is individually expensive, but they all run
  on `@MainActor`, several concurrently (a seek during a staged quality
  change runs the 25 ms, 50 ms and 100 ms loops together), and each wake-up
  competes with SwiftUI layout on tvOS. More importantly they are the reason
  the ownership fences in F-apple-3 exist: a poll loop that re-reads state
  every tick needs a guard every tick; a continuation that resumes on an
  event checks once. The 2 s status poll is a network request every 2 s for
  hours on an audiobook in the background.
- Proposed change (M, low risk): `awaitItemReady` → `for await status in item.publisher(for: \.status).values`
  with a `Task.timeout`; the reporter/answer handoffs → `AsyncStream`/`CheckedContinuation`
  (`PlaybackControlAnswers` already has the lock; add a waiter list); seek
  presentation → `AVPlayerItemVideoOutput.setDelegate(_:queue:)` +
  `outputMediaDataWillChange`; status poll → back off to 10 s when the info
  panel is closed and pause it while `UIApplication.shared.applicationState == .background`
  and `wantsPlayback == false`.

#### F-apple-7: Now Playing info is rewritten twice a second, and tvOS has no Now Playing / remote-command integration at all
- Severity: **P3** · Category: performance / ops · Confidence: **CONFIRMED**
- Evidence:
  - `PlayerController.swift:6823` calls `self.updateNowPlaying()` from the
    periodic observer installed at 2 Hz (`CMTime(seconds: 1, preferredTimescale: 2)`, 6734),
    and `updateNowPlaying` (8832-8844) assigns a fresh dictionary to
    `MPNowPlayingInfoCenter.default().nowPlayingInfo` each time.
  - `PlayerController.swift:8845-8847`: `#else private func updateNowPlaying() {}` —
    on tvOS it is a no-op, and `installRemoteCommands()` is inside `#if os(iOS)` (2612).
- Why it matters: each `nowPlayingInfo` assignment is an XPC to
  `mediaremoted`; Apple's guidance is to set it on state changes and let the
  system extrapolate from `MPNowPlayingInfoPropertyPlaybackRate`. On tvOS the
  absence means Control Center shows nothing for plurx, "Hey Siri, pause" and
  HomePod/iPhone Remote app transport controls do not reach the player, and
  the tvOS 17+ "Now Playing" Top Shelf integration is dead — table stakes
  parity with AVPlayerViewController-based apps (Plex, Infuse both populate it).
- Proposed change (S): update Now Playing on play/pause/seek/item change only
  (there are already explicit call sites at 2787, 2926, 3517, 3698, 6895, 6904);
  drop the periodic call. Enable `MPRemoteCommandCenter` + `MPNowPlayingInfoCenter`
  on tvOS too (they are available and are how Siri reaches a custom player).

#### F-apple-8: Lossless/PCM audio is not claimed, so FLAC and WAV are re-encoded to AAC although AVFoundation plays them natively
- Severity: **P3** · Category: video-quality (audio) · Confidence: **LIKELY**
- Evidence:
  - `Caps.swift:142` `audio: ["aac", "ac3", "eac3", "alac", "mp3"]` while
    `Caps.swift:146` `containers: [... "flac", "wav"]`.
  - Server decision: `crates/plurx-core/src/playback/mod.rs:217-221`
    `fn allows_audio(&self, codec: &str) -> bool { self.audio_codecs.iter().any(...) }` —
    a `.flac` file passes the container check and fails the codec check, so
    the audio is transcoded (the copy path emits `-c:a aac -b:a 256k`,
    `PLAYBACK.md` "Copy-video HLS").
- Why it matters: AVFoundation has decoded FLAC (progressive `.flac` and FLAC
  in fMP4/HLS) since iOS 11 and Apple's HLS authoring spec lists FLAC and
  Apple Lossless as permitted audio. A lossless music/audiobook library is
  needlessly degraded to lossy and burns server CPU to do it; PCM in `.wav`/`.mov`
  likewise. `alac` is claimed, so the intent was clearly lossless support.
- Proposed change (S, low risk): add `"flac"` and `"pcm_s16le"/"pcm_s24le"`
  to the audio claim (both wire spellings), gated by a one-time
  `AVURLAsset.isPlayable`-style probe if you want evidence rather than a
  table; add a fixture to the XCTest caps matrix. Also worth a hardware check:
  tvOS 18+ Apple TV 4K passes through DTS-HD/DTS:X when the receiver supports
  it — if true for the fleet's Apple TVs, `dts` could be claimed per device.

#### F-apple-9: Swift 5 language mode with no strict-concurrency checking, no warnings-as-errors; one `@unchecked Sendable` singleton has unguarded mutable state
- Severity: **P3** · Category: architecture (build hygiene) · Confidence: **CONFIRMED**
- Evidence:
  - `clients/apple/project.yml:14-18`:
    ```yaml
    settings:
      base:
        SWIFT_VERSION: "5.9"
        MARKETING_VERSION: "0.3.0"
        CURRENT_PROJECT_VERSION: "173"
    ```
    No `SWIFT_STRICT_CONCURRENCY`, no `SWIFT_TREAT_WARNINGS_AS_ERRORS`, no xcconfig.
    Built with Xcode 26 (`ci.yml:1025` runner label `xcode-26`), i.e. the Swift 6
    compiler in Swift 5 mode — data-race diagnostics are off.
  - `Session.swift:8-14`:
    ```swift
    final class Session: @unchecked Sendable {
        static let shared = Session()
        var origin: String = ""
        var token: String?
    ```
    written from `AppModel` (`@MainActor`, e.g. `AppModel.swift:101,106,177`) and
    read from non-isolated contexts (`AuthImageCache.localImage` at
    `AuthImage.swift:222` runs off the main actor by design, comment at 112).
    Unlike the other eight `@unchecked Sendable` types, which each carry an
    `NSLock` (`PlaybackControlSession.swift:672`, `LiveTv.swift:635`), this one has no lock.
  - Nine `MainActor.assumeIsolated` call sites (e.g. `PlayerController.swift:6310, 6629, 6737`) —
    correct today because the notifications/queues are main, but unverified by the compiler.
- Why it matters: a torn read of a Swift `String?` is a real crash, not a
  theoretical one (the buffer is refcounted); it would surface as a rare
  `AuthImageCache` crash right after a server switch/sign-out — exactly when
  `origin`/`token` change while poster loads are in flight. And the
  concurrency fences of F-apple-3 are hand-checked because the compiler is
  not allowed to check any of them.
- Proposed change (S): `SWIFT_STRICT_CONCURRENCY: complete` as warnings first
  (expect a few hundred, mostly benign), `SWIFT_TREAT_WARNINGS_AS_ERRORS: YES`
  for the app targets once clean; put `origin`/`token` behind an `OSAllocatedUnfairLock`
  or make `Session` an `actor` with a synchronous `snapshot()`.

#### F-apple-10: `PlayerView` observes a 21-`@Published` controller, so the whole player body (and its tvOS focus tree) re-evaluates at 2 Hz
- Severity: **P3** · Category: performance · Confidence: **SUSPECTED**
- Evidence: 21 `@Published` properties on `PlayerController` (lines 1853-2087),
  including `currentMs` (1920) written every periodic tick (6753) and `surface`
  (1999), a struct republished on every `present(...)`. `PlayerView` holds it
  as `@StateObject private var controller` (`PlayerView.swift:687`) and its
  `body` (712-1026) contains the `PlayerSurface` representable, the focusable
  tvOS overlay, the transport, banners and `PlaybackStatsView`; `updateUIView`
  (`PlayerSurface.swift:278-288`) re-runs `applyPGSOverlay` + `attach` each time.
- Why it matters: `ObservableObject` invalidates every dependent view on any
  published change, so two clock ticks per second re-diff the entire player
  hierarchy, including the tvOS focus containers. It is the first place to look
  for the "tvOS focus lag" question, and it is free to fix.
- Proposed change (S): move the fast-changing values (`currentMs`,
  `bufferedRunway`, `surface`) into a second small `PlayerClock: ObservableObject`
  observed only by the timeline/stats subviews, or adopt `@Observable`
  (iOS 17 is already the floor) so SwiftUI tracks per-property access.

#### F-apple-11: The multivariant playlist advertises the *source* bitrate for every session, so `indicatedBitrate` is wrong for transcodes and the playback-info "network" tone mis-grades them
- Severity: **P3** · Category: video-quality (diagnostics) · Confidence: **CONFIRMED**
- Evidence:
  - `crates/plurxd/src/http/hls.rs:12413-12416`:
    ```rust
    let bandwidth = file.bitrate.unwrap_or(25_000_000).max(128_000);
    out.push_str(&format!("#EXT-X-STREAM-INF:BANDWIDTH={bandwidth},AVERAGE-BANDWIDTH={bandwidth}"));
    ```
    used for copy *and* transcode sessions; the Apple client always requests
    the master (`nativeSubtitles: true`, `PlayerController.swift:4441` →
    `hls.rs:2594-2601`).
  - The client then trusts it: `PlayerController.swift:2412-2414`
    `indicatedBitrate = accessLog()?.events.last?.indicatedBitrate`, and
    `PlayerView.swift:3262-3272` grades network health as
    `delivered < expected * 0.55 && runway < 2 → .critical`.
- Why it matters: a 45 Mb/s 4K source transcoded to a 3 Mb/s 720p rung reports
  `indicatedBitrate = 45 Mb/s`; the panel paints "critical" on a perfectly
  healthy delivery, and the stall beacons carry the same misleading number.
  The HLS spec also says `BANDWIDTH` is the peak of *this* variant; AVFoundation
  uses it for `preferredPeakBitRate` filtering, so a future cellular cap would
  refuse the only rung.
- Proposed change (S): for transcode sessions emit the rung's configured
  video+audio target (the encoder args know it); for copy sessions keep the
  source bitrate. Add `RESOLUTION` from the rung, not the file, for transcodes
  (`hls.rs:12417-12421` uses `file.width/height`).

#### F-apple-12: Player parity gaps against AVPlayerViewController-class apps — no AirPlay route picker, no scrub thumbnails, discrete-only Siri Remote scrubbing
- Severity: **P3** · Category: architecture (UX) · Confidence: **CONFIRMED**
- Evidence: `rg 'AVRoutePickerView' clients/apple` → 0 (AirPlay is reachable
  only through Control Center); `rg -i 'trickplay|thumbnail' PlayerView.swift PlayerController.swift` → 0;
  `PlayerRemoteAdapter.swift:26-31` maps only `onMoveCommand` discrete
  directions — no `UIPanGestureRecognizer` on the touch surface, so tvOS
  scrubbing is step-only.
- Why it matters: these are the three things viewers notice first when moving
  from the system player; the server already has the machinery for trickplay
  (`ARCHITECTURE.md` §2.2 lists "thumbnails/trickplay" as node-local cache).
- Proposed change (M): `AVRoutePickerView` in the iOS transport; BIF/`EXT-X-IMAGE-STREAM-INF`
  or a plain JPEG sprite endpoint consumed by the timeline; a pan recogniser on
  the tvOS surface feeding the existing `PlayerInputRouting` table as a
  continuous scrub input.

---

### Answers to the brief's questions (where not already a finding)

1. **PLAYER config.** `automaticallyWaitsToMinimizeStalling = true`,
   `appliesMediaSelectionCriteriaAutomatically = false` (deliberate, P2-8),
   `preferredForwardBufferDuration = 60` on growing HLS / 0 otherwise
   (`PlayerController.swift:1806-1810`; 12 s on Live TV, `LiveTvView.swift:172`).
   `preferredPeakBitRate`, `preferredMaximumResolution`,
   `canUseNetworkResourcesForLiveStreamingWhilePaused`,
   `preferredPeakBitRateForExpensiveNetworks` are all unset — defensible for a
   single-variant stream, but it means Auto quality is entirely the server's
   stall-driven "lower the rung" logic; AVFoundation's own ABR is unused.
   Master playlist: one `EXT-X-STREAM-INF` with `BANDWIDTH`, `RESOLUTION`,
   `FRAME-RATE`, `CLOSED-CAPTIONS=NONE`, and `VIDEO-RANGE`/`CODECS`/`SUPPLEMENTAL-CODECS`
   **only for HDR HEVC** (`hls.rs:12437-12461`); SDR masters are deliberately
   codec-neutral per a hardware-tested ruling (`transcode.rs:9107-9109`) — see
   open question 2. HDR10 static metadata is promoted into `hvcC`/`mdcv`/`clli`
   (`fmp4.rs:2284-2296`) which is the correct fix for AVPlayer's
   init-segment-based VIDEO-RANGE eligibility. Subtitles are native HLS WebVTT
   renditions with forced/AUTOSELECT/DEFAULT authored per RFC 8216; PGS is a
   client `AVSynchronizedLayer` overlay. Audio passthrough is AC-3/E-AC-3 only
   (AVPlayer limit; correct). PiP: `AVPictureInPictureController(contentSource:)`
   with automatic start, blocked while PGS overlay is active. AirPlay:
   `allowsExternalPlayback` toggled off only under PGS. Background: `audio`
   mode present; video pauses on background unless PiP (system behaviour, not
   handled — see F-apple-2). tvOS transport: SwiftUI `@FocusState` + one
   `PlayerRemoteAdapter`, with an explicit focus sink during teardown
   (`PlayerView.swift:728-738`) — a real fix for the "no focusable views" flash.
2. **STALL/ERROR.** Observed: `status` KVO, `DidPlayToEndTime`, `isExternalPlaybackActive`,
   `reasonForWaitingToPlay` sampled, access log sampled. Classification:
   transport = `NSURLErrorDomain` allowlist or access-log 5xx (walks the node
   list); compatibility = `AVError` media codes + VideoToolbox/CoreMedia
   `-12906/-12909/-12910/-12911/-12927/-15517/-17694` + string heuristics
   (`PlayerController.swift:7561-7683`). -12888/-11866 are treated as
   "content not updated" via the end-of-item path and owned server-side (RCA
   doc). Retry: one same-delivery reopen per stall episode with a rolling
   `recoveryReopenBudget`, one HDR-base strip, one compatibility transcode,
   create retries on "still building" with a 60 s ceiling; every reopen
   re-posts `selectedAudio/selectedSubtitle/selectedHeight` and seeks to the
   film position (`open()` 4287-4706), so position and tracks are preserved.
3. **CONCURRENCY.** All UI state is `@MainActor`; no `DispatchQueue.main.sync`,
   `Thread.sleep`, semaphores or `Task.detached` anywhere (`rg` confirmed).
   `[weak self]` discipline is consistent (38 uses in the controller), KVO and
   time observers are removed in `stop()` (3985-4086), and the eight lock-backed
   `@unchecked Sendable` bridges are correct. Weak spots are F-apple-3 (fences),
   F-apple-6 (polls) and F-apple-9 (`Session`).
4. **NETWORKING/DATA.** Three `URLSession`s with `waitsForConnectivity`, 30 s /
   180 s (playback preparation) / 5 s (logout) timeouts (`PlurxAPI.swift:77-99`);
   JSON decoded off the main actor (`run` is non-isolated); images: ImageIO
   thumbnail decode to cell size, `NSCache` (300) + 256 MiB LRU disk,
   stale-while-revalidate, in-flight dedup (`AuthImage.swift`); token in Keychain
   (`TokenVault.swift`); `?token=` on direct URLs is documented policy
   (`SECURITY.md:71-74`). Polling: DVR 5/30 s and stops on background; player
   status 2 s does not (F-apple-6); Live TV heartbeat 5 s.
5. **UI.** SwiftUI throughout with `UIViewRepresentable` only for the player
   layer and PDF; `LazyVGrid`/`LazyVStack` everywhere; identities are stable
   (`Identifiable` by server id). Problems are F-apple-5 and F-apple-10.
6. **BUILD.** iOS/tvOS 17.0 floor, Swift 5.9 mode, XcodeGen, issue-keyed
   release notes with `make apple-build-bump`, CI on a self-hosted Xcode 26
   Mac running `make apple-test` on iPhone/iPad/tvOS simulators with
   build-once/test-twice. 658 XCTests; the state machine is testable without a
   device because AVPlayer effects are injected seams (`ItemPreparation`,
   `MediaSelectionPreparation`, `waitCreateRetry`, …). Gap: F-apple-9.

---

### Already good — do not undo

1. **Image pipeline** (`AuthImage.swift`): `CGImageSourceCreateThumbnailAtIndex`
   sized to the cell, `kCGImageSourceShouldCache: false`, count-bounded
   `NSCache`, 256 MiB LRU disk cache under `Caches/`, `urlCache = nil` to avoid
   double-caching, stale-while-revalidate, per-source download coalescing, one
   retry. This is textbook.
2. **Capability snapshot discipline** (`Caps.swift`): four probes captured once
   per decision, repeated verbatim on every session create, DV claimed only
   when HEVC ∧ HDR display ∧ `availableHDRModes.contains(.dolbyVision)`,
   `dvhls=1`, `progressive_hevc_sample_entries: ["hvc1"]`. The comments record
   the exact overclaim failures this prevents.
3. **HLS authoring for AVFoundation** (server): `VIDEO-RANGE`/`CODECS`/
   `SUPPLEMENTAL-CODECS` with version 10 only when needed, `CLOSED-CAPTIONS=NONE`,
   forced renditions `DEFAULT=NO`, RFC 8216 AUTOSELECT uniqueness, no
   `EXT-X-INDEPENDENT-SEGMENTS` on open-GOP copies, `hvc1` re-tag, HDR10 SEI →
   `hvcC`/`mdcv`/`clli` promotion, master on its own path to dodge AVPlayer's
   URL cache. Each rule cites the device failure that motivated it.
4. **Compatibility ladder ordering**: DV → strip to HDR base → SDR transcode,
   each once, gated on a media-error allowlist so transport faults can never
   spend an HDR fallback; established-HDR playback gets one same-delivery
   reconnect before any downgrade (`apple.established-hdr-recovery`).
5. **Owning media selection** (`appliesMediaSelectionCriteriaAutomatically = false`)
   with explicit audio + legible selection per item, and the honest fallback
   "That subtitle track could not be turned on" instead of a silent burn.
6. **Token and origin hygiene**: Keychain storage, address+token written
   together, logout against the *captured* origin/token with a 5 s bound,
   failover origins refused if they downgrade https→http (`Session.swift:27-40`).
7. **PGS overlay** via `AVSynchronizedLayer` + `CABasicAnimation` keyed to item
   time: zero re-encode, DV/HDR untouched, seek-safe by construction.
8. **Reopen queue / open-and-drain** (`openAndDrain`, 4260-4284): exactly one
   trailing reopen per completed change, no overlapping server replacements —
   the right invariant for a stateful transcode session.
9. **Foreground/background aware polling** for DVR/Live TV/Home
   (`HomeView.swift:98-123`, `DvrRecordings.swift:156-169`) with exponential
   backoff on failure.
10. **Release engineering**: issue-keyed per-build notes, `apple-build-bump`,
    `PrivacyInfo.xcprivacy`, documented ATS rationale, CI that builds once and
    tests every destination, contract fixtures shared byte-for-byte with web
    and Android.

### Hot spots (fix commits since 2026-08-20; 77 of 305 Apple commits)

| File | `fix` commits | Notes |
|---|---:|---|
| `Sources/PlayerController.swift` | **32** | 12 on 2026-09-13 alone; all fence/ownership races (F-apple-3) |
| `Sources/LiveTvView.swift` | 12 | 3,776-line view file containing its own player model |
| `Sources/PlaybackControlReporter.swift` | 10 | reporter/verdict handoff (F-apple-6 polling) |
| `Sources/PlayerView.swift` | 9 | overlay/focus/chrome |
| `Sources/PlaybackControlSession.swift` | 9 | ask/offer waits |
| `Sources/LibraryChannels.swift` | 7 | third AVPlayer stack (F-apple-4) |
| `Sources/LiveTv.swift` | 5 | |
| `Sources/PreparedReplacement.swift` | 4 | second AVPlayer pipeline |
| `Sources/Models.swift` | 4 | |

`project.yml` changed 108 times in the month (build-number bumps) — fine, that
is the versioning tool doing its job.

### Open questions I could not settle from code

1. **Has any Apple TV in the fleet been run in "4K SDR + Match Dynamic Range +
   Match Frame Rate"?** If every test box is hard-set to 4K DV/HDR, F-apple-1
   would never have been visible. One evening with the Bedroom Apple TV in
   Apple's default configuration settles it (watch the TV's own info overlay
   for "24 Hz / HDR" during playback).
2. **SDR masters without `CODECS`** are a recorded hardware-tested ruling
   (`transcode.rs:9107-9109`), but Apple's HLS Authoring Specification requires
   `CODECS` on every `EXT-X-STREAM-INF`, and `mediastreamvalidator` will flag
   it. Was the ruling "adding CODECS broke a device" or "we never tried"? If
   the former, which device and which string — an H.264 `avc1.640028` string
   is very unlikely to be the problem; a wrong HEVC level string is.
3. **Growing EVENT playlists vs a synthesised full VOD playlist.** Much of
   `PlayerController`'s complexity (custom timeline, `baseMs` mapping,
   `seekRoute`, reopen-on-seek, pause/resume repair, -12888 exposure) exists
   because AVPlayer is handed a live-shaped playlist for an on-demand file.
   Jellyfin's `DynamicHlsController` instead writes the complete
   `#EXT-X-PLAYLIST-TYPE:VOD` playlist up front and (re)starts the encoder at
   whichever segment the player asks for; AVPlayer then gets a real duration,
   native seeking and no LIVE treatment. Was that shape evaluated and
   rejected (the HA "any node can produce segment N" design would actually
   make it easier), or is the EVENT shape inherited? This is a server-side
   decision but it is the single change that would delete the most client code.
4. **Interruption behaviour on hardware** (F-apple-2): a phone call during
   playback on the iPhone 17 Pro Max and a headphone unplug are two-minute
   experiments that would confirm or refute the traced nudge/reopen sequence.
5. **`?token=` on direct-play URLs handed to AirPlay receivers** is documented
   policy; given the server already mints per-session capabilities for HLS,
   is a short-lived per-play capability for `/direct` on the roadmap? (Not a
   finding — the policy is stated and reasoned — but the Apple client is the
   one place the long-lived bearer leaves the device.)
6. **DTS on tvOS 18+**: does the fleet's Apple TV/receiver chain pass DTS-HD
   through? If so `dts` could join the Apple audio claim and a lot of MKV
   remuxes would stop transcoding audio.

---

## Android client review — clients/android (Kotlin / Media3) — 2026-09-20

Scope: `clients/android/app` (38.6k lines main, 15.6k JVM tests, 2.1k instrumented), at
`main` = a1414368. Read-only. Toolchain as pinned: Media3 **1.10.1**, Kotlin 2.3.10,
AGP 9.3.2, Compose BOM 2026.06.01, OkHttp 4.12, Retrofit 2.11, Coil 2.7, minSdk 23 /
target+compile 37, R8 + shrinkResources on for `release`.

Headline: the playback *policy* layer is unusually well engineered — pure reducers with
shared cross-client fixtures, a disciplined error ladder, honest capability probing.
The gaps are in the *platform* layer that the policy sits on: buffer sizing that starves
4K on TV heaps, no refresh-rate matching, no background/foreground-service model, three
secondary players built without the care the main one got, and a 4.3k-line Controller
that no test can instantiate.

---

### Findings

#### F-android-1: Playback buffer byte budget starves 4K remux/direct play on TV heap classes
- Severity: **P1** · Category: stability / performance · Confidence: **CONFIRMED** (arithmetic and Media3 semantics); stall exposure LIKELY on fleet hardware
- Evidence:
  - `clients/android/app/src/main/java/tv/plurx/app/player/PlaybackLoadControl.kt:15-33`
    ```kotlin
    internal fun playbackBufferTargetBytes(memoryClassMb: Int): Int =
        (memoryClassMb.coerceAtLeast(16) / 8).coerceAtMost(64) * 1024 * 1024
    ...
        .setTargetBufferBytes(playbackBufferTargetBytes(memoryClass))
        .setPrioritizeTimeOverSizeThresholdsForStreaming(false)
        .setPrioritizeTimeOverSizeThresholdsForLocalPlayback(false)
    ```
  - Applied to every player: `Controller.kt:4030`, `LiveTvPlayer.kt:166`, `LibraryChannelPlayer.kt:66`, `OfflineDownloads.kt:392`.
  - Test pins the intent: `test/.../PlaybackBufferBudgetTest.kt:9-14` — "`2 * perPlayer <= heap / 4`" for a 256 MB class.
  - Introduced by `f49a1d7e 2026-09-17 fix(android): bound playback buffers to the application heap` (an OOM fix for the Lenovo TB322FC with two pipelines primed).
  - No `android:largeHeap` in `AndroidManifest.xml` (grep: none).
- Why it matters: with `prioritizeTimeOverSizeThresholds=false`, `DefaultLoadControl.shouldContinueLoading` stops at the byte target regardless of buffered time. Budget = memoryClass/8 MiB, so 128 MB class → 16 MiB, 192 → 24 MiB, 256 → 32 MiB; the 64 MiB cap needs a 512 MB class, which no non-largeHeap TV box reports. Forward buffer at realistic bitrates:

  | source | 16 MiB (128 MB class) | 24 MiB (192) | 32 MiB (256) |
  |---|---|---|---|
  | UHD BD remux, 80 Mb/s | 1.7 s | 2.5 s | 3.4 s |
  | 4K HEVC web, 35 Mb/s | 3.8 s | 5.7 s | 7.7 s |
  | 1080p, 10 Mb/s | 13 s | 20 s | 27 s |

  Media3's own default is `DEFAULT_VIDEO_BUFFER_SIZE` = 2000 × 64 KiB = 128 MiB for video (≈13 s at 80 Mb/s) bounded by 50 s. The devices with the smallest heaps (Fire TV / Google TV dongles) are the ones asked to direct-play the heaviest files, and the client's own stall machinery (`OpenPlaybackStallTracker.establishedThresholdMs = 8_000`, `PlaybackTelemetry.kt:281`) then reopens the session over a 2–3 s Wi-Fi hiccup that a normal buffer would have absorbed. Note the halving is paid *always*, not only while a successor is primed.
- Proposed change (S–M, low risk):
  1. `android:largeHeap="true"` — the norm for media apps (Plex, Jellyfin AndroidTV, VLC ship it) — and size from `ActivityManager.largeMemoryClass`.
  2. Give the incumbent a floor (≥ 48 MiB, ideally `max(48 MiB, 10 s × source bitrate)` — the plan already carries `source.bitrate`) and shrink only the **successor** pipeline while a prime is in flight (it needs `bufferForPlayback` worth, not a steady-state buffer). `buildSuccessorPlayer` (`Controller.kt:3990`) is the seam.
  3. Keep the byte cap as an OOM guard but restore `prioritizeTimeOverSizeThresholdsForStreaming = true` below `minBufferMs`, which is the ExoPlayer default behaviour the fix removed.
  4. Record `memoryClass`/`largeMemoryClass` in `capabilityDiagnostics` so the fleet numbers stop being a guess (see Open questions).

#### F-android-2: No display refresh-rate / frame-rate matching on Android TV — 24p judders at 60 Hz
- Severity: **P1** · Category: video-quality · Confidence: **CONFIRMED** (absence)
- Evidence: `grep -rn "preferredDisplayModeId|setFrameRate|supportedModes|Display.Mode|VIDEO_CHANGE_FRAME_RATE" clients/android/app/src/main` → nothing. `buildPipeline` (`Controller.kt:4002-4046`) sets tunneling, audio attributes and decoder fallback only. Nothing in `docs/clients/ANDROID-CLIENT-PARITY.md` or `clients/android/README.md` mentions refresh rate, so this is an omission, not a recorded decision.
- Why it matters: Media3's `VideoFrameReleaseHelper` calls `Surface.setFrameRate` with the default `C.VIDEO_CHANGE_FRAME_RATE_STRATEGY_ONLY_IF_SEAMLESS`. HDMI 60→24 Hz switches are never seamless, so on every TV in the fleet a 23.976 fps film is shown with 3:2 pulldown; on a 60 Hz panel that is a visible cadence stutter on every pan, and on TVs with motion interpolation it invites soap-opera processing. Plex, Jellyfin ("Refresh rate switching"), Kodi ("Adjust display refresh rate") and the Shield's own "Match content frame rate" all do this; it is the single largest picture-quality lever left on TV after HDR passthrough, which this client already gets right.
- Proposed change (M, med risk — physical verification needed):
  1. On television only: `player.setVideoChangeFrameRateStrategy(C.VIDEO_CHANGE_FRAME_RATE_STRATEGY_ALWAYS)` (API 31+; honours the system "Match content frame rate" preference).
  2. For API 23–30 and for TVs whose OS setting is "seamless only", pick a `Display.Mode` from `display.supportedModes` with the same pixel size and a refresh rate that is an integer multiple of the source fps (23.976 → 23.976/47.952/119.88; 24 → 24/48/120; 25 → 25/50/100; 29.97 → 29.97/59.94; 30 → 30/60), preferring the lowest multiple the panel lists, set `window.attributes.preferredDisplayModeId`, wait for `DisplayManager.DisplayListener.onDisplayChanged` (bounded ~2 s) **before** `prepare()`, and restore on release. The plan already carries the source fps before the first frame, so the switch does not have to be mid-stream. Skip for Live TV (interlaced 59.94) and phones. Reference implementation: jellyfin-androidtv `RefreshRateSwitchingBehavior` / `VideoManager.setRefreshRate`.
  3. Surface it in Settings → Playback as Off / Seamless only / Always, defaulting to Always on TV.

#### F-android-3: Backgrounding never pauses VOD; no foreground service or wake mode for audio
- Severity: **P1** (phones: battery/data; audiobooks die) / P2 on TV · Category: stability · Confidence: **CONFIRMED** (no ON_STOP handling); audiobook termination LIKELY (needs a device run)
- Evidence:
  - The VOD screen registers only a foreground *flag*: `PlayerScreen.kt:1089-1097` `controller.setPresentationForeground(lifecycle.currentState.isAtLeast(STARTED))`; `Controller.kt:2640-2647` only feeds `surfaceOwner.hidden(...)` and the stall sampler — `playWhenReady` is untouched. The only ON_STOP handlers in the app are DVR observation (`MainActivity.kt:166`) and Live TV (`LiveTvScreen.kt:313 controller.stopUnlessRetained()`).
  - PiP is armed only when `isPlaying` and the user *leaves via Home* (`PlayerScreen.kt:1182-1199`); a screen-off/lock or a TV without PiP takes the no-PiP path and the player keeps running.
  - No `MediaSessionService`, no `startForeground`, no `FOREGROUND_SERVICE_MEDIA_PLAYBACK` in `AndroidManifest.xml:12-17` (only `dataSync` for downloads); `MediaSession` is activity-scoped (`Controller.kt:476`). No `setWakeMode` anywhere (grep). `keepScreenOn = true` unconditionally on the PlayerView (`PlayerScreen.kt:1314`), even while paused.
- Why it matters: (a) On a phone, locking the screen mid-film keeps the video renderer decoding into Media3's placeholder surface at full rate and keeps the radio streaming — a 4K HEVC stream in a pocket. (b) On an Android TV without PiP, pressing Home leaves the film's audio playing behind the launcher until the process is cached. (c) Audiobooks ("first-class audiobook … through the shared audio player", README:159) rely on background audio, but without a foreground service Android 12+ moves the process to cached and the freezer/LMK stops it within minutes; without `WAKE_MODE_NETWORK` Doze can cut the socket. (d) A paused player holds the screen on indefinitely. Plex/Jellyfin: pause video on `onStop` unless PiP; audio-only continues under a `MediaSessionService` notification.
- Proposed change (M, low–med risk): in `PlayerScreen`'s lifecycle observer, on ON_STOP and not in PiP: if `plan.videoCodec != null` → `player.pause()` (record as owner-initiated via `viewerTransport` so the surface reducer does not raise `buffering`); if audio-only → hand the player to a `MediaSessionService` with `setWakeMode(C.WAKE_MODE_NETWORK)` and a media notification. Replace `keepScreenOn = true` with `setWakeMode` (Media3 holds the screen only while playing).

#### F-android-4: Live TV treats `ERROR_CODE_BEHIND_LIVE_WINDOW` as a fatal "stream failed"
- Severity: **P2** · Category: stability · Confidence: **CONFIRMED**
- Evidence: `LiveTvPlayer.kt:481-488`
  ```kotlin
  internal fun liveTvPlaybackErrorCode(code: Int): String = when (code) {
      PlaybackException.ERROR_CODE_DECODER_INIT_FAILED, ... -> "codec_unsupported"
      else -> "stream_failed"
  }
  ```
  and `LiveTvPlayer.kt:176-186`: anything but `codec_unsupported` → `stopWithMessage(liveTvMessage(code))`, which `detach()`es the player and releases the tuner. The server window is 24 one-second segments (`crates/plurxd/src/live_tv.rs:140-158`, "24 s on an encode route and 24 source GOPs on a copy route" — ~12 s for 0.5 s broadcast GOPs). Media3's documented handling of 1002 is `seekToDefaultPosition(); prepare()`. The VOD controller already implements the finite-timeline variant (`Controller.kt:711-741`) and explicitly says "a live item keeps today's `Fail`".
- Why it matters: any buffering gap longer than the window — a Wi-Fi roam, an AVR HDMI re-handshake, a paused viewer coming back after 15 s — ends the session with "Live TV stopped. Select a channel to resume" and a tuner re-acquire (owner round-trip, new ffmpeg, 2+ s startup). The `togglePause()` copy (`LiveTvPlayer.kt:313-318`) even warns "resuming has no rewind guarantee", i.e. the product knows this happens. Live TV players (ExoPlayer demo, Jellyfin, Plex Live TV) rejoin at the live edge.
- Proposed change (S, low risk): in the Live TV `onPlayerError`, on 1002 (and once per attach, like the VOD path) call `output.seekToDefaultPosition(); output.prepare()` and post a transient "Rejoined live" message; count it against the existing 30 s no-progress watchdog rather than failing.

#### F-android-5: AudioTrack failures (5001/5002/5004) never enter the compatibility ladder
- Severity: **P2** · Category: stability / video-quality · Confidence: **CONFIRMED** (code path) · impact LIKELY on passthrough routes
- Evidence: `PlaybackPolicy.kt:101-120` — `isTransportPlaybackError` = {2000-2004, 2007, 2008}; `isCompatibilityPlaybackError` = {3001, 3003, 4001-4005}. `Controller.kt:681-800`: an error outside both sets reaches `playbackErrorAction(... mediaCompatibilityFailure=false ...)`, whose third arm (`PlaybackPolicy.kt:77`) is `!mediaCompatibilityFailure -> PlaybackErrorAction.Fail`. `controlErrorCode` (`Controller.kt:3902-3906`) *does* classify 5xxx as `DECODER` — "this device could not render this recipe" — but the ladder never sees it. The capability probe is deliberately route-aware and re-run per decision (`Caps.kt:225-247`; "re-plugging HDMI … changes the truthful answer").
- Why it matters: the exact scenario the route-aware probe was written for — a Shield/AVR route that advertised TrueHD or DTS-HD, then the AVR powers off, HDMI re-handshakes, or the TV takes over as sink — produces `AudioSink.InitializationException` → `ERROR_CODE_AUDIO_TRACK_INIT_FAILED` (5001) or `_WRITE_FAILED` (5002). The client shows "Playback stopped (ERROR_CODE_AUDIO_TRACK_INIT_FAILED)" instead of re-asking `/decision` with the *new* route and getting an EAC3/AAC transcode. The M5.5 measurement itself logged a 5002 on the tunneled Google TV (`docs/playback-control/PLAYBACK-CONTROL-STATUS.md:1381`).
- Proposed change (S, low risk): add 5001, 5002, 5004 to `isCompatibilityPlaybackError` (they are "this device cannot render this recipe" by Media3's own taxonomy), and on that branch re-run `Caps.snapshot` before the rescue create so the new sink claim reaches the server. Add the case to `PlaybackPolicyTest`.

#### F-android-6: "Open in…" hands the account bearer token to an arbitrary third-party app
- Severity: **P2** · Category: security · Confidence: **CONFIRMED**
- Evidence: `ui/DetailScreen.kt:486-491`
  ```kotlin
  val url = Session.mediaUrl("/api/v1/files/${playable.id}/content")
  context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
  ```
  `Session.mediaUrl` (`data/Session.kt:118-123`) appends `token=<Session.token>` — the login bearer — and the server accepts it (`crates/plurxd/src/http/extract.rs:576` `url_decode_lookup(q, "token")`). The intent has no MIME type or package, so the chooser offers browsers and every video app.
- Why it matters: the full-authority, long-lived credential leaves the app's trust boundary into whatever the viewer taps (VLC/MX Player keep URL history; a browser keeps it in history and sync). The rest of the client is careful about exactly this: `Net.capabilityClient` exists so capability URLs never carry the bearer (`Net.kt:41-50`), and `configureNodeOrigins` refuses scheme downgrades to keep the bearer off cleartext. Plex hands external players a transient token; the server already has narrow-authority URL shapes for HLS/offline/publication routes (`http/mod.rs:963-980`).
- Proposed change (S–M, low risk): add a server route that mints a file-scoped, short-lived (minutes) read-only grant and use it here; set the intent's MIME type from the file's container; on TV hide the action (external players are rare and the token exposure is the same).

#### F-android-7: Bearer token stored in plaintext DataStore and included in Android cloud/D2D backup
- Severity: **P2** · Category: security · Confidence: **CONFIRMED**
- Evidence: `data/SettingsStore.kt:14,39,142` (`preferencesDataStore(name = "plurx")`, `stringPreferencesKey("token")`); `AndroidManifest.xml:34` `allowBackup="true"` with `res/xml/backup_rules.xml` / `data_extraction_rules.xml` excluding only `offline/`. DataStore lives under `files/datastore/`, which is inside the default backup set. No `EncryptedSharedPreferences`/Keystore use anywhere (grep).
- Why it matters: the token is uploaded to Google's backup and restored onto any device signed into the account (and travels over D2D transfer). Plex/Jellyfin also store tokens in plain prefs, but the Android guidance (and both apps' manifests) exclude auth data from backup. One line fixes the exposure; wrapping with Keystore is optional hardening.
- Proposed change (S, low risk): add `<exclude domain="file" path="datastore/" />` to both rule files (or `allowBackup=false` — the app re-pairs in two taps). Optionally encrypt the token at rest via `MasterKey`/Tink.

#### F-android-8: One OkHttp dispatcher (5 requests/host) serialises posters and queues API calls behind them
- Severity: **P2** · Category: performance · Confidence: **CONFIRMED** (config); user impact LIKELY on TV Home
- Evidence: `data/Net.kt:23-38` builds `Net.client` with timeouts and the bearer interceptor only — no `dispatcher(...)`, no `connectionPool`, no `cache`. `PlurxApp.kt:22-26` routes Coil through the same client; Retrofit (`Net.kt:73-78`) too. All traffic goes to one host, over cleartext HTTP/1.1 (OkHttp only negotiates h2 via ALPN over TLS). `loadHome` (`AppViewModel.kt:334-362`) fires `hubs`, `libraries` and one `libraryItems(limit=24)` per library concurrently, and each shelf then requests 24 posters.
- Why it matters: OkHttp's async dispatcher caps `maxRequestsPerHost` at 5 and promotes queued calls FIFO. On a TV Home with 6 libraries that is ~150 poster GETs queued 5 at a time on ≤5 keep-alive connections; a `/decision` or `/item` issued while the queue drains waits behind the posters already queued. Media3 uses synchronous `execute()` and bypasses the cap, so playback itself is unaffected — the cost is browse latency and TTFF from Detail (decision + preflight). Coil's own recommendation is a dedicated client or a raised per-host limit.
- Proposed change (S, low risk): `Dispatcher().apply { maxRequestsPerHost = 16 }` on the shared client, or give Coil `Net.client.newBuilder().dispatcher(imageDispatcher).build()` (shares pool + interceptor, separate queue) so API calls never sit behind artwork. Consider `Protocol.H2_PRIOR_KNOWLEDGE` only if the server enables h2c; otherwise leave HTTP/1.1.

#### F-android-9: Three of the four ExoPlayer constructions skip audio focus, becoming-noisy, tunneling and decoder fallback
- Severity: **P2** · Category: stability · Confidence: **CONFIRMED**
- Evidence: the main pipeline (`Controller.kt:4002-4046`) sets `DefaultRenderersFactory.setEnableDecoderFallback(true)`, `setTunnelingEnabled(isTelevision)`, `setAudioAttributes(USAGE_MEDIA/MOVIE, handleAudioFocus=true)`, `setHandleAudioBecomingNoisy(true)`. None of that appears in:
  - `livetv/LiveTvPlayer.kt:164-167` — `ExoPlayer.Builder(context).setMediaSourceFactory(...).setLoadControl(...).build()`
  - `librarychannels/LibraryChannelPlayer.kt:65-68` — same shape
  - `data/offline/OfflineDownloads.kt:391-395` (`cacheOnlyPlayer`) — same shape
  `ExoPlayer.Builder`'s default is `AudioAttributes.DEFAULT` with `handleAudioFocus=false`.
- Why it matters: Live TV and Library channels on a Google TV run untunneled (no SoC A/V sync path for 1080i60 broadcast, the case tunneling exists for) and without decoder fallback (a flaky HW decoder becomes `codec_unsupported` → a compatibility retry through the owner instead of a local software fallback). On phones the offline and Live TV players talk over navigation prompts and calls and keep playing when headphones are unplugged. These are the four lines the Media3 docs put in every player.
- Proposed change (S, low risk): extract `buildPipeline`'s renderer/audio/tunneling block into one `PlurxPlayerBuilder(context, live: Boolean)` used by all four; keep the Live TV load control override.

#### F-android-10: Library grid downloads the whole library and re-sorts it on the main thread on every page
- Severity: **P2** · Category: performance · Confidence: **CONFIRMED**
- Evidence: `AppViewModel.kt:674-681` (`libraryPages`: sequential `limit = 200` pages until `offset >= total`); `LibraryScreen.kt:68-80` accumulates into `loaded` and publishes `LibraryLoad(loaded.toList())` after **every** page; `LibraryScreen.kt:93-95` `remember(load, sort, filter) { sortMerged(...).filter(...) }`; `sortMerged` (`:213-221`) uses `sortedBy { sortableTitle(it.title) }` whose selector lowercases and allocates per *comparison*, and the `added`/`year`/`resolution` variants chain a second `sortableTitle` compare.
- Why it matters: a 6,000-title library = 30 round trips, and on each arrival the main thread copies the list, sorts n log n with ~2 string allocations per compare (≈ 6,000·13·2 ≈ 150k lowercases on the last page), filters, and diffs a 6,000-key `LazyVerticalGrid`. On a Fire TV class CPU that is tens of ms per page × 30 pages during the first seconds of the screen, exactly when the viewer is pressing D-pad. The "sorting is client-side so a sort change never re-fetches" goal (comment at `:67-70`) is good; the implementation cost is not.
- Proposed change (S, low risk): compute sort keys once per item (`sortableTitle` cached in a wrapper or `sortedWith(compareBy(cached))`), do the sort on `Dispatchers.Default` inside `produceState`, publish at most every ~150 ms rather than per page, and bump the page size to 500 (server permitting). Longer term (M): server-side sort + `Paging3`, which is what Plex/Jellyfin do for libraries past a few thousand.

#### F-android-11: `Controller.kt` is a 4,293-line stateful class no test can instantiate — and it is the fix magnet
- Severity: **P2** · Category: architecture / stability · Confidence: **CONFIRMED**
- Evidence: `wc -l player/Controller.kt` = 4293 (plus `PlayerScreen.kt` 2897). `grep -rn "Controller(" app/src/test` → no construction (only a test name at `PlaybackIntentTest.kt:155`); no Robolectric, no `TestExoPlayerBuilder`/`FakeClock` anywhere in `test/`. Hot-spot count: 23 `fix` commits on `Controller.kt` since 2026-08-20, 15 on `PlayerScreen.kt` (see Hot spots). The class owns: the ExoPlayer instance swap, MediaSession, PGS overlay, two watchdog coroutines, status polling, stall recovery, control reporting, prepared replacement rendezvous, node failover, surface ledger, track selection and telemetry.
- Why it matters: the pure pieces (`PlaybackSurface`, `StallReopen`, `PlaybackPolicy`, `PlaybackIntent`, `PreparedReplacement`) are excellent and thoroughly tested, but the last month's defects were overwhelmingly in the *wiring* between them ("a seek and a node failover drew nothing at all", "the second review's blocker, its seven should-fixes", "three defects the port's reconciliation introduced"). None of those are reachable by the current suite; every one was found on a device.
- Proposed change (M): introduce a `PlayerPort` interface over the ~20 ExoPlayer calls the Controller makes (`setMediaItem/prepare/seekTo/playWhenReady/currentPosition/bufferedPosition/isCurrentMediaItemLive/addListener/release/setVideoSurface/volume`) with a JVM fake that can emit `onPlayerError/onTracksChanged/onRenderedFirstFrame`; then Controller tests can replay the three regressions above under `kotlinx-coroutines-test`. Split out `SessionStatusPoller`, `NodeFailover` and `PgsOverlayBinding` as the first extractions — they have no cross-dependencies. Media3's own `TestExoPlayerBuilder` + Robolectric is the alternative if you would rather not add a port.

#### F-android-12: Sideload/deploy path builds `debug`; no Baseline Profile; release signing absent
- Severity: **P2** · Category: performance / ops · Confidence: **LIKELY** (the Ansible client playbook is not in this repo; every in-repo path builds debug)
- Evidence: `Makefile:1713,1742,1751` — `assembleDebug` and `app-debug.apk` copied to `<data_dir>/plurx-android.apk`; `clients/android/README.md:283-285` "all produce the same `app/build/outputs/apk/debug/app-debug.apk`"; `docs/PUBLISHING.md:376-379` "`build.gradle.kts` defines no `signingConfigs`". No `androidx.profileinstaller` / `baseline-prof.txt` in `gradle/libs.versions.toml` or `app/` (grep).
- Why it matters: a `debuggable` APK disables R8/shrinking (the README's own 18 MB vs 3 MB), keeps `debuggable=true` (ART skips some optimisations; `run-as` exposes the DataStore token to anyone with adb), and a Compose app without a Baseline Profile pays JIT warm-up on every cold start and on first scroll of each screen — on a TV stick that is the "first D-pad press stutters" complaint. Plex/Jellyfin ship release + baseline profiles.
- Proposed change (S for signing + release deploy; M for profile): add a release `signingConfig` fed from env/Ansible vault, deploy `assembleRelease`, add `androidx.profileinstaller` + a Baseline Profile generated by a Macrobenchmark run (Home → Library → Detail → Player on the TV AVD), and put `:app:lintRelease` in the gate.

#### F-android-13: `Application.onCreate` does blocking disk I/O for a feature TV never uses
- Severity: **P3** · Category: performance · Confidence: **CONFIRMED**
- Evidence: `PlurxApp.kt:16-19` → `OfflineDownloads.initialize(this)`; `OfflineDownloads.kt:113-141`: `check(Looper.myLooper() == mainLooper)`, `OfflineCatalog(appContext)` (constructor `readInitial()` parses `catalog.json`, `OfflineCatalog.kt:55-62`), `runBlocking(Dispatchers.IO) { SettingsStore(appContext).flow.first() }`, a second `runBlocking` over `recovery.intents()`, then `StandaloneDatabaseProvider` + `SimpleCache`. `OfflineBooks.initialize` (`OfflineBooks.kt:54-59`) at least launches `reconcile()` off-thread. README:259-260 and `OfflineBooks.canUse` (`:61-63`) say TV has no offline feature.
- Why it matters: three synchronous reads (DataStore file, catalog JSON, recovery prefs) on the main thread before the first frame, on eMMC flash — StrictMode `DiskReadViolation` on every cold start, and pure waste on television.
- Proposed change (S): gate on `!isTelevision`, and make `initialize` lazy/async (`DownloadService` and `DownloadsScreen` are the only consumers); keep the main-looper check for the `DownloadManager` construction only.

#### F-android-14: 2 s session-status polling for the life of every HLS session, panel open or not
- Severity: **P3** · Category: performance / ops · Confidence: **CONFIRMED**
- Evidence: `Controller.kt:2225-2256` `startStatusPolling` — `while (isActive && sessionId == polledSessionId) { vm.hlsSessionStatus(...) ... delay(2_000) }`, started from every session attach (`:1735`, `:2037`). Comment: "Poll only while this controller owns an HLS session … showing Standard or Debug cannot keep an abandoned encoder alive" — i.e. the poll is decoupled from the panel being visible.
- Why it matters: 1,800 extra authenticated requests per hour per transcoding client, plus the 10 s progress beat and the control exchanges; on a phone it keeps the radio out of its low-power state (the classic 2 s poll anti-pattern) and it continues in the background (F-android-3). The panel that consumes it is closed >99 % of the time.
- Proposed change (S): poll only while `panel == PlayerPanel.Info || statsMode != Off`, and otherwise once per 30 s for the `encoder`/`vod` facts the badges use; or fold the fields into the existing control exchange.

#### F-android-15: Node failover fires on any `ERROR_CODE_IO_BAD_HTTP_STATUS`, including 401/404/410 — the comment describes a filter that does not exist
- Severity: **P3** · Category: stability / correctness · Confidence: **CONFIRMED**
- Evidence: `PlaybackPolicy.kt:106-107` — "2004 … only 5xx reaches this in practice; a 4xx capability answer is terminal and is filtered at the call site". Call site `Controller.kt:704-707`: `if (isTransportPlaybackError(error.errorCode) && retryMediaOnNextNode(error))`; `retryMediaOnNextNode` (`:2280-2331`) never inspects `InvalidResponseCodeException.responseCode`; `mediaRefusal(error)` is computed just above (`:703`) but only used for the sentence. Media3 maps 404/410 to the same 2004 as 503 (`DefaultLoadErrorHandlingPolicy` just declines to retry them).
- Why it matters: a reaped session (404), an expired credential (401) or a permanent `media_owner_lost` (410, `reopen_required=true` per PLAYBACK-CONTROL-STATUS) is replayed against every advertised peer with a full `setMediaItem/prepare` per node before the viewer sees the verdict — exactly the cost the comment says is avoided. In a 3-voter cluster that is 2 wasted prepares and a few seconds of "Reconnecting on another server address" for an answer that was always going to be the same.
- Proposed change (S): pass `mediaRefusal(error)?.statusCode` into the decision; fail over only when the status is absent (socket/timeout) or ≥ 500 and not a row-8 "not yet" 503; keep 410 → reopen and 401/403 → auth surface. Add the case to `PlayerPolicyTest`.

#### F-android-16: Capability probe under-claims Vorbis/PCM, probes 30 fps only, never states HEVC Main10
- Severity: **P3** · Category: video-quality · Confidence: **CONFIRMED** (each item individually)
- Evidence: `CapsPolicy.kt:260-284` `audioCodecClaims` starts from `aac, mp3, opus, flac` and adds only ac3/eac3/dts/truehd — no `audio/vorbis` check although Vorbis decoding is CDD-mandatory and the server profile vocabulary has it (`crates/plurx-core/src/playback/profiles.toml:13,23`). `Caps.kt:157` `areSizeAndRateSupported(width, height, 30.0)` — a 4K30-only decoder claims 2160 for a 4K60 file. `VideoEntry.profiles` (`CapsPolicy.kt:45`) is never populated, though `MediaCodecInfo.profileLevels` carries `HEVCProfileMain10`. `dts` is claimed from core-DTS sink support (`:274-279`) while the server files DTS-HD MA under the same `dts` (`decision allows_audio` string match, `playback/mod.rs:217-221`), so a core-only sink direct-plays an MA track and hears the lossy core.
- Why it matters: WebM/MKV-with-Vorbis (a real slice of older libraries) is forced through the progressive remux — losing native range seeking for no reason; a 4K60 title on a 4K30 decoder fails into the compatibility rescue rather than being transcoded first; the DTS case is a quiet quality loss rather than a failure.
- Proposed change (S): add `vorbis` (+ `pcm` when the extractor supports it) to the claims; probe `areSizeAndRateSupported` at 30 and 60 and report `vmaxheight` per rate or cap to 1080 when 60 fps fails at 2160; populate `profiles` from `profileLevels`; claim `dts` vs a future `dtshd` split once the server separates them.

---

### Already good — do not undo

1. **Pure policy layer with shared fixtures.** `PlaybackSurface` reducer, `StallReopen`/`CreateRetry`, `PlaybackPolicy`, `PlaybackIntent`, `PreparedReplacement`, `CapsPolicy`, `MediaOrigin` are framework-free and tested against `tests/contracts` / `tests/playback` fixtures the web and Apple clients also run (`build.gradle.kts:75-78`). This is the right architecture; F-android-11 is about the layer *around* it.
2. **SurfaceView through `PlayerView`**, tunneling only on television, `setEnableDecoderFallback(true)`, `USAGE_MEDIA`/`MOVIE` audio attributes with focus handling and becoming-noisy on the main pipeline (`Controller.kt:4002-4046`). HDR10/DV output correctness depends on this; keep it.
3. **Route-aware audio and honest DV claims** (`Caps.kt:225-247`, `CapsPolicy.kt:227-300`): sink encodings from `AudioCapabilities` with the player's own attributes, recomputed per decision; DV only when decoder *and* display agree; P7 only from `DvheDtb`; software decoders capped at 1080p. Exactly what the 2026-08 review asked for.
4. **Error ladder shape** (`Controller.kt:681-864`, `PlaybackPolicy.kt:62-83`): transport vs compatibility split, one-shot rescues, "once HDR has rendered a repeat is visible instead of silently becoming SDR", finite-timeline `BEHIND_LIVE_WINDOW` recovery that preserves position and refuses `seekToDefaultPosition`.
5. **Position truth** via `ProgressiveMediaOrigin` (`MediaOrigin.kt:257-315`) reading `X-Plurx-Media-Origin-Ms` on the loader thread with URI epochs; `baseMs` for HLS. Scrobble positions are correct after seeks and failovers.
6. **Teardown on `viewModelScope`** for `endHlsSession`/`postProgress` (`AppViewModel.kt:818-844`) and `CancellationException` consistently rethrown (`catchingUnlessCancelled`). Sessions get their `DELETE`.
7. **Security hygiene elsewhere**: bearer never sent to `/api/v1/server` during discovery (`Net.kt:29-31`), scheme-downgrade refusal in node failover (`Session.kt:33-41`), capability URLs without bearer (`Net.kt:41-50`), sign-out revokes server-side within a 5 s bound.
8. **Text tracks off by default** (`setTrackTypeDisabled(TEXT, true)`, `Controller.kt:4019`) and selected by server policy; HLS chunkless preparation left on; subtitle renditions not fetched until chosen.
9. **R8 + shrinkResources with a minimal, explained keep file** (`proguard-rules.pro`), `SourceFile`/`LineNumberTable` retained; every `LazyRow/LazyVerticalGrid` keyed; DataStore not SharedPreferences; `rememberSaveable` for sort/filter.
10. **Home as a scrolled `Column` of `LazyRow`s** is a deliberate, documented TV-focus decision (`HomeScreen.kt:165-191`); do not "optimise" it into a `LazyColumn` without re-reading that comment and `ShelfFocusTest`.

### Hot spots (fix commits since 2026-08-20; 66 of 245 Android commits are `fix`)

| file | fix commits |
|---|---|
| `player/Controller.kt` | 23 |
| `player/PlayerScreen.kt` | 15 |
| `player/PlaybackControlSession.kt` | 7 |
| `player/PlaybackControlReporter.kt` | 7 |
| `player/StallReopen.kt` | 6 |
| `player/PlaybackTelemetry.kt` | 6 |
| `player/PlaybackIntent.kt` | 6 |
| `livetv/LiveTvScreen.kt` | 6 |
| `ui/AppViewModel.kt` | 5 |
| `player/SubtitlePolicy.kt` | 5 |
| `data/Models.kt` | 5 |

Reading: the reducer files (`StallReopen`, `PlaybackIntent`, `PlaybackTelemetry`) churned because the *contract* moved under them; `Controller`/`PlayerScreen` churned because the integration has no harness (F-android-11). `PlaybackLoadControl.kt` has one commit and it is the P1 (F-android-1).

### Open questions (not settleable from code)

1. **What does the fleet actually run — `assembleDebug` or a release build?** Every in-repo path produces `app-debug.apk`; the Ansible client playbook is elsewhere. F-android-12 hinges on it.
2. **`ActivityManager.memoryClass` / `largeMemoryClass` on the Lenovo TB322FC, the Google TV and the Shield.** The buffer table in F-android-1 is computed from typical classes; one `adb shell dumpsys meminfo`-style capture per device would turn LIKELY into measured.
3. **Does any Google TV in the fleet run Android 12+ with "Match content frame rate" set to something other than "Seamless only"?** Decides whether `VIDEO_CHANGE_FRAME_RATE_STRATEGY_ALWAYS` alone is enough or the `preferredDisplayModeId` path is required (F-android-2).
4. **Audiobook behaviour with the screen off on Android 14+ for > 10 minutes** (F-android-3) — expected to stop when the process is frozen; not measured here.
5. **Prepared replacement on the tunneled Google TV**: `buildSuccessorPlayer` (`Controller.kt:3968-4000`) deliberately reproduces the M5.5 same-codec dual-prime failure (0/3, `PLAYBACK-CONTROL-STATUS.md:1365-1389`) and `preparedReplacement` defaults to `true` (`ViewerPreferences.kt:140`). What does a viewer see on that device when the server asks to prepare a same-codec rung change today — the bounded fallback break (~0.5 s) or something worse? A tunneling-off successor re-run was proposed and never taken.
6. **Does the server enable h2c?** If yes, `Protocol.H2_PRIOR_KNOWLEDGE` removes the per-host connection limits behind F-android-8 for cleartext deployments.

---

## plurx review — build / ops / code health (2026-09-20, main = a1414368)

Scope: Cargo workspace, lockfile, toolchain, deny.toml, Makefile, Dockerfiles, deploy/,
workflows, validation/ + tests/ Python gates, vendor/, fuzz/, docs/OPERATIONS.md,
docs/DEVELOPMENT_PIPELINE.md, docs/ci/, plurx.example.toml, and overall crate structure.
Read-only. No cargo build or test was run; `cargo tree` (with registry index fetch),
`rg`, `wc`, python and reading were used. Numbers below are measured, not estimated,
unless marked.

### Headline numbers

| Metric | Value |
|---|---|
| Rust files / lines | 237 files, 539,804 lines (crates/ only) |
| Non-test Rust (inline `#[cfg(test)]` modules + `tests/` + `*_tests.rs` removed) | ≈315 k lines (41.7 % of the tree is test code) |
| Files > 100 KB | 45 files = 386,129 lines = **71.5 % of all Rust** |
| Largest file | `crates/plurxd/src/transcode.rs` 47,097 lines / 1.96 MB, of which 23,770 lines (50.5 %) are tests |
| `#[test]`/`#[tokio::test]` functions | 3,947 (plurxd 2,404 · plurx-core 1,370 · cluster-check 134 · pgs 23 · compat-plex 16); 12 `#[ignore]` |
| `#[cfg(test)]` attributes in `src/` | 861, of which only 180 introduce a `mod tests` — **681 are test seams woven into production items** (transcode.rs 208, playback_control.rs 148, http/hls.rs 60, vodserve.rs 49, dv_disk.rs 36) |
| `.unwrap()` in non-test code | **0** (workspace lint `clippy::unwrap_used = "warn"` + `-D warnings`) |
| `.expect()` in non-test code | ≈370 (plurxd 277, plurx-core 78, cluster-check 15); top: transcode.rs 54, vodserve.rs 49, hls.rs 37 — mostly `expect("store")`, `expect("state dir")` (lock/dir invariants) |
| `unsafe {` blocks in `src/` | 380 (fs_secure.rs 118, dv_disk.rs 93, decode_facts.rs 64, fs_secure_windows.rs 28, process_control.rs 15); `// SAFETY` comments: 100 (26 %) |
| `todo!`/`unimplemented!`/`dbg!` in non-test code | 0 / 0 / 0 |
| `println!` in library (non-test) code | plurx-core 15, plurxd 42, cluster-check 54 (CLI output paths; acceptable) |
| Lint suppressions | `#[allow(clippy::too_many_arguments)]` 82 · `#[allow(dead_code)]` 37 · one crate-level `#![allow(dead_code)]` (decoder_health.rs) · 6 misc |
| Doc-comment coverage of `pub` items | 2,432 / 4,825 = 50.4 % |
| Functions > 300 lines (non-test) | 44; > 500: 13; > 1000: 5 (`plurx-cluster-check::run_membership_lifecycle_case` 1,764; `handle_request` 1,489; `hls::create_with_purpose` 1,154; `system::update_settings` 1,099; `hiqlite::migrate_schema` 1,009) |
| Lockfile | 524 packages; plurxd compiles 529 unique crates (543 with build deps) |
| Process tooling | Python gates 27.7 k lines (validation/ 5.5 k + tests/ 22.2 k) · scripts/ 29.4 k lines · workflows+actions 4.1 k lines YAML · Makefile 1,766 lines · regression ledger 853 TOML files / 6.7 k lines · docs/ 149 k lines of Markdown |
| Commits since 2026-08-20 | 3,859 non-merge; 1,240 of them (32 %) touch `validation/regressions.d`; 1,164 touch `validation/`; 304 touch `tests/operations` (136 are `fix:`) |
| Last release tag | v0.3.0 on 2026-08-31 — ~3,100 commits ago; `workspace.package.version` still 0.3.0 |

---

### Findings

#### F-build-ops-codehealth-1: No Rust test — and no Clippy — executes in the gate that merges to `main`
- Severity: **P0** (stability/process: 3,100 commits have reached the fleet since the last tag with compile-only evidence)
- Category: stability / ops
- Confidence: CONFIRMED
- Evidence:
  - `.github/workflows/main-fast-lane.yml:106-120` — the only Rust job on a ready PR runs
    ```yaml
    - name: Compile every Rust target without executing tests
      run: |
        make effort-rust-check
        make hiqlite-vendor-clippy
    ```
  - `Makefile:75-77`: `effort-rust-check: fmt-check spike-lock-check` → `$(CARGO) check --workspace --locked --all-targets`. No `cargo test`, no `cargo clippy` on workspace code (`hiqlite-vendor-clippy` lints only `vendor/hiqlite --lib`).
  - `.github/workflows/ci.yml:3-9`: the sweep that runs `make ci-rust-gate` (fmt + clippy + unit + SQLite contracts, line 283) triggers only `on: push: tags: ["v*"]` and `workflow_dispatch`. `validation-nightly.yml`, `cluster-store-backstop.yml`, `effort-ci.yml`, `release-readiness.yml`, `fix-evidence.yml` are all `workflow_dispatch` only. `docs/DEVELOPMENT_PIPELINE.md:3-18`: "Runtime-test schedules are disabled … Full CI runs only by manual dispatch or an explicit release tag."
  - `.github/workflows/lint.yml:15-16` claims "Main pull requests run Clippy in main-fast-lane.yml after the one adversarial review." — this is false (see above); doc drift inside the workflow itself.
  - Commit `3cd127e2` (2026-09-10, "fix(ci): reserve runtime sweeps for explicit dispatch") is where this state was introduced; `git tag` shows nothing after `v0.3.0` (2026-08-31).
  - The regression ledger maps 511 corrective commits to the check named `rust-gate` and 171 to `rust-gate-ci` (`validation/regressions.d/*.toml`), i.e. to a check that no longer runs on PRs.
- Why it matters: 3,947 unit tests and the whole `history-check` ledger exist to make `fix:` commits keep their regression; none of it fires between merge and deploy. The pre-commit hook (`scripts/pre-commit` → `make precommit-check` = clippy ~3 min warm) is optional and bypassable, so even `-D warnings` compliance on `main` is unverified today. With ~145 commits/day from multiple agents, the first execution of the suite is whenever a human dispatches `ci.yml`. This is exactly the situation the fast lane's own comment describes ("the third was carrying a fix for a regression the first had already shipped to the fleet").
- Proposed change (industry practice: every mainline merge runs the unit suite; Google/Meta "presubmit" = build+unit, "postsubmit" = the rest):
  1. Add one `cargo test -p plurxd --bin plurxd` + `cargo test -p plurx-core --lib` step to `rust_compile` in `main-fast-lane.yml` behind the existing cargo-cache (docs/DEVELOPMENT_PIPELINE.md §4 measured 2 min warm / 4 min cold for 1,504 tests — the fast lane has an 8-minute budget left of its `timeout-minutes: 10`; raise to 15).
  2. Add `cargo clippy --workspace --all-targets -- -D warnings` there too, or delete the false comment in lint.yml.
  3. Re-enable a **scheduled** post-merge `ci.yml` on `main` (every 2–4 h, `concurrency: cancel-in-progress`) so the full fan-out runs without a human, and publish its badge.
  4. Fix the doc: `lint.yml:15`, `DEVELOPMENT_PIPELINE.md` header.
- Size: S · Risk: low

#### F-build-ops-codehealth-2: There is no backup or restore for the authoritative database once Hiqlite is activated
- Severity: **P0** (data loss exposure)
- Category: ops / stability
- Confidence: CONFIRMED
- Evidence:
  - `docs/OPERATIONS.md:351-356`: "There is no automated quorum-aware backup, restore, or permanent-majority recovery path for an activated cluster. No active milestone owns one. A post-activation code rollback therefore means rolling forward…"
  - `docs/OPERATIONS.md:391-399`: "**On an activated node those snapshots are not restore points.** … Capturing current replicated state as a portable backup does not exist."
  - `Cargo.toml:39`: `hiqlite = { version = "=0.14.0", default-features = false, features = ["auto-heal", "macros", "sqlite"] }` — upstream Hiqlite's `backup` feature (`vendor/hiqlite/Cargo.toml:52-56`, `backup = ["dep:cron", "s3", "sqlite"]`: leader-side `VACUUM INTO` on a cron, optional S3 push) is deliberately off.
  - The pre-deploy snapshot the Ansible path takes (`OPERATIONS.md:386-390`) copies `plurx.db`, which after activation is frozen at import time.
  - Schema moved 35 versions this month (`crates/plurx-core/src/store/hiqlite.rs:69-94`), forward-only, in a hand-rolled 1,009-line `migrate_schema` (line 1533) — any migration bug is unrecoverable without a backup.
- Why it matters: the default M2 path ("selects one-voter Hiqlite after a verified SQLite import", `Cargo.toml:36-37`) means every install eventually has its users, API keys, watch state and library identity only inside `<data>/hiqlite/` — a Raft log + SQLite state machine that is *never* consistently snapshotted while running. A bad migration, a disk error, or a bad `fix:` in one of the 66 vendor/hiqlite commits this month is unrecoverable. Jellyfin and Plex both ship scheduled DB backups by default (Jellyfin: `/config/data/…backup`, Plex: "Backup database every 3 days").
- Proposed change:
  1. Turn on Hiqlite's `backup` feature (already vendored; adds `cron` only — verify `s3` can be made optional in the fork) or implement `plurxd backup` as a leader-side `VACUUM INTO <data>/backups/hiqlite-<ts>.db` through the existing consistent-read client, plus `plurxd restore-from-backup` that bootstraps a fresh single-voter cluster from that file (Hiqlite already supports restore-from-backup at start via `HQL_BACKUP_RESTORE`).
  2. Schedule it (daily, keep 7) from the daemon, not Ansible; expose the last-success timestamp on `/metrics` and Settings → System.
  3. Document in OPERATIONS.md the restore drill and test it in `container-smoke`.
- Size: M · Risk: med (touches the vendored fork; but the feature exists upstream)

#### F-build-ops-codehealth-3: Production lifecycle code is compiled differently under test — 681 `#[cfg(test)]` seams inside shipping items
- Severity: P1 (stability: the tested binary is not the shipped binary on the hot lifecycle paths)
- Category: architecture / stability
- Confidence: CONFIRMED
- Evidence:
  - `crates/plurxd/src/transcode.rs:3290-3293`:
    ```rust
    settled: tokio::sync::Notify,
    #[cfg(test)]
    wait_before_await_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
    ```
    and `:3355-3365`, `:3407-3425`, `:3488-3500`, `:3670-3672`, `:3743-3753`, `:5298-5305` (`signal_after_authorization_pause`, `signal_after_flow_reservation_pause`, `terminate_before_reap_pause` fields on the attempt supervisor).
  - Count: `grep -rn '#\[cfg(test)\]' crates/*/src` = 861; 180 precede a `mod`; 681 are fields, branches, `if let Some(pause)` awaits, or `store(true, Release)` inside production functions (transcode.rs 208, playback_control.rs 148, http/hls.rs 60, vodserve.rs 49, dv_disk.rs 36, decode_facts.rs 30, state.rs 14, media_sessions.rs 11).
- Why it matters: (a) struct layouts, `Arc<Mutex<Option<…>>>` lock acquisitions and `.await` points differ between `cargo test` and `cargo build --release`, so races the tests pin (the "settled" / "retirement handoff" ordering in the transcode supervisor) are proven only for the test-shaped state machine; (b) every extra `.await` on a test barrier is a cancellation point the release build does not have; (c) the seams cannot be exercised in the release binary during a field incident. This is the pattern `fail`/failpoints, `tokio::sync::Notify` injection via a trait, or a `#[cfg(feature = "test-seams")]` cargo feature exist to replace — and the repo already has the precedent (`plurx-core` `scratch-fault-injection` feature, `Cargo.toml` of plurx-core:24-27, `PLURX_CLUSTER_ACTIVATION_FAILPOINT` env failpoint).
- Proposed change: introduce one `Hooks` trait object (or the `fail` crate with `fail::fail_point!("transcode.before_reap")`) held once on `Session`/`AttemptSupervisor`; production installs a zero-cost no-op, tests install pausing hooks. Do it file by file starting with transcode.rs (208 sites) and playback_control.rs (148). Compile-time layout then matches; `cargo build --features failpoints` can even reproduce field ordering bugs. Reference: TiKV's `fail-rs` usage; tokio's own `tokio::test` guidance to use `Notify`/`Barrier` via injected handles.
- Size: L (mechanical but 681 sites) · Risk: med

#### F-build-ops-codehealth-4: Unit and integration tests run against ffmpeg 6 while the shipped image asserts ffmpeg 8 capabilities
- Severity: P1 (video-quality/stability: the test oracle is a different encoder)
- Category: video-quality / ops
- Confidence: CONFIRMED
- Evidence:
  - `.github/workflows/ci.yml:267`: `runs-on: [self-hosted, Linux, X64, lab, general, high-cpu, ffmpeg-6]` for "Rust unit and SQLite contracts"; same `ffmpeg-6` label on `cluster_daemon` (848), `web_layout` (882), `vod_web` (930), `coverage` (1228), `release-readiness.yml:14`, `validation-nightly.yml:17`.
  - `.github/actions/ffmpeg/action.yml:9-14`: "ffmpeg 8 declares `-readrate_initial_burst` and ignores it, which silently costs the copy path its startup burst (#380, fixed by #386). **CI never caught it because CI has never run ffmpeg 8**".
  - `Dockerfile:63,85-100`: the runtime installs `jellyfin-ffmpeg8` and hard-fails the build unless `dovi_rpu` bsf (ffmpeg ≥ 7.1), AC-4 decoder and `tonemapx apply_dovi` exist; `PLURX_FFMPEG=/usr/lib/jellyfin-ffmpeg/ffmpeg` (line 147).
  - `Makefile:56-64` (`vodencode-restart-check`) and every `require_ffmpeg()` test (`transcode.rs:5137`) spawn whatever `ffmpeg` is on PATH.
- Why it matters: filter graphs, `-readrate`, `tonemapx`, `dovi_rpu`, `libplacebo`, VAAPI/QSV option names and the CLI's stderr wording (parsed by `decode_facts.rs`/`decoder_health.rs`) differ across 6 → 8. Every ffmpeg-driven regression test passes on a build the fleet does not run.
- Proposed change: provision `jellyfin-ffmpeg8` on the `high-cpu` Linux runners (same apt repo as the Dockerfile) and make it the default lane; keep one `ffmpeg-6` lane only if distro-ffmpeg fallback is a supported configuration. Better: run the Rust unit job inside the `runtime-assets` Docker stage so the test ffmpeg is byte-identical to production. Jellyfin's own CI tests against the jellyfin-ffmpeg build it ships.
- Size: S–M · Risk: low

#### F-build-ops-codehealth-5: Vendored Hiqlite is a hard fork under active repair — 66 commits / 51 `fix:` in 30 days, +78 k lines
- Severity: P1 (stability signal + maintenance liability)
- Category: architecture / stability
- Confidence: CONFIRMED
- Evidence:
  - `git log --since=2026-08-20 -- vendor/hiqlite`: 66 commits, 51 with `fix` subjects; `vendor/` insertions 78,212 / deletions 3,983 (2.3 MB dir).
  - `vendor/hiqlite/PLURX-PATCH.md`: "Plurx carries **fifteen** compatibility patches" covering leader election trigger, snapshot RPC error boundary, WebSocket frame flushing/timeouts, connection supervisors, retained reset notifications, snapshot file-ownership/fsync/rename, proxy-mode reconnect, `ForwardToLeader(None,None)` handling — i.e. the consensus transport and durability layer, not glue.
  - `vendor/hiqlite-wal/PLURX-PATCH.md`: three WAL recovery patches (purge boundary reconstruction, atomic `meta.hql` replacement, mmap incarnation guard) — each describes a crash/startup-panic class found in the field.
  - Upstream is 0.14.0 = latest (crates.io, 2026-07-06); openraft 0.9.25 = latest 0.9. No upstream PRs are referenced in the patch notes.
- Why it matters: plurx now owns correctness of a Raft transport it did not write, with no upstream review, tested only by its own harness (which, per F-1, does not run on merge). Every patch note ends "Remove this patch only after upstream …", but nothing tracks upstreaming. The repeated-fix rate on this directory is the highest in the repo outside `Makefile`.
- Proposed change: (1) open upstream issues/PRs for the durability patches (snapshot fsync/rename, WAL meta atomic replace, frame flush) — these are generic bugs; (2) freeze the fork API surface behind `plurx-core::store::hiqlite` and add a fork ledger under `docs/cluster/` (e.g. HIQLITE-FORK) with upstream links + "drop condition" per patch; (3) run `cluster-wal-check` + `cluster-store-check` on a schedule (see F-1). Reference: how CockroachDB/TiKV carry `etcd/raft` forks with an explicit upstream-sync cadence.
- Size: M · Risk: low (process)

#### F-build-ops-codehealth-6: Two crypto backends, two `reqwest`s, QUIC and an S3 client are compiled into plurxd because of one unconditional feature edge in the vendored fork
- Severity: P2 (build time, binary size, attack surface)
- Category: performance (build) / architecture
- Confidence: CONFIRMED
- Evidence:
  - `cargo tree -d` (this session): `aws-lc-sys v0.39.1` **and** `v0.43.0`, `aws-lc-rs v1.17.3`, `ring v0.17.14`, `reqwest 0.12.28` and `0.13.4`, `quinn/quinn-proto/quinn-udp v0.11`, `rand 0.8/0.9/0.10`, `thiserror 1/2`, `syn 2/3`, `getrandom 0.2/0.3/0.4`, `sha2 0.10/0.11`, `base64 0.13/0.21/0.22`, `hashbrown 0.16/0.17`.
  - Root cause: `vendor/hiqlite/Cargo.toml:190-192` `[dependencies.cryptr] version = "0.10" features = ["s3"]` (unconditional) → `cryptr` → `s3-simple` → `vendor/s3-simple/Cargo.toml:58-62` direct `aws-lc-rs` + `aws-lc-sys 0.39` + `quinn`. plurx disables hiqlite's `backup`/`s3` features (`Cargo.toml:39`) but the edge is not gated. `vendor/s3-simple/PLURX-PATCH.md:7-9` acknowledges: "Hiqlite 0.14 enables cryptr's S3 feature even when plurx builds hiqlite with only `macros` and `sqlite`. That makes this otherwise unused package part of the production resolution."
  - `cargo tree -i aws-lc-rs`: rustls → `axum-server` (`tls-rustls` pulls rustls default `aws-lc-rs` provider) and `hyper-rustls`, while the workspace deliberately picks `ring` (`crates/plurx-core/Cargo.toml:37-43`, `plurx-cluster-check/Cargo.toml:21`).
- Why it matters: `aws-lc-sys` is a CMake/C build (two copies!) and `onig_sys` a second C build; on a cold self-hosted runner these are minutes of the "fast Rust gate" and every `docker build`. Two TLS crypto providers in one process is also the classic rustls "no default provider / multiple providers" panic vector the code already works around (`plurx-core/Cargo.toml:32-36` comment). `s3-simple`, `quinn`, `cryptr` are dead weight in the shipped binary (and in `cargo audit` surface — the vendor exists *only* to patch an advisory in a crate nothing calls).
- Proposed change: in the fork (already patched 15 times) make it `cryptr = { version = "0.10", default-features = false }` and add `s3 = ["backup", "cryptr/s3"]`; set `axum-server = { features = ["tls-rustls-no-provider"] }` only; add `rustls = { default-features = false, features = ["ring","std","tls12"] }` to the workspace so unification lands on ring. Then delete `vendor/s3-simple` (and its audit special-casing in `scripts/vendor-audit-lock`). Verify with `cargo tree -i aws-lc-sys` → empty. Add `[bans] deny = [{ name = "aws-lc-sys" }, { name = "openssl-sys" }, { name = "native-tls" }]` to `deny.toml` so it stays out.
- Size: S · Risk: low

#### F-build-ops-codehealth-7: Semantic search drags a CPU ML stack (candle + gemm + rayon + Oniguruma C) into every build for a 423-line optional feature
- Severity: P2 (build time / binary size / RSS)
- Category: performance (build) / architecture
- Confidence: CONFIRMED
- Evidence:
  - `crates/plurxd/Cargo.toml:26-30`: `candle-core`, `candle-nn`, `candle-transformers` `=0.11.0`, `tokenizers = { features = ["onig"] }` — unconditional dependencies.
  - `crates/plurxd/src/library_search/semantic.rs` (423 lines) is the only consumer: `all-MiniLM` BERT via `candle_transformers::models::bert`, model 90.8 MB downloaded at runtime (`FILES` const, line 23-38).
  - `cargo tree -p plurxd` subtree: `gemm` ×7 crates, `pulp`, `rayon`, `rayon-core`, `half`, `safetensors`, `onig` + `onig_sys` (C library), `esaxx-rs`, `spm_precompiled`, `fancy-regex`, `derive_builder`, `monostate`, … ≈60 crates.
  - The crate comment on `live-hls-recovery` (`plurxd/Cargo.toml:12-19`) records a deliberate "no compile flags" policy — but that reasoning was about a *default-on* feature that nothing disabled; this one is default-off at runtime ("loaded only when semantic search is enabled").
- Why it matters: `gemm`'s generic SIMD kernels are among the slowest crates to compile in the ecosystem; `onig_sys` compiles Oniguruma from C in `cargo check` too (build script). `rayon` also spawns a global thread pool (num_cpus threads) on first use inside a tokio process. For a media server whose CPU budget belongs to ffmpeg, this is the wrong default trade.
- Proposed change: (1) `tokenizers = { default-features = false, features = ["fancy-regex"] }` (pure Rust; removes the C build, supported upstream); (2) gate the whole module behind `[features] semantic-search = ["dep:candle-core", …]`, default **on** for the Docker image (so the runtime story is unchanged) but off in the fast lane (`cargo check --no-default-features`) — or move it to a separate `plurx-embed` binary spawned like ffmpeg, consistent with the process-isolation design in ARCHITECTURE.md; (3) limit rayon: `rayon::ThreadPoolBuilder::new().num_threads(2).build_global()` before first embed.
- Size: S (feature gate) / M (separate process) · Risk: low

#### F-build-ops-codehealth-8: systemd unit and Compose have no memory/task/OOM/fd limits; ffmpeg children run at daemon priority
- Severity: P1 (stability under load; possible daemon OOM-kill instead of the transcode)
- Category: ops / stability
- Confidence: CONFIRMED (absence) / LIKELY (fd exhaustion consequence)
- Evidence:
  - `deploy/plurxd.service` full text: `NoNewPrivileges`, `ProtectSystem=strict`, `ReadWritePaths`, `ProtectHome` — and nothing else. No `LimitNOFILE`, `MemoryMax`/`MemoryHigh`, `TasksMax`, `OOMScoreAdjust`, `PrivateTmp`, `ProtectKernelTunables`, `ProtectControlGroups`, `RestrictSUIDSGID`, `LockPersonality`, `RestrictRealtime`, `CPUWeight`/`IOWeight`.
  - `deploy/docker-compose.yml`: no `mem_limit`, `pids_limit`, `ulimits`.
  - `grep -rn 'setrlimit|RLIMIT|nice|setpriority|ionice|oom_score_adj|cgroup' crates/plurxd/src crates/plurx-core/src` → **no matches** in non-test code. Children are spawned via `tokio::process::Command` with only `kill_on_drop` and fd inheritance (`crates/plurxd/src/ffmpeg.rs:84-114, 151-156`).
  - `vendor/hiqlite/PLURX-PATCH.md` first patch: duplicate connection targets "eventually exhausts file descriptors" — fd pressure has already been observed.
- Why it matters: (a) systemd's default soft `RLIMIT_NOFILE` for services is **1024**; Rust std/tokio do not raise it. Each HLS session = ffmpeg pipes (3) + held source fd + up to 7 reserved sidecar fds (`ffmpeg.rs:88`) + segment files + client sockets (a TV client opens 6+), plus Hiqlite pool (4 readers + writer), WAL mmaps, mDNS, GDM. A dozen sessions plus a scan can hit EMFILE, which surfaces as random `accept`/`open` failures rather than a clear error. (b) With no `OOMScoreAdjust`, the OOM killer picks by RSS; a plurxd holding a 90 MB embedding model, log rings and caches can be chosen over a wedged x265 child — Plex/Jellyfin both run under Docker `mem_limit` or a unit with `OOMScoreAdjust`. (c) Software 4K HEVC → AV1/x265 children at nice 0 starve the daemon's HTTP threads (segment delivery stalls even though the encode is "ahead").
- Proposed change: unit: `LimitNOFILE=65536`, `OOMScoreAdjust=-500`, `TasksMax=4096`, `MemoryHigh=…` (documented knob), `PrivateTmp=true`, `ProtectKernelTunables=true`, `ProtectControlGroups=true`, `RestrictSUIDSGID=true`, `LockPersonality=true`, `RestrictRealtime=true`, `ProtectClock=true`, `RestrictNamespaces=true` (all compatible with `/dev/dri` via `SupplementaryGroups`). Daemon: in `pre_exec` for ffmpeg/ffprobe/dovi_tool children `setpriority(PRIO_PROCESS, 0, 10)`, `ioprio_set(IOPRIO_CLASS_BE, 7)` and write `oom_score_adj=+500` (children *should* die first); raise soft NOFILE to hard at startup and log it. Compose: `ulimits: nofile: 65536`, `pids_limit: 4096`, and a documented `mem_limit`. Reference: `systemd.exec(5)`/`systemd.resource-control(5)`; Jellyfin's `jellyfin.service` ships `LimitNOFILE`; Plex Transcoder is niced by the server.
- Size: S · Risk: low

#### F-build-ops-codehealth-9: The `history-check` + `tests/operations` text-contract gates cost a third of all commits and mostly assert prose, not behaviour
- Severity: P2 (velocity / maintainability; masks real signal)
- Category: architecture (process)
- Confidence: CONFIRMED
- Evidence:
  - `validation/history.py:21-37` `ISSUE_RE`: a commit whose subject contains *keep, remove, bound, use latest, route, scope, signal, reuse, normalize, refresh, preserve, avoid, correct…* is "corrective" and must add a test or a ledger file. Result: `validation/regressions.d/` = 853 files; 139 entries are `ignore = true`; 151 reasons cite "ledger-only"/"documentation"/"non-runtime"; `0022fc43-activity-ledger.toml` reason: "The history-regressions check requires this ledger-only correction to remain mapped to the validation framework…" (a ledger entry whose reason is the ledger).
  - Churn: 1,240 / 3,859 commits touch `validation/regressions.d`; 176 commit subjects are about the ledger/catalog itself; `tests/operations`: 304 commits, 136 `fix:` — the second-highest fix rate in the repo after `Makefile` (70 fixes / 131 commits).
  - Representative text tests: `tests/operations/test_contracts.py:1067-1093` (`assertEqual(dockerfile.count("&& apt-get clean"), 2)`, `assertIn("-h filter=tonemapx", dockerfile)`), 80 `read("Makefile|Dockerfile|.github/…|deploy/…")` string asserts across `tests/`; `test_mobile_build_claims.py` regex-matches STATUS.html markers; `test_status_pr_claims.py` greps prose for "OPEN".
  - Meanwhile the checks these ledgers point at (`rust-gate`: 511 entries) do not run on merge (F-1).
- Why it matters: the gate's failure mode is a *green* status that costs an agent a ledger edit per commit and never a red one for a real regression; the Dockerfile's own `RUN … || exit 1` assertions are the actual test of the Dockerfile. Every refactor of a Makefile target, workflow step or Dockerfile line breaks a Python test that must be re-worded, which is where the 136 `fix:` commits in `tests/operations` come from. 27.7 k lines of Python + 29 k lines of scripts is a second codebase with its own bug rate, competing with the 3,947 Rust tests for maintenance.
- Proposed change (cost-down without losing signal):
  1. Replace `ISSUE_RE` with an explicit conventional-commit rule: only `fix(...)`/`perf(...)` subjects are corrective; drop the verb heuristics. Let the ledger be *opt-in* metadata (`Regression-Test:` trailer in the commit) — the compiler and `cargo test` are the enforcement, not the ledger.
  2. Delete text-matching contract tests whose subject already has a runtime assertion (Dockerfile capability checks, Compose port stability, `apt-get clean` ordering). Keep the ones that protect security posture (`persist-credentials: false`, immutable action pins) — those are ~10 tests.
  3. Replace `test_docs_index.py`/`test_status_pr_claims.py` with a markdown-link checker (lychee/`markdown-link-check`) in the docs-only lane.
  4. Convert catalog/point ownership (`validation/points.toml`, 991 lines) to CODEOWNERS-style path globs read by `ci_scope.py` only.
  Target: `tests/` Python < 8 k lines, `regressions.d` frozen (no new entries required), fast-lane preflight unchanged in wall-clock.
- Size: M · Risk: low

#### F-build-ops-codehealth-10: Raw `libc` in 380 `unsafe` blocks where `rustix` (already in the tree) gives the same syscalls safely
- Severity: P2 (memory-safety surface; review cost)
- Category: security / architecture
- Confidence: CONFIRMED
- Evidence:
  - `crates/plurxd/src/dv_disk.rs:1154` `unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) }`; `:2134` `unsafe { libc::fsync(parent.raw_fd()) }`; `:2140` `unsafe { libc::unlinkat(...) }`; `:2416-2426` `renameat`/`renameatx_np`; `fs_secure.rs` 118 blocks (`openat`, `fstatat`, `O_NOFOLLOW` walks); `decode_facts.rs` 64 blocks (pidfd, fork/exec supervisor); `process_control.rs` 15; `ffmpeg.rs:96-113` `pre_exec` dup2 dance.
  - `// SAFETY` comments: 100 for 380 blocks (26 %). `Cargo.toml:96` `libc = "0.2"` is a direct workspace dep; `rustix` is already compiled (via `tempfile`, `tokio` → `mio`), so switching costs no new crate.
- Why it matters: every `RawFd` juggled across `.await` boundaries (the code's own comment at `dv_disk.rs:1161-1163` explains one such lifetime hazard) is an I/O-safety bug class rustix's `BorrowedFd`/`OwnedFd`-typed API removes at compile time; `openat2`/`RESOLVE_BENEATH` (rustix::fs::openat2) would also replace much of `fs_secure.rs`'s hand-rolled no-follow walk with the kernel's own guarantee (Linux ≥ 5.6).
- Proposed change: adopt `rustix` for fs/process/io syscalls; keep `libc` only for `fork`-side async-signal-safe code (`inherit_file_descriptors`, pidfd bootstrap). Add `#![deny(unsafe_op_in_unsafe_fn)]` and `clippy::undocumented_unsafe_blocks = "warn"` to `[workspace.lints]` so the remaining blocks each carry a SAFETY comment. Reference: Rust I/O safety RFC 3128; `cap-std`/`rustix` as used by wasmtime for exactly this sandboxing pattern.
- Size: M · Risk: med (fs_secure is security-critical; do it with the existing bind-mount tests as oracle)

#### F-build-ops-codehealth-11: Build profile leaves easy wins on the table (debuginfo for panics, fat LTO/CGU=1, overflow checks, floating base image)
- Severity: P3
- Category: performance / ops
- Confidence: CONFIRMED
- Evidence:
  - `Cargo.toml:143-145`: `[profile.release] lto = "thin"  strip = "symbols"` — no `codegen-units`, no `debug`, no `overflow-checks`, no `split-debuginfo`. `panic` is left at `unwind` — **correct**: the design contains panics per tokio task/connection; `panic = "abort"` would turn one bad `expect("store")` into an outage for every session.
  - No `.cargo/config.toml` → no `target-cpu` flags (correct for a distributable binary); no `lld`/`mold` for Linux links (lld is installed only in the Windows cross job, `main-fast-lane.yml:153`).
  - `Dockerfile:3` `FROM rust:1-bookworm` (floating) vs `Dockerfile.store-shard:9` `FROM rust:1.97.1-bookworm` and `rust-toolchain.toml` pin 1.97.1 → the image build downloads the pinned toolchain through rustup on every uncached build (rustup home is not a cache mount).
  - No `sccache`; the `cargo-cache` action bounds a runner-local target dir (well done), but `Swatinem/rust-cache` is still used in `lint.yml:27` and `release-readiness.yml:20` — the very cache the action's comment says has no eviction on Forgejo.
- Why it matters: `strip = "symbols"` removes function names from panic backtraces (`RUST_BACKTRACE=1` prints addresses only) — the first thing an SRE wants from a field panic. Fat LTO + `codegen-units = 1` typically gives 5–10 % smaller binary and a few % CPU for release with no runtime risk (cost: build time on the release lane only). `overflow-checks = true` in release is cheap insurance for the byte parsers (fmp4.rs already uses 37 `checked_*` — the remaining `+` sites wrap silently in prod, panic in test).
- Proposed change:
  ```toml
  [profile.release]
  lto = "fat"; codegen-units = 1; debug = "line-tables-only"; strip = "debuginfo"  # keeps symbol names
  overflow-checks = true
  [profile.ci]  inherits = "dev"; debug = 0; incremental = false   # fast-lane check/test
  ```
  Pin `rust:1.97.1-bookworm` in `Dockerfile`; add `RUN --mount=type=cache,target=/usr/local/rustup`; add `-C link-arg=-fuse-ld=lld` via `.cargo/config.toml` for Linux x86_64/aarch64 (halves link time of a 500-crate binary); drop `Swatinem/rust-cache` from the two remaining workflows.
- Size: S · Risk: low

#### F-build-ops-codehealth-12: No panic hook, no structured/JSON log mode, and ANSI escapes are written to journald/docker by default
- Severity: P3 (ops ergonomics; but it hides crashes from the in-app log)
- Category: ops
- Confidence: CONFIRMED
- Evidence:
  - `grep -rn set_hook crates/plurxd` → none (only a test-scoped hook in `plurx-core/src/cluster/migration.rs:2519`). A panic in a spawned task prints via the default hook to stderr and never enters `logbuf::LogBuffers` (`main.rs:1533-1560`), so Settings → System shows nothing when a session task dies.
  - `main.rs:1544` `tracing_subscriber::fmt::layer()` with no `.with_ansi(...)`, `.json()`, or `.with_target()`; `Cargo.toml:63` `tracing-subscriber = { features = ["env-filter"] }` keeps default `ansi`. `tracing-subscriber-0.3.23/src/fmt/fmt_layer.rs:743`: `let ansi = cfg!(feature = "ansi") && env::var("NO_COLOR")…` — no TTY detection, so `journalctl -u plurxd` and `docker logs` carry `\x1b[2m…` sequences. `Dockerfile`/`plurxd.service` do not set `NO_COLOR`.
  - `docs/OPERATIONS.md:4566-4582` documents only the human format; no JSON option, no `PLURX_LOG_FORMAT`.
- Proposed change: `std::panic::set_hook` → `tracing::error!(target="plurxd::panic", %info, backtrace=?)` then chain the default hook; `with_ansi(std::io::stdout().is_terminal())`; add `PLURX_LOG_FORMAT=json|text` (tracing-subscriber `json` feature) and mention journald `-o cat`. Reference: tokio-rs tracing guidance; Jellyfin ships Serilog JSON option.
- Size: S · Risk: low

#### F-build-ops-codehealth-13: Fuzzing covers only the PGS parser; DV RPU, fMP4, NFO/EPUB/EXIF and HLS-playlist parsers take untrusted bytes unfuzzed
- Severity: P2
- Category: security / stability
- Confidence: CONFIRMED
- Evidence: `fuzz/fuzz_targets/` contains one target, `inspect_sup.rs`; `validation-nightly.yml:73-74` runs only "bounded PGS parser fuzz campaign" (and only on manual dispatch). Untrusted parsers in tree: `crates/plurx-core/src/fmp4.rs` (8.5 k lines, box parsing), `crates/plurxd/src/dv_disk.rs` + `dolby_vision` RPU rewrite (media-supplied NAL payloads), `plurx-core/src/scan` NFO via `quick-xml`, EPUB via `zip`, `kamadak-exif`, Plex compat XML, M3U/EPG in `live_tv.rs`.
- Proposed change: add `cargo fuzz` targets for `fmp4::parse_*`, `dv_disk` RPU rewrite entry, NFO parse, EPUB container walk; seed from `tests/playback/dv-p7-rpu.hex` and fixtures; run 10 min each in the scheduled post-merge lane (F-1.3). Reference: libfuzzer targets in `image-rs`, `mp4parse-rust` (Mozilla).
- Size: S–M · Risk: low

#### F-build-ops-codehealth-14: File and function size is past the point where agents can edit safely
- Severity: P2 (maintainability; correlates with the fix rate)
- Category: architecture
- Confidence: CONFIRMED
- Evidence: 45 files > 100 KB hold 71.5 % of all Rust; `transcode.rs` 47,097 lines (its own comment at `decoder_health.rs:64-66` calls it "a 36,000-line file" — it grew 30 % since that comment), `playback_control.rs` 29,987, `http/hls.rs` 28,508, `state.rs` 12,753. Functions: `hls::create_with_purpose` 1,154 lines (`http/hls.rs:1744`), `system::update_settings` 1,099 (`http/system.rs:2463`), `hiqlite::migrate_schema` 1,009 (`store/hiqlite.rs:1533`), `media_sessions::lease_loop` 871, `state::run_cluster_fragment_index_job` 724, `http::router` 563 (`http/mod.rs:82`). `#[allow(clippy::too_many_arguments)]` ×82. `#![allow(dead_code)]` at `decoder_health.rs:66` is stale (169 external references now exist) and hides real dead code in a 3,281-line module.
- Why it matters: a 2 MB file is a 500 k-token context; agents edit it via `grep` windows, which is how test seams (F-3) and duplicated helpers accumulate; rustc/rust-analyzer incremental units are per-crate but LLVM codegen units split per file badly for one giant module.
- Proposed change: (1) `git mv` inline test modules of the top 10 files into `src/<mod>/tests/*.rs` (`#[cfg(test)] mod tests;` in the parent) — zero behaviour change, halves the files; (2) split `transcode.rs` along its existing section comments (supervisor / attempt / scratch cleanup / hold-suspend / HLS writer) into a `transcode/` directory; (3) make `migrate_schema` table-driven (`const MIGRATIONS: &[(i64, &[&str])]`) as refinery/sqlx do; (4) remove the stale crate-level allow and add `clippy::too_many_lines = "warn"` (threshold 300) to `[workspace.lints]`.
- Size: M (mechanical) · Risk: low if done as pure moves with `git mv` + `cargo fmt` diff review

#### F-build-ops-codehealth-15: Release/versioning has stalled — 0.3.0 for 3 weeks and 3,100 commits, CHANGELOG [Unreleased] under-reports by an order of magnitude
- Severity: P2 (ops: rollback needs a known-good artifact)
- Category: ops
- Confidence: CONFIRMED
- Evidence: `git tag` → `v0.3.0` (2026-08-31), `v0.2.8` (2026-08-29); `Cargo.toml:18` `version = "0.3.0"`; CHANGELOG `[Unreleased]` = 339 lines for ~3,100 commits (0.3.0 section itself was 116 lines for ~200 commits). `docs/OPERATIONS.md:223-340` describes a fleet registry of untagged `main` images ("build once, pull everywhere") — so the fleet runs an unversioned rolling head, and "Rolling back a deploy" (§341) has no tagged image to roll back to.
- Proposed change: cut `v0.3.x` tags at least weekly from a green scheduled `ci.yml` run (F-1.3); make `publish-release.yml` the only path to the fleet registry alias; keep the last two tags' images pinned for rollback. Reference: Jellyfin's weekly unstable + monthly stable tags.
- Size: S · Risk: low

---

### Already good — do not touch

1. **`unwrap_used` lint at workspace level with `-D warnings`**: zero `.unwrap()` in ~315 k lines of production Rust is rare and valuable (`Cargo.toml:107-108`).
2. **Exact toolchain pinning** (`rust-toolchain.toml` 1.97.1, `dtolnay/rust-toolchain@<sha>`, immutable action SHAs, `persist-credentials: false` everywhere).
3. **`tokio` features enumerated, not `full`**; `reqwest` with `default-features = false, rustls-tls`; no `openssl`/`native-tls` anywhere in the graph.
4. **Docker runtime image**: non-root user, `HEALTHCHECK` on `/readyz` (readiness, not liveness), build-time capability assertions for `dovi_rpu`/AC-4/`tonemapx`, pinned `dovi_tool` with SHA-256, `mkvtoolnix` version pinned, OCI `revision` label, licenses shipped, BuildKit cache mounts, native arm64 runner instead of QEMU.
5. **Bounded runner-local cargo cache with LRU budget and disk preflight** (`.github/actions/cargo-cache`) — solves the real Forgejo no-eviction problem; keep it.
6. **`profile.dev.package.argon2/blake2 opt-level = 3`** and **`profile.test debug = 0`** — measured, targeted, correct.
7. **`deny.toml` license allow-list is tight** (no copyleft, `confidence-threshold = 0.9`, `unknown-registry/git = deny`); `scripts/vendor-audit-lock` re-scanning vendored crates under registry provenance is a genuinely clever fix for cargo-audit's path-crate blind spot.
8. **Readiness semantics**: `/healthz` never touches storage; `/readyz` reports `maintenance`/`quorum unavailable`/`store unavailable` distinctly (`http/mod.rs:1003-1085`); `plurxd healthcheck` speaks raw HTTP/1.0 with no reqwest dependency.
9. **Prometheus metrics are fixed-cardinality** and scrape-free of Store I/O (`OPERATIONS.md:4602-4605`).
10. **`kill_on_drop` + job/process-group ownership** on every child spawn (`ffmpeg.rs`, `process_control.rs`), bounded stderr tail (`drain_diagnostics`, 8 KiB) — no unbounded child output buffers.
11. **Compose**: bind-mounted data dir (not a named volume) with the rationale written down; `name: plurx` project pinning; discovery split to a host-network sidecar.

### Hot spots (fix commits since 2026-08-20, my area)

| Path | commits | `fix:` |
|---|---|---|
| `validation/` | 1,164 | 97 |
| `tests/operations/` | 304 | **136** |
| `scripts/` | 159 | 56 |
| `docs/OPERATIONS.md` | 138 | 56 |
| `Makefile` | 131 | **70** |
| `.github/workflows/ci.yml` | 101 | 30 |
| `vendor/hiqlite/` | 66 | **51** |
| `deploy/` | 45 | 22 |
| `Dockerfile` | 14 | 9 |
| `deploy/runner-janitor/` | 14 | 9 |
| `.github/workflows/main-fast-lane.yml` | 15 | 4 |

Reading: the process layer (Makefile, Python contracts, workflows, scripts) has a higher fix density than most product code, and `vendor/hiqlite` is being repaired at ~2 fixes/day.

### Open questions (not settleable from code)

1. How often has `ci.yml` been dispatched manually since 2026-09-10, and has `make ci-rust-gate` (clippy `-D warnings` + unit suite) passed on current `main` at all? (`validation/ci-flake-ledger.json` is from 2026-08-09 and predates the change.)
2. Do the fleet nodes run under systemd or Compose (the private Ansible is not in the repo)? The fd-limit and OOM findings (F-8) differ in severity between the two.
3. Actual release binary size and RSS at idle/under 4 transcodes — no `target/` present and nothing in STATUS.md records it; the store-contract test binary is described as "100+ MB" and a full debug tree as 21 GB.
4. Has any of the 15 Hiqlite patches or 3 WAL patches been proposed upstream? No issue/PR links appear in `PLURX-PATCH.md`.
5. What fraction of agents install `make hooks`? If most commit with `--no-verify`, Clippy has effectively not run on the tree since 2026-09-10 (see F-1).
6. Is Windows (native service, `deploy/windows`, `windows-service` crate, 30-minute cross-compile job on every PR) a supported target the owner wants, or agent-driven scope creep? It is the single most expensive fast-lane job.
