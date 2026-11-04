# Cold playback review — Astra findings and the revised proposal

**Status:** done · **Scope:** Astra proposal review and finding dispositions ·
**Reviewed:** 2026-11-02 EST · **Reviewer:** GPT-6 Astra ·
**Disposition owner:** Codex · **Runtime changes:** none.

Companion to [the RCA and proposal](COLD-PLAYBACK-STARTUP-RCA-AND-PROPOSAL.md)
and [the retained incident packet](COLD-PLAYBACK-STARTUP-INCIDENT-20261102.json).
Read the revised proposal first, then use this record to inspect what Astra
found and exactly how the proposal changed. The user will submit this packet
to Fable. This is a design review and disposition record, not permission to
implement, deploy or claim production qualification.

Revision note: this is the historical review of proposal revision 1. The
[source-ledger redesign and removal of both refusals](COLD-PLAYBACK-STARTUP-FABLE-REVIEW.md)
supersede its conservative preparation-refusal disposition. Revision 2 has
its own re-review status in the canonical proposal.

## 1. What Astra reviewed

Astra independently read the draft proposal, the manually retained evidence,
the documentation diff and bounded relevant source at `c0f0051a1`. The
investigated runtime was `ed708eda6`; the relevant runtime files had no diff
between those revisions. The reviewer ran no live commands, benchmarks or
tests and made no edits. Ripwire and knowledge-query cache/lock writes were
refused in the reviewer's sandbox, so it used bounded source reads and did
not independently validate the primary investigator's knowledge-query result.

The review found the measured cold-I/O diagnosis and conservative treatment
of the previous publication-policy decision sound, and identified **one P1
and two P2 proposal gaps**. Astra's disposition was ready for Fable after
explicit resolutions or recorded deferrals. Codex made the changes below;
there has been no second Astra review of those changes.

## 2. A1 / P1 — resolve competing viewers rather than promising incompatible service

**Finding:** the proposal said that protected copy playback would preempt
whole-file preparation and that another viewer with no usable fallback
would retain preparation priority. `analysis_source_may_continue` accepts
any live waiter on a request. Downgrading only the fallback viewer therefore
does not resolve a second required-preparation waiter, a different request
on the same disk, or continuous overlapping copy sessions. A lifetime copy
lease could also starve preparation indefinitely.

