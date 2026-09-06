# plurx developer tasks. `make` or `make help` lists everything.
# CI enters through `scripts/validate`, which composes the same Make targets a
# developer runs. `make validate` is the ordinary local entry point; `make
# check` remains the portable baseline inside it.

CARGO ?= cargo
CARGO_RAW := $(CARGO)
ANDROID_IMAGE ?= plurx-android-build
ANDROID_PLATFORM ?= linux/amd64
ANDROID_DATA_DIR ?=

.DEFAULT_GOAL := help

.PHONY: help
help: ## List available targets
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
	  | sort \
	  | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

## ---- day to day --------------------------------------------------------

.PHONY: build
build: ## Debug build of the whole workspace
	$(CARGO) build --workspace

.PHONY: run
run: ## Run the server (http://localhost:32400)
	$(CARGO) run -p plurxd

.PHONY: fmt
fmt: ## Auto-format all code
	$(CARGO) fmt --all

# The ordinary lane keeps cheap integration contracts alongside unit tests,
# but excludes the separate-process cluster member. Replicated Store contracts
# and daemon fixtures also require explicit features, so production dependency
# feature unification cannot pull them into this target accidentally.
# `--no-fail-fast` reports every failing target in one run.
.PHONY: unit test test-full
unit: spike-lock-check ## Run the fast Rust unit and SQLite contract lane
	$(CARGO) test --workspace --exclude plurx-cluster-check --no-fail-fast

test: unit ## Run the fast Rust test lane

test-full: ## Run every Rust test, including replicated and daemon contracts
	$(CARGO) test --workspace \
	  --features plurx-core/hiqlite-contract-tests,plurxd/cluster-integration-tests \
	  --no-fail-fast

## ---- baseline gates ----------------------------------------------------

.PHONY: fmt-check
fmt-check: ## Verify formatting without changing files
	$(CARGO) fmt --all --check

# Effort branches optimize for integration feedback rather than release
# evidence. Compile every target so production and test-only code must remain
# type-correct, but leave execution to focused local checks and the final
# effort-to-main qualification run.
.PHONY: effort-rust-check
effort-rust-check: fmt-check spike-lock-check ## Compile every Rust target without running the test suite
	$(CARGO) check --workspace --locked --all-targets

.PHONY: lint
lint: ## Clippy across the workspace, warnings are errors
	$(CARGO) clippy --workspace --all-targets -- -D warnings

.PHONY: rust-check
rust-check: fmt-check lint test ## Rust format, lint, and workspace tests

# The fast CI Rust lane owns formatting, Clippy, unit tests, and SQLite
# contracts. Cluster jobs own WAL, replicated Store, topology, and daemon
# contracts. Explicit test features keep those processes out of this lane.
.PHONY: ci-rust-gate
ci-rust-gate: fmt-check spike-lock-check lint ## CI Rust gate: format, Clippy, and fast workspace tests
	$(CARGO) test --workspace --locked --exclude plurx-cluster-check --no-fail-fast

# The real mount-namespace exercises for scratch aliasing and mount points
# inside scratch. They call `mount --bind`, so they need CAP_SYS_ADMIN and are
# not part of `check`: a developer without privileges would get a failing
# baseline for a machine capability rather than for the code. Both tests return
# immediately unless PLURX_RUN_BIND_MOUNT_TEST is set, so they are inert
# everywhere else — which is exactly why they need a target that sets it. Run
# this on a privileged Linux host before shipping a change to fs_secure.
.PHONY: bind-mount-check
bind-mount-check: ## Privileged Linux: run the opt-in mount-namespace scratch tests
	@test "$$(uname -s)" = Linux || { echo >&2 "bind-mount-check requires Linux"; exit 1; }
	@test "$$(id -u)" -eq 0 || { echo >&2 "bind-mount-check requires root and CAP_SYS_ADMIN"; exit 1; }
	PLURX_RUN_BIND_MOUNT_TEST=1 $(CARGO) test --locked -p plurx-core \
	  fs_secure::tests:: -- --nocapture

.PHONY: storage-pressure-check
storage-pressure-check: ## Root Linux: prove a real ENOSPC cache/scratch device preserves authority
	@test "$$(uname -s)" = Linux || { echo >&2 "storage-pressure-check requires Linux"; exit 1; }
	@test "$$(id -u)" -eq 0 || { echo >&2 "storage-pressure-check requires root and CAP_SYS_ADMIN"; exit 1; }
	PLURX_RUN_STORAGE_PRESSURE_TEST=1 $(CARGO) test --locked -p plurxd \
	  a_full_cache_or_scratch_device_never_relocates_authoritative_bytes -- --nocapture

.PHONY: history-check
history-check: ## Verify every corrective commit has current regression evidence
	@scripts/history-audit --report target/validation/history.json

.PHONY: operations-check
operations-check: ## Verify deploy, CI, container, and client shipping contracts
	@python3 -m unittest discover -s tests/operations -p 'test_*.py'

.PHONY: check
check: validation-lint history-check operations-check benchmark-check rust-check ## History + operations + catalog + benchmark + Rust baseline

.PHONY: spike-lock-check
spike-lock-check: ## Prove the isolated spike's lockfile still resolves
	@# `spikes/hiqlite-m0` is a SEPARATE workspace that path-depends on
	@# plurx-core, so adding a dependency to plurx-core strands its lockfile —
	@# and nothing else in this Makefile touches that workspace, so the first
	@# thing that notices is `cargo clippy --locked` twenty minutes into CI.
	@#
	@# `cargo metadata --locked` resolves without compiling anything: under a
	@# second, and it fails with the same "cannot update the lock file"
	@# message the CI job does. The fix when it fires is one command:
	@#   cargo update --manifest-path spikes/hiqlite-m0/Cargo.toml --workspace
	@$(CARGO) metadata --locked --manifest-path spikes/hiqlite-m0/Cargo.toml \
	  --format-version 1 >/dev/null

.PHONY: hiqlite-spike
hiqlite-spike: ## Run the isolated M0 raft/SQLite semantic proof
	$(CARGO) test --locked --manifest-path spikes/hiqlite-m0/Cargo.toml \
	  --test hiqlite_m0 -- --nocapture

.PHONY: hiqlite-vendor-clippy
hiqlite-vendor-clippy: ## Deny warnings in the vendored production snapshot transport
	$(CARGO) clippy --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --lib --no-default-features --features auto-heal,cache,macros,sqlite \
	  -- -D warnings

.PHONY: hiqlite-baseline
hiqlite-baseline: ## Measure the manual M0 one-voter cost gate on a quiet host
	$(CARGO) test --release --manifest-path spikes/hiqlite-m0/Cargo.toml \
	  --test hiqlite_m0 \
	  single_voter_cost_stays_inside_the_m0_budget -- --ignored --exact --nocapture

