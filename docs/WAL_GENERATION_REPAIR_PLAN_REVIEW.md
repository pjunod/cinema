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

**Required and accepted:** return one
`Result<Vec<Vec<u8>>, Error>` over a one-shot channel. Do not unwrap a cancelled
response send. Test a failed action followed by a successful action on the same
reader thread.

## 3. P1 findings — keep corruption claims and concurrency claims exact

### 3.1 Incarnation does not serialize an in-flight suffix rewrite

The reader drops `wal_locked` after refreshing its local clone. The writer
mutates physical files before publishing their new headers through that lock.
An incarnation change would therefore affect the next reader action, not one
already scanning while a conflicting suffix is rewritten.

**Required and accepted:** narrow this PR to the reproduced full-purge path,
where a mapped inode is unlinked and a different file reuses its number. A
general suffix-rewrite repair needs a shared/exclusive action guard or
copy-on-write files and its own interleaving proof.

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
