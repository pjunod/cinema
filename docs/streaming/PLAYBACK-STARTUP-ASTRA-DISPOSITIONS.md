# Playback startup review dispositions — what changed after Astra's request

Companion to the [implementation ledger](PLAYBACK-STARTUP-IMPLEMENTATION-STATUS.md)
and [frozen-candidate implementation review](PLAYBACK-STARTUP-ASTRA-IMPLEMENTATION-REVIEW.md). This
document records source changes and remaining review/qualification evidence for
the seven findings against candidate `2bc1e54e1`. The candidate review requested
changes; no finding is called closed until the final implementation review and
focused execution confirm it.

## Findings 1–4 have implementation handoffs, not qualification

| Finding | Current source disposition | Regression added | Evidence still owed |
|---:|---|---|---|
| 1 · Exact source settlement | Durable no-reader/joined proof, exact settlement replay, and local fsynced join receipts are recorded in the private review packet. | `background_jobs::tests::joined_source_settlement_retries_without_losing_exact_receipt`; `background_jobs_resources::source_tests::committed_registration_with_lost_ack_replays_no_reader_exact_owner`; `analysis_source_join_replays_only_its_fenced_receipt`; `settled_source_replay_requires_exact_no_reader_or_joined_proof`; `source_custody::settlement_journal_tests::joined_receipt_survives_operation_restart_with_exact_identity` | Review the final source and schema/import paths, then execute the focused regressions and Rust compile checks. |
| 2 · Coalesced pause revisions | Child stop and acknowledgement state is revision-bound; a later pause re-verifies even if the intermediate Running watch update was coalesced. | `process::bounded::pause::tests::coalesced_resume_and_repause_reacknowledges_new_revision` | Final review and focused Rust execution. |
| 3 · Private VOD reader authority | Parent-child bindings and rendition identity gate control telemetry; each child uses its own timeline and frontier. | `vod::tests::continuous_private_readers_share_source_from_exact_parent_control` | Final review and focused Rust execution. |
| 4 · Optional companion reads | Optional generations retain speculative purpose and use finite, revocable shared source service; accepted quality promotes the exact owner. | `vod::tests::companion_source_purpose_follows_selected_and_optional_generations`; `vod::tests::controlled_future_loading_retires_work_without_disposing_incumbent_bytes`; `store::background_jobs_resources::source_tests::deferred_companion_yields_original_reads_to_primary_refill` | Final review, focused Rust execution, and source-service qualification. |

## Finding 5 has explicit physical-pause and credit boundaries

Library probes use the existing owned stop/read-join driver. Physical pause
and active-time credit are separate: credit requires verified aggregate stop,
an acknowledged source-slot pause, and a live finite-quota wait. It ends before
resume/validation RPCs and synchronously when the quota wait ends. A pressure
revision fences notifications arriving during RPC waits. Store waits, unknown
pressure and outstanding daemon reads consume active time; the wall bound
remains finite. Final finding verification and focused execution are pending.

The pause/probe handoff names the added controller, quota-guard and owned-input
regressions. They have not run.

## Findings 6–7 have browser source fixes; final review remains

The known bootstrap companion now stays pending under the original quality
deadline; unrelated choices retain the outside-family refusal. A Play rejection
after observation-unavailable retains transport failure authority without
restoring the retired startup timer or presentation credit.

Added regressions:

- `tests/web/continuous-quality.test.js::known pending companion waits within the original quality deadline`
- `tests/playback/native-hls-startup.test.js::resume play rejection after observation conclusion retains transport failure authority`

Both tests are present in source and have **not** been executed. Their findings
remain under final review.

## Qualification state is separate from dispositions

The Linux production/core compile passed on `2bc1e54e1`, the frozen
pre-findings candidate; it does not qualify the changed source. An approved cluster
membership query confirmed an eligible learner; its pinned 18 GiB, four-CPU
compiler slot is prepared. Full Linux daemon test-target compilation,
all focused regressions, final adversarial review, and the main fast lane remain
pending. No behavioral tests have run and the <=5-second startup outcome is
unmeasured.
