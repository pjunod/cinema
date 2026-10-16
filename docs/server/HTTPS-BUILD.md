# HTTPS build — checkpoints, ownership and qualification

**Status:** implementation built; physical acceptance open; merge qualification tracked by task and promotion PRs · **Written:** 2026-10-15
**Owner:** GPT-6.1 Sol builder; parent coordinates and reviews

This canonical ledger executes [the contract](HTTPS-IMPLEMENTATION.md).
Work checkpoint by checkpoint; flag trust/lifecycle contract changes before
implementing them. No live deployment is implied by this build.

## 1. Isolation and compiler prerequisite

Use the dedicated HTTPS worktree from `9eb7f6738`; the main checkout contains
unrelated user work. Integrate one cohesive implementation task, PR #1118,
into `effort/https-support`. H1–H4 are checkpoints owned by Sol across the same
shared files, not separate task branches or intermediate landings. The
disjoint-task exception does not apply. Follow [AGENTS.md](../../AGENTS.md) and the
[pipeline](../DEVELOPMENT_PIPELINE.md), with the session's explicit contributor
instructions taking priority over conflicting older descriptions.

PATH initially selected Homebrew Rust 1.98.0; pinned Rust 1.97.1 is installed
and verified with `rustup run 1.97.1 rustc --version`. Sol must establish a
successful baseline check before Rust edits and use the pinned compiler,
Clippy and rustfmt throughout. If local dependencies block, use the
[source-only loop](../ci/AGENT-COMPILE-LOOP.md). Keep target warm; do not use
CI to discover compiler failures. Recheck the exact current candidate.

## 2. Review and checkpoint ownership

| Step | Owner/files | Acceptance |
|---|---|---|
| D0 | Parent: contract, ledger, review, index/backlog | Independent adversarial review resolved before build release. |
| H1 | Sol: core config; daemon TLS module; manifests/lock; tests | Default/precedence, validation, persistence and trusted identity tests; pinned check/Clippy. |
| H2 | Sol: startup, HttpAcceptor, drain, TLS and URL consumers | Real TLS, HTTP parity, handshake concurrency/cap/deadline, observation transition and shutdown tests. |
| H3 | Sol: AppState, admin route, router, existing settings scripts | Admin/node/CAS/persistence/env guards, saved-vs-active UI, safe links and restart/disable tests. |
| H4 | Sol: maintained references, ledger/index/backlog and validation | Focused suites, affected checks, parent review, qualification and explicit acceptance gaps. |

### Approved workflow amendment — 2026-10-15

Independent workflow review approved consolidating the already-built H1–H4
checkpoints into one cohesive end-to-end implementation task, PR #1118. This
explicitly supersedes the original separate-checkpoint task PR plan before
any H2–H4 branch, commit or intermediate landing exists. Ownership and shared
file boundaries are unchanged; this is task sizing, not the disjoint-task
exception or a waiver of contributor gates. The workflow verdict is not
built-code approval.

Preserve all earlier local evidence, review findings and failed/superseded CI
history. Parent canceled only superseded run #5203 before changing its head;
its completed policy/web/Apple/Android checks are historical evidence, not a
green gate. The combined current source must pass focused regressions,
compile/static checks, the normal hook and one fresh exact-final-head Effort
development gate before PR #1118 merges into the effort. The PR and landing
carry the complete deduplicated `Regression-Test: <path>::<test name>` list.
Use `fix(https): ...` for user-visible behavior. Never accept red, canceled,
skipped or stale evidence as a passing gate.

Final promotion remains unchanged: freeze the candidate, merge fresh main,
obtain exactly one fresh adversarial built-code review with Sol-owned fixes,
and require the full exact-tree qualification receipt plus Main promotion
gate before merging effort into main. Parent manages PRs, gates and merges;
Sol builds and fixes. No deployment or live TLS setting is authorized.

## 3. Regression matrix

| Area | Required cases |
|---|---|
| Config | Old defaults/TOML; both/one/no paths; env and data_dir precedence; invalid boolean/port/collision; SAN bounds and IPv6. |
| Identity | Create once; restart/off-on preserve fingerprint; permissions; concurrent creation; corrupt/partial/symlink state; expired identity; SAN mismatch; oversize files; FIFO/device rejection; public-chain-only export; valid private CA; key mismatch. |
| Socket | Trusted generated/custom chain accepted; unknown trust/wrong hostname rejected; plaintext rejected on TLS; legacy HTTP still works; silent/malformed peer cannot starve healthy client. |
| Lifecycle | Pending cap/deadline; stop cancels pending and drains active streams; observation transition; bind failure; task cleanup. |
| Settings | Anonymous/non-admin denial; node-local only; node/revision conflict including after restart; no blind UI retry; atomic failed save; concurrent saves; env override conflict; disable with missing file; exact saved values after restart. |
| Web | Lazy loader/retry, node identity, active/desired distinction, restart message, toggle/paths, HTML escaping, IPv6/no-token link, graduation contracts. |
| Playback | HTTPS bootstrap/API/artwork/subtitles/HLS/ranges; generic upgrade preservation (no product WebSocket endpoint); cluster origin stays HTTPS; no global verification bypass. |

Choose exact behavioral test names at build time and record commands/results.
Minimum check shape, plus selected focused tests and affected static checks:

```bash
rustup run 1.97.1 cargo check -p plurxd --all-targets
rustup run 1.97.1 cargo clippy -p plurxd --all-targets -- -D warnings
rustup run 1.97.1 cargo fmt --all -- --check
python3 -m unittest tests.operations.test_docs_index
node --test tests/web/settings-sections.test.js
```

Core storage regressions require `--features hiqlite-store` or `make unit-core`.
Run affected validation catalog, JS syntax/assets and infrastructure checks.
H2/H3 must also run the complete rolling-producer ownership inventory module
before push; its structural counts include test fixtures.
Update SECURITY/OPERATIONS/API and other maintained claims that public HTTPS
is absent. New web files require shell/WEB_ASSETS/layout-map updates together.

## 4. Promotion and remaining acceptance

Freeze tasks, integrate current main, reverify the exact candidate and resolve
parent code review before promotion. Require Main promotion gate and the
exact-tree qualification receipt under contributor rules. Never deploy or
change live TLS settings as a side effect of building.

Keep physical browser/native trust and playback acceptance explicit. Keep
the usable Developer switch with named outstanding evidence until accepted;
then graduate it to System. Built source awaiting physical proof is
**acceptance**, not complete physical qualification.

## 5. Evidence ledger

