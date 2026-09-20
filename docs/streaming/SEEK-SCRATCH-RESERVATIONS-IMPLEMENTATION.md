# Seek scratch reservations — implementation handoff for Opus

**Status:** ready for unit A; B mechanism specified for verification; C
enforcement design/proof required · **Written:** 2026-09-20 ·
**Executes:** R1–R6 in the
[approved RCA](SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX.md) ·
**Delivery:** three review units, one combined promotion ·
**Runtime changes completed by this document:** none

Read the RCA first for the incident evidence and reviewed guarantees. This
document supplies the build order, ownership boundaries, proposed interfaces,
failure cases and evidence Opus must produce. The RCA received Fable's third
review **APPROVE, no blockers**; this implementation handoff is a new document,
not a claim that its proposed types or C mechanism have been reviewed or built.

Work in an isolated checkout from then-current main. Preserve unrelated work
in Paul's shared checkout. Follow [AGENTS.md](../../AGENTS.md), the current
[development pipeline](../DEVELOPMENT_PIPELINE.md), and the
[validation contract](../VALIDATION.md). Implement ordinary details without
asking Paul to reapprove R1–R6. If an approach cannot meet a safety invariant,
record the failing proof and continue independent work; do not quietly weaken
the invariant to finish the milestone.

## 1. Outcome — repeated seeks preserve playback and charge real obligations

At the incident base, one rolling producer reserves 2 GiB + 64 MiB. Three
reservations charge 6,643,777,536 bytes; a fourth asks for 2,214,592,512 bytes
and exceeds the 8,589,934,592-byte default. Retirement preserves that full
reservation even after most future output is impossible. The create then
returns 500, and the refused target corrupts the incumbent's control snapshot
until its startup actor expires despite visible frames.

The completed repair must satisfy these observable outcomes:

1. Retired sessions shed unused future capacity after **all** writers settle,
   retaining charges for existing objects and open readers.
2. Repeated +10 input advances the desired target. Three separately committed
   taps from 100 seconds while create is pending mean 110, 120, 130; three
   taps within the existing coalescing window mean one +30 execution.
3. Identical pending creates are suppressed; changed recipes and explicit
   Retry remain possible. Every abandoned admitted create is eventually
   settled, even if its browser request was cancelled.
4. A refused or slow replacement cannot make the incumbent report a seek it
   is not executing. Healthy incumbent presentation survives the 30-second
   startup deadline. Genuine startup/local-seek failures still expire.
5. Real scratch exhaustion returns a classified 503 and preserves the
   incumbent and latest retry destination. No new automatic retry loop.
6. Eligible same-viewer releases shorten grace, with exact incarnation
   fencing and accepted reads protected. Native/unknown classes retain the
   existing promise until a shorter bound is proved.
7. Smaller reservations grow under **enforced** write limits. At least four
   low-bitrate rolling producers present under the unchanged 8 GiB cap on
   every supported Unix rolling writer path when their charges fit.

No finite cap promises unlimited instantaneous seeks. Tests must distinguish
false reservation pressure from genuine retained bytes, pending writers,
unsettled creates and pinned readers. A full scratch budget remains a valid
refusal, not permission to kill a healthy predecessor.

## 2. Decisions — implement these without reopening product scope

| Decision | Required behavior |
|---|---|
| R1 | Incumbent survival is part of A. Keeping the element attached momentarily is insufficient; prove server presentation state beyond the startup deadline. |
| R2 | Suppress an identical in-flight request before invalidating its generation. Compare the complete normalized recipe and identity, not just position. |
| R3 | Shorter retention with explicit safeguards. Proposed signal: existing exact-session DELETE, with create-time transport metadata and audited teardown ordering. |
| R4 | Smaller initial reservations with enforced growth. Native writes have an exact grant boundary; direct FFmpeg needs C's mechanism proof. |
| R5 | Preserve manual Retry and existing request ownership. No queue or automatic retry policy is added. |
| R6 | Paul selected **one combined promotion**. Review A, B and C into one effort; do not release A separately while B/C are unresolved. |

**Non-goals:** raising/disabling the cap; rolling buffered-seek reuse; a
second seek controller or independent position accumulator; changing the
control wire for R1; weakening actual startup proof; deleting another
viewer's objects; bypassing incarnation/owner fencing; changing immutable
VOD retention; reindexing production libraries; deploying during build work.

Unix is the incident/runtime qualification target. Windows is already merged,
not a future unimplemented port: its process-control abstraction supports
suspend/resume, while native runtime receipts remain open. Preserve Windows
compilation and existing conservative behavior. An unproved Windows growth
mode stays conservative and is reported as a limitation; do not silently
enable reduced reservations there or claim platform-wide runtime proof. Use
the [Windows status](../features/WINDOWS-PORT-STATUS.md) for that evidence.

## 3. Start — establish the exact base and compiler before editing Rust

