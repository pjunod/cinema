# Seek scratch reservations — implementation receipt

**Effort:** `effort/seek-scratch` · **Base:** `a1414368400720884599732e3f8f3c71a9272edc`
· **Written:** 2026-09-20 · **Executes:** R1–R6 of the
[approved RCA](../streaming/SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX.md) under the
[implementation handoff](../streaming/SEEK-SCRATCH-RESERVATIONS-IMPLEMENTATION.md)

`pending` below is an honest value. Nothing is marked done that has not been
run, and every limitation the repair ships with is named rather than left for
a reader to discover.

## Source identity

| Field | Value |
|---|---|
| Base SHA | `a1414368400720884599732e3f8f3c71a9272edc` — `origin/main` at the time, and the RCA's own evidence base |
| Branch | `effort/seek-scratch`, one combined promotion (R6) |
| Working clone | An isolated clone from Forgejo on the build host. Paul's `~/code/plurx` checkout was never touched. |
| Promotion head/base/tree | pending — recorded at the qualification receipt |

## Environment

| Field | Value |
|---|---|
| Compiler | `rustc 1.97.1 (8bab26f4f 2026-07-14)`, the pinned toolchain, verified rather than inferred from the default `cargo` |
| Build host | `nuc3`, Ubuntu, 16 cores. The session container is two cores and seven gigabytes; one `cargo test` link of `plurxd` there took over half an hour and swapped, so the loop was moved. |
| Node | v22 for the web suites |
| Writer paths exercised | Native copy (`copyseg`) and direct/transcoded FFmpeg, in accounting; see the limitations below for what that does and does not prove |

## A — first green

**2026-09-20, patch `c5` on the isolated clone.** `scratch_charge_` 23 passed,
`seek_scratch_` 4 passed, `mkv_hls` 16 passed, `cargo fmt --all -- --check`
clean, `cargo check -p plurxd --bin plurxd --all-targets --locked` clean at
1.97.1. Revalidation against the exact promotion candidate is a separate line
and is recorded at qualification, not here.

## Accounting proof

| Question | Answer |
|---|---|
| Entry key and ownership | `ScratchKey`, minted by `ScratchLedger::reserve` **before** a producer or a session id exists, and bound to the session with `ScratchPermit::bound_to` rather than released and reacquired. Session ids are reused across incarnations in cluster recovery; the key never is. |
| Lock order | One `std::sync::Mutex` inside the ledger, held across map operations and integer adds only. Never held across `.await`, filesystem I/O, a process wait, an actor exchange, a response send or a grace timer. The old `scratch_reservation_gate` tokio mutex is gone: the ledger's own critical section is the linearization point for admission, growth, conversion and release. |
| Writer fence | `register_writer` is called before the copy worker is spawned and its guard is moved **into** the worker, so retirement cannot see zero writers for a reader that is about to exist. `begin_retirement` fences new registrations; the ones already issued stay counted. A classification the actor rejects as `SessionEnded` does not release the guard — the worker does, by ending. |
| Measurement result semantics | `Session::measure_scratch_bytes` returns `Complete(bytes)` / `Absent` / `Incomplete` / `NotScratch`. Only `Complete` and `Absent` may collapse a reservation. `commit_quiescent_measurement` additionally refuses a measurement whose `inventory_generation` moved while the directory was being walked. |
| Pin identity | `(object name, apparent length)` within one incarnation, refcounted, so concurrent range readers of the same object charge once. Never an inode: the ledger does not open the file. It can over-count a coincidence, never under-count a distinct object. The pin lives on `MediaResponseAuthorization`, which is already the move-only owner of a streamed response, so EOF, cancellation and transport error all settle it exactly once. |
| Once-only release | `account_unlink_all` sets used and granted bytes to zero, moves still-open objects to pin ownership, and drops the entry when nothing can write it, no name remains and no reader holds it. A duplicate cleanup completion finds no entry and subtracts nothing. |
| Flat-directory scanner audit | Session scratch is one flat directory. `init.mp4`, `index.m3u8`, `segNNNNN.m4s`, their `*.tmp` staging names and the `.plurx-retention-*` renames are all regular files at its top level; nothing writes a subdirectory there. `entry.metadata()` follows a symlink, so a link planted in the directory is charged for its target's apparent length rather than skipped, and a `NotFound` between `read_dir` and `metadata` is skipped rather than failing the scan. The sum is `metadata.len()` — apparent length in one namespace, not `st_blocks` and not a claim about physical reclamation. |

