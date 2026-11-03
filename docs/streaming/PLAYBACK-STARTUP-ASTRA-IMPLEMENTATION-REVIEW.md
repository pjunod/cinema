# Final adversarial implementation review

**Verdict: Request changes.** Seven actionable findings remain in frozen candidate `2bc1e54e1467baa9afe7f2f5219b439e14da4902`, compared with `origin/main` / `bd213c81942175f591912e4bb0a2cc10f2a7a195`. This is the complete batched implementation review, not a proposal review. The implementation was not edited and no behavioral tests were executed.

Review source: frozen candidate `2bc1e54e1467baa9afe7f2f5219b439e14da4902`.
All source paths below are repository-relative. The cited lines belong to that frozen candidate.

## 1. P1 — Preserve exact settlement recovery after source work has joined

**Primary location:** `crates/plurxd/src/background_jobs.rs:528-543`.
Related paths: `background_jobs.rs:411-430`; `crates/plurxd/src/state.rs:10048-10053,10080-10085`; `crates/plurxd/src/source_custody.rs:575,611-620`.

`ActiveBackgroundJob::finish` retires its heartbeat and job, attempts one source-custody deletion, logs an RPC error and returns. Analysis does the same after its read/child join. The exact custody receipt then has no retry owner. An RPC failure before the DELETE commits leaves a physical-custody row that neither lease expiry nor a successor job fence can remove. `source_io_occupancy` deliberately continues charging such rows. A transient store failure can therefore permanently consume the source domain even though the physical work is already known to have ended; storage-domain remapping is blocked too.

There are three distinct recovery cases to fix:

- **Joined work, live process:** retain the exact token and retry/resolve settlement in the existing operation owner. The sequential `settle_read_owner` implementation in `subtitle_translation/read_attestation.rs:343-390` and the offline `run_source_part` release loop are existing models; do not introduce cleanup spawned lazily from Drop.
- **Ambiguous registration before any input starts:** `ActiveBackgroundJob::start` propagates a registration error with `await?`. The insert may already have committed, leaving no returned receipt and no cleanup owner. Close/fence this exact attempt, resolve its immutable owner key, and settle the no-reader receipt. Its late-success/expired-deadline branch also discards release errors. Analysis registration has the same error propagation shape. Do not resolve or delete a successor's fence.
- **Restart after physical settlement but before deletion acknowledgement:** the foreground `run_owner` retries in memory, and some background paths now retry in memory, but neither an in-memory `release_pending` bit nor a retained future survives daemon restart. Persist a source/attempt/boot-bound physical-settlement or no-reader proof through an existing durable operation/receipt facility before forgetting the completed owner, and replay its exact deletion after restart. Unknown surviving readers must remain charged. Job lease expiry, a changed daemon UUID, or a generic terminal job state is not physical-reap proof.

**Trigger:** finish a real source job, fail the source DELETE RPC before commit, and let `finish` return; a later job on that domain remains charged against a reader that no longer exists. Separately commit registration but lose its acknowledgement, and separately restart while an acknowledged physical join is waiting to settle its receipt.

**Proposed regressions:** `background_jobs::tests::joined_source_settlement_retries_without_losing_exact_receipt`; `background_jobs::tests::committed_registration_with_lost_ack_settles_without_starting_input`; a durable restart test that replays only a persisted settled receipt while retaining an otherwise identical still-unknown/D-state receipt. Exercise the analysis path as well.

## 2. P1 — Track the pause revision inside each physical group driver

**Location:** `crates/plurx-core/src/process/bounded/pause.rs:273-303`.

`ChildPauseGuard::run_group` only sends STOP, verifies the group, and acknowledges the controller while its local `paused_since` is `None`. It does not associate that local state with `SourcePauseObservation.revision`. `resume` resets every child's acknowledged flag; the next `request_pause` advances the revision. Tokio watch updates can coalesce, so a child driver can miss the intermediate Running state.

**Trigger:** establish aggregate pause revision 1; reacquire the slot and call `resume(1)`; before one child driver polls, another participant/source-pressure update requests pause revision 2. The delayed child sees Stopping for revision 2 with `paused_since=Some` from revision 1. It skips verification and acknowledgement forever. Aggregate quiescence never becomes Paused, resume cannot complete, and this child's active timeout remains disabled until the much larger wall-age limit. The indexer plus companion scanner make multiple participants a real production configuration; even one participant must not depend on observing every intermediate watch value.

**Fix:** bind local stop/paused clock state to the controller revision and handle a newer pause even if the old group never observed CONT. Revalidate/acknowledge the new incarnation, and account only verified aggregate pause intervals. Keep the resume-CAS-before-CONT rule.