The RCA's source evidence is
`a1414368400720884599732e3f8f3c71a9272edc`. During investigation, the shared
checkout was on older `a603b4e26` and contained unrelated uncommitted docs.
An isolated baseline checkout existed at `/private/tmp/plurx-seek-scratch`
on `codex/seek-scratch-accounting`. It is an investigation artifact, not the
required implementation base; inspect before reusing it.

1. Read the current repository instructions and docs index. Check the working
   tree, remotes and current main. Preserve every unrelated edit; do not
   hard-reset the shared checkout or stage all of its docs.
2. Create or reuse a clean isolated worktree from verified current main.
   Suggested integration branch: `effort/seek-scratch`; task branches:
   `codex/seek-scratch-a`, `codex/seek-scratch-b`, `codex/seek-scratch-c`.
   Check for existing branches/PRs before creating duplicates.
3. Carry this handoff and the RCA with their index rows into the effort in a
   documentation commit. If still untracked in the shared checkout, copy only
   these two files and add only their index rows to the destination index.
4. Verify the pinned compiler and run a baseline check before Rust edits.
   The earlier passing check proves only the evidence-base environment.
5. If the checkout host cannot run 1.97.1, establish the source-only
   [agent compile loop](../ci/AGENT-COMPILE-LOOP.md). Archive committed source;
   never transfer `.git`, repository credentials or tokens. Keep `target/`
   warm and repeat checks against the exact candidate after each base change.

```bash
git status --short                          # identify work to preserve
git rev-parse HEAD                          # record the actual baseline
rustup run 1.97.1 rustc --version            # must report the pinned compiler
rustup run 1.97.1 cargo check --workspace --locked --all-targets
make hooks                                 # adopt the tracked commit hook
```

**M0 acceptance:** receipt contains base SHA, worktree and branch, actual
compiler version, command/results, and a re-resolved source map. No CI run is
used to discover basic compile errors.

## 4. Source map and ownership — serialize changes to shared files

These are real paths/symbols at the evidence base. Re-resolve on the build
base; the shared older checkout predates the web split. Do not copy an old
monolithic web function over newer source. Proposed new type names in later
sections are design sketches, not existing APIs.

| Surface | Existing anchors | Work owner |
|---|---|---|
| `crates/plurxd/src/transcode.rs` | `RollingScratchReservation`, `reserve_rolling_scratch`, `global_flow_bytes`, `Session::refresh_scratch_bytes`, `spawn_copy_reader_owner`, retirement/cleanup owners, `prepare_retired_object_promise`, `apply_ahead_window` | A establishes ledger/barrier/pins; B adds exact release deadline; C adds producer growth. Sequential ownership. |
| `crates/plurxd/src/copyseg.rs` | `run`, `SessionDir::publish_file`, `write_init`, `write_segment`, playlist publication | A covers worker lifetime; C introduces exact grants before temp writes. |
| `crates/plurx-core/src/transcode/mod.rs` | FFmpeg argv builders, `COPY_SEGMENT_MAX_BYTES`, segment targets, publish gate | C only as required by the chosen writer mechanism; preserve codec/segment semantics. |
| `crates/plurxd/src/process_control.rs` | `ProcessSignal`, Unix and Windows `signal`, child ownership | C only if enforcement needs changes; reuse existing ownership and platform boundary. |
| `crates/plurxd/src/http/hls.rs` | `CreateSession`, `resolve_plan`, `session_start_error`, `delete`, `release_session`, `end_media_session_for_release`, streaming-body owners | A maps actual refusal and pins response lifetime; B carries release metadata; C classifies growth/startup failures. |
| `crates/plurxd/src/media_sessions.rs` and existing remote-start/activation definitions | Durable request/route identity, replay and exact owner epoch | B only where transport metadata must survive the existing placement path. A preserves current lifecycle authority. |
| `crates/plurxd/src/playback_control.rs` | `RollingStartupState::observe_control`, actor exchange/expiry, presentation proof | A real-actor regression; avoid policy changes merely to make a broken snapshot pass. C must preserve legitimate hold semantics. |
| `crates/plurxd/src/web/player/transport.js` | `nudge`, `commitPendingSeek`, `seekTo`, desired-position helper callers | A input accumulation and cancellation. |
| `crates/plurxd/src/web/player/decode-tiers.js` | `requestPlaybackMediaChange`, `executePlaybackMediaChange`, `startCopyHls`, create options | A dedupe and execution ownership; B transport metadata and fallback compatibility. |
| `crates/plurxd/src/web/player/session.js` and `crates/plurxd/src/web/playback-control.js` | `playbackControlSnapshot`, reporter capture/send | A incumbent-scoped observations and generation fencing. |
| `crates/plurxd/src/web/player/decode-margin.js` and `directed-change.js` | `retirePlaybackPredecessor`, `teardownHls`, `resetMediaSource`, `releaseSession` | B release-site audit and required ordering fixes; A stale-result settlement. |
| `clients/apple/Sources/PlayerController.swift` | Release at supersession and stop, item replacement | Read for B compatibility. No Apple edit required merely to retain its current grace. |
| `tests/playback/seek-control.test.js`, `web-policy.test.js`, Rust module tests | Real adapter functions and actor/lifecycle fixtures | Each unit extends its affected regression surface; no replacement mock implementation. |
| `validation/points.toml`, `validation/regressions.d/`, behavior docs | Path-to-test mapping and regression coverage | Each unit owns its evidence/catalog updates; integrate shared entries sequentially. |

