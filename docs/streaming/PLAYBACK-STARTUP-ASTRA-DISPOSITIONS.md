# Playback startup review dispositions — what changed after Astra's request

Companion to the [implementation ledger](PLAYBACK-STARTUP-IMPLEMENTATION-STATUS.md)
and [frozen-candidate implementation review](PLAYBACK-STARTUP-ASTRA-IMPLEMENTATION-REVIEW.md). This
document records source changes and remaining qualification evidence for the
seven findings raised against candidate `2bc1e54e1`. Astra's follow-up source
review closed all seven against exact source commit
`bd3bfffba2feacc22ee85dfd323f3e15c6539b6d`; the reviewed-diff binding is in the
private review packet. Source-review closure does not claim behavioral test,
CI, or merge qualification.

## Findings 1–4 have source-review closure; execution remains pending

| Finding | Current source disposition | Regression added | Evidence still owed |
|---:|---|---|---|
| 1 · Exact source settlement | Durable no-reader/joined proof, exact settlement replay, and local fsynced join receipts preserve exact owners through lost acknowledgements and restart. | `committed_activation_with_lost_ack_aborts_only_exact_nonce`; `committed_registration_with_lost_ack_replays_no_reader_exact_owner`; `analysis_source_join_replays_only_its_fenced_receipt`; `settled_source_replay_requires_exact_no_reader_or_joined_proof`; `joined_receipt_survives_operation_restart_with_exact_identity`; `live_activation_intent_is_skipped_while_other_replay_progresses` | Focused regressions and affected Rust compilation. |
| 2 · Coalesced pause revisions | Child stop and acknowledgement state is revision-bound; later pauses revalidate credit despite coalesced Running updates or pressure revisions. | `coalesced_resume_and_repause_reacknowledges_new_revision`; `slot_rpc_waits_spend_active_time`; `verified_pressure_without_finite_quota_still_spends_active_time`; `unrelated_pressure_signal_rechecks_finite_hold_and_charges_rpc_time`; `finite_wait_drop_closes_confirmed_slot_credit_before_driver_poll` | Focused regressions and affected Rust compilation. |
| 3 · Private VOD reader authority | Parent-child bindings and rendition identity gate control telemetry; each child uses its own timeline and frontier. | `continuous_private_readers_share_source_from_exact_parent_control` | Focused regression and affected Rust compilation. |
| 4 · Optional companion reads | Optional generations retain speculative purpose and finite, revocable shared source service. Before init discovery, exact accepted parent selection acquires controlled admission for the candidate-bound companion. Independent `control_required` and `quality_required` facts preserve both owners; family publication and Prepare still wait for init verification. | `companion_source_purpose_follows_selected_and_optional_generations`; `controlled_future_loading_retires_work_without_disposing_incumbent_bytes`; `discovered_companion_upgrades_on_accepted_prepare_then_returns_to_optional`; `accepted_parent_candidate_selection_requires_only_its_companion_before_discovery`; `deferred_companion_yields_original_reads_to_primary_refill`; `companion_read_window_tracks_selected_purpose_at_physical_read`; `directed pending companion waits for its accepted parent selection before discovery` | Focused regressions and source-service qualification. |

## Finding 5 has explicit physical-pause and credit boundaries

Library probes use the existing owned stop/read-join driver. Physical pause
and active-time credit are separate: credit requires verified aggregate stop,
an acknowledged source-slot pause, and a live finite-quota wait. It ends before
resume/validation RPCs and synchronously when the quota wait ends. A pressure
revision fences notifications arriving during RPC waits. Store waits, unknown
pressure and outstanding daemon reads consume active time; the wall bound
remains finite. Astra closed finding 5 by source inspection; focused execution is
pending.

The pause/probe handoff names the added controller, quota-guard and owned-input
regressions. They have not run.

## Findings 6–7 are source-reviewed; browser execution remains pending

The known bootstrap companion now stays pending under the original quality
deadline; unrelated choices retain the outside-family refusal. A Play rejection
after observation-unavailable retains transport failure authority without
restoring the retired startup timer or presentation credit.

Added regressions:

- `tests/web/continuous-quality.test.js::known pending companion waits within the original quality deadline`
- `tests/playback/native-hls-startup.test.js::resume play rejection after observation conclusion retains transport failure authority`

Both tests are present in source and have **not** been executed. Astra's final
source review closed both findings.

## Qualification state is separate from dispositions

The normal pinned Mac commit hook passed on `bd3bfffba2feacc22ee85dfd323f3e15c6539b6d`.
The Linux production/core compile passed on frozen pre-findings `2bc1e54e1`; it
does not qualify the changed source. Current Linux all-target Clippy is running
on the eligible learner with 18 GiB and four CPUs. Full Linux test-target
compilation, C controls, all focused regressions, and the main fast lane remain
pending. The renderer and complete-title C controls now pass on reviewed source hashes; other focused checks remain pending and the <=5-second startup outcome is
unmeasured.
