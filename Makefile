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
	  network::raft_client::tests::sqlite_install_snapshot_preserves_mismatch_for_offset_reset \
	  --lib -- --exact
	$(CARGO) test --locked --manifest-path vendor/hiqlite/Cargo.toml \
	  --no-default-features --features auto-heal,cache,macros,sqlite \
	  network::raft_client::tests::cache_install_snapshot_preserves_mismatch_for_offset_reset \
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