The files overlap, so the independent-main-task exception in AGENTS does not
apply. Build A first; base B on the current effort after A, then C on that
effort. C's design investigation can proceed earlier without editing files
owned by another active unit. This plan does not require parallel agents.

## 5. Unit A — repair accounting, lifetime, input and incumbent survival

### 5.1 A1 — introduce one authoritative ledger

Adapt the existing reservation and admission gate. Do not leave the old
atomic total and a new ledger as competing authorities. Admission reads one
consistent ledger snapshot; live/retired maps remain lookup structures.

Suggested internal contract, with final names chosen to fit the code:

```text
ScratchKey = unique local allocation identity bound to exact session/attempt
Entry = { key, lifecycle, charge_bytes, generation, writer_count,
          linked_bytes, unlinked_pinned_bytes, unused_grant_bytes }

reserve(key, initial) -> owned permit or classified CapacityUnavailable
bind_session(permit, exact_session, attempt) -> same entry, no reacquisition
begin_retirement(key) -> fence new writers, keep conservative charge
commit_quiescent_measurement(key, generation, inventory) -> atomic conversion
account_unlink(key, object_identity) -> remove name; preserve existing pins
release_pin(key, object_identity) -> release only after last pin and unlink
release_unused(key) -> idempotent release after no writer can consume it
snapshot() -> one generation, total and mutually exclusive categories
```

Choose a provisional allocation key **before** a producer/session is exposed.
If the session ID is assigned later, bind the existing key rather than
dropping and reacquiring capacity. Include local incarnation/attempt identity
where IDs can be reused; a stale callback cannot resize its successor.

**Invariants:**

- Each incarnation has exactly one charge throughout provisional, producing,
  retiring, retained, deleting and released states. Registry moves do not
  change the total. Provisional means an actual unpublished start.
- A preserves the full producer reservation until writers settle. C later
  replaces producer sizing; do not accidentally introduce partial growth in A.
- Admission, resize and explicit release linearize under a short ledger
  critical section. Use checked nonnegative byte arithmetic and reject
  overflow. Do not hold this lock over `.await`, filesystem I/O, process
  waits, actor exchanges, response sends or grace timers.
- Filesystem scanning happens outside the lock under a writer/mutator fence.
  Commit only if the measurement still belongs to the exact entry and valid
  inventory generation; otherwise retry while retaining the conservative
  charge. Coordinate cleanup mutations with this protocol too.
- Measure actual bytes even when they exceed the original reservation. A
  discovers and charges an overrun; it must not clamp the sample to conceal it.
- Category totals come from the same snapshot. Do not infer provisional
  charges by subtracting live/retired folds from an independent atomic total.
- Drop is a backstop, not permission to free bytes still owned by a worker,
  namespace or reader. Request cancellation transfers ownership to cleanup.

**A1 acceptance:** reproduce the old live→retired double-fold interleaving
with deterministic barriers and a genuinely provisional start. After the
change, each key is counted once; concurrent fitting admissions cannot spend
the same freed capacity twice. Preserve the manual-drop admission test, but
do not use manual drop as the new conversion proof.

### 5.2 A2 — settle every writer before conversion

Install writer ownership before spawn/pipe installation. The native copy
worker owns its completion guard until it has finished draining, publishing
and disposing its output authority. FFmpeg reap alone is insufficient.

1. Inventory scratch writers: direct FFmpeg muxer, native copy reader,
   playlist/publication writes, subtitles if colocated, and any detached
   mutator. Put every byte-producing path under the lifetime fence.
2. Give cancellation-independent retirement ownership the process-reap
   evidence and the writer completion barrier. Fence new writer registration
   before it begins waiting; registrations already issued remain counted.
3. Preserve existing pipe/classification ordering through
   `classify_copy_producer_exit_before`. A late classification rejected as
   `SessionEnded` must still signal actual worker completion.
4. Cover spawn failure, pre-install cancellation, panic, failed final write,
   and executor handoff. A completion guard may drop only after the worker
   can no longer write; task abortion with outstanding blocking I/O is not
   automatically that proof.
5. Once quiescent, reuse `refresh_scratch_bytes`. Change its result contract
   to distinguish a complete sample from a retained old value on read/stat
   error. Include regular temp/init/playlist/segment/garbage files once.
6. Convert the charge atomically to retained inventory plus independent pins.
   A stalled writer keeps the conservative charge and emits a reason; a
   timeout must not pretend capacity became free.

Record one receipt sentence confirming the flat scratch layout and existing
regular-file/symlink scanner semantics. No duplicate scanner or unrelated
filesystem traversal redesign is needed.

