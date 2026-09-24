# Seek scratch reservations — implementation receipt

**Status:** open · **Reconciled:** 2026-09-20

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
| Flat-directory scanner audit | Session scratch is one flat directory. `init.mp4`, `index.m3u8`, `segNNNNN.m4s`, their `*.tmp` staging names and the `.plurx-retention-*` renames are all regular files at its top level; nothing writes a subdirectory there and nothing writes a symlink. `DirEntry::metadata` does **not** traverse a link, so `is_file()` is false for one and it is skipped rather than charged for its target — an undercount of a thing this directory never contains, recorded because a reader deserves to know which way the scanner is wrong. A `NotFound` between `read_dir` and `metadata` is skipped rather than failing the scan. The sum is `metadata.len()` — apparent length in one namespace, not `st_blocks` and not a claim about physical reclamation. |
| Bytes the walk has not seen | Two further terms, both folded into the charge. `pending_bytes` is authorized for a write in flight; `written_bytes` is what landed since the last complete measurement. Without them an authorization would be satisfiable an unlimited number of times per scan interval, because the only thing it compared against moved on a directory walk. |

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

The authorization is **held**, not merely checked. It debits `pending_bytes`
under the same lock that tests the allowance and is released only when the
rename settles, at which point the bytes move to `written_bytes` and stay
charged until a directory walk subsumes them. The first version of this only
compared `grant - used`, and `used` moves on a 250 ms walk — so N consecutive
objects between two walks all passed the same comparison, and what looked like
a write bound was a timing bound. `scratch_charge_two_writes_cannot_share_one_allowance`
is the regression that keeps it honest.

The wait is bounded only for a session that has **never published**. That is
the one case where waiting is waiting for nothing: no playlist exists, so no
client can drain anything, and a cap too small to publish a first playlist
would hold the session forever. Once the playlist is out, a parked writer is
the same thing as the flow controller's own hold and waits as long as the
session lives — its idle and startup deadlines bound that, and the writer also
bails the moment retirement fences the allocation, so a parked writer can no
longer hold a conservative producer charge past the fence.

* **Enforced by code:** the copy writer's per-object grant; the ledger's
  admission, growth and release arithmetic; the refusal-to-write on a denied
  grant; the bounded wait (120 s) and its classified failure.
* **Measurements, not bounds:** the envelope
  (`readrate × bitrate × interval × 5`) and the startup sizing. An observed
  maximum is not a hard bound, and this receipt does not claim otherwise.
  `MediaFile::bitrate` is ffprobe's whole-container average over the whole
  title, and an opening reel routinely runs at two or three times it, so the
  startup allowance carries a 3× peak factor. Under-sizing the *startup* is
  the one thing growth cannot recover from, because a session that cannot
  publish a first playlist has nothing to drain it.
* **Known and accepted:** a grant timeout before first publication is carried
  as `CopyProducerExitClassification::ReaderFailed`. The reason string is
  prefixed `rolling_insufficient_capacity:`, the daemon's existing marker, but
  the typed class a copy reader can carry has no capacity variant, so the
  client sees a producer fault rather than a retryable capacity refusal.
  Adding one means a new variant through the control actor; it is named here
  rather than papered over.
* **Known and accepted:** an unknown-rate source gets a 256 MiB envelope,
  which is larger than the 67 MiB a *known* 80 Mb/s source gets. That reads
  backwards until you remember what the envelope is for; it is the price of
  not knowing, and the write boundary is what keeps it from compounding.

**Direct and transcoded FFmpeg output keeps the whole per-session ceiling.**
`-f hls` writes its own segment and playlist files with no Rust hook before
them, and a measurement taken afterwards can only discover an overrun. The
candidates were weighed and none passed:

> **Superseded 2026-09-24.** `-method PUT` gives the muxer an owned output
> boundary without changing the muxer, so the first candidate below did not
> need the pipeline change it was rejected for. See the
> [FFmpeg HLS write boundary receipt](ffmpeg-hls-write-boundary-receipt-2026-09-24.md).

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

These fail — or never finish — identically at the base SHA and are not caused
by this work. Each was confirmed by running the same command against a pristine
extraction of `a1414368`:

* `copyseg::tests::the_playlist_matches_the_template_and_produce_can_parse_it`
  and `copyseg::tests::an_unsupported_final_tail_stays_typed_after_local_playlist_creation`
* `tests/playback/web-policy.test.js` — the Android `behindLiveWindow.attached()`
  count assertion (expects 2, `Controller.kt` has 1)
* `tests/playback/web-control.test.js` — "the in-flight exchange's verdict does
  not answer this ask"
* `scripts/web-hls-startup-browser-check` — Playwright times out in this
  container at the base too
