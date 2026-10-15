# HTTPS build — milestones, ownership and qualification

**Status:** H1 locally verified; effort gate/integration pending · **Written:** 2026-10-15
**Owner:** GPT-6.1 Sol builder; parent coordinates and reviews

This canonical ledger executes [the contract](HTTPS-IMPLEMENTATION.md).
Work milestone by milestone; flag trust/lifecycle contract changes before
implementing them. No live deployment is implied by this build.

## 1. Isolation and compiler prerequisite

Use the dedicated HTTPS worktree from `9eb7f6738`; the main checkout contains
unrelated user work. Integrate on `effort/https-support` with serial task
branches/PRs into it. Milestones share files, so the disjoint-task exception
does not apply. Follow [AGENTS.md](../../AGENTS.md) and the
[pipeline](../DEVELOPMENT_PIPELINE.md), with the session's explicit contributor
instructions taking priority over conflicting older descriptions.

PATH initially selected Homebrew Rust 1.98.0; pinned Rust 1.97.1 is installed
and verified with `rustup run 1.97.1 rustc --version`. Sol must establish a
successful baseline check before Rust edits and use the pinned compiler,
Clippy and rustfmt throughout. If local dependencies block, use the
[source-only loop](../ci/AGENT-COMPILE-LOOP.md). Keep target warm; do not use
CI to discover compiler failures. Recheck the exact current candidate.

## 2. Review and milestone ownership

| Step | Owner/files | Acceptance |
|---|---|---|
| D0 | Parent: contract, ledger, review, index/backlog | Independent adversarial review resolved before build release. |
| H1 | Sol: core config; daemon TLS module; manifests/lock; tests | Default/precedence, validation, persistence and trusted identity tests; pinned check/Clippy. |
| H2 | Sol: startup, HttpAcceptor, drain, TLS and URL consumers | Real TLS, HTTP parity, handshake concurrency/cap/deadline, observation transition and shutdown tests. |
| H3 | Sol: AppState, admin route, router, existing settings scripts | Admin/node/CAS/persistence/env guards, saved-vs-active UI, safe links and restart/disable tests. |
| H4 | Sol: maintained references, ledger/index/backlog and validation | Focused suites, affected checks, parent review, qualification and explicit acceptance gaps. |

H1–H3 are serial due to file overlap. Each task has focused regressions and
`Regression-Test: <path>::<test name>` in its PR and landing commit. Use
`fix(https): ...` for user-visible behavior. Dispatch and pass Effort
development gate before merging each task PR into the effort. Never accept
red, skipped or stale evidence as a passing gate.

## 3. Regression matrix

| Area | Required cases |
|---|---|
| Config | Old defaults/TOML; both/one/no paths; env and data_dir precedence; invalid boolean/port/collision; SAN bounds and IPv6. |
| Identity | Create once; restart/off-on preserve fingerprint; permissions; concurrent creation; corrupt/partial/symlink state; expired identity; SAN mismatch; oversize files; FIFO/device rejection; public-chain-only export; valid private CA; key mismatch. |
| Socket | Trusted generated/custom chain accepted; unknown trust/wrong hostname rejected; plaintext rejected on TLS; legacy HTTP still works; silent/malformed peer cannot starve healthy client. |
| Lifecycle | Pending cap/deadline; stop cancels pending and drains active streams; observation transition; bind failure; task cleanup. |
| Settings | Anonymous/non-admin denial; node-local only; node/revision conflict including after restart; no blind UI retry; atomic failed save; concurrent saves; env override conflict; disable with missing file; exact saved values after restart. |
| Web | Lazy loader/retry, node identity, active/desired distinction, restart message, toggle/paths, HTML escaping, IPv6/no-token link, graduation contracts. |
| Playback | HTTPS bootstrap/API/artwork/subtitles/HLS/ranges/WebSockets; cluster origin stays HTTPS; no global verification bypass. |

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

Post-review persistence clarification: rename publishes the revision. A later
directory-sync error is a committed save with `durability_confirmed: false`;
H3 must adopt desired/revision and show the warning without blind retries.
The injected post-publication sync regression covers this distinction.

Windows scope clarification: supplied material uses regular local disk PEM
files through no-reparse opens; device/pipe/UNC namespaces and cert-manager
symlinks are rejected. Unix cert-manager symlinks remain supported. No
Windows compile or physical safety acceptance is claimed.

H1 is implemented on `codex/https-h1-config-identity`. The full configuration
suite passes (22 tests), and final identity/trust fixtures pass (8 tests).
Pinned all-target compilation
and denied-warning daemon Clippy pass. Parent reviewed the full source and
found no remaining blocker after the documented clarifications. The
new daemon identity module is temporarily `cfg(test)` in H1: its all-target
checks and real TLS regressions compile it, while H2 must remove that
staging attribute when the production listener owns it. H2/H3 integration,
all task gates, promotion and physical acceptance remain pending. Nothing
has been deployed or enabled on a live node.

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