**A2 acceptance:** hold the native worker immediately before a final write,
reap FFmpeg and initiate retirement. Charge remains conservative; after the
write and actual completion, inventory includes the tail and conversion
occurs once. Repeat with partial scan error, panic and cancellation. Actor
terminal state is not delayed merely to wait for accounting cleanup.

### 5.3 A3 — make cleanup and reader pins explicit

Use the existing streaming response owner if it can carry an object pin;
inspect `StreamedBodyTerminal` and the media response stream constructors.
Acquire the pin for the opened exact object before cleanup can remove its
accounting ownership. A raw pathname is not an immutable object identity.

Count an object's apparent byte length once while it is linked **or** pinned.
Multiple ranges/readers do not duplicate the charge. On unlink, transfer
ownership to pins if needed; on final reader close, release it. Use actual
object identity or a safe immutable-generation equivalent so a later file
at the same path is a distinct allocation. Playlist replacement must also
handle simultaneous old/new objects without losing the old reader's bytes.

Maintain the existing grace for A. Cleanup can remove eligible names, but a
failed unlink retains the charge and retry owner. Missing directories are
idempotent only once writers cannot recreate them. Unregistered failed starts
need the same cleanup authority. EOF, downstream cancellation and error all
drop the response pin. Unrelated status/session Arcs must not hold the full
reservation after successful cleanup or resurrect it as provisional.

These are apparent logical bytes, not `st_blocks` or physical reclamation.
Keep the existing physical free-space guard separate. Avoid double counting
the same linked object both in the scanner sample and as a read pin.

**A3 acceptance:** a range response survives grace expiry and unlink, remains
charged exactly once, then releases on close. Test concurrent readers,
response cancellation, failed deletion, duplicate cleanup and a lingering
non-reader session Arc. Windows sharing-related unlink failure stays charged.

### 5.4 A4 — classify admission failure through the real HTTP path

Use the existing `capacity_error` convention or typed equivalent returned by
the actual reserve operation. Feed that error through `session_start_error`:
scratch exhaustion must yield 503, with requested/charged/configured bytes
in bounded server diagnostics. Do not classify arbitrary unrelated errors as
capacity failures or test only a hand-constructed prefix string.

The browser must show the existing retryable change refusal, retain desired
target/recipe, and preserve the incumbent. No automatic retry queue, false
seek completion or movie-corrupt/stopped surface for this failure.

**A4 acceptance:** fill the ledger with real retained obligations, attempt
create, and exercise the handler and client failure mapping. Assert 503,
specific temporary-capacity treatment, no incumbent teardown, and Retry
using the latest desired request.

### 5.5 A5 — separate desired intent from attached execution

Keep one source of desired position under the existing viewer intent. Reuse
`positionForPlaybackIntent` and the pending media-change recipe. Distinguish
an intended replacement from a seek currently mutating an attached element.

| Situation | Input/UI position | Incumbent reporter |
|---|---|---|
| Replacement pending | Latest desired target accumulates relative input. | Actual incumbent position/render state/selection; no foreign seek target. |
| Replacement refused | Keep target/recipe for Retry and new input. | Continue honest incumbent observations; notify promptly when foreign execution is detached. |
| Replacement attaches | New attachment owns destination landing. | Old capture/send work is fenced; successor reports its own state. |
| Direct or immutable-VOD local seek | Existing local target and clamp behavior. | Actual local seek still reports seeking/target until landing. |
| Close, title change, cancellation | Clear only the intent owned by the ended operation. | Stale callbacks cannot mutate a newer attachment or reporter. |

Suppress identical **in-flight** requests before incrementing the generation
that would invalidate the still-authoritative execution. Compare normalized
file, target, playback identity and the full requested selection/recipe:
audio, subtitles, quality, codec/dynamic range and other output-affecting
options. Do not dedupe by position or an incomplete ad hoc key. Explicit
Retry after refusal/completion is a new attempt, not the same in-flight work.

Distinct latest targets may still leave abandoned creates unresolved. Every
late success must release its admitted session under existing ownership.
AbortSignal delivery alone is not proof the server producer stopped. Do not
impose a fictitious two-producer upper bound on the ledger or tests.

Fence observations at capture **and** send by the existing attachment/session
generation and control epoch. Do not globally clear desired state to make
snapshots look healthy; that would lose Retry/relative-seek behavior. Do not
force `rendering` when the actual element is paused, waiting or failed.

**A5 acceptance:** test the shipped adapter functions, not rewritten helpers.
Exercise rapid coalesced input and separately committed +10 taps while the
executor remains pending; assert targets, execution counts, generations,
final attachment and release of late results. Include reverse direction,
clamps, keyboard/pointer, changed recipe at the same target and explicit Retry.

### 5.6 A6 — prove incumbent survival through the real actor

Use real scoped web snapshots as input to the existing control handler/actor.
The incumbent's control URL, `generation` and `control_epoch` already identify
its scope. R1 requires no wire extension.