## B — release proof

| Question | Answer |
|---|---|
| Create field | `transport`, an optional bounded string on `CreateSession` and on the durable `SessionRequest`. Validated at ingress (1–32 characters, lower-case ascii, digits, `-`, `.`) so a client string cannot become a metric label; an unrecognized but well-formed value is stored and reads as conservative. |
| Fingerprint | Deliberately **not** in `intent_fingerprint_with_user_scope`. Adding a field there would change every existing fingerprint and make a mixed-version cluster disagree about request identity; the durable recipe already answers "what class is this exact session". |
| Replay / remote / takeover | The class is read at release time from `MediaSessionRoute.recipe_json`, which is the serialized create request. A remote owner, an idempotent replay and an owner takeover therefore all read the answer the create wrote. A recipe an older node wrote carries no transport at all and reads as conservative. |
| Eligible class | `hlsjs` only. `retirePlaybackPredecessor` destroys the instance before `releaseSession` sends the DELETE (`decode-margin.js:399-408`), and a destroyed instance issues no further requests. |
| Conservative classes | Native HLS, AirPlay, Apple, unknown, absent. Apple releases the session **before** `replaceCurrentItem` (`PlayerController.swift:4593/4620`), so the old item still exists while the release runs and no finite AVFoundation retry bound was established. They keep the original promise. |
| Allowance | One session segment target — `ROLLING_PRESENTATION_TARGET_SECS`, 16 s at this base. The 20 s server `SEGMENT_WAIT` is a server-side blocking budget and is deliberately not used. |
| Non-renewing | A single `AtomicBool` latch takes the first accepted release; the deadline can only move in. A duplicate DELETE cannot extend, renew or re-shorten it. A release that arrives before retirement finishes is folded into `prepare_retired_object_promise`, so the promise is published already short rather than racing the cleanup owner's sleep. |
| Terminal scope | Only `Terminal::Deleted` — a client release. An admin stop, a revocation or a supersession is not the viewer saying they are finished with the bytes. |

## C — mechanism

**Selected: an owned write boundary for the native copy writer; the direct
FFmpeg path stays conservative.**

`copyseg::SessionDir::publish_file` holds the complete slice before it creates
the temporary file, so every object a copy session materializes — the
initialization segment, each media segment, each playlist rewrite — passes
`ScratchLedger::authorize_write` before it exists. The grant asks for the
slice twice over, because the temporary name and the final name overlap during
the rename and a playlist rewrite overlaps its predecessor. A refused grant
backpressures the Rust writer itself, which is the point: suspending FFmpeg
does not stop a pipe reader from draining and writing.

* **Enforced by code:** the copy writer's per-object grant; the ledger's
  admission, growth and release arithmetic; the refusal-to-write on a denied
  grant; the bounded wait (120 s) and its classified failure.
* **Measurements, not bounds:** the envelope
  (`readrate × bitrate × interval × 5`) and the startup sizing. An observed
  maximum is not a hard bound, and this receipt does not claim otherwise.

**Direct and transcoded FFmpeg output keeps the whole per-session ceiling.**
`-f hls` writes its own segment and playlist files with no Rust hook before
them, and a measurement taken afterwards can only discover an overrun. The
candidates were weighed and none passed:

| Candidate | Why it was not selected |
|---|---|
| Owned output boundary | Would mean routing transcoded output through the fragmented-pipe + `copyseg` path the copy sessions use. That is a substantial media-pipeline change — playlist tags, discontinuities, subtitle and audio-only handling, DV — and it cannot be qualified without the physical runs this session cannot perform. Surfaced here rather than disguised as a local refactor. |
| Hard writable quota | XFS/ext4 project quotas need privileged provisioning that cannot be assumed of a deployment. `RLIMIT_FSIZE` is per file, not an aggregate directory limit. `ffmpeg -fs` counts cumulative muxed bytes across the whole run, not current on-disk bytes, so on a rolling session with retention it would truncate a long film rather than bound its scratch — it was prototyped in reasoning and rejected on that. |
| Enforced envelope plus hold | The pieces exist — `ROLLING_PUBLICATION_POLL` is 250 ms and refreshes the measurement, the flow worker re-grants on each fetch and on the repair pass, and `ProcessSignal::Suspend` actuates — but `FLOW_CONTROL_REPAIR_INTERVAL` is a requested cadence and not a proven maximum gap, the 90 s `-readrate_initial_burst` runs flat out, and declared bitrate is an estimate. Sizing a smaller reservation on that would be presenting a measurement as a bound. |

