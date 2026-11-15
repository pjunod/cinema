# Raft read-confirm timeouts: a TCP dial inside an 800 ms budget

**Status:** built — Hiqlite patch 28 fixes the root cause and passed fleet
acceptance; the physical-loss items below remain open · **Owner:** Claude (Opus 5.5) continuing the shared-library
playback NIC/RPC investigation · **Date:** 2026-11-14

## The symptom

The leader logged `hiqlite::network::raft_client` timeouts: empty `append_db`
requests, 800–801 ms elapsed against an 800 ms budget, some propagated as
"timeout while confirming leadership for read request". Separately, lab hosts
with the Intel I226-V NIC (media1, lab3, lab4) count rising `rx_missed_errors`
(hardware MPC). The open question was whether the RX loss caused the timeouts.

## What the evidence shows

A retained one-hour, two-ended capture of TCP SYN and SYN-ACK packets between
the leader (media1) and voter lab6 on the Raft port (both ends, kernel drops
0) answers it:

| Measure | Value |
|---|---|
| SYNs media1 → lab6 | 802,950 in 1 h (~223/s) |
| Handshakes seen at both ends | 798,364 |
| SYNs that left media1's tap and never reached lab6's | **32** |
| SYN-ACKs lost lab6 → media1 | 0 |
| Originating timeouts to lab6 in the hour | 33 |
| …that follow a lost SYN by 800.0–801.7 ms | **32** |
| media1 RX MPC increments in the hour | 442, none tied to a timeout |

So the timeouts in that window were caused by lost SYNs, in the direction
that media1's RX misses cannot affect.

## Why a lost SYN times a read out

- OpenRaft 0.9.25 `RaftCore::handle_check_is_leader_request` runs for every
  `Raft::ensure_linearizable`, which Hiqlite calls for every
  `query_consistent*`. It calls `RaftNetworkFactory::new_client` for each
  voter, sends one empty AppendEntries with a heartbeat-interval TTL, and
  drops the client.
- OpenRaft's factory contract says `new_client` "should not create a
  connection". Hiqlite's `NetworkStreaming::new_client` spawned a supervisor
  that dialed a fresh TCP + WebSocket (+ auth) connection immediately and
  closed it on drop.
- Every linearizable read therefore ran a full handshake inside its 800 ms
  budget. Linux's initial SYN retransmission timeout is 1 s, so one lost SYN
  is a guaranteed timeout. Loss on an established socket recovers in
  ~200 ms. An earlier paired capture saw three such losses recovered in about
  208 ms with no timeout.

The read rate decides how many dials there are; the dial decides that a rare
loss becomes a failure.

## The fix (Hiqlite patch 28)

The factory keeps a small per-target pool of healthy idle connections. A
dropped client parks its connection when no reset, cancellation, queued
request or snapshot attempt touched it. The next `new_client` for the same
group and target leases it exclusively, so each socket still carries one
client's RPCs at a time and nothing shares a socket with replication. At most
64 connections are parked per target (first 8; raised after the post-deploy
measurement below). Each parked supervisor owns its
30 s idle deadline and closes its socket when it passes, even if the pool is
never touched again (a former leader). A parked supervisor whose socket ends
exits instead of reconnecting, and nothing is leased after Raft stops. There
is no client keepalive, so a peer or middlebox idle timeout under 30 s only
closes parked sockets early, which falls back to dialing. No timeout,
heartbeat, retry or watchdog changes. This is what OpenRaft 0.10 builds in
as a dedicated heartbeat client; on 0.9 it belongs in the factory.

## Still open (not caused by, and not fixed by, this change)

1. **SYN loss media1 → lab6, ~4e-5, seen once.** In the 2026-11-14 capture it
   happened between media1's transmit tap and lab6's driver. lab6 `rx_missed`
   stayed flat and its listen queue dropped nothing. It did not reproduce on
   2026-11-15. Counted at the receiver, every probe media1 sent toward lab6
   arrived: 300,000 ICMP echo requests, plus 66,750 TCP SYNs in bursts of 50
   (lab4 sent 66,900 as a control). About 15 losses were expected at the
   earlier rate. Location and trigger are unknown; it may need the load
   present on 2026-11-14.