**Disposition: addressed with an explicit conservative policy and limitation.**
[Proposal §4.2](COLD-PLAYBACK-STARTUP-RCA-AND-PROPOSAL.md#42-m1--make-copy-readers-visible-and-make-fallback-preparation-yield)
now separates lifetime source ownership from source scheduling precedence.
Protected copy wins over conflicting preparation, including another viewer's
required hash. That viewer gets a bounded typed unavailable response; its
preparation remains durably queued and observable. The proposal explicitly
does not guarantee preparation progress during continuous copy viewing.
It cannot simultaneously claim preserved preparation service or count the
refusal as fast-start success.

The conflict table covers two viewers on one preparation request, different
requests sharing a source domain, overlapping copy sessions, and work that
cannot safely quiesce. Another foreground copy is not cancelled as if it
were background work, and this is not a new single-stream limit. A departing
copy cannot clear other readers' ownership. Old waiters cannot cancel newer
requests or bypass the arbiter merely because a viewer remains live.

Tests for those cases are required. Shared-volume identity and remote-node
arbitration remain explicit prerequisites before a fleet-wide claim; a
node-local signal is not evidence that another host has stopped reading.
A fair-sharing/reserve-based policy would be a separate design amendment.
Fable and the user must evaluate this conservative service limitation before
implementation; the revised document does not bury it as an incidental detail.

## 3. A2 / P2 — give the ready-output target an implementation owner

**Finding:** the measured ready-output trial took 3.855 seconds, exceeding
the proposed 2-second target. Source arbitration, audio identity and rolling
bootstrap changes do not necessarily remove this ready-path delay. The
673 ms VOD-create and 1,741 ms attachment-to-presentation observations do
not explain the full gesture-to-first-frame path.

**Disposition: addressed.**
[Proposal §4.1](COLD-PLAYBACK-STARTUP-RCA-AND-PROPOSAL.md#41-m0--retain-the-completed-causal-measurements-and-add-missing-runtime-attribution)
now owns the ready-output investigation and repair, including client and
server changes when the trace identifies them. It requires monotonic gesture,
decision, create, attach, master/child/init, append/decode and presentation
boundaries, joined to server request/phase identity without subtracting
unrelated clocks. It requires a matched before/after ready-output trial.
A successor retains the original gesture origin, and a failed ready cell
cannot be renamed cold or credited to another milestone.

The target remains proposed and unmet by the one retained ready trial.
No performance improvement is claimed from this documentation change.

## 4. A3 / P2 — bound quiescence while retaining cleanup ownership

**Finding:** the startup handshake required existing readers to stop before
a protected producer starts, but had no deadline or failure outcome. An NFS
reader can fail to reap promptly. Retaining a fence safely is correct;
keeping the foreground caller waiting without a bound is not.

**Disposition: addressed.**
[Proposal §4.2](COLD-PLAYBACK-STARTUP-RCA-AND-PROPOSAL.md#42-m1--make-copy-readers-visible-and-make-fallback-preparation-yield)
sets one absolute acquisition/quiescence deadline: the earlier of 1,000 ms
from entering acquisition and the operation's existing deadline. It does
not restart the startup episode. Deadline or cancellation removes pending
startup demand and settles its waiter. Proposed HTTP 503
`startup_source_busy` reports the resource failure; it is not decoder
failure or successful latency acceptance.

A separate cleanup owner retains the unsettled background reader's resource
fence until confirmed reap. Late acknowledgement cannot launch a producer,
and failed reap cannot free the source or hold the caller indefinitely.
The required regression holds cleanup across deadline and cancellation,
asserts bounded response and no producer spawn, removes waiting demand,
then verifies safe eventual cleanup without releasing another owner's fence.
Initial spawn failure, completion and retry ownership are covered as well.

The 1-second value and new error code are proposed implementation contracts;
qualification and API documentation must establish their final behavior.

## 5. Nonblocking improvements were incorporated

| Review improvement | Disposition |
|---|---|
| Make measurements reproducible | The JSON now retains named run IDs, exact positions/offsets, bytes, monotonic elapsed results and sanitized recipes for FFmpeg, raw reads, cross-client order, NAS-local reads, read-ahead and scanner pause/resume. Exact experiment-start wall timestamps were not retained and remain explicitly unavailable; none were invented. The packet remains manually retained observations, not signed qualification. |
| Put restart/invalidation acceptance with preparation work | Completed-attestation/index restart reuse and source/pipeline invalidation moved into M1 preparation scope, out of M2 selected-audio identity parity. Unfinished-hash persistence remains a separate integrity design. |
| State the rate/supply envelope | M4 requires sufficient measured supply for successful 1x/2x continuity. Near-real-time or otherwise insufficient supply at 2x is an expected refusal/recovery test, not an impossible success requirement. Such failures cannot be used to claim universal fast startup. |

## 6. The later transfer notice does not explain the retained experiments

After the measurements the user reported moving large movies to the external
NAS disks. The user clarified that the transfers had begun only about 30
seconds before that notice, after the retained read benchmarks, and were
not active during the original 09:06 EST failure. No further throughput
measurements were made after the notice. The proposal records that chronology
and requires concurrent operator workloads to be recorded in qualification.
The source-scanner A/B/A retains its own background-workload limitation;
it does not claim a completely idle disk or a normal maximum device speed.

## 7. What Fable should evaluate

The packet is ready for a second independent proposal review. The material
trade-off is explicit: the initial source arbitration favors active copy
viewing over conflicting required preparation and can defer that preparation
under continuous viewing. Fable should assess that choice, the bounded
quiescence/cleanup split, source-domain identity across nodes, and whether
the small-window packaging experiments and ready-path work can satisfy the
2-second ready / 5-second cold targets without new stalls or hidden quality
changes.

Implementation, source-exact regression execution for new behavior, physical
continuity qualification and deployment are still outstanding. No Astra
finding is marked resolved by a runtime test that has not happened.