**So R4 is complete for the native copy writer and explicitly incomplete for
direct FFmpeg.** That is stated on the status page, in the Developer tab's
advisory card, and here. The incident path (Safari copy-HLS) and the
more-than-three-producers case are both the copy path.

| Constant | Value | Why |
|---|---|---|
| Startup sizing | `max(rolling runway at rate, copy publish gate) + one segment target`, times the output rate, plus the envelope | At 1× that is 48 s + 16 s = 64 s of output, not the 12 + 15 a reading of the copy writer alone suggests. A session sized below its effective gate can never publish a playlist for a client to drain. |
| Unknown-rate bootstrap | 256 MiB | A bootstrap, not a floor and not a safety proof. An unknown-rate 80 Mb/s source must obtain further grants before it consumes them, and the write boundary is what makes that true. |
| Envelope safety | 5× | Five times the estimate is what five times the estimated bitrate produces in the same interval. Named as a multiplier on a measurement, not as a bound. |
| Grant wait | 120 s, polled at 250 ms | Waiting is right — the budget frees as other sessions retire — but waiting forever is not, so the expiry is a classified writer failure and never a silent stall. |
| Writer settle | 30 s | A writer that outlives it keeps the conservative producer charge and records `writer_stalled`. A timeout is not settlement. |
| Windows | conservative, unproved | `process_control.rs` implements `NtSuspendProcess`/`NtResumeProcess`, but no native runtime receipt exists for growth enforcement. Compilation is preserved and behaviour is unchanged. |

## Regression results

Commands run on the pinned toolchain against the exact task head:

```bash
rustup run 1.97.1 rustc --version                                         # 1.97.1 (8bab26f4f)
rustup run 1.97.1 cargo fmt --all -- --check                              # clean
rustup run 1.97.1 cargo check -p plurxd --bin plurxd --all-targets --locked  # clean
rustup run 1.97.1 cargo test -p plurxd --bin plurxd scratch_charge_ --locked # 23 passed
rustup run 1.97.1 cargo test -p plurxd --bin plurxd seek_scratch_ --locked   # 4 passed
rustup run 1.97.1 cargo test -p plurxd --bin plurxd mkv_hls --locked         # 16 passed
node --test tests/playback/seek-control.test.js                           # 5 passed
node tests/web/settings-sections.test.js                                  # 26/26
node tests/playback/player-input-contract.test.js                         # pass
node tests/playback/playback-surface-contract.test.js                     # pass
node tests/web/player-dom.test.js                                         # pass
node tests/web/nav-keyboard.test.js                                       # pass
node tests/web/layout-containment.test.js                                 # pass
node tests/web/theme-family.test.js                                       # pass
node tests/web/page-read-budget.test.js                                   # pass
python3 -m unittest discover -s tests/operations -p test_docs_index.py    # OK
```

`scratch_charge_` and `seek_scratch_` are the filters the RCA proposed; both
match a positive executed count and neither is reported from a filter that
matched nothing.

