# Passive voter timing observations

**Status:** open — passive instrumentation built and independently reviewed · **Executes:** C4 preparation from the cluster audit · **Written:** 2026-11-19

Delivery and qualification evidence is tracked in
[promotion #1811](http://forge.lan:3000/noirr/plurx/pulls/1811).
Actual post-deployment voter observations and the measured election-floor
decision remain open; Paul owns deployment and fleet acceptance.

## 1. Objective and boundary

Prepare direct timing observations and a bounded external collector. Existing deployed C2 source was observed on all three voters, but current leader-change and media-stall counters cannot measure voter starvation. Do not tune any election floor, deploy, restart or induce faults/load as part of this change.

## 2. Observation semantics

Record lateness when the existing serving-monitor sleep resolves, against that same Sleep's monotonic deadline. Keep the exact sleep and refresh order, and add no poller/task. This is serving-monitor wake lateness, which includes runtime scheduling/timer delay; it does not attribute the cause to CPU starvation.

Record DB AppendEntries RPC attempts at the existing RaftNetwork API entry. Also record incoming DBAppend handler entry/completion gaps and handler duration, using the request vote and current passive membership endpoint identity. Include empty heartbeats and data appends. Call the outgoing observation RPC-attempt gap: it precedes transport queueing and is neither physical socket completion nor peer delivery/RTT. Incoming handler duration includes Raft/store work and is not network RTT. Do not move clock-capture, pooling, connection/reset or snapshot logic.

A process-local observational registry retains at most64 local-node/target entries. Assign source-instance epochs at the existing outbound pool and incoming AppState lifetimes. Reset each series on source instance, leader term or endpoint identity change, ignore older sources/terms, and evict only observational state at capacity. Do not carry history across process/source changes. Snapshots copy plain bounded data under a short non-async lock; failures never affect authority.

## 3. File ownership

Own vendor hiqlite `append_timing.rs`, its lib export and the DBAppend API hook in network/raft_client.rs plus handler hooks in network/raft_server.rs; the single AppState/start epoch field and generic fork patch documentation; daemon serving_fence.rs observation fields/monitor sampling, cluster.rs Observations projection, http/system.rs metrics rendering; a bounded standard-library collector and focused tests. No OpenRaft dependency change, raw client getter, new runtime task or product toggle.

## 4. Collector and evidence

A one-second collector takes explicit discovered endpoint configuration, with at most16 targets and a finite duration capped at24hours. It performs no authenticated mutations, follows no redirects, writes only bounded filtered timing/build/role/term/process-epoch records, and marks missing scrapes/source/process changes. Prometheus transport latency is a collector measurement, not heartbeat delivery latency. Voter role and source identity must be present to treat a window as voter evidence.

## 5. Validation and remaining acceptance

Run deterministic registry tests for same-term gaps, heartbeat/data inclusion, older-term rejection, endpoint/term reset and capacity eviction. Test existing monitor lateness sampling without changing lease/authority outcomes. Test collector filtering, redirect refusal, identity discontinuity and finite bounds without contacting fleet. Exact committed source compile/tests use the root-owned compiler slot when available. Physical post-deployment observations remain open; prepared instrumentation is not C4 acceptance.

## 6. Prepared collector use

Prepare private configuration from an observed roster, using the shape in `scripts/collect-voter-timing.example.json`. Replace the full expected source and exact published build before collection. Run `python3 scripts/collect-voter-timing.py --config <private-discovered-fleet.json> --output <agent-receipt-directory>/c4.jsonl --duration-seconds 3600`. Exclusive output creation prevents replacing an earlier receipt. The collector does not deploy or enable instrumentation.

The requested scrape cadence is one second; every sample records actual elapsed time, scrape duration and poll lateness. Slow/unavailable nodes may overrun that cadence and must be treated as missing coverage. Duration is at most24hours; output is at most256MiB (64MiB default), with an explicit incomplete end record if capacity ends the window. Only allowlisted timing/build/role/term data is persisted; redirects, embedded credentials and authenticated mutations are refused.

The metrics are bounded summaries (counts, sums, maxima and age), not individual gap histories or quantiles. RPC-attempt gaps precede transport queueing; receive-completion duration includes Raft/store processing. Correlate them with monitor wake lateness and independent pressure observations; none alone attributes CPU starvation or proves physical heartbeat delivery. Fleet measurement acceptance remains open until the qualified source is deployed and representative windows are actually collected.

## 7. Absolute collector deadlines

The collector requires Unix `ITIMER_REAL` signal delivery in its main thread and exclusive ownership of that process timer; unsupported platforms/embedding contexts are refused explicitly. Every request's absolute alarm is the earlier of750ms or the remaining run deadline and covers open/DNS/connect/TLS/body parsing together. Socket-operation timeouts remain an additional bound, not the finite-request argument. The run deadline is rechecked before each target and bounds the final sleep. There are no uncancellable worker threads or subprocesses in the collector.

Regression tests use an owned localhost TCP server that continuously drips bytes faster than the socket timeout, plus a one-second run with16slowtargets, a controlled resolver stall, and actual HTTP unknown-source/missing-telemetry controls. These tests do not contact the fleet. Output capacity and redirect bounds are unchanged.

### Source-instance ownership verified against implementation

`vendor/hiqlite/src/store/mod.rs::start_raft_db` constructs one `NetworkStreaming` with an `Arc<IdleConnections>` and moves that factory into `Raft::new`. `RaftNetworkFactory::new_client` calls `lease_or_dial(&self)`; both its reused-client and newly dialed-client branches downgrade that same factory Arc. The timing OnceLock therefore belongs to the stable factory/pool, not each read-confirm RPC, replication client or socket. A recreated Raft constructs a new pool/source epoch. Incoming AppState has a separately allocated epoch at its sole constructor in `start.rs`.

The registry key includes `(local_node, remote_node, AppendTimingStage)`. Outbound `RpcAttempt`, inbound `ReceiveEntry` and inbound `ReceiveCompletion` thus have independent source ordering: a newer outbound-pool epoch cannot suppress the receiver AppState's earlier epoch. Each stage resets gaps on a newer source, term or endpoint; older source/term records are ignored. `since_last_seconds` remains visible during a continuing gap. No history is compared across a process epoch or exported source/term/endpoint cohort.