**Proposed regression:** `process::bounded::pause::tests::coalesced_resume_and_repause_reacknowledges_new_revision`. Hold a child driver's polling behind a barrier, perform resume followed by a new pause, then release it. Assert the new revision reaches Paused, resumes, and does not inherit an indefinitely frozen active clock.

## 3. P1 — Resolve continuous private readers through their actual parent attachment

**Location:** `crates/plurxd/src/vod/driver.rs:239-250`.

`project_source_sharing` treats every active rendition reader key as a key in `shared.sessions`. Continuous media readers are inserted under private `child.reader_id` UUIDs (`vod/serve/create.rs:1690-1721`), while their control snapshot belongs to the parent session. The root's parent reader is authority-only for this graph. Consequently the video/audio child lookup fails, `fresh` becomes false, and no child lead is computed. A FilmV1 owner remains Refill rather than Sharing even when its entire group is stopped and it has ample materialized lead. This can prevent the same-domain background work from progressing throughout otherwise healthy continuous playback.

**Trigger:** create a continuous FilmV1 primary plus shared audio, send fresh accepted parent rendering control, materialize more than the high watermark, and stop the producer. The private reader IDs cannot be found in `sessions`, so the sharing projection still refuses.

**Fix:** use the existing parent-to-child binding, not a free-standing UUID reverse lookup. `Session.children` binds `reader_id` to the actual `Arc<Rendition>`; the session incarnation/lifecycle and its accepted control snapshot own telemetry authority. `vod/serve/control.rs:244-267` already resolves this relation under the parent lock when applying control. Preserve all-reader conjunction and use each child's own timeline/frontier. A replaced parent must not supply telemetry to an old child.

**Proposed regression:** `vod::tests::continuous_private_readers_share_source_from_exact_parent_control`, covering primary, soundtrack, companion, and a same-session-ID replacement whose old child must not inherit fresh control.

## 4. P1 — Keep an unselected companion speculative at the original-source boundary

**Location:** `crates/plurxd/src/vod/generation.rs:169-183,208-216`.
Related paths: `vod/serve/quality.rs:195-201,235-240,786-827`; `vod/serve/create.rs:1712-1715`.

After the first Presented receipt, companion admission reserves CPU with `Priority::Speculative`, but that purpose does not reach the source owner. The companion has no leased background preparation, so `spawn_generation` takes the ordinary foreground `SourceInput::open` branch. FilmV1 also selects Rolling policy at `generation.rs:25-26`, for which bridge reads bypass finite transfer allowance. Thus an unselected, optional encoder can start unrestricted original-media reads while the selected primary is still building its reserve. The foreground ledger does not yield this reader to primary refill pressure. CPU capacity being available does not establish spare disk/NFS bandwidth.

The private reader also inherits the parent's ordinary admission priority and `admit_controlled_video_before` clears `authority_only` without changing it. Preserve speculative purpose through subsequent producer admission decisions as well as the first reservation.

**Trigger:** primary first presentation arrives with a short runway on a source domain whose bandwidth is near the selected stream's consumption rate. There is spare CPU, so the deferred companion starts; its foreground Rolling input then competes with primary/audio reads and can drain that runway. The initial-start deferral merely postpones the same source contention until immediately after first frame.

**Fix:** carry the companion's optional purpose through its actual read owner and all generations. Use a bounded, revocable source-service allowance that yields to selected-reader refill, with exact physical lifetime retained through stop/read join. Upgrade purpose only when the accepted quality owner makes it required. Do not turn optional source work into a generic foreground owner solely because it shares a playback parent.

**Proposed regression:** `vodencode_tests::deferred_companion_yields_original_reads_to_primary_refill`. After durable Presented and successful speculative CPU admission, exhaust the background source allowance or put the primary in Refill. Assert the companion issues no further original reads while the primary can progress; then restore service and verify the same optional owner resumes without changing attachment or family identity.

## 5. P2 — Exclude verified finite-service holds from library probe execution time

**Location:** `crates/plurxd/src/library_work/env.rs:118-130`.

The new library input provider installs `SourceReadBudget` and a private source bridge, but installs neither a `SourcePauseController` nor an owner driver. `SourceInput::open_background_held` consequently captures `pause: None` (`source_input.rs:295-296`), and `process::bounded::output` retains its ordinary wall timeout. `scan/probe.rs` gives the metadata probe 60 seconds and first-frame probe 30 seconds. Legitimate finite byte grants can therefore consume these entire limits while the child is only waiting for source service, rather than performing active parsing. A library probe may be recorded without codec/duration/track data, and first-frame enrichment may be silently skipped, although the source and parser are healthy.

