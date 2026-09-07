# WAL generation repair plan review — fix two blockers before touching the reader

**Status:** review complete and reconciled · **Reviews:**
[WAL_GENERATION_REPAIR_PLAN.md](WAL_GENERATION_REPAIR_PLAN.md) · **Verified
against:** `origin/main` at `a0eb5216` · **Written:** 2026-08-25

This is the independent adversarial review requested before implementation.
It checked the plan against the vendored WAL and OpenRaft adapter rather than
accepting the incident narrative. Read §1 for the verdict, §2 for the blockers
that changed the design, and §3 for the accepted scope boundary.

## 1. Verdict — the diagnosis holds, but the first design would corrupt a partial refresh

**Request changes before implementation.** The stale-mmap diagnosis survives:
the reader can retain a mapping of an unlinked inode while the same pathname
names a clean replacement WAL. That explains the clean stopped-node inspector
beside the running process's false missing-log error.

The draft nevertheless had two P0 defects. Adding an incarnation predicate to
the existing positional refresh could attach one file's boundaries to another
file's mmap. Sending `Err` and then the stream terminator could panic the reader
after the consumer dropped its bounded receiver. Both are corrected in the
reconciled plan before implementation begins.

## 2. P0 findings — reconstruction and response framing must change

### 2.1 Retain-then-enumerate loses file alignment

The existing refresh retains reader files by number, then updates retained
entry `i` from writer entry `i`. With an incarnation predicate, this reachable
shape is unsafe:

```text
reader: [WAL 1 / A, WAL 2 / A]
writer: [WAL 1 / B, WAL 2 / A]
retain: [WAL 2 / A]
update: retained WAL 2 receives writer WAL 1 boundaries
append: writer WAL 2 is appended a second time
```

Only a debug assertion notices the mismatch. A release build can read WAL 2's
mmap under WAL 1's boundaries.

**Required and accepted:** reconstruct the deque in writer order, reusing an
old object only for an exact `(wal_no, incarnation)` match. Test a replaced
front with an unchanged suffix and a reset followed by rollovers.

### 2.2 An error is terminal; it cannot be followed by `None`

The current response channel has capacity one. Its consumer returns through
`?` immediately after receiving `Err`, dropping the receiver. A producer that
then sends `None` blocks or receives cancellation and panics on `unwrap()`.

**Required and initially accepted:** make `Err` terminal, do not unwrap a
cancelled response send, and test a failed action followed by a successful
action on the same reader thread. The post-PR adversarial review then rejected
whole-batch one-shot staging because a valid 128-entry range can exceed 2 GiB
of raw payload. The final protocol is a capacity-one stream of `Record`
messages followed by exactly one `Done(Result<(), Error>)`; it retains terminal
error semantics without the raw-memory amplification.

## 3. P1 findings — keep corruption claims and concurrency claims exact

### 3.1 Incarnation does not serialize an in-flight suffix rewrite

The reader drops `wal_locked` after refreshing its local clone. The writer
mutates physical files before publishing their new headers through that lock.
An incarnation change would therefore affect the next reader action, not one
already scanning while a conflicting suffix is rewritten.

**Required and initially accepted:** narrow the claim to the reproduced
full-purge path. The post-PR adversarial review found a remaining interleaving:
an unmapped old reader could refresh, the writer could recreate the pathname,
and that reader could then map the replacement inode before publication. The
final repair therefore holds a shared layout guard through refresh/mmap/read,
takes the exclusive guard before remove/truncate physical mutation, and renews
the incarnation for in-place suffix rewrites.

### 3.2 Absence outside retained WAL metadata is not automatically corruption

OpenRaft's `try_get_log_entries` permits an absent result; a stricter wrapper
checks exact ranges where required. A short result becomes `Error::Integrity`
only when one `WalFile` claims the requested subrange and its bytes contradict
that metadata.

**Required and accepted:** keep the exact-count check inside `WalFile::read_logs`
and separately test legitimate absence from the set.

### 3.3 Malformed offsets must return errors instead of panicking

Unchecked offset addition, direct mmap slicing, unwrapped mmap/read operations,
and a presumed nonempty `LogState` buffer can still terminate the reader.

**Required and accepted:** use checked offset arithmetic and checked slice
access, propagate map/read failures, publish memos only after a complete read,
and prove that the reader remains usable after one failed action.

## 4. Timeout and compatibility — approved with boundary tests

A process-local checked `u64` incarnation is compatible with WAL version 1;
identity exhaustion must fail rather than wrap. The 120-second default and
10–3,600-second operator range are reasonable. Test 9 · 10 · 3,600 · 3,601 and
the environment parser. Document that Hiqlite's zero non-final-segment timeout
causes OpenRaft to apply the install timeout across snapshot transfer work.

Older binaries tolerate unknown `[cluster]` keys, so the new timeout setting
does not break config parsing during rollback. No WAL header change is needed.

## 5. Post-PR adversarial review — three P1 findings repaired

A fresh agent reviewed PR #574 after implementation and requested changes for
three P1 issues: physical replacement could still race an unmapped reader,
count validation did not prove the exact outer log-ID sequence, and the
one-shot response duplicated up to roughly 2 GiB of valid raw payload. The
final implementation adds the shared/exclusive layout boundary and a
barrier-driven full-purge test, validates every selected outer ID in optimized
builds, and restores capacity-one incremental delivery with an explicit
terminal result. Its follow-up tests also cover suffix-incarnation renewal,
bounded backpressure, a request spanning the retained floor, and an internal
gap that fails after an earlier record was streamed.

The review reported no P0 findings. Its P2 history-scope concern is resolved by
the validation commit and PR body explicitly naming the unrelated Darwin
mapping required to restore `main`'s history audit.
