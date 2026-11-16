**Owner:** Claude (Opus 5.5), session_01BCWL9ywv2KsdzUoiYaqYru · **State:** ready — handed to the merge train · **Progress:** built → adversarial review (Fable) done, all should-fix findings addressed in `fix(hiqlite): close parked Raft sockets at their own deadline` → fast lane via merge train → merge. Server deploy is Paul's.

## What

Hiqlite patch 28: the Raft client factory leases idle connections instead of dialing a new TCP + WebSocket for every `new_client`.

## Why (root cause, not a symptom)

OpenRaft 0.9.25 leadership read-confirm (`handle_check_is_leader_request`, i.e. every `ensure_linearizable` / Hiqlite `query_consistent*`) calls `new_client` per voter per read with a heartbeat-interval (800 ms) TTL and drops the client. OpenRaft's factory contract says `new_client` "should not create a connection". Hiqlite's factory dialed one anyway and closed it on drop, so each read ran a full handshake inside its budget. A single lost SYN cannot be retransmitted before TCP's 1 s initial RTO, so it is a guaranteed timeout. On an established socket the same loss recovers in about 200 ms.

Evidence (retained two-ended SYN capture, leader media1 ⇄ voter lab6, 1 h, kernel drops 0): ~223 dials/s; 32 SYNs lost in transit; **32 of 33 originating `append_db` timeouts follow a lost SYN by 800.0–801.7 ms**; 0 SYN-ACKs lost, so the leader's own NIC RX misses did not cause these. Full write-up: `docs/cluster/RAFT-READ-CONFIRM-DIAL-RCA-2026-11-14.md`.

## How

- A dropped client parks its connection only if no reset, cancellation, queued request or snapshot attempt touched it during the lease. The next `new_client` for the same group and target leases it exclusively, so no RPC shares a socket with another client or with replication.
- Bounds: at most 8 parked per target. Each parked supervisor owns its 30 s idle deadline and closes its own socket, so a former leader that never leases again does not keep sockets. Nothing is leased after Raft stops.
- A parked supervisor whose socket ends exits instead of reconnecting in the background.
- No timeout, heartbeat, retry, watchdog or feature-gate changes.

## Verification

- `cargo test` (vendored hiqlite, `auto-heal macros sqlite`) `network::`: 94 passed, including an answered read-confirm cycle (5 RPCs, 1 socket), a real RPC timeout preventing a park, the cap, and the idle deadline. Whole lib: same 20 environment failures with and without this change (identical failure set; base 190 pass, head 193 pass).
- Each new test rejects its mutation: dial-per-client, ignoring reset during a lease, reconnecting while parked, and removing the supervisor's park deadline each fail at least one test.

## Review disposition (Fable adversarial review)

- Should-fix 1 (TTL only enforced at pool access): fixed; the supervisor owns the deadline.
- Should-fix 2 (Release/Acquire store-buffer race): fixed; `SeqCst`.
- Should-fix 3 (lease after Raft stop): fixed.
- Should-fix 4 (no RPC-level tests): added the four tests above.
- Nit 5 (drop under lock): fixed.
- Nit 6 (leased vs dialed counter): not added; acceptance uses the kernel's `TcpActiveOpens` and the existing connect log line, no new surface.
- Nit 7 (no keepalive, short idle timeouts): safe by construction (the socket closes and the next client dials); documented.
- `make hiqlite-vendor-clippy` equivalent (`auto-heal,cache,macros,sqlite`, `-D warnings`) clean; `tests.operations.test_hiqlite_patch_ledger` OK; `scripts/paperwork --body-file` 15/15.

## Acceptance after deploy (not claimed here)

On the leader with unchanged process epochs: Raft-port `TcpActiveOpens` falls from ~223/s per voter to near zero, `Trying to open WebSocket stream` log rate follows, and originating timeouts are counted alongside NIC/softnet deltas. Still open and **not** addressed by this PR: media1→lab6 SYN loss (~4e-5, location unknown) and I226-V RX MPC on media1/lab3/lab4.

Regression-Test: vendor/hiqlite/src/network/raft_client.rs::read_confirm_cycle_reuses_one_socket_across_answered_rpcs
Regression-Test: vendor/hiqlite/src/network/raft_client.rs::dropped_healthy_client_is_leased_without_a_new_socket
Regression-Test: vendor/hiqlite/src/network/raft_client.rs::timed_out_rpc_closes_its_connection_instead_of_parking_it
Regression-Test: vendor/hiqlite/src/network/raft_client.rs::parked_supervisor_closes_its_socket_at_the_idle_deadline
Regression-Test: vendor/hiqlite/src/network/raft_client.rs::client_reset_during_lease_is_closed_not_reused
Regression-Test: vendor/hiqlite/src/network/raft_client.rs::parked_connection_whose_socket_ends_exits_and_is_not_leased

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_01BCWL9ywv2KsdzUoiYaqYru