Drive two accepted rendering observations with at least 250 ms of media
position advancement. Establish `Presented`, advance beyond the original
30-second startup deadline, and keep the replacement refused or unresolved.
The incumbent must remain live. Include an old asynchronous target callback
and a late create after another attachment wins.

Keep negative cases: a stream with no accepted presentation progress expires;
a genuine local seek that does not land is not hidden. Browser `playing`
alone is not actor proof. No test may bypass observation by setting
`Presented` directly for the scenario intended to prove R1.

**A completion:** A1–A6 pass against the exact task head, the old behavior is
demonstrably caught, and the incident plus twenty-seek fixtures pass. Record
the **first-green UTC date and SHA**, current revalidation SHA, commands and
test counts. Review into the effort; R6 prohibits separate promotion.

## 6. Unit B — shorten only a proved exact-session release

### 6.1 B1 — propagate create-time transport metadata

Proposed additive create contract:

```text
transport: "hlsjs" | "native" | omitted

effective release class:
  hlsjs + verified client release contract -> eligible for short grace
  native / omitted / unrecognized / ambiguous -> original grace
```

This field belongs to `CreateSession` and exact session metadata, not the
control snapshot, media codec choice or general capability label `hls`.
Native Apple clients may omit it and keep existing behavior. The web client
must use its selected actual transport, not user agent or server guess.
Unknown strings must not opt into short grace; bound/normalize them using
the API's normal validation without creating arbitrary metric labels.

Implement the complete propagation path: public create parsing, selected
recipe, durable request replay semantics, local and remote owner start,
prepared/abandoned sessions where relevant, terminal/release resolution, and
retained metadata. Reuse existing recipe metadata if it already carries the
necessary persistence; do not invent a schema migration without need. If a
migration is needed, add the normal migration/old-record tests.

Treat retention-relevant metadata as immutable request semantics under the
existing fingerprint/replay rules. A replay of the same accepted request
cannot silently change class, and an old node dropping an unknown field must
leave the resulting owner conservative. An owner restart/takeover must not
upgrade missing metadata to hls.js. Record the mixed-version behavior.

Audit **every release site** reachable by a client that declares `hlsjs`:
predecessor replacement, close, error, title change, prepared abandonment and
late successful create. Attached instances must be destroyed before DELETE;
never-attached sessions must have no live consumer. Either fix the ordering
at all eligible sites or keep that session conservative. Transport metadata
alone is not a teardown acknowledgement. If actual transport can fall back
to native within the same session, choose a conservative class at create or
start a correctly classified session; do not retain stale short eligibility.

**B1 acceptance:** new hls.js client, native client, legacy omission, unknown
value, remote placement, replay, owner replacement and transport fallback
all produce the expected retained class. Missing/ambiguous information never
shortens grace. Control protocol remains unchanged.

### 6.2 B2 — consume DELETE under existing release authority

The actual route is `DELETE /api/v1/hls/{session}` → `release_session` →
`end_media_session_for_release`. The durable route resolves the owner epoch;
the public request has no browser-supplied epoch. Apply the deadline to the
exact retired incarnation resolved by that coordinator, including remote
projection. Preserve capability authentication and idempotent responses.

```text
new_deadline = min(original_deadline,
                   first_accepted_eligible_release_time + allowance)

eligible hls.js allowance = one truthful session segment target
                         = 16 seconds at the evidence base
```

Duplicate DELETE cannot extend the deadline. Stale/unknown ID, wrong resolved
epoch/incarnation and another viewer's release are no-ops on this entry.
Ensure late callbacks cannot update a new entry reusing an identifier. The
read admission fence and cleanup/pin transfer must agree atomically on which
reads were accepted before the deadline.

Safari native source replacement is known: it removes the old src and calls
`load()` before setting the new source. Its retry tail is unproved, and DELETE
may precede reset. Apple replacement explicitly releases before replacing
the AVPlayerItem. Keep original grace for both, and for AirPlay, unknown,
unaudited and no-DELETE paths. Do not derive retry allowance from the 20 s
server `SEGMENT_WAIT`. Existing hls.js retries do not outlive destruction.

**B2 acceptance:** exact release shortens only the eligible entry and never
renews it; old accepted range reads finish beyond the deadline and remain
charged; new post-deadline reads receive the established gone response.
Another viewer and native/legacy sessions retain their promises. Measure
80 Mb/s retained backlog at stated tap cadences; report real-cap refusals
honestly rather than claiming all native 4K seek storms are solved by B.

## 7. Unit C — grant capacity before a writer can spend it

### 7.1 C0 — select and prove the direct-writer mechanism

This is an implementation milestone, not permission to defer R4. Produce a
small mechanism decision and executable proof before enabling smaller
reservations for direct FFmpeg output. A and B may progress while it is built.

Direct HLS uses FFmpeg's muxer and filesystem paths. Rust currently has no
pre-write hook there. Existing `Bytes`/`Global` holds suspend the process;
they are useful but do not by themselves bound bytes produced before a
delayed suspension. The default 90 s initial burst runs ahead of normal 2×
pacing, the 15 s repair interval is not a maximum evaluation gap, bitrate is
estimated, and already-issued writes have a tail.