* `transcode::tests::failed_retention_garbage_unlink_cannot_reexpose_a_served_path`
  — **hangs**, at the base SHA as well as here. It parks forever on the first
  `pause.wait()` because `gc_expired_segments` returns at `expired.is_empty()`
  and therefore never spawns the cleanup worker the barrier is waiting for:
  nothing in the fixture's path moves a segment from `Advertised` into
  `Grace`. This change touches the test (its fixture is now admitted against
  the manager's own ledger, so the global-cap assertion tests something again)
  and touches the function under it (one `observe_scratch_bytes` mirror), but
  the hang predates both and the edits cannot be exercised until the fixture
  is repaired. Left for the batch that owns the full suite; recorded here
  rather than left for someone to rediscover.

## Physical acceptance

**pending.** Repeated seeks against a forced rolling-HLS fixture, a long 4K
copy, four low-bitrate producers, a deliberately small cap, and a refusal held
beyond 30 s. None of it can be done from this session; a prompt for the
session that has the devices is in the status page.

## Adversarial review

Three independent reviewers, one per unit boundary, before the fast lane.
Twenty-four findings; the ones that changed the code:

| Finding | Disposition |
|---|---|
| `authorize_write` compared an allowance but never debited it, so N writes between two directory walks all passed the same comparison — the central claim of unit C was false | Fixed. `pending_bytes` and `written_bytes`, debited and settled under the ledger lock, with a regression that fails on the old shape. |
| The retired cleanup owner returned without settling when its map row had been overwritten by a same-id successor, leaving a charged entry whose last `Arc<Session>` was about to drop — an unrecoverable leak of exactly the kind the repair exists to remove | Fixed. Exactness governs the map row, not the directory: `w-{uuid}` is this incarnation's alone, so cleanup and release now run either way. |
| `regrant` lowered the ceiling under an in-flight write, handing the budget capacity a writer was about to spend | Fixed. The ceiling never falls below what is materialized or authorized. |
| `authorize_write` took the lock three times and could return true against a grant a concurrent regrant had moved | Fixed. One critical section, re-checked after the grow. |
| A refused `register_writer` was ignored and the worker wrote anyway, into a directory whose final inventory had already been committed | Fixed. `None` now means the worker does not start. |
| The per-session capacity hold was evaluated inside `physical_ahead.and_then(...)`, and `physical_ahead` is `None` for exactly the session it is about — a copy producer inside its publish gate | Fixed. `scratch_grant_hold` is applied before, in both lease modes. |
| A pin acquired after cleanup removed the names was recorded as linked, so its bytes went uncharged | Fixed. The pin reads the entry's lifecycle. |
| An entry that became collectable on the last writer's exit was never collected | Fixed. `finish_writer` collects. |
| `transport` on a `deny_unknown_fields` envelope would 400 at an un-upgraded peer | Fixed by bumping `PROTOCOL_VERSION` to 6, which is the mechanism that already exists for this: offers are filtered on it, so a mixed-version cluster simply places locally instead of across the version line. The field is now also bounded at worker ingress, not only at the public door. |
| A parked writer could outlive the retirement fence by 90 s, holding the conservative producer charge it was waiting for the release of | Fixed. The wait bails on the fence. |
| The shortened release deadline moved the cleanup owner's sleep but not the serve gate, so cleanup could delete objects the segment index still advertised | Fixed. The promise, the retired map row and every per-object grace deadline move together. |
| Retention renames and their removal updated `live_bytes` but not the ledger | Fixed. Both mirror into the ledger in the same breath. |
| The R2 dedupe compared the raw target while the in-flight key was built from the clamped one, so it never fired in the last two seconds of a title or for any audiobook | Fixed. The guard runs after clamping and after the part-timeline conversion. |
| The recipe key described the *player* rather than the *request*, and read fields the change had not written yet, so an in-flight rung switch looked like a plain seek | Fixed. The key is built from the merged change, exactly as `requestPlaybackMediaChange` builds it. |
| The desired destination outlived its replacement, so a replay after a refused seek froze the scrubber at the old destination and mis-based every subsequent tap | Fixed. The destination only speaks while a replacement is pending or retained. |
| The Developer card asked for the transport with `hevcCopy` hardcoded false, reporting hls.js for an HEVC copy session the server had correctly classed conservative, and claimed a class with no player open | Fixed. It reads the open session, or says there is none. |
| `closePlayer` released before it destroyed — harmless by scheduling, but a client claiming destroy-before-DELETE at *every* release site should not have an exception | Fixed. |
| An existing regression asserting that failed physical deletion stays inside the global cap silently passed on zero, because its fixture had no ledger entry | Fixed. The fixture is admitted against the manager's own ledger, so the assertion tests the thing again. |
| The dedupe, the relative-seek base and the scrubber change had no behavioural test — the catalogue entry's falsifiability claim was false | Fixed. Three tests with assertions, each mutation-checked: reverting the base, keying on position alone, and dropping the clamp each fail one. |
| The survival test proved lease renewal, not survival: nothing in it carried a refused destination | Fixed. `seek_scratch_the_old_snapshot_shape_reaps_a_playing_incumbent` replays the pre-repair shape at the same cadence and requires `StartupExpired`. |
| The replayed request omitted `supported_actions`, a shape the web player never sends | Fixed. |
| The symlink claim in the scanner's comment was backwards | Fixed, in the comment and above. |

Findings recorded and not changed: the unknown-rate envelope being larger than
a known one (correct, explained above); the grant-timeout classification
(named as a limitation above); `pbRelativeSeekBase` reading the module `PLAYER`
while its callers hold a captured `me` (the same object at every call site
today).

## Review and qualification

| Item | State |
|---|---|
| Adversarial code review | Three reviewers, twenty-two findings addressed, three recorded as accepted limitations. Above. |
| Fast lane | pending |
| `tests/ui-structure.golden` | **known drift.** The Developer tab's advisory card adds DOM to `settings-developer`. Regenerating the golden needs `scripts/ui-baseline --self-host --update`, which boots a `plurxd` and drives Playwright; the container's Playwright times out and the build host has no browser. Not in the fast lane; owed before the full suite is green. |
| Deployment | not performed, and not authorized by this document |
