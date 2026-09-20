# MKV duration and sliding HLS — implementation status

**Status:** implementation in progress · **Updated:** 2026-09-19 EDT ·
**Branch:** `effort/mkv-duration-sliding-hls` · **Base:** `87664620`

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
| W2b publication clock | code complete; review findings fixed; test pending | Raw writer revisions remain staged; an exact-attempt actor observation commits immutable served snapshots on an independent 0.5T/T/1.5T clock; batch and initial-runway media scale with the accepted 0.5×–4× playback rate; a producer below the requested rate retires with `rolling_insufficient_capacity` instead of silently draining; the 180 s pause grace is non-renewing and terminates as `pause_expired` | Run publication, 0.5×/1×/2× pacing, typed-capacity, EOF, replacement, pause, and fetch-stopped regressions in the final fast lane |
| W3 object promises | code complete; test pending | Segment state is explicit Advertised → Grace → Deleted; prefix removal is coalesced into immutable segment-bearing snapshots; grace deadlines use segment plus prior bounded playlist duration; range/init/media reads retain exact retired ownership without lease renewal; grace bytes remain in global hard-cap accounting until successful cleanup | Run deadline-edge, range/init, retirement, replacement, failed-unlink, and cap regressions in the final fast lane |
| W4 diagnostics/clients | code complete; review findings fixed; test pending | Live status adds produced/served/staged publication coordinates, fixed target and deadlines, rate provenance, pause grace/retirement reason, maintenance, and Advertised/Grace/reserved/live bytes; whole-directory scratch measurement includes init, temporary, and pre-playlist files; producer admission reserves the configured per-session ceiling plus a bounded in-flight envelope before spawn; Activity renders typed pause expiry; Apple and Android stop reporting a `410 pause_grace_expired` without an automatic reopen; Developer owns the explicit enable switch with advice that never gates it | Run server serialization, reservation race, client contract, served-web-tree policy, and paused-recovery regressions in the final fast lane |
| W5 review/qualification | adversarial review complete; findings fixed; fast lane pending | The one final adversarial review found four issues: playback-rate pacing, pre-publication scratch charging, generic pause expiry, and substring diagnostic repair. All four are fixed on the merged-main candidate; the pinned all-target compile passes | Run the one final fast lane, fix only observed failures, create the consolidated PR, and merge after its gate passes |

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

No behavioral test has run yet. A skipped or zero-match command will not be
recorded as acceptance.

## Promotion state — nothing is deployable yet

Final fast-lane tests, the Forgejo pull request, promotion receipt, merge,
deployment, device acceptance, and production exact-identity requeue are all
outstanding. The implementation is not an incident fix until
the forced rolling route and the exact-identity immutable route both pass their
respective acceptance boundaries.