Evaluate mechanisms against the following gate, selecting the smallest one
that actually passes:

| Candidate | Proof required | Insufficient substitute |
|---|---|---|
| Owned output/write boundary | Every output file/write, temp/init/playlist and overwrite passes a grant boundary; muxer semantics, secure output paths, bounded memory and cancellation are preserved. | Assuming the native copy writer already owns direct/transcoded muxer output. |
| Hard writable quota for the session | Aggregate charged namespace/output is constrained before overflow; grants update it safely; all files and supported deployment filesystems are covered; quota errors settle and classify correctly. | `RLIMIT_FSIZE` alone: a per-file limit is not an aggregate directory limit. Privileged host provisioning cannot be silently assumed. |
| Enforced production envelope plus hold | Enforced maximum output rate/burst, bounded output buffering, an enforced evaluation/suspension deadline and bounded write tail yield a finite allowance under startup, VBR and delayed work. | Declared/measured average bitrate × readrate × a nominal timer, faster polling alone, or a watcher that notices overflow after writes. |

Record which constraints are enforced by code/OS and which are measurements.
An observed stress-test maximum is not a hard bound. Prove the chosen method
with adversarial input, delayed evaluation, no client fetches and multiple
writers. If the mechanism needs an external filesystem/service or changes
the media pipeline substantially, surface the concrete deployment/codec
consequence for review; do not disguise that as a completed local refactor.

If no candidate meets the contract, leave direct FFmpeg conservative, record
the smallest remaining mechanism decision and keep C/promotion incomplete.
Do not mark R4 done from copy-only success. Do not ask Paul to choose between
uninvestigated alternatives; first supply the proof/failure and recommendation.

**C0 acceptance:** decision, exact interception/enforcement points, numeric
bound derivation, failure/cancellation behavior, platform support matrix and
an executable overrun test. This becomes C's implementation contract.

### 7.2 C1 — exact native-copy grants

`copyseg::SessionDir::publish_file` has the whole slice before writing its
temporary file and renaming it. Acquire a grant for the actual next file
there, covering init, media and playlist writes. `Limits.max_bytes` / 64 MiB
is a cut-policy threshold with a crossing fragment, not a hard file maximum.

Under the same authoritative ledger, atomically allocate unused credit or
grow the charge if the global/per-session policy permits. Transfer that
credit into used bytes without an uncharged gap. Concurrent writes cannot
spend the same grant. Retain old/new playlist overlap until old names/pins
are settled. On cancellation or failure, return only proven unused credit;
partial temp output stays charged and owned until cleaned up.

If credit is unavailable, backpressure the Rust writer, not merely FFmpeg.
Its pipe reader can still drain and write after process suspension. Bound
buffer memory; cancellation must wake a waiter and must not strand the
writer-completion barrier. Avoid lock ordering that waits for a grant while
holding a lock needed by cleanup to return capacity. Prevent growth starvation
with an explicit bounded waiter/fairness policy appropriate to existing flow
ownership; this is not a new client retry queue.

**C1 acceptance:** refuse before an oversized next slice writes; grow when
capacity is available; preserve old/new playlist accounting; cancel a waiter
and an in-progress write safely; release capacity through actual cleanup.

### 7.3 C2 — implement direct-FFmpeg enforcement and initial sizing

Implement C0's selected mechanism. For an envelope approach, the accounting
contract is `charge = actual + remaining enforced envelope`: regrant before
resuming and preserve hold when growth is denied. Use one ledger across
native/direct producers. Never shrink an allowance below already-issued
write obligations, or describe a quota-killed child as safely held.

Compute initial sizing from effective settings and all enabled publish gates:

```text
effective_gate = max(writer_gate, served_playlist_startup_gate, other enabled gates)
startup_media = effective_gate + maximum_complete_segment_duration
known_rate_initial >= estimated_output_bytes(startup_media) + enforced_envelope
unknown_rate_initial = 256 MiB + enforced_envelope  # proposed bootstrap
```

At the evidence base: copy gate 12 s; preferred segment ceiling 15 s; truthful
segment target 16 s; rolling startup gate 48 s at 1×, scaled with playback
rate. Thus known-rate copy/rolling sizing uses at least **64 seconds** at 1×,
not 27. At modeled 80 Mb/s this is 640 MB before enforcement overhead. Account
for all tracks and uncontrolled initial burst beyond that allowance. Re-read
configuration and rate; short-title ENDLIST may open publication earlier.

Estimates size the first grant, never authorize ungranted writes. Test unknown
80 Mb/s output and declared 16 Mb/s / actual 80 Mb/s output. The 256 MiB
bootstrap must grow if required. If no grant can cover a publishable startup,
produce bounded classified refusal/cleanup instead of waiting forever for a
client that has no served playlist to drain. Before first publication this
is a create/start failure; after the response is committed, use the existing
session failure/control path rather than pretending an HTTP status can be
changed retroactively. Preserve the predecessor until successful handoff.

