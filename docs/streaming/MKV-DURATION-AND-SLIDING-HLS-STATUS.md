# MKV duration and sliding HLS — implementation status

**Status:** qualified candidate; promotion pending · **Updated:** 2026-09-19 EDT ·
**Branch:** `effort/mkv-duration-sliding-hls` · **Base:** `535f95d2`

Companion to the
[reviewed RCA](MKV-DURATION-AND-APPLE-SLIDING-HLS-RCA-AND-FIX.md) and
[implementation contract](MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md).
This page is the live ledger: it separates code completed, evidence collected,
review disposition, qualification, and production work still requiring an
operator decision.

## Package ledger — code, proof, and remaining work stay separate

| Package | State | Evidence | Next |
|---|---|---|---|
| W0 reproductions | qualified | The 16-test `plurxd` `mkv_hls` group covers the startup, packet-duration, publication, retention, and failure reproductions on the final candidate | Preserve the tests through promotion |
| W0.5 startup admission | qualified | Exact-attempt presentation admission and the non-renewing deadline pass in the focused Rust group | Preserve the actor-owned evidence boundary |
| W1a diagnostic/history | qualified | Typed diagnostic/history regressions and denied-lint compilation pass | Preserve unknown provenance rather than coercing it |
| W1b packet duration | qualified | Bounded packet-head/tail, signed span, cap, timeout, process, and real-FFmpeg cases pass | Physical-media acceptance remains a later operator run |
| W1c terminal repair | qualified | SQLite and Hiqlite repair use exact typed JSON matching; whitespace, nested-code, malformed-decoy, identity, and idempotency cases pass | Production requeue remains unauthorized |
| W2a fixed target | qualified | Copy segmentation, no-clean-point ceiling, every rolling writer, and one-tick-over target regressions pass | Preserve the served 16 s invariant |
| W2b publication clock | qualified | Publication, playback-rate pacing, typed capacity, pause, fetch-stopped, replacement, and EOF cases pass | Preserve actor-owned publication timing |
| W3 object promises | qualified | Rolling-session retention and object-promise regressions pass, including grace ownership and hard-cap accounting | Preserve immutable served snapshots |
| W4 diagnostics/clients | qualified for affected surfaces | Rust serialization/reservation tests, the full web lane, Android build/JVM/lint, Apple compilation, and the three changed Apple tests pass. Developer enablement remains advisory | Broad Apple-suite failures stay with the separate unit-failure batching process |
| W5 review/qualification | PR #377 open; promotion gate pending | The single adversarial review's four findings are fixed. Candidate `063a638d` passes the affected fast lane; `e0b68578` integrates current `main`, claims Apple build 171 and Android versionCode 108, and passes exact-tree compile/static policy | Wait for the current PR head's promotion gate, then merge |

## Working decisions — defaults remain visible

1. **One final pull request.** The implementation uses coherent commits on one
   effort branch and one main-bound PR, because the owner requested batched
   review and a single final test cycle. This replaces the handoff's intermediate
   task-PR sequence for this execution.
2. **Compile continuously; test once.** Rust checks catch type and ownership
   failures without spending the final behavioral-test budget. Focused and
   fast-lane tests run after the one adversarial review and its fixes.
3. **Developer enablement is advisory.** The Developer settings surface may
   show readiness and risks, but unmet advice does not block the operator from
   enabling the behavior. Internal safety invariants—source identity, leases,
   hard storage bounds, and truthful playlists—remain mandatory.
4. **No production mutation.** Deployment and exact-cohort requeue remain
   outside this build until explicitly authorized.

## Verification ledger — only exact commands count