2. **I226-V RX MPC on media1, lab3 and lab4.** About 0.04–0.07 % of received
   packets. On 2026-11-15 it was the only loss the probes found: 22–26 of
   300,000 packets (7–9e-5) lost *into* media1 on each path. That covered echo
   replies from lab6 and lab4, whose requests all arrived (counted at the
   receiver), and echo requests from lab6. Nothing was lost in either direction
   between lab4 and lab6. lab6's Realtek NIC misses about 100× less. Softnet
   drops are 0 and RNBC is about 0. Read against the driver's counter
   semantics, that points to the on-chip receive buffer overflowing while
   descriptors were available, not to a software drop. Misses per packet rise
   with burstiness at low average rates: 1.5 → 57 per million packets as the
   0.5 s average rises to 100 Mbit/s on a 2.5 Gbit/s link. **The mechanism is
   not proven.** The leading candidate is a burst arriving while the PCIe link
   or the CPU package is in a low-power state that takes longer to wake than
   the buffer lasts.
   - ASPM L1 and L1.1 are enabled on every I226 link, and the device LTR
     permits 3–15 ms snoop latency.
   - media1's deepest idle state (C3_ACPI) has a 1,048 µs exit latency and
     holds most of its idle time; lab3 and lab4 reach C10 at 230 µs.
   - Two reversible A/B experiments would test it: compare misses per packet
     at matched load with ASPM L1 off on one I226 link
     (`/sys/bus/pci/devices/<nic>/link/l1_aspm`), and with idle depth capped
     (PM QoS, `cpupower idle-set -D`). Each is a runtime host setting, so each
     needs the owner's approval to run. The fix follows from whichever one
     moves the rate, applied per host through the deployment configuration.

The 800 ms timeouts no longer follow from either item. In the 2026-11-14
acceptance window the leader took 24,042 RX misses in one 165 s slice with no
RPC error (see below).

## Acceptance results

- **Patch 28 (8 idle connections per target), 2026-11-14.** Steady operation
  21:24:10–21:57:10Z (33 min, all four nodes on the build, epochs unchanged):
  0 originating `raft_client` errors and 0 read-confirm failures. Before the
  fix ~18 were expected in that window. Leader SYNs per voter fell from ~223/s
  to 0.78/s and 0.38/s. The 137 errors at 21:24:08–10Z were connection refused
  while lab4 restarted for the deploy.
- **Idle cap 64, 2026-11-15.** Leader on the build since 11:27Z: 0
  `raft_client` errors 11:37–12:45Z, and leader SYNs per voter at 0.1–0.2/s.
  The errors at 11:28–11:36 and 12:46 were connection refused during the
  lab3 and lab6 restarts of the same deploy.
- **Not exercised:** a leader failover under the new client.

## Acceptance for patch 28

After deployment, on the leader with process epochs unchanged:
`TcpActiveOpens` to the Raft port drops from about 223/s per voter to near
zero; the `Trying to open WebSocket stream` log rate drops to match;
originating `append_db` timeouts are counted in the same window as the
NIC/softnet counter deltas. Any timeout that remains needs its own
attribution; this change does not explain it.

## Post-deploy measurement, 2026-11-14

With the leader on `8785492b`, its new Raft-port SYNs fell from ~223/s per
voter to about 1.7/s (lab6) and 2.6/s (lab4) in a 60 s capture. Every one of
them came in bursts: SYNs appeared in only 9–10 distinct seconds, up to ~55 in
one second, while `plurx_http_store_operations_by_method_total` showed page
loads issuing parallel consistent reads (DVR attention and overview,
`get_setting`, settings snapshots, token checks). A burst wider than the idle
cap leases every parked socket and dials the rest, so the cap went from 8 to
64. Batching the reads themselves into one confirmation (read-index batching)
is the larger fix and belongs to the read-authority work, not this patch.