Choose and document growth granularity, low/high water behavior and bounded
startup-wait timing from this proof. They are implementation constants to
justify and test, not missing product preferences. Keep physical disk guard,
per-session limits, startup protection and publication progress distinct.

**C2 acceptance:** force delayed evaluations beyond 15 s and a full 90 s burst,
oversized/VBR output, suspension latency/tail, no segment requests and a cap
too small to finish startup. Actual obligations never exceed admitted capacity
under the proven mechanism; no deadlock, hidden overrun or false progress.

### 7.4 C3 — qualify every intended writer and report remaining platforms

Run at least four low-bitrate streams under 8 GiB through native copy and
each direct/transcoded Unix writer path that survives current routing.
Verify actual presentation and later growth; creating four empty directories
does not prove the product result. Exercise copy audio conversion and output
variants that select another writer. Record skipped/unavailable paths by name.

Keep Windows build/test-target compilation green and conservative behavior
where smaller-reservation enforcement is unproved. Document its runtime
receipt gap through the existing port status; do not invent native evidence.
Any enabled platform-specific small-reservation mode must meet the same
invariants before activation.

**C completion:** C0–C3 pass for supported Unix writers; startup liveness and
underestimated bitrate tests pass; exact constants and platform limitations
are recorded. Conservative direct FFmpeg on Unix is an incomplete C.

## 8. Regression matrix — evidence must exercise the actual failure

Use the RCA §6 as the complete behavioral checklist. The matrix below assigns
minimum ownership and test shape. Prefer deterministic barriers/fake clocks
for races and deadlines; physical playback supplements, not replaces, them.

| ID | Owner | Required regression |
|---|---|---|
| A-L1 | A | Default one playback + three replacements reproduces old refusal and now fits when real retained inventory fits. |
| A-L2 | A | Twenty 128 MiB retired inventories, normal grace, exactly one incumbent and successor: 6.625 GiB with full A reservations; all fitting admissions succeed. Use real lifecycle conversion, no manual permit drops. |
| A-L3 | A | Barrier-driven map-transfer interleaving plus provisional start; simultaneous reserve/convert/release cannot double count or spend capacity twice. |
| A-W1 | A | Reap child while native worker has a last write pending; tail is charged before conversion. Cancellation/panic/spawn failure preserve ownership. |
| A-M1 | A | Partial read/stat failure retains the previous conservative measurement; later complete scan settles it. Sparse files test apparent lengths. |
| A-P1 | A | Unlink while multiple ranges/readers are open; one object charge until final close. Failed delete, EOF, error and cancellation settle exactly once. |
| A-C1 | A | Cleanup removes names but an unrelated Arc remains; no provisional residue or double release. Include unregistered failed starts. |
| A-H1 | A | Real reserve error through actual HTTP mapper returns 503; real exhaustion remains refused. |
| A-I1 | A | Full adapter: three coalesced +10 taps → one +30 create; three separate pending taps → 110/120/130; identical pending recipe → one execution. |
| A-I2 | A | Same target/new recipe, explicit Retry, reversed direction, close/title change and late-success release; newest target wins. |
| A-S1 | A | Refused and >30 s pending replacement: real web snapshots establish actor Presented and incumbent survives. Honest unpresented/local-seek failure still expires. |
| B-R1 | B | Exact DELETE changes one eligible retired deadline once; stale ID/epoch, other viewer, unknown/native/legacy do not shorten it. |
| B-R2 | B | Transport survives local/remote create, replay and retained release; missing metadata stays conservative. Audit release ordering and fallback. |
| B-P1 | B | Read admission races shortened deadline; accepted pin survives, later new read is gone. |
| C-G1 | C | Native actual-slice grant includes crossing fragment, temp/init/playlist overlap; simultaneous writers cannot reuse credit. |
| C-G2 | C | C0 mechanism survives initial burst, delayed evaluation, underestimated rate and outstanding writes at hold. No fetch needed for enforcement. |
| C-S1 | C | Effective startup gates at supported rates, short ENDLIST, unknown 80 Mb/s and 5× underestimated bitrate; grant growth or bounded refusal. |
| C-N1 | C | Four or more low-bitrate producers start, present and grow below unchanged cap on each intended Unix writer path. |
| ALL-1 | Combined | 80 Mb/s seek series at recorded cadence/window, mixed viewers, all pending creates and readers counted; fitting requests succeed, real pressure preserves incumbents. |
| ALL-2 | Combined | Direct play and immutable VOD retain local-seek behavior; startup/decode/codec and publication contracts remain intact. |

For HTTP/accounting tests use a proposed `scratch_charge_` prefix, and use
discoverable `seek_scratch_` names for new actor tests. These filters are not
existing evidence: list matches and record a positive executed test count.
Bind the exact new commands into the validation catalog; adding a test file
that no selected check runs does not satisfy acceptance.

