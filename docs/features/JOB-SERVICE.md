# How Cinema schedules library work and playback dependencies

**Status** · Open · E-6BDE01613C · incident baseline `8afed3496`

A viewer who needs a subtitle or an enhancement decision has a deadline. A library scan needs eventual progress across thousands of titles. Both need durable identity, safe reassignment and a result that still belongs to the current source. This document proposes the service contract. The incident repairs cover a subset of it.

## What a caller requests

| Work | Identity and lifetime | Service |
|---|---|---|
| Recurring maintenance | Schedule and due occurrence. Coalesce missed occurrences where one pass covers them. Persist the next due occurrence alongside completion. | Run within spare capacity. Age eligible jobs, rotate across independent source scopes and retain progress across a restart. |
| One-time analysis | File, source generation, component, recipe and requested output. Repeated requests join the same work. | Preserve the durable request until settlement. Bound intake, with an explicit refusal and retry information when full. |
| Playback dependency | A renewable session interest in an exact result, with a deadline and an explicit fallback selected by the playback caller. | Wake an eligible worker on admission. Promote matching queued work. Dispatch the smallest usable output first, such as the next subtitle interval. Expire the viewer interest on disconnect or deadline. Retain independent library interests. |

Work may execute more than once after failure. Conditional publication makes the result visible once for its identity. A successful cache hit satisfies a new interest immediately. A later viewer can reuse a valid result from an earlier session.

## How workers make progress

1. **Submit or join.** Check the source and output identity, join an existing request, then apply intake limits for *new* work. A playback interest can use bounded headroom. A completion, checkpoint, cancellation or lease release can always reach its own fenced state transition under a full intake queue.
2. **Choose eligible work.** A worker supplies capabilities and source reachability. The scheduler chooses live dependencies by deadline, then background work by eligible waiting time. A bounded scan advances past a blocked source so that unrelated runnable work gets a turn. Record why a job was skipped and the next event or time that can change the decision.
3. **Acquire one attempt.** Atomically check current generation, previous lease, worker eligibility and admission for the resource set the attempt will actually use. Record the worker incarnation, attempt number and expiry. Each heartbeat, checkpoint and publication must present that attempt token. Capacity policy supplies admission and revocation decisions.
4. **Yield safely.** Running work has a cancellation boundary and a maximum time between checkpoints. Stop and join the physical reader before returning its capacity. An expired database lease revokes publication authority. Actual reader custody needs separate release or reconciliation. A partitioned worker loses the right to publish and stops when it cannot renew.
5. **Recover automatically.** A bounded sweep rediscovers due retries and expired attempts even when no viewer remains. A periodic scan covers lost wakeups and restarts. Classify transient failures, back off with jitter, and expose a terminal reason when the retry budget runs out. A permanently failing item must leave other eligible work runnable.

## One instance and a Raft cluster

Use one scheduling contract with two authorities. A standalone daemon claims and settles work in local SQLite transactions and recovers from its own durable state on boot. In cluster mode claims and fences use the replicated store. Eligible workers on multiple nodes can execute simultaneously. Only small ownership and progress transitions need consensus. Media reads, inference, temporary output and progress samples stay on the worker. A worker publishes an output reference only when the consumer can reach that output.

Placement considers source access, tool or model identity, current capacity and the job deadline. Two nodes reading the same physical device share its admission budget even if mounted under different paths. Independent physical devices can progress concurrently. Additional nodes contribute CPU, accelerators and accessible storage according to their capabilities. Quorum loss suspends new authoritative claims. Recovery reconciles custody before resuming conflicting reads.

## Ownership at integration

| Owner | Interface to this effort |
|---|---|
| Job service, E-6BDE01613C | Request identity and interests, admission outcomes for the durable queue, deadline and fairness ordering, attempt lifecycle, exact-job diagnosis, recovery and idempotent completion. |
| Raft architecture, E-E154B131A2, PR 1811 | Supplies authoritative transactions and bounded reads. Reuse its authority and observation boundaries. |
| Adaptive playback, E-9D4B6E79FD, PR 1248 | Supplies physical admission, placement evidence and yield decisions for playback, subtitles and FEL. The scheduler consumes those decisions when selecting work and records the reason for delay. |

The adaptive candidate changes title dispatch, resource claims and source custody. The cluster candidate also changes worker and playback ingress paths. Compare changed functions at integration and run combined tests on the resulting tree. A clean textual merge alone cannot verify these interfaces.

## November 20 repair scope

At the deployed revision a four-root library yields four source reservations for a title read from one file. Synthetic disjoint files in that library block each other. Catalog paths establish logical containment. Safely narrowing the durable reservation also requires binding the worker's opened source to the configured physical domain, including aliases, nested mounts and node-specific mount layouts. Preserve the broad reservation whenever the worker cannot establish that binding. Existing reservations must survive migration until their attempts settle.

The local viewer-demand producer now wakes the title worker after the store acknowledges the interest. The periodic poll discovers remote interests and covers restarts. Cross-node wake latency remains a qualification item.

A separate dispatch-turn trigger lets manual library work take precedence over a current title viewer even when source admission passes. A pending enhancement recipe also suppresses discovery of an expired title attempt after its viewer departs. These mechanisms can be reproduced independently. Their individual contribution to the reported 40-hour wait remains unmeasured.

The existing [queue incident ledger](../server/JOB-QUEUE-ROOT-CAUSE-AND-FIX.md) tracks earlier source-custody repairs and scan chunking. This follow-up covers the November 20 incident and service-level acceptance.

The dispatch migration preserves wait credit and installs new trigger definitions. Older replicas require the previous exact catalog at bootstrap. Qualify a coordinated version change and the restore procedure on a disposable copy before updating a deployed cluster.

## Acceptance before rollout

- With the background queue full, finish existing outputs, retry publication, join duplicate requests, and start a live dependency within its deadline when capacity exists. Report source change, stale generation and backlog saturation separately.
- Start maintenance on two independent physical sources. Submit live work sharing one source. Verify that it gets the reserved reader safely, the independent job continues and a conflicting maintenance reader waits. Repeat with aliases for one physical device.
- Run the same claim and completion scenarios against SQLite and replicated storage. Restart the standalone daemon mid-attempt. In a real multi-node Raft fixture kill the worker, change leader, restore the stale worker, and verify a single visible result and retained attempt history. Record the nodes that actually ran distinct jobs concurrently.
- Exercise subtitle extraction and incremental AI transcription during playback, a cold Dolby Vision request, disconnect/rejoin and a missed notification. Measure enqueue-to-first-usable-result, missed deadlines and oldest eligible background age. Establish per-workload budgets from player deadlines and measured worker capacity before promotion.
- Inspect one file, request and attempt through the authenticated daemon. Show live ownership, expired ownership, physical custody, refusal or wait reason and next retry. Recover the exact entry without erasing existing valid work. Repeat the November queue observation against the deployed candidate when authorized.
