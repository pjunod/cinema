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
| Configuration, persistent identity and trust verifier | 22 core config and 8 identity tests, normal trust-store acceptance plus unknown-root/hostname rejection | Combined current-source recheck and final-head PR #1118 gate/merge |
| Listener admission and lifecycle | 16 HTTPS tests, nine existing startup/drain regressions; URL audit in §6 | Combined current-source recheck and final-head PR #1118 gate/merge |
| Node-local API and Developer controls | 13 API/Control regressions, 71 settings tests, complete request budget; 21 inventory/admission/docs checks | Combined current-source recheck and final-head PR #1118 gate/merge |
| Browser/native trust and actual playback | Explicit public-chain/fingerprint UI and synthetic socket/HTTP evidence | Real trust installation, direct/range/HLS/artwork/subtitle, cluster-origin, QR/remote and physical-device matrix; no physical acceptance claim |
| Windows | Existing compile waiver; bounded local-file/no-reparse contract implemented | Windows compilation/runtime ACL/client-trust qualification; Unix FIFO results are not Windows proof |
| Release candidate | Local checks and parent reviews recorded above | Freeze candidate, merge fresh main, fresh adversarial code review, full exact-tree qualification receipt and Main promotion gate before main merge |

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

## 9. Combined current-source verification

The unified candidate restored all H1–H4 checkpoints after the independently
reviewed §2 amendment. No behavior-contract change was introduced during
consolidation. Parent's checkpoint source/document reviews remain recorded;
they do not replace the one fresh adversarial built-code review at final
promotion. Superseded run #5203 is canceled, not a passing gate.

Current combined source passed these pinned Rust 1.97.1 commands:

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
The complete PR body carries 66 unique resolvable regression anchors, including
all prior checkpoint anchors and the selected existing regressions.

The normal commit hook remains mandatory before push. A fresh exact-final-head
Effort development gate is required before task integration; earlier/canceled
attempts never substitute for it. Promotion requires a frozen candidate with
current main integrated, one fresh adversarial built-code review with Sol-owned
fixes, the full exact-tree qualification receipt and Main promotion gate.
Task and promotion PR receipts record the actual run/tree bindings and merge
outcomes. This work does not authorize deployment, service restart or live
HTTPS setting changes.

The browser-fixture follow-up changes only `scripts/ui-baseline`,
`scripts/live-tv-browser`, their operations contracts and the reviewed golden
facts. Production Rust and web assets remain identical to `d3abdbc809`; the
full-source promotion still qualifies the final frozen tree. The repaired
full browser commands run sequentially to preserve the fixture's default
Jellyfin listener. Their external results and the fresh final-head Effort gate
are required before merge and are recorded in task/promotion PR receipts.
Synthetic fixtures do not establish physical HTTPS trust/playback acceptance.