Starting commands, to be refined with exact test names in each receipt:

```bash
rustup run 1.97.1 cargo fmt --all -- --check
rustup run 1.97.1 cargo check --workspace --locked --all-targets
rustup run 1.97.1 cargo clippy -p plurxd --all-targets --locked -- -D warnings
rustup run 1.97.1 cargo test -p plurxd --bin plurxd scratch_charge_ --locked -- --list
rustup run 1.97.1 cargo test -p plurxd --bin plurxd scratch_charge_ --locked
rustup run 1.97.1 cargo test -p plurxd --bin plurxd seek_scratch_ --locked
rustup run 1.97.1 cargo test -p plurxd --bin plurxd mkv_hls --locked
rustup run 1.97.1 cargo test -p plurxd --bin plurxd copyseg --locked
node --test tests/playback/seek-control.test.js tests/playback/web-policy.test.js
make web-check
scripts/validate lint
python3 -m unittest discover -s tests/operations -p test_docs_index.py
git diff --check
```

Extend Clippy/focused tests to changed core crates and use the current Windows
MSVC compile command from validation rather than inventing a new lane. The
commands above do not replace repository-required checks or final exact-tree
qualification. Run the smallest relevant tests during development, then the
required affected-surface checks; do not repeatedly rerun unrelated suites.

**Physical receipt:** use forced rolling HLS in Safari, then hls.js for the
short-grace class. Test rapid taps, one-second committed taps during a slow
create, settled playback seeks and deliberate refusal beyond 30 s. Record
desired/presented positions, transport, writer, actual bitrate, tap cadence,
cap, producer/retained/pin charges, unresolved creates, cleanup timing and
incumbent survival. Include four low-bitrate producers and 80 Mb/s output.
Use test media and sanitized session labels. Do not infer success from a
small scratch directory after playback has ended.

## 9. Integration and promotion — one qualified candidate

1. Commit each unit normally and record its focused regression command in
   its PR. Target the current effort, not main. The hook is not a test suite.
2. Pass `Effort development gate` and required manual compiler/evidence
   checks on the exact task head before merging into the effort. Rebase or
   merge the current effort and reverify when it moves.
3. Keep A's first-green timestamp even if B/C take longer; update its current
   revalidation SHA after shared files change. Unit completion is not
   deployment, and production remains unfixed by this effort until deployment.
4. When A/B/C and combined acceptance are complete, freeze task merges, sync
   current main into the effort and open the promotion as draft. Obtain the
   required adversarial code review; the RCA reviews do not review the patch.
5. Follow the pipeline's current workflow correction and AGENTS together:
   manual effort/compiler evidence, draft-to-ready main fast lane, blocking
   `Main promotion gate`, and required exact-candidate qualification receipt.
   Do not rely on superseded claims that merge automatically builds/deploys.
6. Verify current head/base/tree against the receipt. Any movement invalidates
   that qualification; requalify the new candidate. No stale green result,
   missing Windows compile or unrun test filter is acceptable evidence.
7. Update the playback/operations docs and both document status headers to
   the actual delivered behavior. Keep native grace and any platform growth
   limitation explicit. Deployment is a separate authorized operation.

Do not split off A for emergency shipment without a new owner decision that
changes R6. Do not turn unresolved C into an unmentioned follow-up while
calling the combined effort complete.

## 10. Implementation receipt — leave the next reviewer concrete evidence

Maintain a receipt in the effort's normal review record or an indexed subject
document. Use this schema; `pending` is an honest value and must not become
`pass` without the stated evidence.

| Receipt field | Required content |
|---|---|
| Source identity | Base SHA, unit head SHA, current effort SHA; final promotion head/base/tree and qualification link. |
| Environment | Compiler version, OS, FFmpeg build, writer/transport paths exercised, relevant settings. |
| A first green | UTC date/time, exact SHA, commands, test counts; current revalidation state separately. |
| Accounting proof | Entry key/ownership, lock order, writer fence, measurement result semantics, pin identity, once-only release; flat-directory scanner audit sentence. |
| B release proof | Final create field semantics, replay/remote propagation, release-site audit, eligible classes, allowance, legacy/native fallback. |
| C mechanism | Selected enforcement boundary, enforceable bound derivation, actual constants, failure/hold behavior, startup-liveness result, platform matrix. |
| Regression results | IDs from §8, exact commands, executed counts and failure-before/fix-after evidence where applicable; explain genuine unavailable cases. |
| Physical acceptance | Sanitized trace/recording and measurement artifact for repeated seeks, true exhaustion, 4K backlog and four producers. |
| Review/qualification | Code-review findings and disposition; required gates; exact-candidate receipt; limitations and deployment status. |

**Definition of done:** the implementation, regression catalog, behavior docs
and receipts agree; A/B/C satisfy their contracts; combined acceptance and
current-candidate promotion gates pass. Until then, report the completed units
and the precise remaining gate. A green compile or approved design alone is
not a repaired playback system.