| Date | Tree | Command | Result |
|---|---|---|---|
| 2026-09-19 | `1ae2c4ec3` | `rustup run 1.97.1 rustc --version` | `rustc 1.97.1` |
| 2026-09-19 | `1ae2c4ec3` | `rustup run 1.97.1 cargo check -p plurxd --all-targets --locked` | passed in 1m 24s |
| 2026-09-19 | startup candidate | `rustup run 1.97.1 cargo fmt --all && rustup run 1.97.1 cargo check -p plurxd --all-targets --locked` | passed in 37 s; behavioral tests compiled but did not run |
| 2026-09-19 | W1a candidate | `rustup run 1.97.1 cargo fmt --all && rustup run 1.97.1 cargo check -p plurxd --all-targets --locked` | passed in 11 s; behavioral tests compiled but did not run |
| 2026-09-19 | W1b candidate | `rustup run 1.97.1 cargo fmt --all && rustup run 1.97.1 cargo check -p plurxd --all-targets --locked` | passed in 8 s; behavioral tests compiled but did not run |
| 2026-09-19 | W1c candidate | `rustup run 1.97.1 cargo fmt --all && rustup run 1.97.1 cargo check -p plurxd --all-targets --locked` | passed in 15 s; behavioral tests compiled but did not run |
| 2026-09-19 | W2a candidate | `rustup run 1.97.1 cargo fmt --all && rustup run 1.97.1 cargo check -p plurxd --all-targets --locked && git diff --check` | passed in 14 s; copy, EOF-tail, raw-writer validation, and fixed-header regressions compiled but did not run |
| 2026-09-19 | W2b candidate | `rustup run 1.97.1 cargo fmt --all -- --check && rustup run 1.97.1 cargo check -p plurxd --all-targets --locked && git diff --check` | passed in 22 s; produced/served, cadence, hard-deadline, pause-grace, and flow regressions compiled but did not run |
| 2026-09-19 | W3 candidate | `rustup run 1.97.1 cargo fmt --all && rustup run 1.97.1 cargo check -p plurxd --all-targets --locked && git diff --check` | passed in 14 s; immutable-prefix, live/retired grace, no-renewal, namespace, accounting, and cleanup regressions compiled but did not run |
| 2026-09-19 | W4 candidate | `rustup run 1.97.1 cargo fmt --all && rustup run 1.97.1 cargo check -p plurxd --all-targets && scripts/js-check && git diff --check` | passed; server/client additive diagnostics, Developer advisory enablement, and web syntax compiled/parsed; behavioral tests did not run |
| 2026-09-19 | merged-main review-fix candidate | `rustup run 1.97.1 cargo fmt --all && rustup run 1.97.1 cargo check -p plurxd --all-targets --locked` | passed in 18 s after the four adversarial findings were fixed; behavioral tests did not run |
| 2026-09-19 | `063a638d` | `rustup run 1.97.1 cargo fmt --all -- --check` and `cargo clippy -p plurxd --all-targets --locked -- -D warnings` | passed with Rust 1.97.1 |
| 2026-09-19 | `063a638d` | `cargo test -p plurxd --bin plurxd mkv_hls --locked` | 16 passed, 0 failed |
| 2026-09-19 | `063a638d` | focused copyseg rolling-session, no-clean-point ceiling, every-writer fixed-target, one-tick-over target, and typed-refusal commands | 6 passed, 0 failed across the named filters |
| 2026-09-19 | `063a638d` | `cargo test -p plurx-core mkv_hls --locked` | 5 passed, 0 failed in the matching unit target |
| 2026-09-19 | `063a638d` | `make web-check` | passed; Playwright evidence skipped because Playwright is not installed, as the target permits |
| 2026-09-19 | `063a638d` | `python3 -m unittest discover -s tests/operations -p test_docs_index.py` and `git diff --check` | 4 passed; diff clean |
| 2026-09-19 | `063a638d` | `make apple-build` outside the filesystem sandbox | iOS and tvOS builds passed; the sandboxed attempt failed because Xcode could not reach simulator/macro services |
| 2026-09-19 | `063a638d` | changed Apple status-decode, pause-expiry reporter, and typed-transport tests via `xcodebuild test-without-building -only-testing:…` | 3 passed, 0 failed |
| 2026-09-19 | `063a638d` | `make apple-test` | 644 ran with 88 broad-suite failures from the current client refactor; retained for the separate unit-failure batching process, not expanded into this feature PR |
| 2026-09-19 | `063a638d` | `make android` | passed in the pinned Android image; debug APK built |
| 2026-09-19 | `063a638d` | `make android-test` | passed; JVM unit tests and `lintDebug` green |
| 2026-09-19 | `e0b68578` | `rustup run 1.97.1 cargo fmt --all -- --check`, `cargo check -p plurxd --all-targets --locked`, and `cargo clippy -p plurxd --all-targets --locked -- -D warnings` | passed on the tree reconciled with `535f95d2`; Clippy first exposed one boolean assertion from the incoming main changes, corrected in `e0b68578`, then passed |
| 2026-09-19 | `e0b68578` | `scripts/js-check`, changed-from `validation.mobile_versions`, `validation.apple_build --merge-target origin/main`, and `git diff --check` | passed; Apple build 171 and Android versionCode 108 exceed the current merge target and all generated Apple claims agree |

The affected behavioral lane is complete. No zero-match command is counted as
acceptance; the first combined Apple command matched two tests because one
selector was stale, and the corrected selector was then run separately.

## Promotion state — candidate is ready for its gate

Forgejo PR #377 is open. Its hosted promotion receipt, merge, deployment,
physical-device acceptance, and production exact-identity requeue are
outstanding. This candidate authorizes only PR promotion and merge; it does not
authorize deployment or production mutation. The implementation is not an
incident fix until the forced rolling route and the exact-identity immutable
route both pass their physical acceptance boundaries.