**Trigger:** keep foreground TransferService valid, admit a library scan, and issue small positive shared grants requiring more than 30/60 seconds to deliver the probe's requested input. This is not loss of authority and does not activate the library's foreground-pressure cancellation branch.

**Fix:** connect library probes to their existing owned operation's aggregate stop/read-join controller and driver. Exclude only verified quota holds from the active timeout; store RPC waits, unknown pressure and actual daemon reads must still consume active time. Retain cancellation and a finite wall-age bound.

**Proposed regression:** `scan::probe::input::tests::finite_source_holds_do_not_exhaust_library_probe_active_budget`, using short virtual active budgets and a real provider/owner scope; also assert outstanding daemon read and RPC stalls still time out normally.

## 6. P2 — A known but pending companion is not outside the family

**Location:** `crates/plurxd/src/web/player/continuous-quality.js:705-708`.

V2 deliberately exposes a primary-only family while its known companion builds, and `continuousQualitySelectionCompatible` now accepts that companion from the bootstrap binding. However `choose` performs only one refresh, swallows its error, and immediately returns `outside_family` when the companion is not yet in the returned rows. This also happens on a successful refresh that still returns revision 1; it does not require a network failure. The directed-change owner then shows “Quality change did not complete” for a manual choice, or backs off the Auto target for five minutes (`directed-change.js:252-305`). A normal pending build is being converted to a failed optional target.

**Trigger:** select the bootstrap companion after first presentation but before its init is ready; refresh returns the valid primary-only revision or typed `503 segment_pending`. The companion may become ready within the original directed-change deadline, but the choice has already been failed.

**Fix:** for the exact bootstrap-bound companion, retain the directed change and retry pending family discovery within its existing absolute deadline and supersession/attachment signal. Reserve `outside_family` for a genuinely unrelated candidate. No new session, new overall deadline, or extra replacement owner is needed.

**Proposed regression:** `tests/web/continuous-quality.test.js::known pending companion waits within the original quality deadline`, covering both unchanged revision 1 and typed pending 503, followed by revision 2; verify supersession cancels promptly and actual deadline exhaustion retains the incumbent.

## 7. P2 — A post-conclusion Play rejection still needs a transport failure owner

**Location:** `crates/plurxd/src/web/player/player.js:890-903` (new observation-unavailable Resume path); failure filter at `player.js:607-612` and generic diagnosis at `stall-diagnosis.js:939-943`.

Resume now legitimately calls Play while the native episode is `observation_unavailable`. A current rejection other than NotAllowedError is only recorded when `episode.state==='active'`; consequently this new Resume path logs the rejection but retains no `playRejection`. Its original deadline has already been retired, and `stallDiagnose` returns early in observation_unavailable unless there is a separate element error or server failure. A rejected Play promise need not deliver either. The notice can continue saying “Playback continues” while the requested transport command has failed and the element remains paused.

**Trigger:** reach the observation-unavailable conclusion with valid advancing clock/decode evidence, pause, then resume the same attached source; reject that exact Play invocation with a non-permission error and no separate `video.error`. The existing freshness/intent/source checks accept it as current, but no failure state or user-facing transport diagnosis is produced.

**Fix:** preserve the separate transport-rejection owner after the observation conclusion and let it replace the observational notice. Do not resurrect the old startup timer or award presentation proof. Continue fencing stale/cancelled Play invocations and preserve the special permission path.

**Proposed regression:** `tests/playback/native-hls-startup.test.js::resume play rejection after observation conclusion retains transport failure authority`, plus a retired invocation control that must have no effect.

## Review evidence and limits

The review used the clean frozen diff (206 files), repository instructions, Ripwire navigation, outlines, and targeted current source reads. Scoped knowledge reported no reviewed coverage for the new custody path; the matched existing routing decisions retain source identity and advisory status. Empty coverage was not treated as absence of obligations.

Two narrow mechanical explorations used `gpt-6-sol` at high reasoning: one traced web frame/native transport and continuous-family updates; one traced strict Linux syscall barriers and owned DV inputs. Astra retained the adversarial verdict and checked the concrete findings against current source. The parser pass found no confirmed FD-alias/close/read bypass in its scope; an initial DV early-return concern was rejected because the outer runtime joins input on normal returns.

No test, build, service call, credential access, PR write, or implementation mutation was performed in this review. The reported Mac/Linux compile and lint results belong to the supplied packet, not to independent execution by this reviewer. Linux daemon test-target memory limits, packaged parser compatibility, non-Linux stopped-group qualification, and the <=5-second cold runtime outcome remain explicit qualification limits. They are not presented as code findings or test passes. Run the final requested tests after the findings are addressed, on the resulting exact candidate.