| Date | Step | Result |
|---|---|---|
| 2026-10-15 | D0 baseline | Isolated source `9eb7f6738`; pinned Rust 1.97.1 verified; Sol baseline check established before edits. |
| 2026-10-15 | D0 compiler | Sol baseline pinned `cargo check -p plurxd --locked` and formatting passed; warm target retained. |
| 2026-10-15 | D0 review | [Independent adversarial review](HTTPS-REVIEW.md): five P2 clarifications incorporated; architecture approved. Sol 6.1 released to build H1–H4. |
| 2026-10-15 | H1 source review | Parent reviewed configuration, filesystem/identity ownership and persistence outcomes. Fixed masked-value precedence, intentional data-root symlinks, read-only directory validation and post-publication sync semantics; no remaining source blocker. |
| 2026-10-15 | H1 compiler/lint | Pinned Rust 1.97.1 all-target daemon check and Clippy with `-D warnings` passed; final test-only `unwrap` calls replaced with explicit fixture assertions. Catalog maps both new source paths. |
| 2026-10-15 | H1 focused evidence | Current-source core `--features hiqlite-store --lib config::`: 22 passed; daemon `--bin plurxd https::tests::`: 8 passed, including normal generated/private-CA trust, unknown trust/hostname rejection and FIFO bounds. Formatting, docs index (4 tests), catalog and infrastructure-name checks passed. |
| 2026-10-15 | H1 base refresh | Manual effort run #5158 refused authenticated history binding because current main was not an ancestor of the effort. Merged main `a0b8cb72c` into effort normally as `4fb1c1943`, then merged that refreshed effort into H1 without conflicts or rebasing. Earlier H1 results are retained history; the exact combined source has now passed 22 configuration tests, 8 identity/trust tests, all-target compilation, formatting, docs index (4 tests), infrastructure checks and catalog lint. Normal pinned workspace hook is required on the merge commit before push. |
| 2026-10-15 | H1 landing attribution | Effort run #5161 passed ancestry but exposed incoming main PR #1104's malformed landing subject. Merged exact existing repair `5ac706387`: its `[[landings]]` row binds the immutable SHA, tree, ordered parents and title, retaining mandatory own-tree regression resolution. Parent verified the Forgejo merged PR identity. No Rust changed from `b7b594797`; prior focused Rust results apply to identical source. Verified-landing regression plus docs index passed (5 tests), pinned all-target check passed, and the unauthenticated local history diagnostic returned zero errors with all three PR #1104 own-tree trailers resolved. The manually dispatched gate remains authoritative for live binding. |
| 2026-10-15 | H1 fixture ownership | Gate #5166 passed history; its only validation failures were the new fixture inventory counts. Reviewed four namespaced spawn sites: six retained/joined identity-creation threads, deadline-observed FIFO reads (joined on success; a regression timeout leaves only a test-process-lifetime worker), and two finite TLS fixture tasks (awaited on success, runtime-owned on panic). One Barrier::wait site is in-process synchronization, not a child-process wait. Counts change 951 to 955 and 326 to 327 without relaxing scope or patterns; no production process or owner is added. Parent reviewed the exact diff; complete inventory plus docs index passed (11 tests), changed FIFO regression passed, and pinned all-target check, catalog and infrastructure checks passed. |
| 2026-10-15 | H1 second base refresh | Gate #5185 correctly refused live binding after main advanced to `443c44c29` (Live TV Stop feedback). Merged current main into effort normally as `2ba52232`, then into H1. Incoming changes contain no Rust/dependency or ownership-count differences. The separately verified H2 draft was backed up and removed before this refresh; it is excluded from H1. Exact refreshed source passed 22 configuration and 8 identity tests, pinned all-target compilation, 11 inventory/docs tests, catalog and infrastructure checks. Normal pinned hook is required on the merge commit before push. |
| 2026-10-15 | H1 bounded CI repair | Effort runs #5174, #5191 and #5197 reached the Rust job's ten-minute wall-clock limit without compiler diagnostics; #5197 spent 87 seconds in setup/cache, leaving about 8m33s for the compile pair. Parent's read-only runner diagnosis found no OOM kill, memory-limit event or disk pressure. Increase only the effort Rust job budget to 20 minutes, retaining all commands, pinned toolchain, runner labels, cache bounds, aggregate gate and service CPU/memory limits. The existing exact-step/command contract now requires the bounded budget: it fails against 10 and passes against 20. Ten focused compile-only, explicit-timeout, preflight, runner-trust, cache and docs-index checks passed, along with catalog/infrastructure/whitespace checks. Rust source remains identical to reviewed `8d4f815c94`; its 22 config and 8 identity test evidence remains applicable. No red gate permits merge; normal hook and a fresh exact-head manual effort gate are still required. |
| 2026-10-15 | Workflow amendment | Independent workflow review approved one cohesive PR #1118 with H1–H4 checkpoints before any H2–H4 task branch/commit exists. Parent canceled only superseded run #5203; completed checks are history, not green-gate evidence. Separate checkpoint task PRs are superseded; fresh combined-source verification, final-head Effort gate, one fresh adversarial built-code review and full exact-tree promotion qualification remain mandatory. Workflow approval does not approve built code. |
| 2026-10-15 | Developer browser follow-up | Initial diagnostic sweep stopped after 29 captures once HTTPS heading order, checkbox and SAN presentation defects were confirmed. Reused settings controls/theme, kept the independent error-shell loader, and bounded long identity text to the card. Parent reviewed desktop/mobile captures and exact six-record golden attribution: eight new HTTPS controls/read plus the already-shipped Diagnostic ZIP card from `9ca45b52a` (text later updated by `069615e85` and `8c80b9a81`); all 59 prior focus descriptors remain in order and no DOM/request/registry residual remains. Only the six Developer golden records may change; diagnostic implementation is untouched. All six focused Developer captures passed with 2,171 facts, identical before/after the wrapping-only change. Expanded long-path/public-fingerprint mobile geometry passed: viewport/body 390px, card/scroll 362px (before wrapping, body 617px). Settings 71/71, full request-budget, static web, pinned all-target compile, docs index, catalog and infrastructure checks passed. The Python Playwright cleanup warning is external harness output, not a captured app console/page error; no focused capture failed. Full final-source UI sweep/Live TV companion remain required before merge. |
| 2026-10-15 | Inherited browser-fixture repairs | The first full sweep captured all 78 pages without app console/page errors but failed 130 structural facts: 104 inherited static Analysis/auth/player facts (`b8bcc7443`, `9ca45b52a`), 14 startup-process facts and 12 Jellyfin readiness facts from concurrent fixtures competing for port 8096. Parent approved only the 104 static facts across 18 records after exact DOM/focus/request attribution. A serial diagnostic stopped after 60 captures, explicitly incomplete, identified the registered Live TV caption-probe child and confirmed Jellyfin reachability met; neither transient was put in the golden. Added a bounded 120-second current-final-boot completion/registered-child wait, without normalizing application facts or requests. The companion exposed inherited master-playlist (`b20982f60`), persistent feedback (`ce0df7640`) and protocol-3 request-hint recovery (`de07dd4cf`) fixture mismatches; only fixtures/assertions changed. Kept strict authentication, same-capability replay/resume, ten allocations/nine confirmed releases, bounded DELETEs and a deliberately unconfirmed capability. Exact detached-media checks use paused/no src or srcObject/NETWORK_EMPTY/HAVE_NOTHING/zero buffers/no HLS or lease; an isolated browser probe proved currentSrc can retain a revoked historical blob URL. The original failures and diagnostic artifacts remain retained. Focused fixture/poll/ownership/docs checks passed 15 tests; existing Live TV lease tests passed 89 cases. Full repaired companion and final solo structural sweep remain merge prerequisites; run #5213 was canceled, not green evidence. |
| 2026-10-15 | Recovery fixture liveness correction | The full repaired companion reached its last recovery assertion, then correctly issued no resume for a freshly touched hint. Production `pagehide` refreshes retained hints after an unconfirmed DELETE; a prior 30-second pause does not age the replacement document's hint. The fixture now leaves through about:blank into Settings, verifies a fresh document with no lease/resume state, observes the stored timestamp crossing the contract-derived orphan-liveness window with a five-second bounded margin, then opens Live TV by SPA navigation without another pagehide. It preserves exact same-capability/no-new-POST/frame-advancement, privacy and ten/nine lifecycle assertions. Existing fresh-hint and resume unit cases protect the protocol; production remains unchanged. Remote run #5224 is superseded; only a new exact-head green gate and both full browser commands permit merge. |
| 2026-10-15 | Effort compiler concurrency bound | Run #5226 reached the 20-minute Rust deadline during all-target checking; no compiler diagnostic preceded cancellation, and Clippy was not reached. The effort job omitted the explicit `CARGO_BUILD_JOBS=1` already present in the main fast lane; neither its Make target nor cache action supplies that bound. Add the same job-local bound while retaining the 20-minute ceiling, exact commands, toolchain, runner labels and cache/service limits. The strict job-environment regression fails before the fix and passes afterward. This closes a verified configuration gap; current runner memory pressure remains unmeasured because the approved SSH route timed out, so memory thrashing is not claimed as the cause. Fresh exact-head gate evidence remains mandatory. |
| 2026-10-15 | Final implementation review correction | The single fresh adversarial review of promotion PR #1134 at `e95621a5f` found one P2: an old PUT response could replace a newer Reload/Save while page generation and outer mount still matched. Sol fences both PUT completion branches with the captured operation epoch, preserving existing GET/page/mount checks. The deferred shipped-function regression fails before the fix and covers late success/rejection after another-node Reload and after Reload plus a newer Save, with no cache/card/epoch/feedback/toast/button effects. The superseded combined-tree browser diagnostic stopped cleanly with zero captures before source edits; it is incomplete, not passing final evidence. Parent verifies the correction directly without a second adversarial review; final-source browser checks and exact-tree promotion qualification remain mandatory. |
| 2026-10-15 | Qualification dispatch form compatibility | Before dispatching final qualification, the authenticated Forgejo form displayed `Invalid input type ""` for each of six existing inputs and rendered no controls, including the required immutable promotion binding. Add explicit `type: string` to those six declarations without changing names, descriptions, optional/default values or dispatch semantics. The focused contract fails before the fix and verifies every complete input declaration afterward. The branch-selected form still read main's untyped schema. The existing authenticated `fj` CLI submitted exactly bound run #5244 through supported dispatch; its localized success-print panic occurred after API success, verified in the UI without retry. No unbound dispatch, injected fields or credential workaround was used. Application/browser inputs were unchanged. |
| 2026-10-15 | Full qualification topology prerequisites | Exact-bound full run #5244 reached the real membership campaign after growth passed, but its old URL probes attempted to install a second removal owner. Reuse the installed manager through a validation-only entry into the unchanged real claim transaction: duplicate claims leave the roster unchanged, and a tombstoned origin is actually reassigned then restored. The new real one-voter regression reproduces the old failure before the fix. The complete local lifecycle then exposed `node-N` identities rejected by the strict UUID reduction wire contract and missing target post-fence heartbeats. Migrate actual fixture identities/references to deterministic canonical UUIDs, preserving readable paths/hostnames and unrelated store labels; keep every production proof/clock deadline unchanged. Start the real target heartbeat only after explicit write-budget controls, reject duplicate workers, and abort/join the retained task on EOF or request read/write failure. Local remote-removal and both healthy/degraded self-leave scenarios pass, as do canonical identity, duplicate-worker and broken-reply cleanup regressions. The default complete controller still runs every scenario; `leader-leave` provides only a focused selector for the existing two self-leave cases. |
| 2026-10-15 | Full qualification transport count | Run #5244 executed all 70 Linux transport contracts successfully, then the exact-count guard rejected its stale expectation of 67. The three retained-identity watermark/purge regressions came from `11e27c04c`; require exactly 70 without removing the guard. Pinned local inventory resolves all seven exact cluster selectors and all five vendor selectors once under their original feature sets. All 68 portable transport tests pass locally; two existing Linux-only real-child diagnostic cleanup cases explain the Linux total, not macOS proof of those cases. The strict command/surface contract fails before the count correction and passes afterward. |
| 2026-10-15 | Full qualification compiler bound | Run #5244's compiler job reached its ten-minute ceiling during all-target checking without a Rust diagnostic. Its job also omitted the Cargo concurrency bound already proven by effort run #5233 (Rust 9m32s). Match only that compiler job to the proven 20-minute/one-job allowance; preserve commands, pinned toolchain, labels and cache/service limits. The separate measured runtime allowances below do not change test deadlines or execution. The strict workflow regression fails before the fix and passes afterward. The configuration gap and timeout are observed; no unmeasured memory-pressure cause is inferred. |
| 2026-10-15 | Full qualification Android focus prerequisite | Run #5244's allowed emulator retry executed all 78 tests and exposed one real focus failure, not a pre-test infrastructure failure. Reporting from `593f745506` intercepted Settings → Info. Reuse the exact source/test correction from existing draft PR #1129 commit `5a918f7192`: reporting follows the contiguous standard controls, and the original test additionally requires Right to reach reporting and Left to return to PiP. Advance the required Android release-input counter to 172; fresh main still declares 171. Both debug and instrumentation APKs compile locally in the pinned image, and metadata confirms 172. The first local Gradle daemon disappeared; the successful source-identical retry bounds only local workers/in-process Kotlin and disables file watching, without changing workflow/toolchain/resource configuration. Fresh emulator qualification and physical-device acceptance are not inferred from compilation. |
| 2026-10-15 | Full qualification UI listener isolation | Run #5244 completed all 78 captures with no application console/page errors, then found only 12 Developer badge facts. A controlled real-daemon experiment identifies exactly `jellyfin_compatibility.standard_port`: occupied shared port 8096 makes it unmet, while an explicitly allocated private listener makes it met; every other requirement status is unchanged. The disposable selfhost fixture now allocates four distinct ports and writes the Jellyfin port alongside HTTP/Raft/API across its boots. The occupied-port regression and actual startup/current-boot caption completion/authenticated readiness/Chromium row probe pass. No product readiness behavior, request inventory, normalizer or golden fact changes. Original run failures remain history; only a fresh final candidate qualification can pass promotion. |
| 2026-10-15 | Full qualification source censuses and historical fixtures | Full run #5244 exposed seven source-census assertions and three genuine Store-fixture failures before its time limits; earlier tail-only reports missed those failures. Existing annotated media/session reads and atomic root-removal SQL were absent from maintained inventories, and the placeholder scanner did not compose the known root-removal guard/optional lease fragments. Update only those censuses/scanner with None/Some runtime-SQL regression coverage; preserve the unannotated ceiling, unknown-template refusal and unrelated classifications. All 24 focused census tests and denied-warning Clippy pass. Three throwaway historical Store fixtures built current schema 82 then stamped 61/62/65 while retaining later Sharing objects; remove only that later-schema fixture state before exercising the real upgrade. All three original regressions pass, and the strict production schema-lineage negative remains passing; no migration guard or Store assertion is weakened. |
| 2026-10-15 | Bounded historical Store audit | Because #5244 stopped before the remaining historical fixtures, inspect the same rewind family rather than waiting for another full run. Use the existing ordinary-open baseline (81), distinct from activated Source schema 82, in 18 marker expectations across 14 cases. Remove later Sharing state before the additional offline-audio/read-index and second negative-path rewinds; remove only two named later Jellyfin watch triggers while rebuilding predecessor media_sessions, and verify their real restoration. The v32 fixture must reconstruct actual v31 table/ALTER history from the frozen v10 schema, with an empty-session assertion and exact index/trigger restoration; dropping a current recovery column produced an invalid hybrid layout. Preserve all runtime migration/lineage guards and original forward-migration assertions. All 15 bounded historical cases pass (12 family cases plus the corrected v32, offline-audio and analysis cases), as do two strict lineage/layout negatives; the three original #5244 regressions also pass. Exact post-integration reruns remain required. |
| 2026-10-15 | Full qualification runtime allowances | Run #5244 exhausted Rust unit/SQLite 30 minutes (9m37 compile plus about 20 minutes of core execution), legacy Store 40 minutes (2m39 compile, 181 passes and three known fixture failures among 367 before timeout), and WAL 30 minutes (all observed exact WAL tests passed; killed while compiling the remaining daemon regression). The core suite grew from 1,142 to 1,892 cases; its three unfinished finite workloads cover 4,100 migration rows, 8,193 history rows and 5,000 ranking comparisons. Use bounded whole-job allowances of 90 minutes for unit/SQLite, 120 for the serial Store+schema lane and 60 for WAL. These are provisional until a fresh complete run proves them. Preserve every command, feature, execution mode, serial Store thread setting, assertion, compiler concurrency and production/per-test deadline; do not skip, shard or retry tests. Strict workflow/command regression fails on all three old ceilings and passes on the new declared bounds. |
| 2026-10-15 | Full qualification Apple fixture prerequisites | Run #5244's iPhone suite executed 1,089 tests and found six failures in five named tests before diagnostic collection ended. The Auto tick gained a parameter (`15e5a0d95`), so its source-contract slice now matches the function prefix while retaining semantic assertions. Register the already-shipped diagnostics Developer card (`4e523845b`) and require its actual explicit Awaiting/graduation statement. Processing-generation callbacks (`e3970c07d`) now precede readiness callbacks: fixtures observe the actual readiness-bearing response plus a following request before holding/replaying its complete queued batch, preserving current/session/end/new-intent delivery assertions. The terminal race now holds the actual terminal first response after its capture guard, then proves End revokes publication. No reporter/player behavior or timeout changes. The required generated Apple counter advances 251 to 252; all 168 selected iOS tests pass, including the complete shared session class; platform-wide qualification and physical acceptance are not inferred from that focused result. |
| 2026-10-15 | Full qualification package source input | Run #5244's amd64 package rejected Fontconfig input whose old URL returned a Git landing page; arm64 qualification passed separately. Reproduce the previously trusted archive hash from the pinned upstream commit, then use the immutable official GitHub mirror archive with all 618 source objects, executable bits and symlinks equivalent after root normalization. Retain the original trusted equivalence and source version; change only the archive URL/hash/root and identify public input/hashes on checksum rejection. A cold audit verifies all 169 unique pinned downloads (226,960,530 bytes); the four focused packaging suites pass 37 tests with one existing Linux-only linker contract skipped on macOS. No checksum check or source-version requirement is relaxed. |
| 2026-10-15 | Current-main subtitle integration | After the reviewed qualification repair commit, normally merge main `220157aba` with both subtitle efforts. Preserve managed subtitle setup/generation, its independent schema and browser checks alongside HTTPS. Resolve only four conflict files: combined API inventory has 369 routes; exact additive ownership counts are 973 namespaced spawns, 1,784 timers and 767 launch-shaped methods; validation paths cover both features. Retain main's independently measured 30-minute effort compiler bound with one Cargo job; the separate full compiler repair remains 20 minutes/one job. Combined all-target check passes on pinned Rust 1.97.1 (2m45), settings pass 74 cases, both new real subtitle browser cases pass, and 138 operations/docs/route/ownership/package checks pass (one existing Linux-only package skip on macOS). The current core census (24 cases) and complete configuration filter (22 cases) pass; the two managed-schema reads change only their maintained census pin from 46 to 48, retaining the 237 unannotated ceiling. Historical fixture rewind now removes the complete disposable managed-subtitle extension only after proving its model receipts empty, preserving exact trigger restoration and the production partial-schema refusal. Prior browser snapshots are historical; final current-source structural/Live TV and affected Store results are required before qualification. |
| 2026-10-15 | Repeated managed-subtitle bootstrap | The current historical analysis regression exposed a real second-bootstrap failure before any fixture rewind: the legacy probe installer replaced the complete managed resource view, so strict extension validation refused the resulting hybrid. Preflight before base DDL refuses partial shapes, including a missing view or marker row; permit only the empty fresh install and measured exact resource/probe predecessors. Preserve a canonical complete managed view and marker. For a predecessor, execute exact snapshot guard, probe replacement and complete managed install in one transaction; a concurrent winner aborts the stale proposal before DROP, and only strict current-winner read-back can settle it. Keep receipt/queue data, active Source marker and ordinary version recognition intact. Dedicated pure and real replicated regressions cover fresh/legacy/current/partial, rollback, stale-winner and supported concurrent retries. All four managed-schema pure tests, 24 census tests and 22 configuration cases pass. All 21 focused replicated Store cases pass, including the three populated new bootstrap regressions, the unmasked original analysis retry, 13 historical migrations, offline audio and the original three qualification failures; three strict schema negatives also pass. The current Store inventory is 374 cases (seven more than #5244), not a claim that all 374 ran locally. The final bootstrap-fixed HTTPS filter passes 29 tests, the nine existing startup/drain regressions pass individually, and locked workspace all-target checking passes in 34.31 seconds. The two new reads have adjacent authority reasons and an exact census pin of four; the 237 unannotated ceiling and SQLite transaction inventory scope are unchanged. |
| 2026-10-15 | Reviewed current-main UI baseline | Final bootstrap-fixed local sweep captures all 78 pages in 437 seconds with no application console/page errors, then exits 1 on 194 static golden differences from incoming subtitle work. Parent reviews all 18 records: six item mounts (`e71bc3ad3`), six player generated-caption mounts/ledger rows (`894d5b6dc`) and six Developer records after the subtitle card moved to Maintenance (`e2453a675`). Exact attribution proves 6 item DOM, 30 player DOM, 78 Developer DOM and 80 shifted focus-position facts; only Configure transcription leaves focus traversal. All other controls/order, requests, metadata, readiness and other 60 records remain identical. Replace only those reviewed record blocks; strict comparison against the preserved complete capture matches all 10,771 facts with zero differences. Retain the original exit-1 evidence, not a claim that it passed. Fresh exact-head full CI still runs the unweakened complete browser check; the final Live TV companion remains separately required. |
| 2026-10-15 | Committed client-history coverage | Fresh full run #5264 and ready run #5263 both refuse preflight history: corrective commit `30a94b904` changed mobile clients without its required `tests/client-fixes.toml` mapping. Add one primary Android focus row and five Apple supplemental rows for the shipped Developer registry, parameterized tick source contract and actual terminal/readiness callback fixtures. Each names the existing meaningful regression already included in the 111-anchor promotion body; fixture corrections are not represented as new reporter behavior. No audit boundary, scanner, assertion or application input changes. The prior 51 history unit tests are not a committed-history audit; run actual `make history-check` again after committing this metadata. Both failed runs remain failures and require fresh qualification. |
| 2026-10-15 | Final bootstrap main refresh | Main advanced to `b2d83ae8d` through PR #1140 while the client-metadata correction was held locally. Its bootstrap retry repair overlaps the already reviewed guarded implementation. Retain our production implementation byte-for-byte, including preflight partial-layout refusal and the atomic stale-observer guard; retain main's four managed Store-method inventory entries (490 total) and strengthened fresh/retried-bootstrap marker/view assertions. Only test inventory/assertions and integration evidence change. Both incoming regression declarations join the complete promotion body (113 unique anchors). Pinned exact-merged verification passes both incoming regressions and all three populated bootstrap cases, all 50 schema/census/configuration cases, locked workspace all-target check (28.77 seconds), feature-specific denied-warning Clippy (22.78 seconds) and four docs checks. Recheck actual committed history after the normal merge hook; unchanged application/browser inputs retain their hash-bound local evidence. |
| 2026-10-16 | Topology fixture storage and compatibility | Fresh full run #5274 (internal run 5300) passes membership and both leader-leave drills, then the learner has current zero-lag readiness but no voter storage proof. The harness opens `node-N/state_machine/db/auth.db`, while membership correctly measures `node-N/hiqlite/state_machine/db/plurx.db`; the missing image makes required storage unknown even when the separate durability probe reports free space. Align the actual Hiqlite configuration, passive observer, direct FTS access and snapshot readers with the production layout, preserving node labels, telemetry roots and all production storage/quorum rules and deadlines. A real one-voter regression fails against the old layout and passes with actual live-image requirement, published storage readiness, real compaction/current snapshot and explicit shutdown/reaping. Its first outer-root diagnostic was corrected before claiming the actual-attempt-root red/green proof. The unchanged full learner lifecycle passes; the affected request protocol also exposes a stale negative fixture selecting supported baseline 81 as incompatible. Select one version below the ordinary baseline (80), retaining exit 42, named refusal and unchanged membership. That complete protocol, canonical configuration, all 68 portable transport contracts and all seven ownership inventory checks pass locally. The initial Linux transport count remains 70 before the four separately recorded snapshot-publication regressions. The complete controller passes locally; its final topology command exposes the separately recorded stale query-count contract. Fresh exact-head qualification remains required; no passing full target is inferred from that controller prefix. |
| 2026-10-16 | WAL retained-status clock fixture | The same fresh full qualification retains a peer terminal snapshot observation correctly, but its test compares two live-clock projections and fails when sample age advances from 0 to 1 millisecond and the remaining deadline from 29,999 to 29,998. A temporary two-millisecond delay makes the old test deterministically fail; remove that diagnostic and pause only this test's Tokio clock, explicitly assert the instant stays fixed, and retain equality of the complete observation. All 14 snapshot-executor tests pass on pinned Rust 1.97.1. No production clock, status field, retention policy, deadline or assertion is relaxed. |
| 2026-10-16 | Asynchronous recovery snapshot publication | Both full voter and learner campaigns stop after the bounded low-threshold writes finish, before the asynchronous snapshot copy publishes. Await a strictly newer snapshot after the same maximum three-threshold trigger load, using the original absolute recovery deadline for every observation and poll; add no writes or renewed budgets. Four actual-used-helper regressions fail against immediate completion and pass with delayed publication, already-expired refusal, bounded in-flight observation cancellation and late-ready-value refusal. Together with the existing request-deadline case, five focused cases pass. The exact transport inventory deliberately rises from Linux 70/portable 68 to 74/72; production snapshot, purge, role-cycle and timing contracts remain. All seven ownership inventory checks pass unchanged: their declared daemon/process/scan roots exclude cluster-check; these inline observation/sleep futures remain owned by the recovery caller, with no detached task. Real Linux smoke and fresh full role campaigns remain required. |
| 2026-10-16 | Versioned topology catalogue accounting | The complete local controller passes growth, membership, both leader leaves, learner, singleton, partition, failure and proxy contracts, then its final topology workload reports 512 local queries for 256 successful operations with zero errors/consistent queries. Commit `5ba02212f1` intentionally reads the saved bounded-read preference before each catalogue lookup; the existing Store contract already requires that pair. Preserve that authority/preference behavior and require exactly two local queries per operation. Raw schema and workload advance to v3 rather than reinterpret v2 artifacts; unchanged outer campaign v2 strictly validates its retained raw v3 bytes. The count regression fails on the old validator, then the eight-case topology suite passes with exact 512, adjacent-count and authority-read negatives, legacy-v2 refusal in Rust/JSON and correctly hashed retained-v2 refusal in the campaign. All operation, concurrency, performance and resource budgets remain unchanged. The complete final-source `make cluster-harness-check` exits 0, including all schema/named cases, the entire real controller and fresh three/four-voter raw v3 artifacts, each with 512 local queries, zero authority queries/errors and 256 samples. |
| 2026-10-16 | Full compiler observation budget | Full run #5274 reaches the 20-minute compiler ceiling without a Rust diagnostic: the all-target check runs 19m43s until cancellation, with no identifying progress beyond package-start messages. The same-source ready run passes its check in 10m54s, then vendor Clippy in 1m27s and workspace Clippy in 6m26s. These differing whole-job contexts do not prove a resource cause. Raise only the full compiler allowance to the existing effort bound of 30 minutes, retaining its exact check command, one Cargo job, toolchain, runner/cache/service settings and every other job deadline. The strict workflow regression fails at 20 and passes at 30. Adequacy remains provisional until fresh full completion. |
| 2026-10-16 | Disposable native Chromium ownership | Full #5274 passes all 78 structural captures and Live TV/control browser checks, then Cinema's headed disposable Chromium refuses Linux uid 0 without its required sandbox option. Limit that launch flag to Linux root disposable profiles; preserve headed/default arguments for other users/platforms. Bounded diagnostics retain spawn/exit/stderr context. Native CDP Browser.close, bounded graceful/TERM/KILL settlement and profile removal retain actual child ownership through reaping, including failure paths. Seven local launcher tests pass; the actual Linux/Xvfb suite passes 14 tests with one existing Router skip, plus repeated ownership/cleanup checks. No application, UI golden or production browser policy changes. |
| 2026-10-16 | Final local repair verification and remaining fetch failure | Pinned Rust 1.97.1 locked workspace all-target check passes in 56.02 seconds and denied-warning Clippy in 1m10s; formatting, catalog/infrastructure/whitespace checks pass. All 85 operations contracts, four docs checks, seven ownership checks, seven launcher cases, eight topology cases and 72 portable transport cases pass; all 130 declaration anchors resolve. The complete cluster harness exits 0. The server/core/mobile application trees and existing structural/Live TV harness inputs remain unchanged; changed cluster CLI, test-only vendor clock and Cinema harness are explicitly identified, with their fresh evidence rather than a claim all harness inputs are unchanged. Full #5274's AMD64 package later fails with temporary DNS resolution (`EAI_AGAIN`) during public source fetching, not a checksum or compiler failure; its log lacks the exact failed URL. A targeted first-input fetch is available with the locked hash, but does not identify the failed input or prove runner DNS health. No package/DNS/service mutation is justified; fresh complete qualification must exercise the normal fetch and every remaining lane. |
| 2026-10-16 | Queue receipt authority and historical fixture follow-up | The completed full Store lane has 349 passes, 22 failures and three ignored cases. One real defect lets ENQUEUE_SQL return an existing receipt before checking current analysis/source ownership; move those existing fences ahead of the receipt shortcut without changing the required authority. The new backend-neutral four-scenario no-mutation regression deterministically fails against the old SQL and passes after the fix. Fixtures establish genuine valid queue owners; current/v14 imports retain exact payload assertions and explicitly inventory 15 known empty Sharing tables, and historical SQLite tests rewind complete extensions before older-marker probes. All 13 focused cases pass, including seven original failures, the receipt matrix and five adjacent cancellation/reclaim/source/idempotence cases. Parent directly reviewed the four-file patch; additional Sharing and watch-count fixture verification remains separate. The shared production SQL changes, so the earlier application-source invariance proof stops at a6ac; final compilation, source evidence and full qualification must describe the new candidate. No full Store suite is claimed locally. |
| 2026-10-16 | Watch read accounting fixture | Three late Store failures expect one total local query, but the saved bounded-read preference plus watch data query deliberately cost two, matching the previously attributed catalogue contract. Each exact case is red at `(0, 1)` and green at strict `(0, 2)`; authority remains `(1, 0)` and empty/fence assertions remain. A task-scoped counter around all 18 fenced calls additionally requires exactly one actual watch query, so the extra preference lookup cannot hide a duplicated watch read. All three original cases and the existing backend-neutral summary/rails equivalence case pass. Only test accounting changes, with no production read-path, authority, deadline or fallback change. |
| 2026-10-16 | Installed Sharing fixtures and combined Store verification | The remaining principal/runtime/receiver/source tests now use genuine supported Source activation and measured readiness; actual three-voter authority, admission/replay/claim/cap/privacy/cleanup assertions remain. Missing custody is explicitly refused before supported sealed-empty custody reaches the existing release checks. Parent reviewed all four test-module diffs. All 13 final cases pass; only a harmless redundant reference needed lint cleanup. Together with the 13 receipt/import/migration cases, four watch/equivalence cases and explicit allocator case, every one of the 22 original failures has focused evidence. The compiled Store inventory is 376, including two new regressions; no complete local-suite claim is made. Current combined-source feature-specific denied-warning Clippy passes in 12.69 seconds, workspace all-target check in 1m11s and formatting passes. The current strict census filters execute 25 cases (4 consistent-read, 19 placeholder, 2 transaction), superseding earlier 24-case count reports without changing ceilings or scanners. Normal hook, committed-history audit and fresh full qualification remain required after this follow-up. |

Post-review persistence clarification: rename publishes the revision. A later
directory-sync error is a committed save with `durability_confirmed: false`;
H3 must adopt desired/revision and show the warning without blind retries.
The injected post-publication sync regression covers this distinction.

Windows scope clarification: supplied material uses regular local disk PEM
files through no-reparse opens; device/pipe/UNC namespaces and cert-manager
symlinks are rejected. Unix cert-manager symlinks remain supported. No
Windows compile or physical safety acceptance is claimed.

H1 checkpoint evidence on `codex/https-h1-config-identity` comprised 22
configuration and 8 identity/trust tests, pinned all-target compilation and
denied-warning Clippy. Parent reviewed that source and resolved its findings.
At that checkpoint the identity module was staged under `cfg(test)`. H2
removes that staging for its production listener, and H3 consumes the loaded
public-chain export without a dead-code allowance. These checkpoints are now
restored into the one combined task; current-source verification and the fresh
final-head gate are recorded separately below. Promotion and physical
acceptance remain pending. Nothing has been deployed or enabled live.

H1 focused commands, run with Rust 1.97.1:

```bash
cargo +1.97.1 test --locked -p plurx-core --features hiqlite-store --lib config::
cargo +1.97.1 test --locked -p plurxd --bin plurxd https::tests::
cargo +1.97.1 check --locked -p plurxd --all-targets
cargo +1.97.1 clippy --locked -p plurxd --all-targets -- -D warnings
cargo +1.97.1 fmt --all -- --check
python3 -m unittest tests.operations.test_docs_index
make validation-lint
scripts/scrub-infra-names --check
```

## 6. H2 URL audit and evidence boundary

The additional listener serves the same router and keeps discovery on HTTP.
This audit changes no peer transport or remote certificate verification.

| Surface | Existing call site | HTTPS behavior |
|---|---|---|
| API | `web/core/app.js` sets `API = "/api/v1"`; `core/api.js` fetches `API+path`. | Requests inherit the page origin. |
| Direct/range/remux | `http/stream.rs::delivery_plan` builds `/api/v1/files/{id}/direct`, `stream.mp4` and `hls/sessions`; `web/core/file-context.js::playbackFileUrl` uses the validated file base. | Relative resources stay on HTTPS; the same stream/range handlers run. |
| HLS | `http/hls/create.rs` builds relative master/schedule/family links; `media_sessions.rs` checks the exact relative session playlist. | No browser redirect to a worker origin. |
| Subtitles | `http/hls/playlist_text.rs` emits relative `subs/{index}/index.m3u8` and VTT segment names; file-context builds validated file aliases. | Inherit the HTTPS playlist/file origin. |
| Artwork | `http/dto.rs::image_url` emits `/api/v1/images/...`; `images.rs` fetches peers internally with redirects disabled. | Browser remains on the original listener. |
| Server QR | `web/core/theme-menu.js` and `core/auth.js` pass `location.origin` to `/connect.svg`; `http/web.rs::connection_qr_svg` explicitly accepts HTTP and HTTPS. | QR retains the selected scheme/port and contains no credential. |
| Remote control/pairing | `web/core/remote-client.js` uses `location.origin` with `redirect: "error"`; `http/remote.rs` accepts HTTPS Origin and proxies owner RPC internally. Pairing links use `cinema-remote:` identifiers. | Pairing cannot select an HTTP API host or downgrade the browser. |
| Cluster relay/redirects | `http/hls/relay.rs`, `media_sessions.rs` and remote control return proxied responses; `peer_transport.rs` refuses redirects. | Internal membership addresses do not become browser navigation targets. No cross-node HTTPS identity is invented. |
| WebSocket | No product endpoint/constructor exists in the audited Rust router/web scripts. `serve_http` retains Hyper upgrade support and HTTP/2 CONNECT. | Generic capability preserved; no product WebSocket acceptance claim. |

Real socket regressions cover normal TLS trust, HTTP/1 and HTTP/2 ALPN,
ConnectInfo, unchanged full-router bootstrap/auth/QR behavior on both startup
owners, malformed/silent peers, cap/deadline release and two-phase shutdown.
The active-stream fixtures establish driver drain behavior; they are not
physical playback acceptance. Browser/native trust installation, real media
playback/range/subtitle/artwork use, cluster topology and device-specific QR
acceptance remain unqualified until their own recorded checks. Existing
HTTP/GDM/Bonjour/cluster/sharing transports remain the documented interfaces.

## 7. H3 local administration checkpoint

H3 was preserved as a separately recoverable draft above H2. Under the
approved §2 amendment it is restored into the same end-to-end PR #1118,
without an intermediate task branch or landing. Parent reviewed the
production Control, exact route eligibility and strict ownership changes;
review fixes fence both fulfilled and rejected old GETs against newer saves.
The shared capacity decisions preserve maintenance refusal before the async
role read, plus the existing fenced/error bodies and global serving fences.

The admin API uses decimal-string revisions and a node-local integer envelope.
An owned blocking worker retains its mutex through publication and adoption
if its HTTP waiter disappears. Public export comes only from active loaded
DER. Exact local HTTPS routes remain manageable on learners and in maintenance;
the Developer card remains independently loadable while replicated settings
show their original error/Retry. This is a narrow management exception, with
no broader system-route or authentication exemption.

Local draft evidence on pinned Rust 1.97.1: all 13 `local_https` tests passed,
including exact large revisions/overflow, concurrency, pre-publication failure,
post-publication uncertainty, canceled waiter adoption, environment masking,
real admin/non-admin admission, bounded sanitized JSON, loaded-chain fingerprint
and disable/re-enable after supplied-file loss or managed-file corruption.
All-target daemon check, denied-warning Clippy and formatting passed. All 71
settings-section tests and the complete request-budget file passed, including
late GET success/error, navigation, explicit conflict reload, authenticated
public-chain download and isolated local retry. JavaScript syntax (93 scripts),
web-types (unchanged baseline), catalog and infrastructure checks passed.
Combined current-source verification, the fresh final-head task gate and
physical trust/playback qualification remain pending. No local result waives
the task gate or full final promotion.

## 8. H4 maintained-reference checkpoint and acceptance still owed

The maintained transport/security/API/features/architecture/operations and
Developer lifecycle references now describe additive HTTPS, restart-only
node-local administration and explicit trust. Deployment guidance names
server/container paths, private material ownership, optional port publication
and read-only PEM directory mounts. Only commented examples are added to the
Compose override and systemd unit; base publications and discovery stay as
configured. No deployment, service restart or live setting is performed.

H1 manual effort retries encountered infrastructure limits: run #5173 timed
out fetching its checksummed K08 upstream fixture after 90 seconds, and runs
#5174, #5191 and #5197 reached the Rust job's ten-minute deadline without compiler
diagnostics while other jobs passed. Fresh retries retain the same reviewed
source; none of these red runs authorizes a merge. Authenticated history
binding, the fresh unified-task gate and final exact-tree promotion remain mandatory.

| Acceptance | Current evidence | Still owed |
|---|---|---|
| Configuration, persistent identity and trust verifier | 22 core config and 8 identity tests, normal trust-store acceptance plus unknown-root/hostname rejection | Full exact-candidate promotion qualification and receipt; task integration is recorded by PR #1118 |
| Listener admission and lifecycle | 16 HTTPS tests, nine existing startup/drain regressions; URL audit in §6 | Full exact-candidate promotion qualification and receipt; task integration is recorded by PR #1118 |
| Node-local API and Developer controls | 13 API/Control regressions, 71 settings tests, complete request budget; 21 inventory/admission/docs checks | Full exact-candidate promotion qualification and receipt; task integration is recorded by PR #1118 |
| Browser/native trust and actual playback | Explicit public-chain/fingerprint UI and synthetic socket/HTTP evidence | Real trust installation, direct/range/HLS/artwork/subtitle, cluster-origin, QR/remote and physical-device matrix; no physical acceptance claim |
| Windows | Existing compile waiver; bounded local-file/no-reparse contract implemented | Windows compilation/runtime ACL/client-trust qualification; Unix FIFO results are not Windows proof |
| Release candidate | Local checks, current-main integration, the single final adversarial review and parent correction reviews recorded above | Full final exact-tree qualification receipt and Main promotion gate before main merge; a moved base/candidate requires fresh qualification |

H4 local reference checks: 105 of 106 docs/API/infrastructure/install/container
operations tests passed with the host's default temporary directory. The sole
installer ownership assertion compared macOS `/var` with its physical
`/private/var` spelling; its exact focused rerun with canonical
`TMPDIR=/private/tmp` passed. No installer or test code changed. Catalog and
infrastructure-name checks and whitespace validation passed. This records the
environment-specific result without calling the original run green.

Missing physical acceptance is distinct from missing implementation. HTTPS
retains its meaningful Developer switch and graduation line; it moves to System
only after the named trust/playback acceptance is recorded. Readiness never
silently changes the saved switch.

## 9. Combined checkpoint verification and final-source boundary

The unified candidate restored all H1–H4 checkpoints after the independently
reviewed §2 amendment. No behavior-contract change was introduced during
consolidation. Parent's checkpoint source/document reviews remain recorded;
they do not replace the one fresh adversarial built-code review at final
promotion. Superseded run #5203 is canceled, not a passing gate.

The unified task checkpoint passed these pinned Rust 1.97.1 commands;
later main integrations and exact promotion bindings are recorded in §5 and
the task/promotion receipts:

```bash
cargo +1.97.1 test --locked -p plurx-core --features hiqlite-store --lib config::
cargo +1.97.1 test --locked -p plurxd --bin plurxd https
cargo +1.97.1 check --locked --workspace --all-targets
cargo +1.97.1 clippy --locked --workspace --all-targets -- -D warnings
cargo +1.97.1 fmt --all --check
```

The configuration suite passed 22 tests, and the combined HTTPS filter passed
29 identity/trust, admission/lifecycle and API/Control tests. Nine affected
existing regressions passed individually using
`cargo +1.97.1 test --locked -p plurxd --bin plurxd <name>`:

- `k06_clock_only_socket_denies_application_and_transfers_existing_keepalive`
- `pending_socket_answers_starting_not_not_found`
- `accept_error_backs_off_then_serves_the_next_connection`
- `the_server_answers_until_it_is_asked_to_stop_and_then_drains`
- `playback_progress_pending_at_shutdown_is_committed_not_lost`
- `jellyfin_standard_port_is_best_effort_and_drains_with_the_server`
- `a_measured_node_boots_serves_and_drains`
- `a_drained_boot_records_a_clean_plex_census_stop`
- `a_connection_that_never_finishes_does_not_hold_the_drain_open`

That is 60 selected Rust tests. Workspace all-target check passed in 2m38s;
workspace denied-warning Clippy passed in 1m46s; formatting passed. The
existing macOS linker unwind-size warning is unrelated to denied Rust lints.

Web evidence: `node tests/web/settings-sections.test.js` passed all 71 tests;
`node tests/web/page-read-budget.test.js` passed its complete file.
`make effort-web-static-check` passed served JavaScript syntax (93 scripts),
generated source inventory, unchanged type baseline, shape checks and 484
contrast pairs with zero failures. No physical browser/device result is inferred.

Using canonical `TMPDIR=/private/tmp`, the combined ownership inventory,
route-admission publication, docs index, API inventory, infrastructure,
installer, Docker hardware/startup, CI budget/timeout/preflight/runner and cache
sweep passed all 125 tests. This includes the earlier host-temp-path installer
case; the earlier default-temp failure remains recorded in §8. Catalog lint
(3,534 governed files), infrastructure-name and whitespace checks passed.
The complete PR body carries 156 unique resolvable regression anchors, including
all prior checkpoint anchors and the selected existing regressions.

The normal commit hook remains mandatory before push. The fresh task-head
Effort development gate passed before task integration; earlier/canceled
attempts never substitute for it. The single fresh adversarial implementation
review and Sol-owned correction are complete, as recorded in §5. Promotion
still requires a frozen candidate with current main integrated, the full
exact-tree qualification receipt and Main promotion gate.
Task and promotion PR receipts record the actual run/tree bindings and merge
outcomes. This work does not authorize deployment, service restart or live
HTTPS setting changes.

The earlier task browser-fixture follow-up preserved application assets at
`d3abdbc809`; those runs are historical evidence for that source snapshot.
Subsequent main integration and qualification repairs require fresh browser
checks against the final application, harness and golden inputs. The selfhost
fixture now reserves its own Jellyfin listener alongside HTTP/Raft/API; run
the full structural sweep and Live TV companion sequentially and record their
source bindings and outcomes in promotion PR receipts. Full exact-tree
qualification remains mandatory. Synthetic fixtures do not establish physical
HTTPS trust/playback acceptance.

## Full-unit resident-capacity prerequisite (2026-10-16)

The reviewed Store follow-up is committed as `0ecc8fc025`; its normal hook
passed. Full run 5274's daemon test compilation measured about 6.5 GiB
resident plus 3.5 GiB swapped under a 7 GiB resident bound, with sustained
memory pressure. Diagnostic package-local codegen-unit experiments at one
and 256 units both failed under 6 GiB before producing a binary; no profile
change is retained. The Docker experiment VM had only 1 GiB actual swap. The original full
unit job subsequently exhausted its existing 90-minute deadline during
compilation, without a Rust diagnostic; no further timeout increase is made.

The approved capacity remedy keeps the pinned image, full command, profile,
90-minute deadline, one Cargo job and two test threads. Only the full unit
job selects dedicated `unit-memory-12g` capacity, with explicit Docker bounds
of 12 GiB RAM, 14 GiB combined memory, three CPUs and 4096 PIDs. The existing
capacity-one registration is isolated from general jobs; no new registration
or unrelated service/resource change is included. Its prior clean stop has
no retained cause, and approval to activate comes from the authorized CI
repair, not from that inactive status. Operator contract:
[RUNNER-DISK.md](../ci/RUNNER-DISK.md#dedicated-full-unit-resident-capacity).

The strict workflow regression first fails against the generic selector and
missing child bound, then passes with the dedicated declaration. Activation
and a source-only exact committed candidate run remain required evidence;
no successful 12 GiB completion is claimed at this preparation checkpoint.
