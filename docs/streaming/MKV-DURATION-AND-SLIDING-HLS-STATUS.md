# MKV duration and sliding HLS — implementation status

**Status:** implementation in progress · **Updated:** 2026-09-19 EDT ·
**Branch:** `effort/mkv-duration-sliding-hls` · **Base:** `1ae2c4ec3`

Companion to the
[reviewed RCA](MKV-DURATION-AND-APPLE-SLIDING-HLS-RCA-AND-FIX.md) and
[implementation contract](MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md).
This page is the live ledger: it separates code completed, evidence collected,
review disposition, qualification, and production work still requiring an
operator decision.

## Package ledger — code, proof, and remaining work stay separate

| Package | State | Evidence | Next |
|---|---|---|---|
| W0 reproductions | in progress | Exact 32 s published / 20 s fetched / 0 ms presented regression added and compiled | Add deterministic missing-duration and reserve-drain regressions |
| W0.5 startup admission | code complete; test pending | Actor owns generation/epoch/attempt-bound presentation evidence and the non-renewing 30 s deadline; all-target compile passed | Run `mkv_hls_startup` only in the final fast lane |
| W1a diagnostic/history | code complete; test pending | Unknown future provenance remains observable; authoritative max-attempt policy is passed into the settings-free telemetry transaction; rollback/schema regressions compile | Run `mkv_hls_diagnostic` and `mkv_hls_typed_outcome` in the final fast lane |
| W1b packet duration | code complete; fixture/test pending | Full metadata range comparison; bounded 256-packet head and three natural-EOF tails; 1 MiB/4 MiB caps; signed PTS span; typed failure matrix; symmetric ±2 s agreement all compile | Run parser, cap, process, timeout, and real FFmpeg fixture matrix in the final fast lane |
| W1c terminal repair | not started | Existing explicit force path named in the contract | Prove terminal-to-ready transition and add a read-only preview |
| W2a fixed target | not started | Rolling copy writer and mutable target named in the contract | Enforce a fixed covering target before first publication |
| W2b publication clock | not started | Existing actor observation contract named in the contract | Separate produced and served inventory and schedule publication |
| W3 object promises | not started | Retention consumers and accounting boundary enumerated | Preserve advertised URIs through prune and retirement |
| W4 diagnostics/clients | not started | Apple, Android, and web consumers enumerated | Expose terminal state and keep client recovery bounded |
| W5 review/qualification | not started | Final fast-lane command set is defined in the handoff | Review once at the final candidate, fix findings, run the fast lane, merge |

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

No behavioral test has run yet. A skipped or zero-match command will not be
recorded as acceptance.

## Promotion state — nothing is deployable yet

Adversarial review, final fast-lane tests, the Forgejo pull request, promotion
receipt, merge, deployment, device acceptance, and production exact-identity
requeue are all outstanding. The implementation is not an incident fix until
the forced rolling route and the exact-identity immutable route both pass their
respective acceptance boundaries.