.PHONY: cluster-wal-check
cluster-wal-check: override CARGO := scripts/require-test-count $(CARGO_RAW)
cluster-wal-check: ## Run exact Hiqlite and WAL recovery regressions
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::frame_io::tests::raw_write_frame_stays_idle_after_transport_recovers_until_an_explicit_flush \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::frame_io::tests::real_tls_tail_backpressure_completes_for_every_payload_direction \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::frame_io::tests::real_tls_no_backpressure_control_completes_without_an_extra_frame \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::frame_io::tests::frame_completion_flushes_the_underlying_transport \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::frame_io::tests::flush_failure_is_a_failed_frame_write \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::frame_io::tests::permanently_blocked_flush_expires_the_combined_budget \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::frame_io::tests::close_frame_uses_the_short_fixed_budget \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::production_raft_request_writer_flushes_a_snapshot_chunk_through_tls \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::production_raft_writer_reports_flush_failure_to_its_supervisor \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_server::tests::production_raft_response_writer_flushes_serialized_response_through_tls \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_server::tests::production_raft_response_writer_reports_flush_failure_and_closes \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_server::tests::production_prepares_snapshot_status_before_biased_socket_close_drops_unpolled_work \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_server::tests::long_lived_socket_derives_a_fresh_admission_deadline_per_request \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_server::tests::first_snapshot_identity_reports_terminal_status_when_reader_eof_wins \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_server::tests::changed_snapshot_identity_reports_terminal_status_when_reader_eof_wins \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_server::tests::queued_snapshot_status_is_prepared_before_biased_reader_eof \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_server::tests::inbound_snapshot_status_guard_preserves_worker_owned_and_completed_results \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::production_api_request_writer_flushes_serialized_request_through_tls \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::production_api_writer_reports_flush_failure_to_its_supervisor \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::queued_leader_change_wins_before_queued_api_request \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::queued_api_response_wins_before_queued_leader_change \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::queued_api_response_wins_before_leader_during_writer_backpressure \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::leader_handoff_settles_decoded_response_before_failing_unresolved_requests \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::dedicated_leader_control_bypasses_application_backlog_during_writer_backpressure \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::proxy_handoff_settles_decoded_response_before_failing_unresolved_requests \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::dropped_caller_still_allows_its_proxy_refusal_to_claim_handoff \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::concurrent_proxy_refusals_coalesce_into_one_endpoint_advance \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::teardown_drained_proxy_refusal_claims_the_socket_handoff \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::teardown_drained_non_proxy_refusal_preserves_terminal_ambiguity \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::api::tests::production_api_response_writer_flushes_serialized_response_through_tls \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::api::tests::production_api_response_writer_reports_flush_failure_and_closes \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features server \
	  server::proxy::stream::tests::production_proxy_response_writer_flushes_serialized_response_through_tls \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::production_raft_reader_reports_malformed_frames \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::raft_transport_debug_logging_exposes_only_bounded_metadata \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::production_api_reader_reports_malformed_frames \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::handshake::tests::silent_peer_cannot_hold_the_server_handshake_open \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::snapshot_executor::tests::executor_runs_one_queues_one_and_drops_abandoned_queued_work \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::snapshot_executor::tests::shutdown_reports_running_work_without_cancelling_it \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::snapshot_executor::tests::cancelled_shutdown_wait_retains_the_executor_handle \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::snapshot_executor::tests::shutdown_before_submission_is_latched_and_admits_no_work \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::snapshot_executor::tests::admission_deadline_never_executes_the_timed_out_job \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::snapshot_executor::tests::accepted_partial_write_finishes_before_same_offset_retry \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::snapshot_executor::tests::production_snapshot_executor_worker_records_status_around_injected_installer \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::snapshot_executor::tests::production_snapshot_executor_fifo_status_ignores_later_admission_waiter \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::snapshot_executor::tests::production_snapshot_executor_ticket_order_survives_reverse_waiter_polling \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::snapshot_executor::tests::production_snapshot_status_follows_worker_start_after_pre_ticket_deschedule \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::snapshot_executor::tests::admission_waiter_budget_bounds_cancelled_ticket_retention_and_reports_busy \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::snapshot_executor::tests::production_shared_executor_retains_abandoned_other_peer_terminal_status \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::snapshot_executor::tests::inbound_result_disposition_distinguishes_mismatch_higher_vote_and_fatal \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::handler_coordinator_consumes_retained_reset_when_request_queue_is_full \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::replacement_socket_drops_cancelled_request_after_consuming_reset \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::live_request_on_stale_socket_requires_reconnect \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::reset_interrupts_write_enqueue_under_backpressure \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::latched_reset_prevents_write_queue_ownership_transfer \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::latched_reader_failure_prevents_write_queue_ownership_transfer \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::shutdown_interrupts_write_enqueue_under_backpressure \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::handler_coordinator_observes_writer_failure_while_reader_is_pending \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::handler_coordinator_treats_writer_panic_as_terminal \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::handler_coordinator_observes_reader_failure_while_writer_is_pending \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::handler_coordinator_treats_reader_panic_as_terminal \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::forced_reset_cleanup_does_not_wait_for_full_writer_queue \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::dropping_connection_off_runtime_keeps_cleanup_owned \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::repeated_connection_failures_return_supervised_tasks_to_baseline \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::production_connection_supervisor_does_not_invent_snapshot_work \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::production_natural_reconnect_advances_physical_socket_epoch_without_reset \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::production_snapshot_catch_up_preserves_physical_socket_epoch_during_interleaving \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::two_client_churn_cannot_overwrite_the_snapshot_connection_owner \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::sqlite_install_snapshot_preserves_mismatch_for_offset_reset \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::cache_install_snapshot_preserves_mismatch_for_offset_reset \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::snapshot_chunk_deadlines_advance_without_renewing_the_transfer_window \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::non_final_snapshot_rpc_uses_hard_ttl_when_it_is_shorter_than_chunk_budget \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::final_install_deadline_latches_once_and_mismatch_restores_transfer_deadline \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::final_install_respects_the_first_rpc_hard_cap_and_transfer_expiry \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::mismatch_cannot_reenter_final_install_after_transfer_expiry \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::watchable_snapshot_deadline_switches_to_the_active_phase \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::simultaneous_final_phase_update_wins_over_stale_transfer_timer \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::production_final_mismatch_restores_transfer_deadline_before_reread \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::production_final_transport_error_retains_final_install_deadline_in_status \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::production_higher_vote_response_never_advances_snapshot_acknowledgement \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::sqlite_full_snapshot_enters_bounded_wrapper \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::cache_full_snapshot_enters_bounded_wrapper \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::advancing_transfer_may_exceed_one_chunk_window_and_finish_before_transfer_expiry \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::unanswered_non_final_rpc_expires_at_chunk_budget_and_resets_socket \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::repeated_mismatch_style_resets_expire_at_the_original_transfer_deadline \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::final_install_may_exceed_chunk_window_but_cannot_renew_install_window \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::stale_snapshot_guard_cannot_clear_a_newer_attempt \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::caller_cancellation_drops_the_active_snapshot_rpc_guard_immediately \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::aborting_snapshot_owner_publishes_attempt_guarded_terminal_status \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::outbound_snapshot_attempt_bounds_identity_before_supervisor_logging \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::outbound_snapshot_identity_is_bounded_before_retention \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::inbound_snapshot_identity_is_bounded_before_retention \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::outbound_attempted_offset_never_rolls_back_within_one_snapshot_attempt \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::bounded_inbound_display_id_cannot_alias_semantic_snapshot_identity \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::snapshot_fingerprint_correlates_raw_identity_across_directions_without_display_aliasing \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::snapshot_fingerprint_is_optional_for_rolling_status_deserialization \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::local_status_distinguishes_receive_ack_install_retry_and_completion \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::receiver_bytes_are_not_reported_as_sender_acknowledgements \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::inbound_progress_is_monotonic_and_new_identity_clears_attempt_state \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::same_peer_inbound_install_and_outbound_transfer_do_not_collide \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::terminal_wrapper_preserves_the_actionable_failure_category \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::specific_terminal_failure_replaces_prior_transient_category \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::outbound_live_socket_counts_the_first_reconnect \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::production_supervisor_start_before_snapshot_attempt_counts_first_reconnect \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::stale_inbound_completion_cannot_mutate_newer_snapshot_identity \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::abandoned_inbound_admission_does_not_displace_worker_result \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::terminal_receipt_before_earlier_worker_start_keeps_attempt_ids_monotonic \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::inbound_socket_epoch_and_reconnect_count_are_attempt_local \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::configured_inbound_inter_chunk_deadline_uses_minimum_and_maximum \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::configured_peer_capacity_covers_both_groups_and_directions \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::live_membership_add_remove_churn_rekeys_capacity_and_retired_allowance \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::active_observation_remains_visible_through_deadline_then_expires \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::late_first_snapshot_cannot_resurrect_work_expired_after_its_deadline \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::late_first_snapshot_cannot_resurrect_work_without_a_deadline \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::deadline_less_retry_serializes_its_effective_fallback_stall_boundary \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::live_install_longer_than_five_minutes_remains_visible_through_its_deadline \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::configured_chunk_deadline_controls_awaiting_ack_stall_projection \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::completed_observation_never_projects_as_stalled_and_eventually_expires \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::valid_install_does_not_stall_at_the_chunk_window \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  transport_status::tests::retired_peers_are_bounded \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::management::transport_route_tests::production_transport_route_enforces_auth_and_returns_memory_only_json \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::mgmt::tests::production_transport_client_uses_exact_authenticated_route_and_accepts_404 \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::mgmt::tests::snapshot_transport_peer_tracks_current_raft_membership_for_new_learner \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::mgmt::tests::production_transport_client_rejects_oversized_unframed_response_body \
	  --lib -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::peer_fanout_applies_the_one_second_per_peer_deadline \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::peer_fanout_never_exceeds_the_eight_peer_bound \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::production_collector_labels_directory_overflow_without_probing_it \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::peer_status_cache_is_fresh_for_five_seconds_then_expires \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::full_refresh_cycle_keeps_cache_fresh_and_ages_transport_from_local_cache_time \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::remote_status_freshness_uses_local_monotonic_age_despite_clock_skew \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::stale_peer_sample_uses_local_elapsed_time_and_cannot_become_healthy \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::absent_and_expired_cache_are_unavailable_never_peer_limited \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::membership_status_cache_is_fresh_for_five_seconds_then_expires \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::membership_projection_refresh_is_bounded_and_preserves_last_good_sample \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::long_install_crosses_to_stalled_then_expires_after_the_post_deadline_window \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::cached_fresh_active_transport_stalls_exactly_when_deadline_reaches_zero \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::cached_deadline_less_active_transport_uses_the_producer_fallback_boundary \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::cached_terminal_transport_observation_expires_at_five_minutes \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::transport_projection_uses_local_monotonic_age_despite_remote_clock_skew \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::peer_status_refresh_keeps_strict_cycle_overhead_margin \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::aggregate_request_reads_only_node_owned_projections \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::unclustered_membership_cache_loop_exits_without_refresh_or_retry \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::mutation_preflight_refreshes_roster_directory_and_bounded_peer_observations \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::mutation_preflight_bounds_the_current_membership_read \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::mutation_preflight_ages_local_transport_across_peer_collection \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::planned_outage_lease_blocks_membership_mutation_during_fresh_preflight \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::failed_fresh_preflight_releases_its_planned_outage_lease \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::aborted_fresh_preflight_releases_the_exact_planned_outage_claim \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::cancelled_acquisition_waiter_releases_only_after_ambiguous_outcome_is_known \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::abort_during_drain_cancels_the_guard_owned_local_fence \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  serving_fence::tests::unresolved_exact_release_keeps_admissions_fenced_past_the_lease_deadline \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  serving_fence::tests::restart_cancellation_cannot_clear_a_maintenance_owned_fence \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  serving_fence::tests::delayed_same_owner_cleanup_cannot_clear_a_successor_generation \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  serving_fence::tests::overlapping_cleanup_tokens_are_resolved_independently \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  serving_fence::tests::confirmed_release_resolves_latch_but_preserves_timed_fence \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  serving_fence::tests::confirmed_current_restart_cancel_resolves_a_preexisting_exact_latch \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  serving_fence::tests::confirmed_current_restart_cancel_resolves_an_expired_exact_latch \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  serving_fence::tests::confirmed_maintenance_exit_resolves_an_expired_exact_latch \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  serving_fence::tests::confirmed_maintenance_exit_preserves_a_restart_owned_latch \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  serving_fence::tests::stuck_restart_admission_cannot_outlive_the_exact_fence \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::restart_preparation_claims_and_releases_the_replicated_outage_slot \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::cancelled_maintenance_waiter_does_not_cancel_the_owned_commit \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::cancelled_waiter_keeps_the_serialized_operation_gate_with_its_owner \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::cleanup_retry_backoff_is_capped_and_shutdown_interruptible \
	  -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store --lib \
	  cluster::membership::tests::planned_outage_lease_excludes_a_concurrent_production_removal_attempt \
	  -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::released_planned_outage_claim_cannot_be_resurrected_by_a_delayed_write \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::maintenance_and_exact_release_are_safe_in_both_commit_orders \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::ambiguous_operation_acquire_retries_only_while_its_owned_claim_is_live \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::previous_release_lifecycle_writes_cannot_cross_an_outage_lease \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store --lib \
	  cluster::membership::tests::cache_revocation_capability_covers_the_exact_committed_roster_and_rollback \
	  -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::membership_cannot_commit_after_final_roster_read_before_credential_store_write \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::definitive_cache_admin_acquire_release_cycles_leave_no_receipts \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::definitive_cache_admin_singleton_losers_leave_no_receipts \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::zero_after_ambiguous_cache_admin_acquire_advances_watermark \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::ambiguous_cache_admin_acquire_advances_watermark_and_cannot_resurrect \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::repeated_ambiguous_cache_admin_cleanup_keeps_one_bounded_watermark \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::server_commit_response_loss_and_crash_expire_cache_admin_exclusion_conservatively \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::cache_admin_exclusion_is_separate_rolling_safe_and_capability_v3 \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::active_cache_revocation_exclusion_blocks_readiness_without_wall_clock_expiry \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::cache_revocation_capability_keeps_a_joiner_closed_until_self_is_committed \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::rollback_heartbeat_cannot_republish_retired_cache_revocation_capabilities \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::rollback_credential_mutation_is_rejected_before_readiness_refresh \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::three_voter_rolling_upgrade_activates_credential_guard_only_after_full_roster \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::activated_guard_with_live_mutation_lease_is_not_an_activation_candidate \
	  --lib -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::cache_only_admin_proofs_expire_without_sliding_and_refuse_non_admins \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::cache_only_admin_proofs_honor_digest_and_user_invalidation \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::active_revocation_blocks_ticket_publication_until_guard_drop \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::commit_ambiguous_local_mutation_retains_one_bounded_fail_closed_fence \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::replicated_capability_loss_clears_proofs_and_invalidates_in_flight_publication \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::replicated_exclusion_projection_outlives_remote_ttl_and_clock_skew \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::cache_admin_revocation_operation_gate_fails_fast_and_is_raii_released \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::capability_refresh_error_and_rollback_clear_cache_only_admin_authority \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::automatic_activation_runs_outside_the_membership_projection \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::full_roster_projection_activates_without_a_credential_mutation \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::stuck_automatic_activation_does_not_block_membership_refresh_or_shutdown \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  startup_tests::console_password_reset_fails_closed_before_any_store_mutation \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::remote_revocation_fence_expires_bounded_and_reinvalidates \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::remote_revocation_fences_have_a_hard_capacity \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::poisoned_proof_cache_fails_closed_for_authentication_and_revocation \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::internal_auth_revocation::tests::digest_and_user_revocation_on_a_invalidate_primed_b_before_success \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::internal_auth_revocation::tests::credential_revocation_uses_the_exact_committed_security_roster \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::internal_auth_revocation::tests::credential_revocation_accepts_more_than_the_diagnostics_probe_limit \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::internal_auth_revocation::tests::membership_added_between_begin_passes_is_fenced_before_store_admission \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::internal_auth_revocation::tests::replicated_membership_exclusion_spans_final_roster_read_and_peer_end \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::internal_auth_revocation::tests::local_apply_ack_wire_version_rejects_pre_barrier_receivers \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::internal_auth_revocation::tests::begin_ack_installs_memory_fence_before_waiting_for_exact_local_apply \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::internal_auth_revocation::tests::cancelled_local_apply_wait_leaves_peer_memory_fence_closed \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::internal_auth_revocation::tests::origin_waits_for_exact_claim_apply_before_observing_added_member \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::internal_auth_revocation::tests::automatic_activation_checks_the_permanent_marker_before_taking_the_gate \
	  -- --exact
	$(CARGO) test --locked -p plurx-core --lib \
	  store::sqlite::users::tests::password_and_session_revocation_roll_back_together_on_delete_failure \
	  -- --exact
	$(CARGO) test --locked -p plurx-core --lib \
	  store::sqlite::users::tests::failed_combined_promotion_never_authorizes_the_old_session \
	  -- --exact
	$(CARGO) test --locked -p plurx-core \
	  --features cluster-read-cost-validation,hiqlite-contract-tests \
	  --test store_contract \
	  clustered_promotion_requires_the_exact_claim_and_rolls_back_on_token_failure \
	  -- --exact --test-threads=1
	$(CARGO) test --locked -p plurx-core \
	  --features cluster-read-cost-validation,hiqlite-contract-tests \
	  --test store_contract \
	  login_token_insert_requires_the_verified_password_version \
	  -- --exact --test-threads=1
	$(CARGO) test --locked -p plurx-core --lib \
	  store::sqlite::users::tests::concurrent_admin_demote_and_delete_preserve_one_administrator \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::cache_only_admin_proofs_have_a_hard_capacity \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::logout_revocation_generation_rejects_an_in_flight_store_result \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::user_revocation_generation_rejects_all_in_flight_store_results \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::extract::tests::authentication_and_revocation_callers_bracket_store_work_with_generations \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::production_collector_preserves_private_transport_when_public_listener_is_closed \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::peer_transport_age_retains_request_to_receipt_uncertainty \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::slow_public_probe_cannot_discard_completed_private_transport \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::identity_mismatch_discards_peer_local_state \
	  -- --exact
	$(CARGO) test --locked -p plurxd --bin plurxd \
	  http::cluster_operations::tests::public_transport_fallback_requires_matching_observer_identity \
	  -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  config::tests::snapshot_deadline_durations_are_bounded_before_instant_arithmetic \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core \
	  config::tests::snapshot_install_timeout_is_bounded_and_env_values_are_parsed \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core \
	  config::tests::snapshot_chunk_and_transfer_timeouts_validate_bounds_and_relationship \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core \
	  config::tests::snapshot_budget_env_overrides_are_parsed_and_empty_values_do_not_override \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::operations_peer_directory_preserves_identities_beyond_the_probe_limit \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::operations_peer_query_materializes_only_the_committed_roster \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::cache_admin_revocation_roster_includes_pending_removals_and_fails_on_omission \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::status_protocol_query_materializes_only_the_committed_roster \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::membership::tests::committed_roster_bound_fails_closed_instead_of_truncating \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::migration::tests::production_timing_admits_recovery_after_upgrade_and_clean_rolling_restarts \
	  --lib -- --exact
	$(CARGO) test --locked -p plurx-core --features hiqlite-store \
	  cluster::migration::startup_wait_logging_tests::immediate_watermark_errors_cannot_starve_due_startup_log \
	  --lib -- --exact
	@OPENRAFT_MANIFEST="$$( $(CARGO) metadata --locked \
	  --manifest-path vendor/hiqlite/Cargo.toml --format-version 1 \
	  | python3 -c 'import json, sys; data = json.load(sys.stdin); print(next(p["manifest_path"] for p in data["packages"] if p["name"] == "openraft" and p["version"] == "0.9.25"))' )"; \
	  CARGO_TARGET_DIR="$(CURDIR)/target/openraft-regression" $(CARGO) test --locked \
	  --manifest-path "$$OPENRAFT_MANIFEST" --features generic-snapshot-data \
	  network::snapshot_transport::tests::test_chunked_reset_offset_if_snapshot_id_mismatch \
	  --lib -- --exact
	PLURX_EXPECT_TEST_COUNT=10 $(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,macros,sqlite \
	  snapshot_metrics --lib -- --test-threads=1
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,macros,sqlite \
	  client::helpers::tests::configured_leader_probes_do_not_wait_for_the_first_peer \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  client::stream::tests::proxy_failover_cycles_only_through_configured_endpoints \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,listen_notify,macros,sqlite \
	  client::mgmt::tests::remote_shutdown_joins_streams_with_every_endpoint_unavailable \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,macros,sqlite \
	  http_client::tests::management_client_does_not_forward_api_secret_across_redirects \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite-wal/Cargo.toml \
	  metadata::tests::interrupted_metadata_replacement_keeps_the_previous_record_readable \
	  -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite-wal/Cargo.toml \
	  writer::tests::single_file_snapshot_tail_restores_its_missing_purge_boundary \
	  -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite-wal/Cargo.toml \
	  wal::tests::full_purge_replaces_stale_mmap_and_memo_across_reused_wal_numbers \
	  -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite-wal/Cargo.toml \
	  reader::tests::failed_claimed_range_does_not_poison_the_next_reader_action \
	  -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite-wal/Cargo.toml \
	  reader::tests::log_responses_apply_capacity_one_backpressure_before_the_terminal_result \
	  -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite-wal/Cargo.toml \
	  writer::tests::full_purge_waits_for_an_unmapped_reader_before_reusing_the_wal_path \
	  -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite-wal/Cargo.toml \
	  wal::tests::duplicate_outer_log_id_is_rejected_even_when_the_record_count_matches \
	  -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite-wal/Cargo.toml \
	  wal::tests::suffix_rewrite_renews_the_incarnation_before_memo_reuse \
	  -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite-wal/Cargo.toml \
	  reader::tests::range_spanning_the_retained_floor_streams_only_retained_records \
	  -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite-wal/Cargo.toml \
	  reader::tests::internal_retained_gap_is_a_terminal_error_after_any_prior_records \
	  -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,macros,sqlite,validation-test-helpers \
	  store::state_machine::sqlite::state_machine::snapshot_metrics_contracts::validation_apply_resume_cannot_miss_the_registered_waiter \
	  --lib -- --exact

.PHONY: cluster-store-check
cluster-store-check: ## Run the Store contracts against SQLite and three voters
	$(CARGO) test --locked -p plurx-core \
	  --features cluster-read-cost-validation,hiqlite-contract-tests \
	  --test store_contract -- --test-threads=1

.PHONY: cluster-harness-check
cluster-harness-check: ## Run replicated growth and topology harness contracts
	$(CARGO) test --locked -p plurx-cluster-check \
	  --test harness compacted_growth_gate -- --nocapture
	$(CARGO) test --locked -p plurx-cluster-check \
	  topology::tests::topology_artifact -- --nocapture
	$(CARGO) test --locked -p plurx-cluster-check \
	  failure_drills::tests --lib -- --nocapture
	$(CARGO) test --locked -p plurx-cluster-check \
	  named_runner::tests --lib -- --nocapture
	$(CARGO) test --locked -p plurx-cluster-check \
	  storage_evidence::retained_p5_privileged_storage_evidence_matches_the_closed_schema \
	  --lib -- --exact
	$(CARGO) run --locked -p plurx-cluster-check -- check
	$(CARGO) run --locked -p plurx-cluster-check -- \
	  topology target/validation/cluster-topology-semantic.json 3,4

.PHONY: cluster-daemon-check
cluster-daemon-check: ## Run real-daemon activation and activity contracts
	$(CARGO) test --locked -p plurxd --features cluster-integration-tests \
	  --test cluster_activation -- --nocapture
	$(CARGO) test --locked -p plurxd --features cluster-integration-tests \
	  --test cluster_activity -- --nocapture

.PHONY: cluster-check
cluster-check: cluster-wal-check cluster-store-check cluster-harness-check cluster-daemon-check ## Run every replicated recovery and failure contract

.PHONY: cluster-campaign-validate
cluster-campaign-validate: ## Validate P0c campaign (set CAMPAIGN=.../campaign.json)
	test -n "$(CAMPAIGN)"
	$(CARGO) run --locked -p plurx-cluster-check -- \
	  topology-campaign-validate "$(CAMPAIGN)"

.PHONY: cluster-instrumentation-validate
cluster-instrumentation-validate: ## Validate P2f campaign (set CAMPAIGN=.../instrumentation-campaign.json)
	test -n "$(CAMPAIGN)"
	$(CARGO) run --locked -p plurx-cluster-check -- \
	  instrumentation-campaign-validate "$(CAMPAIGN)"

.PHONY: cluster-growth
cluster-growth: ## Measure and gate post-coalescer one-voter compacted growth
	$(CARGO) run --locked -p plurx-cluster-check -- growth

## ---- functionality-point validation -----------------------------------

# `make check` remains the mandatory baseline. The validator sits above it:
# the catalog maps changed paths to named behavior contracts and adds the
# surface-specific checks that ordinary Rust compilation cannot see.
.PHONY: validate-help
validate-help: ## Explain the validation workflow and UI golden commands
	@printf '%s\n' \
	  'Functionality-point validation maps changed files to user-visible promises.' \
	  '' \
	  '  make validate-plan    Explain what the staged change selects; run nothing' \
	  '  make validate-staged  Validate the staged change (the normal local loop)' \
	  '  make validate         Validate every point with the commit profile' \
	  '  make validate-full    Add browser, client, and packaging checks' \
	  '  make validate-nightly Exhaustive playback, recovery, bounds, and packaging' \
	  '  make validation-lint  Check the catalog and path ownership only' \
	  '  make history-check    Map every corrective commit to current evidence' \
	  '  make operations-check Pin deploy, CI, container, and ship contracts' \
	  '' \
	  'UI structure uses a reviewed answer key, tests/ui-structure.golden:' \
	  '  make ui-check         Compare structure and enforce accessibility invariants' \
	  '  make ui-golden        Rewrite the answer after an intentional UI change' \
	  '' \
	  'Details: docs/VALIDATION.md'

.PHONY: validation-lint
validation-lint: ## Verify every governed file maps to a valid functionality point
	@scripts/validate lint

.PHONY: validate-plan
validate-plan: ## Explain which points and checks the staged diff selects
	@scripts/validate plan --profile commit --staged

.PHONY: validate
validate: ## Run the commit-profile checks for every functionality point
	@scripts/validate run --profile commit --all

.PHONY: validate-staged
validate-staged: ## Run the commit-profile checks selected by the staged diff
	@scripts/validate run --profile commit --staged

.PHONY: validate-full
validate-full: ## Run extended browser, client, and packaging validations
	@scripts/validate run --profile full --all

.PHONY: validate-nightly
validate-nightly: ## Run exhaustive playback, recovery, bounds, clients, UI, and packaging
	@scripts/validate run --profile nightly --all --strict

.PHONY: coverage
coverage: ## Line coverage (installs cargo-llvm-cov on first run); writes lcov.info
	@$(CARGO) llvm-cov --version >/dev/null 2>&1 || $(CARGO) install cargo-llvm-cov
	$(CARGO) llvm-cov --workspace --lcov --output-path lcov.info
	@$(CARGO) llvm-cov --workspace --summary-only

## ---- playback lab ------------------------------------------------------

.PHONY: playback-doctor
playback-doctor: ## Check playback-lab codecs, filters, browser, and server binary
	@scripts/playback-lab doctor

.PHONY: playback-fixtures
playback-fixtures: ## Build + ffprobe the deterministic playback corpus
	@scripts/playback-lab fixtures

.PHONY: playback-smoke
playback-smoke: ## Run the risk-weighted end-to-end playback matrix in Chrome
	@scripts/playback-lab run --suite smoke

.PHONY: playback-smoke-safari
playback-smoke-safari: ## Run the playback smoke matrix in Safari (macOS)
	@scripts/playback-lab run --suite smoke --browser safari

.PHONY: playback-smoke-edge
playback-smoke-edge: ## Run the playback smoke matrix in Microsoft Edge
	@scripts/playback-lab run --suite smoke --browser edge

.PHONY: playback-smoke-firefox
playback-smoke-firefox: ## Run the playback smoke matrix in Firefox (needs geckodriver)
	@scripts/playback-lab run --suite smoke --browser firefox

.PHONY: playback-full
playback-full: ## Run every fixture x quality plus playback restart cases
	@scripts/playback-lab run --suite full

## ---- Cinema vs Plex benchmark ------------------------------------------

BENCHMARK_CONFIG ?= benchmarks/cinema-plex.example.toml

.PHONY: benchmark-check
benchmark-check: ## Test benchmark config, runners, raw schema, percentiles, and ratios
	@python3 -m unittest discover -s tests/benchmark -p 'test_*.py'
	@scripts/cinema-plex-bench validate --config $(BENCHMARK_CONFIG) --require-v1 >/dev/null

.PHONY: benchmark-plan
benchmark-plan: ## Expand the selected A/B corpus and scenario plan without contacting servers
	@scripts/cinema-plex-bench validate --config $(BENCHMARK_CONFIG) --require-v1

.PHONY: benchmark-run
benchmark-run: ## Run the selected real Cinema/Plex A/B config (tokens come from its env names)
	@scripts/cinema-plex-bench run --config $(BENCHMARK_CONFIG)

## ---- web UI baseline ---------------------------------------------------

# The layout work rearranges one 6000-line file with no component tests under
# it. `ui-baseline` captures the shipped UI — every registered layout, nine
# routes, two viewports — so a refactor can be *shown* to have changed nothing.
#
# Two tiers, and the difference is the whole design (see the script's header,
# and docs/UI-LAYOUTS-G3-DECISION.md §5/R1). The STRUCTURAL tier —
# tests/ui-structure.golden — is a reviewed answer key designed to be committed
# and enforced by `ui-check`. Nothing in it is a pixel, a path or a clock, so
# it is the same file on every machine. The PIXEL tier stays in
# target/: a PNG hash depends on the Chromium build and on where the fixture
# library sits on disk, so committing it would commit a fact about one laptop
# and go red on every other.
.PHONY: ui-baseline
ui-baseline: ## Capture the UI baseline for every layout (both tiers, into target/)
	@scripts/ui-baseline --self-host

# The gate. Fails on any structural drift and prints which layout, which route,
# which viewport and which key moved. It also rejects deterministic a11y defects
# (unnamed controls, broken ARIA references, duplicate ids, and missing alt).
.PHONY: ui-check
ui-check: ## Sweep every layout and fail if the structural golden moved
	@scripts/ui-baseline --self-host --check

# Every other web test reads the reporter as text. This one runs it. The M5
# fleet run was the first execution the web control plane ever had, and it
# threw before exchange one on a receiver every browser brand-checks and Node
# does not — with the whole suite green.
.PHONY: control-browser-check
control-browser-check: ## Run the shipped playback-control reporter in a real browser
	@scripts/control-reporter-browser-check

# Regenerating the golden is a deliberate act that shows up in a git diff, never
# a side effect of a normal run — a golden that rewrites itself asserts nothing.
# Run this when you MEANT to change the UI, then read the diff before committing.
.PHONY: ui-golden
ui-golden: ## Rewrite tests/ui-structure.golden after an intended UI change
	@scripts/ui-baseline --self-host --update

# `make check` cannot see either of these. index.html is include_str!-embedded,
# so a JS syntax error in it compiles, links, passes every Rust test, and then
# serves a blank page; and the theme tables are data, so a token pair that
# fails contrast is not a type error anywhere. Run this on any web change.
.PHONY: web-check
web-check: ## Test playback policy, embedded JS, and every shipped theme
	@node tests/playback/web-policy.test.js
	@node tests/playback/web-control.test.js
	@node tests/playback/player-input-contract.test.js
	@node tests/web/player-dom.test.js
	@node tests/web/nav-keyboard.test.js
	@node tests/web/reader.test.js
	@node tests/web/layout-containment.test.js
	@node tests/web/page-read-budget.test.js
	@node tests/web/theme-family.test.js
	@node tests/web/activity-node-names.test.js
	@node tests/web/analysis-node-names.test.js
	@node tests/web/settings-sections.test.js
	# The validation runner already has this as `web-membership`, but this is
	# the target a web change reaches for, and the Cluster panel is a web
	# surface like any other here. Two seconds.
	@node tests/web/cluster-membership.test.js
	@scripts/js-check
	@scripts/contrast-check --from-index crates/plurxd/src/web/index.html \
		--foregrounds='--text,--muted,--prose,--accent,--good,--warn,--bad' \
		--allow scripts/contrast-allow.txt

## ---- packaging & setup -------------------------------------------------

# What a build from this tree stamps into the binary. Keep in step with
# crates/plurxd/build.rs — see docs/RELEASING.md.
VERSION := $(shell sed -n '/^\[workspace.package\]/,/^\[/p' Cargo.toml | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)
BUILD_REF := $(shell git describe --tags --always --dirty 2>/dev/null || echo unknown)
HOST_SHORTNAME := $(shell hostname -s 2>/dev/null || hostname 2>/dev/null || echo unknown-host)

.PHONY: version
version: ## Print the version and git build stamp a build would report
	@echo "$(VERSION) ($(BUILD_REF))"

.PHONY: docker
docker: ## Build the container image
	docker build --build-arg PLURX_BUILD_REF="$(BUILD_REF)" -t plurx/plurxd:latest .

.PHONY: container-smoke
container-smoke: docker ## Build, start, probe, restart, and re-probe the container
	@scripts/container-smoke plurx/plurxd:latest

# The Compose deploy, as one command that cannot forget the stamp.
#
# `docker compose up -d --build` is what every doc told people to run, and it
# leaves PLURX_BUILD_REF empty — `.git` is outside the build context, so the
# image comes out stamped "unknown" and the running server cannot say which
# commit it is. That was "fixed" once, by teaching compose to forward the
# variable and documenting that deploys must set it. That is not a fix: it
# moves the work onto a human remembering an environment variable every single
# time, and the failure is silent. Nobody remembered, and the System page read
# "(unstamped build)" for weeks of daily deploys.
#
# So make the correct thing the easy thing. Named `docker-up` and not `deploy`
# because deploying plurx is not one thing: bare metal and systemd are equally
# supported (see deploy/README.md), and a target called `deploy` would claim to
# be the way to ship it while quietly meaning only one of the three. Container
# hosts get this; the other two stamp themselves from their own checkout,
# because `.git` is right there.
#
# **`cd deploy` — never `-f deploy/docker-compose.yml` from here.** Passing
# `-f` turns OFF Compose's automatic override discovery, so
# `docker-compose.override.yml` is silently ignored — and that file is where
# every host keeps the things this tracked repo cannot know: the media mounts,
# the GPU device passthrough, the ports. The stack still comes up, which is the
# worst part: it comes up with no media visible ("path does not resolve" on
# every library), no /dev/dri, and the transcoder fallen back to software x264.
# `-f` also moves the project directory to the repo root, so `deploy/.env` stops
# being read on the way past.
#
# Resolve Compose first so shell variables, deploy/.env, defaults, and override
# files are evaluated with the same precedence as the mutation below. The
# checker refuses a health grace shorter than snapshot recovery plus the named
# startup phases before `compose up` can replace a container.
#
# `--emit-start-period` runs first because the readiness grace is half of a
# pair whose other half usually lives somewhere this repository cannot edit —
# a bind-mounted production `plurx.toml`. Requiring an operator to mirror that
# file's snapshot deadline into `deploy/.env` by hand meant the first report of
# a mismatch was a refused deploy on the box, which is exactly what happened.
# So an unset grace is derived from the same deadline the refusal is computed
# from. A grace the operator did write is passed through untouched and still
# refused by name if it is too short: deriving is for the value nobody chose,
# not a way to overrule somebody who chose to fail fast.
.PHONY: docker-startup-budget-check
docker-startup-budget-check: ## Prove the resolved Compose startup budget before deployment
	cd deploy && period="$$(python3 ../scripts/validate-docker-startup-budget --emit-start-period)" \
	  && PLURX_HEALTH_START_PERIOD="$$period" python3 ../scripts/validate-docker-startup-budget

# The mutation below is character-for-character what somebody runs by hand in
# `deploy/`, with only the build arg and the two values a checkout can work out
# for itself added. That is the point: a convenience target that is not
# equivalent to the command it replaces is a trap, and this one sprang on the
# first real deploy.
#
# The derived period is computed once and reused for both the proof and the
# mutation, rather than depending on `docker-startup-budget-check` and letting
# each recipe derive its own. A preflight that proves one number while
# `compose up` applies another is not a preflight.
.PHONY: docker-up
docker-up: ## Build + (re)start Compose after its startup budget passes
	cd deploy && period="$$(python3 ../scripts/validate-docker-startup-budget --emit-start-period)" \
	  && PLURX_HEALTH_START_PERIOD="$$period" python3 ../scripts/validate-docker-startup-budget \
	  && PLURX_HEALTH_START_PERIOD="$$period" PLURX_BUILD_REF="$(BUILD_REF)" PLURX_NODE_HOSTNAME="$(HOST_SHORTNAME)" docker compose up -d --build
	@echo "up: $(VERSION) ($(BUILD_REF))"

# Fleet voters consume the already-qualified registry image. Pulling is safe
# while the old container is running; the replacement still waits for the
# resolved startup budget proof. That proof reads constants from this checkout,
# so the pulled runtime's immutable image ID must carry the same revision as a
# clean HEAD before the checker is allowed to run. Pin both Compose services to
# that ID for the proof and mutation: neither a moving tag nor an override may
# swap the artifact after it was inspected.
.PHONY: docker-image-up
docker-image-up: ## Pull + (re)start the prebuilt image after its startup budget passes
	cd deploy && image_ref="$$(docker compose config --images plurxd)" \
	  && test -n "$$image_ref" \
	  && docker compose pull plurxd \
	  && image_id="$$(docker image inspect --format '{{.Id}}' "$$image_ref")" \
	  && test -n "$$image_id" \
	  && revision="$$(docker image inspect --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}' "$$image_id")" \
	  && source_revision="$$(git rev-parse HEAD)" \
	  && if test -n "$$(git status --porcelain --untracked-files=no)"; then \
	       echo >&2 "docker-image-up refuses a tracked-dirty checkout because its source cannot prove a published image"; \
	       echo >&2 "commit or stash tracked changes, then check out revision $$revision"; \
	       exit 1; \
	     fi \
	  && if test -z "$$revision" || test "$$revision" = '<no value>'; then \
	       echo >&2 "pulled image $$image_id ($$image_ref) has no org.opencontainers.image.revision label"; \
	       echo >&2 "choose a qualified Plurx image built with PLURX_BUILD_SHA"; \
	       exit 1; \
	     fi \
	  && if test "$$revision" != "$$source_revision"; then \
	       echo >&2 "pulled image $$image_id was built from $$revision, but this checkout is $$source_revision"; \
	       echo >&2 "fetch and check out $$revision, or choose the image published for $$source_revision"; \
	       exit 1; \
	     fi \
	  && resolved_images="$$(PLURX_IMAGE="$$image_id" docker compose config --images plurxd plurx-discovery)" \
	  && set -- $$resolved_images \
	  && if test "$$#" -ne 2 || test "$$1" != "$$image_id" || test "$$2" != "$$image_id"; then \
	       echo >&2 "plurxd and plurx-discovery must both resolve to inspected image $$image_id"; \
	       echo >&2 "resolved images: $$resolved_images"; \
	       exit 1; \
	     fi \
	  && period="$$(PLURX_IMAGE="$$image_id" python3 ../scripts/validate-docker-startup-budget --emit-start-period)" \
	  && PLURX_IMAGE="$$image_id" PLURX_HEALTH_START_PERIOD="$$period" python3 ../scripts/validate-docker-startup-budget \
	  && PLURX_IMAGE="$$image_id" PLURX_HEALTH_START_PERIOD="$$period" PLURX_NODE_HOSTNAME="$(HOST_SHORTNAME)" docker compose up -d --no-build --pull never
	@echo "image up: $(VERSION)"

.PHONY: release-check
release-check: ## Verify the tree is ready to tag the current version
	@test -z "$$(git status --porcelain)" || { echo "working tree is dirty — commit first"; exit 1; }
	@git rev-parse -q --verify "refs/tags/v$(VERSION)" >/dev/null \
	  && { echo "tag v$(VERSION) already exists — bump the version in Cargo.toml"; exit 1; } || true
	@grep -q '^## \[$(VERSION)\]' CHANGELOG.md \
	  || { echo "CHANGELOG.md has no '## [$(VERSION)]' section"; exit 1; }
	@scripts/validate run --profile ci --all --strict
	@echo "Ready: git tag -a v$(VERSION) -m 'v$(VERSION)' && git push && git push --tags"

.PHONY: hooks
hooks: ## Install the functionality-point pre-commit validator
	@mkdir -p .git/hooks
	@install -m 0755 scripts/pre-commit .git/hooks/pre-commit
	@echo "Installed .git/hooks/pre-commit — it runs make validate-staged."
	@echo "Bypass one run with 'git commit --no-verify'."

## ---- apple clients -----------------------------------------------------

.PHONY: apple-build-bump
apple-build-bump: ## Claim the next Apple build number across every generated surface
	@python3 -m validation.apple_build --merge-target "$${PLURX_MERGE_TARGET:-origin/main}"

# iPhone and iPad run the exact same build products, so iOS compiles ONCE with
# `build-for-testing` and both destinations replay it with
# `test-without-building` — the third full Swift compile per run was pure
# waste. The shared DerivedData lives at clients/apple/build/DerivedData so CI
# can cache it between runs; a stale or absent cache only costs a rebuild.
APPLE_DERIVED_DATA := build/DerivedData

.PHONY: apple-build
apple-build: ## Compile iOS and tvOS without running simulator tests
	cd clients/apple && xcodegen generate
	cd clients/apple && xcodebuild -project plurx.xcodeproj -scheme plurx-iOS \
	  -destination "generic/platform=iOS Simulator" \
	  -derivedDataPath "$(APPLE_DERIVED_DATA)" \
	  CODE_SIGNING_ALLOWED=NO build
	cd clients/apple && xcodebuild -project plurx.xcodeproj -scheme plurx-tvOS \
	  -destination "generic/platform=tvOS Simulator" \
	  -derivedDataPath "$(APPLE_DERIVED_DATA)" \
	  CODE_SIGNING_ALLOWED=NO build

.PHONY: apple-test
apple-test: ## Generate the Xcode project, build each platform once, test every destination
	cd clients/apple && xcodegen generate
	cd clients/apple && xcodebuild -project plurx.xcodeproj -scheme plurx-iOS \
	  -destination "$${APPLE_IOS_SIM:-platform=iOS Simulator,name=iPhone 17 Pro}" \
	  -derivedDataPath "$(APPLE_DERIVED_DATA)" \
	  CODE_SIGNING_ALLOWED=NO build-for-testing
	cd clients/apple && xcodebuild -project plurx.xcodeproj -scheme plurx-iOS \
	  -destination "$${APPLE_IOS_SIM:-platform=iOS Simulator,name=iPhone 17 Pro}" \
	  -derivedDataPath "$(APPLE_DERIVED_DATA)" \
	  CODE_SIGNING_ALLOWED=NO test-without-building
	@if [ -n "$${APPLE_IPAD_SIM:-}" ]; then \
	  cd clients/apple && xcodebuild -project plurx.xcodeproj -scheme plurx-iOS \
	    -destination "$${APPLE_IPAD_SIM}" \
	    -derivedDataPath "$(APPLE_DERIVED_DATA)" \
	    CODE_SIGNING_ALLOWED=NO test-without-building; \
	fi
	cd clients/apple && xcodebuild -project plurx.xcodeproj -scheme plurx-tvOS \
	  -destination "$${APPLE_TVOS_SIM:-platform=tvOS Simulator,name=Apple TV 4K (3rd generation)}" \
	  -derivedDataPath "$(APPLE_DERIVED_DATA)" \
	  CODE_SIGNING_ALLOWED=NO build-for-testing
	cd clients/apple && xcodebuild -project plurx.xcodeproj -scheme plurx-tvOS \
	  -destination "$${APPLE_TVOS_SIM:-platform=tvOS Simulator,name=Apple TV 4K (3rd generation)}" \
	  -derivedDataPath "$(APPLE_DERIVED_DATA)" \
	  CODE_SIGNING_ALLOWED=NO test-without-building

## ---- android client ----------------------------------------------------

# CI pre-pulls the image from GHCR keyed on the Dockerfile's hash and sets
# PLURX_ANDROID_IMAGE_READY=1, so two jobs per run stop re-downloading the SDK
# from Google. Anything else — a local build, a cache miss, a fork without
# registry access — falls through to the same docker build as always.
.PHONY: android-image
android-image: ## Build the pinned Android build-env image (JDK 25 + SDK)
	@if [ "$${PLURX_ANDROID_IMAGE_READY:-}" = "1" ]; then \
	  echo "android-image: reusing pre-pulled $(ANDROID_IMAGE)"; \
	else \
	  docker build --platform $(ANDROID_PLATFORM) -t $(ANDROID_IMAGE) clients/android; \
	fi

.PHONY: android-test
android-test: android-image ## Run Android JVM unit tests + lint in the pinned image
	docker run --rm --platform $(ANDROID_PLATFORM) \
	  -u $$(id -u):$$(id -g) -e HOME=/tmp \
	  -e GRADLE_USER_HOME=/workspace/clients/android/.gradle-validation \
	  -v "$(CURDIR)":/workspace -w /workspace/clients/android \
	  $(ANDROID_IMAGE) ./gradlew --no-daemon testDebugUnitTest lintDebug

.PHONY: android-instrumentation-build
android-instrumentation-build: android-image ## Build app + test APKs for an emulator/device run
	docker run --rm --platform $(ANDROID_PLATFORM) \
	  -u $$(id -u):$$(id -g) -e HOME=/tmp \
	  -e GRADLE_USER_HOME=/workspace/clients/android/.gradle-validation \
	  -v "$(CURDIR)":/workspace -w /workspace/clients/android \
	  $(ANDROID_IMAGE) ./gradlew --no-daemon assembleDebug assembleDebugAndroidTest

.PHONY: android-instrumentation-run
android-instrumentation-run: ## Install and run instrumented tests (set PLURX_ANDROID_SERIAL)
	@test -n "$${PLURX_ANDROID_SERIAL:-}" || { echo "set PLURX_ANDROID_SERIAL to a disposable emulator/device serial"; exit 1; }
	adb -s "$${PLURX_ANDROID_SERIAL}" wait-for-device
	adb -s "$${PLURX_ANDROID_SERIAL}" uninstall tv.plurx.app.test >/dev/null 2>&1 || true
	adb -s "$${PLURX_ANDROID_SERIAL}" uninstall tv.plurx.app >/dev/null 2>&1 || true
	adb -s "$${PLURX_ANDROID_SERIAL}" install -r clients/android/app/build/outputs/apk/debug/app-debug.apk
	adb -s "$${PLURX_ANDROID_SERIAL}" install -r clients/android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
	@mkdir -p target/validation
	@output="$$(adb -s "$${PLURX_ANDROID_SERIAL}" shell am instrument -w \
	  tv.plurx.app.test/androidx.test.runner.AndroidJUnitRunner 2>&1)"; status=$$?; \
	  printf '%s\n' "$$output" | tee target/validation/android-instrumentation.txt; \
	  if [ "$$status" -ne 0 ] || ! printf '%s\n' "$$output" | grep -Eq '^OK \([0-9]+ tests?\)\r?$$'; then \
	    echo "Android instrumentation did not report a passing suite" >&2; \
	    exit 1; \
	  fi

.PHONY: android-instrumentation
android-instrumentation: android-instrumentation-build android-instrumentation-run ## Run UI tests on an explicitly selected disposable device

.PHONY: android
android: android-image ## Build the Android debug APK in Docker (no host JDK/SDK)
	docker run --rm \
	  --platform $(ANDROID_PLATFORM) \
	  -u $$(id -u):$$(id -g) -e HOME=/tmp \
	  -e GRADLE_USER_HOME=/workspace/clients/android/.gradle-docker \
	  -v "$(CURDIR)":/workspace -w /workspace/clients/android \
	  $(ANDROID_IMAGE) ./gradlew --no-daemon :app:assembleDebug
	@echo "→ clients/android/app/build/outputs/apk/debug/app-debug.apk"

.PHONY: apk
apk: android ## Build the Android debug APK (alias for android)

.PHONY: android-publish
android-publish: android ## Build the APK + serve it from the web UI (ANDROID_DATA_DIR=/path/to/data)
	@test -n "$(ANDROID_DATA_DIR)" || { echo "set ANDROID_DATA_DIR to the server's data_dir, e.g. make android-publish ANDROID_DATA_DIR=~/.local/share/plurx"; exit 1; }
	cp clients/android/app/build/outputs/apk/debug/app-debug.apk "$(ANDROID_DATA_DIR)/plurx-android.apk"
	@echo "Published -> $(ANDROID_DATA_DIR)/plurx-android.apk (served at /download/plurx-android.apk, no restart needed)"

.PHONY: clean
clean: ## Remove build artifacts and coverage output
	$(CARGO) clean
	@rm -f lcov.info
