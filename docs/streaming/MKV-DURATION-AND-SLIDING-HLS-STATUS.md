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
| W0 reproductions | code complete; test pending | Exact 32 s published / 20 s fetched / 0 ms presented startup regression, missing-duration packet fixtures, scheduled reserve-drain cases, and paused/fetch-stopped clock cases compile | Run the named regression filters in the final fast lane |
| W0.5 startup admission | code complete; test pending | Actor owns generation/epoch/attempt-bound presentation evidence and the non-renewing 30 s deadline; all-target compile passed | Run `mkv_hls_startup` only in the final fast lane |
| W1a diagnostic/history | code complete; test pending | Unknown future provenance remains observable; authoritative max-attempt policy is passed into the settings-free telemetry transaction; rollback/schema regressions compile | Run `mkv_hls_diagnostic` and `mkv_hls_typed_outcome` in the final fast lane |
| W1b packet duration | code complete; fixture/test pending | Full metadata range comparison; bounded 256-packet head and three natural-EOF tails; 1 MiB/4 MiB caps; signed PTS span; typed failure matrix; symmetric ±2 s agreement all compile | Run parser, cap, process, timeout, and real FFmpeg fixture matrix in the final fast lane |
| W1c terminal repair | code complete; end-to-end test pending | Existing revisioned dry-run/apply repair now recognizes direct and attempt-limit-masked `index_completion_unverified` diagnostics on SQLite and Hiqlite; exact identity and idempotent receipt regression compiles | Run terminal → forced successor → ready → immutable VOD acceptance in the final fast lane |
| W2a fixed target | code complete; test pending | Copy segmentation enforces a separate strict 16 s bound before admission and before write; EOF tails use the strict bound; the shared rolling serving adapter validates both ordinary transcode and direct-copy FFmpeg manifests, freezes their served target at 16 s, and retires an oversized revision before actor publication; cached immutable VOD remains separate | Run variable-GOP, crossing-fragment, oversized-fragment, audio-tail, H.264/HEVC, and all-writer regressions in the final fast lane |
| W2b publication clock | code complete; test pending | Raw writer revisions remain staged; an exact-attempt actor observation commits immutable served snapshots on an independent 0.5T/T/1.5T clock; unfinished startup requires 3T; explicit and legacy flow pace from staged media; the 180 s pause grace is non-renewing | Run publication, pacing, EOF, replacement, pause, and fetch-stopped regressions in the final fast lane |
| W3 object promises | code complete; test pending | Segment state is explicit Advertised → Grace → Deleted; prefix removal is coalesced into immutable segment-bearing snapshots; grace deadlines use segment plus prior bounded playlist duration; range/init/media reads retain exact retired ownership without lease renewal; grace bytes remain in global hard-cap accounting until successful cleanup | Run deadline-edge, range/init, retirement, replacement, failed-unlink, and cap regressions in the final fast lane |
| W4 diagnostics/clients | code complete; test pending | Live status adds produced/served/staged publication coordinates, fixed target and deadlines, rate provenance, pause grace/retirement reason, maintenance, and Advertised/Grace/reserved/live bytes; Activity renders the fields; Apple and Android decode them additively; existing single-reopen and paused-intent fences remain intact; the false “every HLS session is VOD” comment is corrected; Developer now owns the explicit enable switch with build, usage, takeover, and physical-client advice that never gates it | Run server serialization, client contract, web syntax/policy, and paused-recovery regressions in the final fast lane |
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
| 2026-09-19 | W1c candidate | `rustup run 1.97.1 cargo fmt --all && rustup run 1.97.1 cargo check -p plurxd --all-targets --locked` | passed in 15 s; behavioral tests compiled but did not run |
| 2026-09-19 | W2a candidate | `rustup run 1.97.1 cargo fmt --all && rustup run 1.97.1 cargo check -p plurxd --all-targets --locked && git diff --check` | passed in 14 s; copy, EOF-tail, raw-writer validation, and fixed-header regressions compiled but did not run |
| 2026-09-19 | W2b candidate | `rustup run 1.97.1 cargo fmt --all -- --check && rustup run 1.97.1 cargo check -p plurxd --all-targets --locked && git diff --check` | passed in 22 s; produced/served, cadence, hard-deadline, pause-grace, and flow regressions compiled but did not run |
| 2026-09-19 | W3 candidate | `rustup run 1.97.1 cargo fmt --all && rustup run 1.97.1 cargo check -p plurxd --all-targets --locked && git diff --check` | passed in 14 s; immutable-prefix, live/retired grace, no-renewal, namespace, accounting, and cleanup regressions compiled but did not run |
| 2026-09-19 | W4 candidate | `rustup run 1.97.1 cargo fmt --all && rustup run 1.97.1 cargo check -p plurxd --all-targets && scripts/js-check && git diff --check` | passed; server/client additive diagnostics, Developer advisory enablement, and web syntax compiled/parsed; behavioral tests did not run |

No behavioral test has run yet. A skipped or zero-match command will not be
recorded as acceptance.

## Promotion state — nothing is deployable yet

Adversarial review, final fast-lane tests, the Forgejo pull request, promotion
receipt, merge, deployment, device acceptance, and production exact-identity
requeue are all outstanding. The implementation is not an incident fix until
the forced rolling route and the exact-identity immutable route both pass their
respective acceptance boundaries.
