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

The corrected commit receives a second adversarial review before any unit test
is run. After approval, focused store/HTTP tests and the repository validation
gate run; test-driven corrections are reviewed again if they materially change
queue semantics. The PR is made ready only when that sequence is recorded and
green.