| ID | Covered by |
|---|---|
| A-L1 | `scratch_charge_a_fourth_replacement_fits_when_the_retained_bytes_fit` — one playback and three replacements at the shipped defaults; the fourth fits because 384 MiB of retained media is what is charged, not three whole reservations. |
| A-L2 | `scratch_charge_twenty_retired_inventories_fit_beside_a_live_pair` — twenty 128 MiB retained inventories through real `begin_retirement` / `commit_quiescent_measurement` transitions plus an incumbent and a successor. No permit is dropped to make room. |
| A-L3 | `scratch_charge_counts_one_incarnation_once_across_registry_transfer` — the interleaving the old two-registry fold double-counted, with a genuinely provisional start beside it that the old subtraction hid. |
| A-W1 | `scratch_charge_keeps_the_producer_charge_until_writers_settle`, `scratch_charge_barrier_waits_for_a_writer_and_reports_a_stall` — a held writer keeps the conservative charge, a stall is reported rather than assumed settled, and the conversion happens once the writer ends. |
| A-M1 | `scratch_charge_an_incomplete_measurement_keeps_the_conservative_charge`, `scratch_charge_refuses_a_measurement_that_raced_a_writer` — including a sparse fixture, which proves the charge is apparent length. |
| A-P1 | `scratch_charge_keeps_an_unlinked_object_charged_until_its_last_reader_closes` — two concurrent ranges over one object, unlink, one charge, released on the last close. |
| A-C1 | `scratch_charge_failed_cleanup_keeps_the_bytes_charged`, `scratch_charge_survives_the_owner_and_releases_once_cleanup_proves_it` — a failed unlink stays charged and named; a dropped owner does not free bytes; a duplicate completion subtracts nothing. |
| A-H1 | `scratch_charge_real_refusal_answers_503_through_the_http_mapper` — the genuinely returned refusal string through the real `session_start_error`. A constructed already-prefixed string would pass on the broken build. |
| A-I1 / A-I2 | `tests/playback/web-policy.test.js` player-adapter rows (now composing `pbRelativeSeekBase`), plus `playbackChangeRecipeKey` / `playbackChangeAlreadyInFlight` in the `web-control` harnesses. |
| A-S1 | `seek_scratch_incumbent_survives_a_pending_replacement`, `seek_scratch_a_genuinely_unpresented_stream_still_expires`, `seek_scratch_an_executing_local_seek_still_reports_its_target`, and the client half in `tests/playback/seek-control.test.js`. Both halves read the same recorded snapshots from `tests/playback/seek-scratch-snapshots.json`, so neither can drift alone. |
| B-R1 | `scratch_charge_an_exact_release_shortens_one_promise_and_never_renews_it`, `scratch_charge_a_stale_or_unknown_release_reaches_no_promise`. |
| B-R2 | `seek_scratch_release_class_comes_from_the_durable_recipe`, `scratch_charge_only_the_audited_transport_earns_a_shorter_promise`. |
| C-G1 / C-G2 | `scratch_charge_growth_respects_the_configured_ceiling`, `scratch_charge_authorize_write_grows_only_by_the_shortfall`, `scratch_charge_regrant_tracks_actual_bytes_instead_of_ratcheting`, `scratch_charge_a_denied_regrant_keeps_the_producer_held`. |
| C-S1 | `scratch_charge_startup_sizing_covers_the_effective_publish_gate`. |
| C-N1 | `scratch_charge_four_low_bitrate_producers_fit_under_the_unchanged_cap` — in accounting. The product claim needs the physical run below. |
| B-P1, ALL-1, ALL-2 | pending — they need the physical acceptance below. |

## Known-failing before this change

Three suites fail identically at the base SHA and are not caused by this work.
Each was confirmed by running the same command against a pristine extraction
of `a1414368`:

* `copyseg::tests::the_playlist_matches_the_template_and_produce_can_parse_it`
  and `copyseg::tests::an_unsupported_final_tail_stays_typed_after_local_playlist_creation`
* `tests/playback/web-policy.test.js` — the Android `behindLiveWindow.attached()`
  count assertion (expects 2, `Controller.kt` has 1)
* `tests/playback/web-control.test.js` — "the in-flight exchange's verdict does
  not answer this ask"
* `scripts/web-hls-startup-browser-check` — Playwright times out in this
  container at the base too

## Physical acceptance

**pending.** Repeated seeks against a forced rolling-HLS fixture, a long 4K
copy, four low-bitrate producers, a deliberately small cap, and a refusal held
beyond 30 s. None of it can be done from this session; a prompt for the
session that has the devices is in the status page.

## Review and qualification

| Item | State |
|---|---|
| Adversarial code review | pending — at the PR, before the fast lane |
| Fast lane | pending |
| `tests/ui-structure.golden` | **known drift.** The Developer tab's advisory card adds DOM to `settings-developer`. Regenerating the golden needs `scripts/ui-baseline --self-host --update`, which boots a `plurxd` and drives Playwright; the container's Playwright times out and the build host has no browser. Not in the fast lane; owed before the full suite is green. |
| Deployment | not performed, and not authorized by this document |
