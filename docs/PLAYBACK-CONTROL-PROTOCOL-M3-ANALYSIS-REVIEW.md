# M3 analysis controls: adversarial review record

This record covers the first adversarial review of draft PR #606 at commit
`a2d784ea`. Tests were deliberately not run before review, following the
required delivery order.

## Verdict

The reviewer requested changes for three P1 correctness issues and one P2
operational issue. Default-off rollout gating, administrator authorization,
path/digest redaction, string-safe 64-bit identifiers, sparse target-node
semantics, and the additive migration did not receive separate blockers.

## Findings and remediation

### P1: request-to-worker handoff was not atomic

The resolver could reopen or enqueue a content-addressed job and then lose its
request fence before marking the request submitted. That left unowned work
behind a request which appeared failed.

The handoff is now one database transaction. Its first statement moves only the
exact running, unexpired request generation to `submitted` after verifying that
the desired worker transition is admissible. Its second statement inserts,
joins, or reopens the exact job only when that submitted request is visible in
the same transaction. SQLite additionally verifies the resulting job before
commit. Hiqlite submits both conditional statements in one Raft transaction.

### P1: full-source hashing could not yield to playback

The earlier selector watched only lease loss and a ten-minute timeout, so a
foreground session could wait behind a full-media SHA-256 read. It also treated
every transient outcome as terminal.

Source attestation now selects against the existing live-media/rollout-stop
signal for the entire hash. Foreground preemption is a fenced retry which
decrements the just-consumed claim attempt; I/O and store failures use bounded,
typed backoff and retain the attempt charge. Claim loss writes nothing.

### P1: duplicate coalescing ignored source generation

The active uniqueness key and join lookup used only file id, component, and
node. A rescan which retained a file id could cause a request for replacement
bytes to join stale work.

The key and lookup now include admitted size and modification time. A source
update trigger cancels active requests for earlier generations, and the new
generation can be admitted immediately.

### P2: terminal history and label inputs were unbounded

Terminal requests accumulated forever, while status-label discovery unioned
the complete request and worker tables before applying a limit.

Status indexes now cover state and recency. Label inputs are independently
bounded before their union. Terminal history has both a 30-day age limit and a
20-generation per-file/component/target cap; each maintenance transaction
removes at most 256 rows and is throttled to hourly cadence per node.

## Validation order

Each correction commit receives another adversarial review before any unit test
is run. After approval, focused store/HTTP tests and the repository validation
gate run; test-driven corrections are reviewed again if they materially change
queue semantics. The PR is made ready only when that sequence is recorded and
green.

## Second review

The second review of `64ce08df` kept the PR at **request changes**. It confirmed
that the fenced handoff, full-hash cancellation, claim-loss no-op,
source-generation uniqueness, and predecessor migrations were materially
correct, then found four narrower gaps:

- a ready content key could not be joined after its scanner file identity
  changed, and an old canceled row retained an expired queue age;
- Hiqlite admission committed and read in separate calls but read only active
  rows, allowing a very fast completion to make a successful POST look failed;
- hourly cleanup was not a hard growth bound; and
- replicated contracts did not exercise forced reopen, ready replacement,
  canceled rebind, or the v11-to-current migration path.

The follow-up uses the content/pipeline key—not obsolete scanner fields—to join
active or ready work. A matching artifact record is retained, but a missing or
terminal ordinary worker row is queued for holder repair because catalog
metadata does not prove the bytes are still available. Force likewise reopens
a queued rebuild. Every terminal rebind receives a fresh queue age. Hiqlite now
always checks the caller's request id in any state after insert, including when
an internal transport retry observes the already-committed conflict; duplicate
joins still require an active exact generation. Terminal transitions enforce
the 8,192-row global ceiling synchronously while preserving the row that just
became terminal. SQLite and real-Raft contracts cover ready replacement,
canceled rebind, forced reopen with artifact retention, ambiguous committed
insert recovery, charged retry exhaustion, the foreground stop signal, and
both v11 and v12 migration paths.

The fourth review found that Hiqlite's second handoff statement could reuse a
durable `submitted` request after its original transaction, allowing a stale
forced replay to reopen a terminal worker. The replicated transaction now
retains the fenced owner only as a one-use, transaction-local handoff token:
the worker mutation requires it and a final statement consumes it. Atomic
commit means no observer sees the temporary owner, and an exact transaction or
caller replay finds no token and cannot mutate the worker. The real-Raft
contract finishes a rebound worker, replays the earlier accepted forced
submission, and requires the ready state, rebound file identity, and timestamp
to remain unchanged.
