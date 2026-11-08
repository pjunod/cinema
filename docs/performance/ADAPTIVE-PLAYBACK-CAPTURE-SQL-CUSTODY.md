# Capture SQL custody — who keeps the late claim?

**Status:** open · SQL task merged; 71 scoped checks passed · 2026-11-08.
This follows the [native/writer task](ADAPTIVE-PLAYBACK-CAPTURE-CUSTODY.md)
merged as #1435 after gate 6687. The [canonical ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md)
retains the wider acceptance boundary. Full acquisition, M0 and real trials
remain open.

## The original claim survives its attach observer

An attach previously awaited ClaimCapture directly. Cancelling that observer
could leave the Store's native mutation running and a late durable capacity
hold, after the capture root had released restart admission. No later native
file or tuner started from the dropped caller, but its SQL work was absent
from that root's completion decision.

The same pre-existing resident capture entry now registers the original SQL
JoinHandle and a separate result cell before releasing its start latch. The
actual claim future owns only Store, its immutable worker incarnation and the
original command. It cannot retain a manager/root backedge. The resident root
holds the result outside the task's unwinding frame. Positive original join
precedes delivery of that result to the caller. Cancelling the observer does
not cancel, replace or renew the original claim.

## Native closure and SQL detach are separate facts

The root may take cleanup authority only after actual acquisition completion
or drop. While acquisition remains live it cannot steal the claim result or
detach its capture. Original writers and files close before the same resident
entry starts its original DetachCapture operation. The detach uses the worker
and epoch actually returned by ClaimCapture, including when that epoch differs
from the requested attempt. The caller no longer starts an unowned replacement
detach after observing a settled sink.

Positive original joins and transactional Applied, Absent or Fenced detach
results precede the entry's closed flag. Actual record/header/alias destruction
still precedes release of the original restart admission. A Store error or
failed original task is ambiguous: its actual result/handle and admission
remain held. A fresh read, SQL lease expiry or an observer deadline does not
supply the missing acknowledgement. This bounded repair intentionally does
not invent recovery proof for ambiguous Store writes.

Detach removes this capture's consumer, but does not certify closure of the
shared ingest/tuner lease. An empty ingest can remain draining until its
original transport releases it or its existing lease expires. That residual
ledger state is not original-mutation completion proof, nor full SQL-family
closure; this task does not issue an unsafe shared Release on another
consumer's behalf. Whole transport/ingest authority remains open.

## Qualification remains source-bound

The first workspace/all-target check passed on `a80656aa`, before the new
regressions were included. It emitted a test-hook unused-method warning and
is not strict-lint or runtime evidence. The first runtime source `d3a17ef3`
had 26 passes and one failure: its epoch fixture detached Capture but left
the original Ingest draining, so the real ledger refused a successor. The
fixture now positively releases that original ingest before claiming again;
production ledger policy and assertions are unchanged.

Intermediate `cfb548dc` passed 28 owner cases. Final source `0388a32c` passes
all 29 owner cases and all 42 other DVR cases: 71 distinct scoped passes,
zero failed or ignored. Twelve owner cases are new. Reruns and earlier
source scopes add no distinct credit. Strict workspace/all-target Clippy
with warnings denied and full pinned formatting also pass on `0388a32c`.
The final successor restores the existing fresh-incarnation check and tests
actual old-worker detach despite incarnation change. It also explicitly
checks remaining draining shared-ingest state after per-capture detach.

The tests exercise dropped observers before and after real SQL commits,
concurrent original cleanup, post-commit claim/detach panic and ambiguous
errors, live acquisition/native-file closure, exact returned epoch, the first
restart cutoff and refusal of sealed first entry. Test-only pause/failure
hooks surround actual Store calls rather than replacing their effects.

The branch now includes CI transport task #1439's landing `d4ecf2eca`, which
changes no Rust/Cargo input; all four reviewed/runtime Rust files are
byte-identical. [PR #1444](http://forge.lan:3000/noirr/plurx/pulls/1444) merged at `d6019139a`
after all eight jobs of gate 6703 passed on `a913bfd10`. Candidate and landing
share tree `cc485737`; all 12 regression trailers are retained. Exact committed
checks, normal commit/push hooks and body-aware paperwork passed. Full M0 and trial eligibility are not claimed.

## Remaining ownership and inventory

This closes neither the whole pre-M acquisition graph nor a complete family
census. Device snapshot/planning, metadata probes, finalizer assembly/output,
unlink, inherited scratch, remote storage authority and the whole transport
graph retain their existing open boundaries. All constructor, allocation,
executor and complete native/kernel peaks remain Unknown or unbridged. The
64-slot admission bound is not a KnownMemoryPeak. Ambiguous SQL completion
stays Unknown rather than being cleared by timeout or a replacement mutation.

Full M0, real trials, V1–V5, current-main integration, Main promotion and
deployment remain open.
