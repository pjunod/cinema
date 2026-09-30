# FFmpeg scratch sink — grant disk writes without replacing the HLS muxer

**Status:** historical design proposal; the direct-FFmpeg write boundary is
built and reviewed, with qualification recorded in the
[implementation receipt](../evidence/ffmpeg-hls-write-boundary-receipt-2026-09-24.md) ·
**Executes:** the then-unfinished direct-FFmpeg portion of R4 from
[the seek/scratch RCA](SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX.md#48-r4--reserve-the-initial-burst-and-authorize-growth-before-writes) ·
**Written:** 2026-09-24 · **Evidence base:**
`b1de0964762d0a9157af30b9d25657d13444b99e` on `origin/main`.

Read §1–§3 for the decision, §4–§8 for the contracts, and §9–§12 for the
work packages, acceptance evidence and review questions. This is a successor
to the R4 mechanism gate in the
[earlier implementation plan](SEEK-SCRATCH-RESERVATIONS-IMPLEMENTATION.md),
not a reopening of R1/R2 or a claim that this transport has been proved.
The source anchors were checked against main; re-resolve the symbols on the
implementation base. Proposed names below are explicitly design sketches.

The sections below retain the proposed review and feasibility sequence.
The implemented loopback PUT boundary and its actual proof are in the
[receipt](../evidence/ffmpeg-hls-write-boundary-receipt-2026-09-24.md);
proposed interfaces and milestone names here are not implementation evidence.

## 1. Decision — own the writes, retain FFmpeg's muxer

Keep FFmpeg's codec, filter, segment and HLS responsibilities. Redirect its
rolling output to an attempt-scoped loopback HTTP receiver in plurxd. The
receiver obtains byte grants before writing each chunk, stages each object,
and publishes only a completed object. The existing rolling publication
clock remains the authority over what clients may see.

```text
resolved movie recipe + attempt-local output target
                        │
                        ▼
              FFmpeg HLS muxer
                        │ HTTP PUT; bounded transport buffers
                        ▼
             attempt-scoped sink route
                        │ obtain byte credit before disk write
                        ▼
             ScratchLedger + owned object inventory
                        │ stage → complete → atomic publication
                        ▼
             existing rolling publication / serving / retention
```

The HTTP receiver is the candidate mechanism, not an already supported
production path. FFmpeg documents HLS HTTP PUT output and temporary-file
renaming in its [HLS muxer reference](https://ffmpeg.org/ffmpeg-formats.html#hls-1).
That establishes a place to investigate; it does not prove fMP4 init upload,
all URI forms, cancellation, backpressure or Windows replacement behavior.
M1 must establish those facts using the actual supported binaries.

**The intended result:** under the unchanged 8 GiB default scratch budget,
at least four low-bitrate rolling FFmpeg sessions start and continue
presenting when their retained working sets fit. The grant boundary remains
safe with unpaced FFmpeg, wrong source bitrate, stalled flow evaluation and
cancelled requests. Source bitrate and encoder settings may improve sizing;
they do not authorize bytes.

### 1.1 What this plan deliberately does not change

- Do not raise the global cap or the configured ahead-byte ceiling to make
  the acceptance case pass.
- Do not introduce a feature switch. Qualified sink output is the normal
  rolling FFmpeg route when this effort ships.
- Do not replace FFmpeg's HLS muxer with `copyseg`, alter codec recipes,
  tone-mapping, Dolby Vision handling, segment targets, or startup gates.
- Do not refactor immutable VOD, progressive remux, Live TV or DVR output
  into this receiver. Inventory shared helpers so their behavior stays
  unchanged; a path discovered to share this exact rolling allocation must
  be accounted for rather than dismissed by its name.
- Do not change seek intent ownership, predecessor survival or release
  grace. Retain the existing R1/R2/R3 contracts and regression coverage.
- Do not claim zero physical filesystem growth from an apparent-length
  ledger. Filesystem metadata, allocation granularity and unrelated writers
  remain outside that metric.
- Do not deploy as part of implementation. Produce the qualified candidate
  and review receipts through the repository's delivery process.

### 1.2 Why the timer proposal is rejected

`regrant_rolling_scratch` holds only when growth fails **and** the ledger
reports the current grant exhausted. That is too late for a writer which
does not consult the ledger. A 15-second repair interval is not a deadline
on measurement, locks, actor work or effective process suspension.

Rolling HLS defaults are 2× readrate and a 90-second initial-burst setting;
the progressive stream's 4×/30-second settings do not describe this path.
Average source bitrate is not a burst bound. VBV is not a wall-clock disk
rate. Renaming a temporary segment does not copy its payload, although old
and replacement playlists can coexist. These facts rule out changing a
constant, removing the envelope guard or adding a watchdog as the R4 fix.

The conservative direct-writer reservation also remains an operational
margin, not a proof that the external process cannot exceed its reservation.
Until every rolling writer is covered, distinguish the new sink's guarantee
from the remaining legacy path's limitation.

## 2. Current source — where the work belongs

The symbols below existed at the evidence base. Line numbers are omitted
because this repository is moving; locate the named implementation, not a
similarly named test-only compatibility builder.

| File | Existing symbols / behavior | Required use |
|---|---|---|
| [transcode.rs](../../crates/plurxd/src/transcode.rs) | `reserve_rolling_scratch`, `RollingScratchSizing`, `regrant_rolling_scratch`, `Session::measure_scratch_bytes`, `refresh_scratch_bytes`, `publication_cycle_at`, `apply_ahead_window`, retirement/conversion owners | Route selection, allocation lifetime, owned accounting integration, publication, flow and cleanup. |
| [scratch_ledger.rs](../../crates/plurxd/src/scratch_ledger.rs) | `authorize_write`, `ScratchWrite`, `observe_used`, `regrant`, writer barrier, pins | Extend the existing budget authority. Do not create a second scratch budget. |
| [copyseg.rs](../../crates/plurxd/src/copyseg.rs) | `SessionDir::publish_file`, `authorize_write` | Preserve exact grants; test compatibility with shared ledger changes. |
| [core transcode/mod.rs](../../crates/plurx-core/src/transcode/mod.rs) | `TranscodeExecution`, `hls_args_for_plan`, direct HLS builders, `Pacing` | Add an attempt-local output target without changing resolved semantic recipes. |
| [core encoder.rs](../../crates/plurx-core/src/transcode/encoder.rs) | `encode_args_for`, `hdr10_encode_args` | Read sizing inputs; do not alter rate-control policy. |
| [ffmpeg.rs](../../crates/plurxd/src/ffmpeg.rs) | capability discovery and `PacingCaps::resolve` | Bound and identify the output capability probe; bind its result to the actual executable/build. |
| [http/hls.rs](../../crates/plurxd/src/http/hls.rs) | create/refusal, object serving and read lifetime | Preserve client URL shape; bind pins to object versions where replacements require it. |
| [playback_control.rs](../../crates/plurxd/src/playback_control.rs) | hold vocabulary, flow intentions, terminal outcomes, metrics | Distinguish a sink waiting for capacity from a process suspended for supply. |
| [core process/mod.rs](../../crates/plurx-core/src/process/mod.rs) | owned process signals | Keep existing owner; suspension is optional flow control, never the sink's disk boundary. |
| [core manifest.rs](../../crates/plurx-core/src/transcode/manifest.rs) | bounded playlist reader and object validation | Reuse appropriate parser/limit contracts; do not import immutable-VOD restrictions blindly. |
| [web shell map](../clients/WEB-SHELL-LAYOUT.md) | current Developer/activity file ownership | Locate the actual diagnostic UI before editing it; preserve script order and asset contracts. |

Proposed new modules are `crates/plurxd/src/scratch_sink.rs` for receiver and
lifetime ownership, and a focused owned-object module beside
`scratch_ledger.rs` if that keeps protocol handling out of accounting.
Register modules through the existing crate root. Test-module placement
follows the current repository convention. These are proposed paths, not
claims that files already exist.

## 3. Hard contract — credit precedes every possible file extension

### 3.1 Define what is bounded

For each owned allocation, keep distinct quantities:

| Quantity | Meaning |
|---|---|
| `G` | Total granted capacity for this allocation. |
| `L` | Apparent lengths of linked objects, including partial and staging objects. |
| `P` | Bytes reserved for issued operations that may extend files but have not settled. |
| `U` | Apparent lengths retained only by accepted readers after unlink/replacement. |
| `C` | Charged capacity, `max(G, L + P) + U`. |

The implementation may conservatively overcount while a write settles.
It must never undercount. A successful operation that can increase `L` must
first obtain enough `P` under the same budget lock that checks projected
global charge. Settlement moves credit into object length; it does not
release landed bytes. For an existing grant, `L + P <= G` must remain true.
Pinned-extra growth at unlink must also be included in the same transition.

With a fixed positive global cap, correct initial inventory, and all writers
owned, admission and mutation maintain `sum(C) <= cap`. Each allocation's
actual linked and pinned bytes are covered by its charge. **This is an
inductive invariant over writes and mutations, not an observation at poll
time.** If the operator lowers the cap below committed obligations, preserve
those obligations, deny increases, and report overcommit until it drains.
Existing over-cap legacy inventory is reported honestly; it is not clamped.

Zero-cap/unlimited semantics must remain compatible with existing settings.
Qualification of a hard cap always uses a positive finite value. Reject
negative sizes, arithmetic overflow and invalid object transitions.

### 3.2 The complete namespace is in the contract

M0 inventories every writer and mutator of a rolling session directory:
FFmpeg objects, daemon-generated publication files, subtitle sidecars,
temporary files, retention renames, deletes and accepted reader pins.
Each row names who owns its disk credit and lifetime.

Every new or replacement disk object in a sink-backed allocation must use
the owned writer API or have an explicitly reserved and enforced separate
allowance included in the same ledger entry. An unaccounted native playlist
rewrite invalidates the guarantee just as an unaccounted FFmpeg segment does.
In-memory served playlists consume bounded memory, not imaginary disk bytes.

Do not shorten current read promises to get space. Moving a file out of its
served name is not reclamation. If deletion fails, keep it charged. If a
reader still holds a deleted object, keep `U` until that reader releases it.

### 3.3 Accounting remains valid when everything else is late

Pause the flow worker and reaper for arbitrarily long test intervals. Pause
the HTTP receiver before the next credit request. A producer can fill only
the bounded transport/memory buffers and already authorized disk writes.
It cannot keep extending files without new credit.

The receiver need not guarantee a real-time response deadline to prove the
disk bound. Controller latency still affects availability and is measured
separately. Tests must distinguish those two results.

## 4. Output transport — one private route per producer attempt

### 4.1 Attempt-local destination, not recipe identity

Proposed core interface shape, to adapt to `TranscodeExecution`:

```rust
// Proposed interfaces; do not add loopback endpoints to ResolvedTranscode.
enum HlsOutputTarget {
    Directory(PathBuf),        // existing non-sink execution paths
    OwnedHttp(OwnedHlsTarget),  // validated, opaque attempt-local destination
}
```

The owned target supplies explicit playlist, segment-pattern and init-object
destinations. Builders must not treat a URL as a filesystem path or produce
Windows path separators in a URL. Preserve start number, media timestamps,
codec arguments, maps, filters, discontinuities and container selection.
Dynamic port/capability values never enter the recipe fingerprint, cache
identity, takeover recipe or persistent settings. A remote worker creates
its own local target when spawning its attempt.

Use explicit PUT and reject/disable ignore-I/O-errors behavior. Configure
HTTP persistence and timeout options only from M1 evidence. Receiver-owned
staging replaces the file-protocol `temp_file` role on this route; preserve
all other HLS flags. Directory output retains its existing flags.

### 4.2 Listener and authority

Use a separate loopback listener bound to an OS-assigned port, owned by the
manager; do not add a public upload endpoint to the main API. Register an
opaque random capability for each exact allocation and producer attempt.
It is distinct from the public playback session ID. The route resolves the
capability to an existing allocation and a closed set of output names; it
never accepts a client-supplied directory.

Use a header capability if all M1 object uploads reliably carry it;
otherwise a private path capability is permitted with explicit redaction in
argv diagnostics, FFmpeg stderr, HTTP logs and receipts. Neither form may
appear in served playlists or remote playback responses. A local transport
capability is not a repository or user credential. Bind only loopback in the
same process/network namespace as FFmpeg; test host, container and Windows
connectivity explicitly.

Accept PUT only. Reject unknown or fenced attempts before opening an output
file. Parse names once and allow only recipe-declared segment/init/playlist
shapes. Reject traversal, decoded separators, unexpected query components,
absolute filesystem names and collisions with daemon-owned files. Use the
repository's existing secure-directory helpers for staging and publication.

Bound accepted connections, headers and active uploads before reading bodies.
Do not spawn an unbounded number of handlers waiting on a semaphore. M1 must
establish the actual per-attempt concurrency needed by every output shape;
M3 turns that observed requirement into a named enforced limit. Resource
limits and their values are part of the M1/M3 receipts, not defaults hidden
in an HTTP library.

### 4.3 Streaming and backpressure

Use a maximum disk-write slice of **64 KiB** initially. This is a proposed
implementation quantum, not a claim about FFmpeg's packet or segment sizes.
A larger incoming frame must be processed in slices, and the memory backing
the original frame still counts toward the receiver's budget.

Before polling more input, obtain bounded receive-memory capacity. Before
writing the next slice, obtain disk credit. On denial, retain only the
already budgeted input and wait for a ledger-change notification, lifecycle
cancellation or the capacity deadline. Register the notification before
rechecking the condition so a release cannot be lost. Do not busy-poll and
do not rely on the 15-second reaper for waking a blocked writer.

No body aggregation, automatic disk spooling, unbounded channel, whole-segment
buffer, transparent decompression or retry queue is allowed in the receiver.
M1 records HTTP parser prefetch and socket buffering separately from the
application memory budget. A suggested starting application budget is
**1 MiB per active upload and 16 MiB globally**; the final enforced numbers
must accommodate required concurrent outputs and pass M3 pressure tests.
Headers, manifest parsing and staging metadata also have explicit bounds.
These values bound memory, not the size of an entire media segment.

Disk ENOSPC, I/O failure and ledger-capacity denial are different outcomes.
Returning an HTTP error to FFmpeg is not a resumable capacity pause. A short
capacity wait keeps the request pending; terminal failure uses the existing
session failure owner and closes the transport. M1 proves the FFmpeg timeout
behavior supports the chosen wait budget.

### 4.4 Object completion and playlist publication

Each upload writes a unique private staging object. A successful response
is sent only after complete HTTP framing/EOF, all accepted disk operations
settling, file close/flush sufficient for immediate readers, and atomic
namespace publication. This is visibility/ownership acknowledgement, not a
new power-loss durability guarantee requiring fsync per segment.

Segments are immutable per attempt and media sequence. A second successful
upload to a completed segment name is rejected; do not overwrite media that
a client may already have consumed. A lost response may therefore terminate
an attempt on retry. This first version makes no transparent retry promise.
If real FFmpeg requires retry semantics to function, M1 must expose that
and the design must define content-checked idempotency before M3 proceeds.

Playlists are mutable replacements. Serialize them per attempt, stage the
new version, validate it, then atomically replace the producer index. Keep
old and new bytes charged during their actual coexistence. A rename of one
object does not add a second payload charge. A disconnected or malformed
upload never replaces the last complete playlist.

URI handling is a compatibility gate. The receiver must emit the same local
object-name contract the existing publication reader expects, with no private
URL or capability. Prefer FFmpeg arguments that generate correct relative
URIs. If rewriting is required, use a bounded HLS parser for every URI-bearing
tag used by supported recipes, including init maps and alternate renditions;
do not globally replace URL substrings. Preserve non-URI tags and ordering.
Resolve references only to completed objects in the exact attempt or to
explicitly retained predecessors already authorized by the current protocol.
An unknown URI-bearing feature fails qualification rather than being stripped.

The sink's complete producer playlist is not permission to bypass
`publication_cycle_at`, startup runway, target validation or the served
sliding window. Keep those existing gates. Record a concrete M1 trace of
segment commit → producer playlist commit → publication clock → client fetch.

## 5. Ledger extension — object ownership replaces racing scan resets

### 5.1 Separate accounting modes explicitly

The existing `observe_used` installs a directory sum and resets
`written_bytes`. A concurrent walk can miss a write that settles during the
walk. Do not reuse that reconciliation for streaming sink objects.

Introduce an explicit allocation accounting mode: legacy observed inventory
versus owned object inventory. Both use the same global cap, grant and
lifecycle authority. Select the mode before spawning; an allocation cannot
silently switch modes while writers or readers exist.

For owned inventory, all creates, extension credits, completion, replacement,
unlink and pin release update object records transactionally. Directory scans
are diagnostic while writers are live: they cannot lower `L`, clear pending
credits or overwrite object identity. A final scan may reconcile only after
writers/mutators are fenced and settled and its generation is still valid.
An unexplained discrepancy preserves a conservative charge and fails the
attempt; it never creates free capacity.

Dispatch `observe_used`, `regrant` and charge conversion by accounting mode.
The legacy `used_bytes + envelope` calculation must not overwrite the owned
inventory's grants or pending obligations. On the owned route, a refused
append credit is immediately a blocked write; the caller does not wait for
`grant_exhausted` to become true. Flow reevaluation can change policy and
wake attempts, but it cannot grant permission outside the writer API.

The native-copy path stays enabled. M2 must exercise the existing
scan/settlement interleaving there as well. If that exposes an existing
undercount, fix the shared reconciliation with a generation fence or move
that owned writer to the same inventory contract before claiming R4-wide
enforcement. Do not conceal a demonstrated shared-ledger failure as an
unrelated issue, and do not assume a factor-of-two grant is its proof.

### 5.2 Proposed operations and linearization points

These signatures describe responsibilities, not a demand for an independent
ledger implementation:

```text
register_sink(allocation, attempt, recipe_outputs) -> SinkLease
begin_object(sink, name, kind) -> ObjectUpload
reserve_append(upload, bytes) -> WriteCredit | Wait | Fenced | TooLarge
settle_append(credit, actual_or_conservative_written) -> charged object bytes
commit_object(upload) -> PublishedObjectVersion
open_and_pin(name, expected_attempt) -> opened object + versioned reader pin
unlink_object(version) -> freed bytes or bytes retained by pins
fence_sink(sink) -> reject new writes; return outstanding-owner barrier
settle_sink(barrier) -> ownership settled, not an assumed timeout success
```

An object ID includes allocation, attempt and a unique object version; a
filename and size alone cannot distinguish two same-size playlist versions.
Integrate pin acquisition with the actual file open and replacement lock so
the pin describes the opened version, including a replaced version still held
by an accepted reader. Use short per-object mutation serialization; never
hold the global ledger lock over filesystem I/O, network waits or actors.

For replacement: keep the old record plus the staged new record charged;
commit the namespace change; transfer old bytes to pinned-extra if needed;
release old unpinned bytes only after the old object is actually gone.
Moving `n` linked bytes to an unlinked reader pin transfers that existing
obligation atomically: decrease `L` by `n`, increase `U` by `n`, and transfer
`n` out of `G` so total charge does not increase or briefly decrease. With
`G >= L + P`, the remaining grant still covers the remaining linked and
pending bytes. This is a charge-preserving ownership transfer, not a new
allocation which can fail because the global cap is already full. Any
subsequent attempt to replenish the linked-write grant is separate growth.
For unpinned deletion, make newly unused credit available according to §6;
do not release it twice through both object removal and a later scan.
Windows replacement can fail while a handle is open. Preserve the previous
published object and staged charge on failure; use the established platform
primitive and prove the supported reader-sharing behavior.

Every mutation has a generation. A stale completion cannot settle a
successor's credit, publish into its namespace, release its charge, or wake
its actor with an old capacity event.

### 5.3 Cancellation is a lifetime problem, not an HTTP status

Do not let dropping a request future drop an unsettled write credit as zero
while a Tokio/blocking filesystem operation can still complete. An owned
upload task carries its writer registration, file, credit and cleanup
responsibility until every submitted disk operation settles. The request
waits on that owner; request cancellation signals it but does not release its
obligations or abort a blocking write into invisibility.

On partial write/error, retain a conservative charge for the maximum bytes
that operation could have landed until exact length is safely established.
An untouched credit can be released; an uncertain submitted write cannot.
Failure to unlink staging leaves a charged cleanup obligation with retries.
EOF, connection reset, task panic and retirement all converge on the same
settlement rules. Panic-safe ownership must leave a charge/owner record a
cleanup supervisor can settle; no `Drop` implementation may assume deletion.

Register sink ownership before spawning FFmpeg. Retirement fences the route
and writer API, wakes waiters, stops/reaps the child through its existing
owner, settles upload tasks, then uses the existing retention/grace owner.
Issued disk operations may settle after the fence, but their result cannot
publish a newly completed object after that fence wins. Recheck the attempt
and fence under the publication/mutation serialization before renaming;
otherwise preserve the staging object for charged cleanup.
Never hold `child_transition` while waiting for an upload that needs that
same transition to exit. A deadline expiring records unresolved ownership;
it is not evidence that bytes or writers disappeared.

On daemon restart, old routes are invalid. Before reopening admission,
reconcile surviving session/staging directories through the existing orphan
cleanup lifecycle and charge or remove them. The spike must establish what
the existing process owner guarantees about children surviving daemon exit;
an old HTTP connection must never become a new attempt's route.

## 6. Capacity policy — smaller reservations do not promise unlimited starts

### 6.1 Initial and growing grants

Retain existing startup sizing inputs and the unknown-rate bootstrap where
appropriate, adapting encoded sizing to the resolved output recipe. Include
the effective publication runway, one required segment of progress and
metadata/replacement space. At this base the native bootstrap uses a 64 MiB
minimum and 256 MiB unknown-rate allowance; these are starting allocations,
not maxima on real output. Reuse the current helper where its semantics fit,
otherwise add a distinct helper with units and edge cases tested.

For a sink allocation, growth is authorized at the actual append boundary.
Use a proposed **1 MiB growth quantum**, rounded with checked arithmetic,
with exact required-byte growth as the fallback when speculative headroom
does not fit. A 64 KiB append must not be refused merely because another
1 MiB of optional headroom cannot be granted. Whole-segment size is not
required in advance.

Keep the configured ahead-byte/time policy and global cap unchanged. Do not
reinterpret the current 2 GiB ahead threshold as a newly introduced hard
maximum on total retained session bytes; ahead and pinned history differ.
Any new object-size or total-session limit is an explicit reviewed policy
change, not a by-product of choosing an HTTP body limit.

At an owned accounting checkpoint, unused speculative grant may be returned
only while preserving all object bytes, issued credits and the selected
startup/working headroom. Use owned counters, not a live directory scan.
Keep enough charged space for required playlist replacement so media writes
cannot consume the last bytes needed to publish completed media. M1/M2 must
derive this metadata reserve from the actual bounded producer playlist,
including long event-playlist growth, old/new overlap and init/sidecar needs.
It is part of `G`, not an uncharged extra budget. If an existing unbounded
producer manifest prevents that calculation, resolve it at the feasibility
gate without silently changing the served playlist contract.

### 6.2 Capacity waits and fairness

Use one budget-change notification source and bounded growth waiters, at
most one pending credit request per active upload. Reclaim only bytes whose
current retention and read obligations allow it. Order growth opportunities
ahead of new session admissions; within growth use bounded round-robin
attempt service so a fast uploader cannot take every newly released byte.
Do not revoke already issued credits to improve fairness.

Proposed review defaults: an individual capacity episode may wait at most
**5 seconds without a successful credit grant**, and must also respect the
existing earlier startup/lifecycle deadline. This is an availability policy,
not a disk safety parameter. M1 proves the HTTP timeout exceeds the chosen
wait plus normal processing allowance; M5 qualifies the user-visible effect.
Fable may require a different explicit duration. Opus records the final named
constant and evidence, rather than inventing a timeout during implementation.

The deadline does not reset merely because a timer fired or an unrelated
session released a byte. The receipt includes whether repeated tiny grants
can sustain playback; existing presentation/startup deadlines continue to
bound overall lack of useful progress. A held startup cannot wait forever for
its own unpublished data to be consumed.

### 6.3 Failure outcomes and client behavior

| Situation | Required outcome |
|---|---|
| Initial grant cannot fit | Existing classified capacity refusal (503 where the current create route uses it); no producer leaks. |
| Startup hits real capacity pressure | Wait within the capacity/startup deadline, then classified refusal and owned cleanup. Preserve a healthy incumbent. |
| Active session waits briefly while old media can be reclaimed | Serve all committed media normally; wake credit requests on reclamation. |
| Active session cannot obtain needed capacity in time | Explicit capacity failure through the existing actor/client terminal contract; do not label it encoder corruption, generic no-progress or a new admission refusal. |
| Replacement fails before publication | Preserve the incumbent and existing manual retry semantics. |
| Sink transport or disk fails | Distinct transport/I/O reason, writer settlement and cleanup; no silent direct-file restart with a small grant. |
| Operator lowers cap below obligations | Report overcommit, prohibit charge increases, preserve already granted writes and reader promises. |

The first release guarantees enforcement and demonstrates sustained
concurrency for the acceptance workloads. It does **not** promise stall-free
playback for every possible bitrate/retention combination admitted by an
estimate. Guaranteeing that would require a separate worst-case working-set
admission contract. Fable must accept this limitation or request that larger
policy effort before approving implementation.

## 7. Flow and diagnostics — a blocked sink is not a suspended process

Keep sink capacity state separate from `session.suspended` and
`suspended_at`: FFmpeg may be running until socket buffers fill, and a sink
can remain blocked while an independent supply hold also exists.

Represent active hold causes as independently owned state. The actor exposes
an explicit scratch-capacity reason in events and diagnostics without losing
an existing demand/time/bytes reason. Clearing scratch capacity does not
resume a producer still held for supply. A flow evaluation must not clear a
sink hold just because `regrant_rolling_scratch` returned `None`.

Whether to suspend FFmpeg as a CPU optimization is optional; it cannot be a
prerequisite for receiver progress, cleanup, or disk safety. For the first
implementation prefer socket backpressure for scratch waits and leave normal
supply suspension unchanged. Prevent the no-progress watchdog from treating
a legitimate bounded capacity wait as encoder failure; do not count the hold
as produced media or let it extend startup indefinitely.

Expose in the existing Developer/activity surfaces:

| Field | How to read it |
|---|---|
| Writer mode / executable capability | Owned sink versus explicitly conservative legacy path; tells the operator what guarantee applies. |
| Granted, linked, pending, pinned and total charged bytes | Pending can outlive HTTP cancellation; pinned means reader promises still own space. |
| Requested growth and capacity wait duration | Nonzero wait with a full cap is capacity pressure, not encoder speed. |
| Active hold causes and last capacity outcome | A cleared capacity reason does not mean every supply hold cleared. |
| Unsettled uploads / cleanup obligations | A timed-out owner still carries bytes; investigate rather than subtract it. |
| Global cap and overcommit after settings change | Overcommit explains denied growth without hiding existing obligations. |

Use bounded-cardinality metrics; do not label metric series with object names,
attempt tokens or session IDs. Update every serialization/hold-reason match
and relay adapter deliberately, preserving older clients' documented unknown
reason behavior. Existing R1 client ownership rules still apply.

## 8. Compatibility — capability is evidence, not an escape hatch

M0 builds the actual reachable rolling-writer matrix. M1 must cover encoded
MPEG-TS, each direct/remux HLS path that remains reachable, fMP4 init/media,
audio-only and subtitle/DV variants where those routes actually exist. Mark
unreachable combinations with code evidence instead of manufacturing support.

Record executable identity/version, platform, muxer arguments, output order,
URI forms, concurrency, memory, timeout and HTTP error behavior. Exercise the
repository-pinned test surface and the deployed FFmpeg builds; FFmpeg 6 test
results alone are not evidence for an FFmpeg 8 deployment, or vice versa.

Capability resolution happens before admission/spawn and is cached against
the actual build identity using existing bounded helper conventions. A mere
`-h muxer=hls` option listing is insufficient: a small real output probe must
demonstrate the critical upload shape. The probe itself has bounded runtime,
output and temporary storage and cannot consume unaccounted rolling scratch.

During development all existing paths remain conservative until M5 enables
smaller grants for a fully owned path. At release, every qualified supported
rolling recipe uses the sink by default. An unqualified external FFmpeg build
or platform may retain conservative admission only as an explicit compatibility
limitation decided in review. It cannot silently use smaller grants, and it
cannot be counted as completion of platform-wide R4. No automatic retry from
sink to direct disk output inside a live allocation is allowed.

Windows compilation is mandatory throughout. Native Windows runtime evidence
is required before enabling reduced sink reservations there. If the fleet
scope remains Unix-first, record the Windows limitation in the receipt and
existing [Windows status](../features/WINDOWS-PORT-STATUS.md); do not claim a
runtime result from a cross-build.

## 9. Work packages — one effort, sequential integration

Use `effort/ffmpeg-scratch-sink`, with `codex/ffmpeg-scratch-m0` through
`codex/ffmpeg-scratch-m6` task branches based on the current effort. Verify
whether those names already exist before creating them. Shared ledger,
transcode and HTTP files mean the independent-main-task exception does not
apply. No parallel agents are required by this plan.

| Package | File ownership and change | Acceptance before the next package |
|---|---|---|
| **M0 — baseline and writer inventory** | This plan/receipt; inspect ledger, transcode, copyseg, HLS serving, subtitle and publication writers. Establish current main and pinned compiler. | A row for every reachable writer/mutator and recipe, startup gate/retention contract, orphan policy, baseline commands and actual toolchain. No unsupported whole-namespace claim. |
| **M1 — HTTP feasibility harness** | Focused test harness and minimal test-only argv target plumbing in core; no production routing/sizing change. | Real FFmpeg traces prove §4/§8, including held uploads and completed playlist consumption by existing publication code. Required variants all pass or this plan records a blocking incompatibility. |
| **M2 — owned object accounting** | `scratch_ledger.rs`, owned-object helper, tests; copyseg reconciliation only if shared proof requires it. | Model/interleaving tests prove §3 and cancellation/replace/pin contracts. Scans cannot erase concurrent writes. No HTTP or sizing switch yet. |
| **M3 — receiver and lifetime owner** | New sink module, crate registration, secure file publication, manager-owned listener lifecycle. | Real streaming/chunked receiver uses M2 credits; partial writes, cancellation, lost responses, pressure, teardown and bounded memory pass. Test-only integration until M4. |
| **M4 — rolling integration at conservative admission** | Production core output-target plumbing, `transcode.rs`, capability selection, HLS pin/publication integration, full namespace writers. | Production rolling commands use sink with existing full reservation. Protocol parity, replacements, lifecycle and real playback pass; no unowned output remains in an enabled allocation. |
| **M5 — growing admission and capacity outcomes** | Startup sizing, append growth, fair wakeups, actor capacity reason and diagnostics; web sidecars as required. | Reduced grants pass startup, active pressure, four-viewer sustained playback and R1/R2/R3 regressions under unchanged cap. No feature switch or timer-based write permission. |
| **M6 — qualification and promotion** | Evidence receipt, behavior docs, catalog/regression mapping, plan status. | Frozen candidate, current-main integration, required gates and platform/runtime receipts. Record any conservative compatibility path as incomplete scope. |

M1 must precede production design commitment. M2 and M3 precede the route
switch. M4 deliberately separates transport regressions from admission
changes; it is an effort integration step, not a request for a separate
production release. If M4 needs to change HLS semantics to pass, return that
finding to design review instead of hiding it in M5.

Every task PR records its exact base/head, changed contract, focused command,
nonzero executed test count and result. Keep one evidence receipt under the
streaming subject folder once implementation begins, indexed in the same
commit; do not create an empty receipt merely to suggest work has started.

## 10. Acceptance matrix — prove the failure boundaries, not just happy output

Tests use deterministic barriers and an injectable file-I/O failure surface
where possible. Resource stress supplements that proof; a sampled directory
size graph alone cannot prove no transient overrun. Record independent object
lengths at controlled write/mutation boundaries and compare with the ledger,
so the test does not merely repeat the implementation's arithmetic.

| ID | Scenario | Required evidence |
|---|---|---|
| A01 | Two uploads race for the final bytes | Only fitting credits succeed; independently tracked linked/pending/pinned obligations remain covered. |
| A02 | Grant 64 MiB, used 60 MiB, growth refused, producer offers 8 MiB | At most remaining authorized bytes land; next slice blocks before disk extension. No dependence on grant-exhausted polling. |
| A03 | Flow/reaper frozen beyond 15 seconds; FFmpeg unpaced | Disk still bounded by credit; transport eventually backpressures. Include 90-second burst configuration and underdeclared bitrate. |
| A04 | Scan stats an object, concurrent append settles, scan completes | Scan cannot discard the appended obligation or permit overlapping reuse of its credit. Cover both owned sink and native copy. |
| A05 | Cancel before credit, after credit, during submitted write, after rename and before HTTP response | Every phase preserves exact or conservative charges until owned settlement; no orphan writes into released space. |
| A06 | Short writes, ENOSPC, flush/close failure, unlink failure | Partial/staged bytes remain charged; old playlist remains readable; classified reason and cleanup owner survive. |
| A07 | Same-name/same-size playlist replacement with old readers | Old and new versions remain distinct; old pinned bytes release only with the last old reader. |
| A08 | Simultaneous retirement, blocked upload and child replacement | No deadlock; no credit/namespace publication by stale attempt; incumbent/successor accounting remains separate. |
| A09 | HTTP truncation, malformed framing, lost final response, duplicate PUT | No partial object becomes published; retry behavior matches the declared no-transparent-retry contract. |
| A10 | fMP4 init, media and every URI-bearing supported playlist tag | Existing reader/publication code accepts complete output; no loopback URI/capability reaches any client. |
| A11 | Long title / many producer event-playlist entries | Manifest/metadata memory and disk overlap fit explicit bounds; media cannot consume publication reserve. No late surprise from whole-title growth. |
| A12 | All sessions are unpublished and global budget is full | Startup reaches publication or bounded classified failure; it cannot wait indefinitely for its own client to drain. |
| A13 | Existing active growth competes with new starts | New admissions cannot continually steal released bytes; growth ordering is bounded and starvation is observable/classified. |
| A14 | Scratch and supply holds overlap in both orders | Clearing either cause leaves the other effective; diagnostics do not lie about process suspension. |
| A15 | Cap reduced below obligations | Already issued writes remain covered by their grants; increases fail; no retrospective erasure of charge. |
| A16 | Daemon restart / old route / surviving staging | Old attempts cannot publish; preexisting files are charged or cleaned before fresh admission. |
| A17 | At least four low-bitrate rolling FFmpeg viewers, default 8 GiB | All present concurrently through at least two actual retention/reclamation cycles and a late media position; collect startup time, runway, stalls, charge and real inventory. Use software capacity adequate to isolate scratch from hardware-slot limits. |
| A18 | High-bitrate and wrong/unknown-rate sources; segment larger than remaining credit | Partial upload remains invisible and bounded; succeeds after reclaim or gives bounded failure. No silent truncation to an HTTP body cap. |
| A19 | Repeated seeks/replacements with active reads | Existing R1/R2/R3 regressions stay green; failed destination preserves incumbent; actual retained backlog remains charged. |
| A20 | Real clients and platform output | Web, Apple and Android playback on affected formats retains startup, A/V, seek, end-of-stream, subtitles and HDR/DV behavior. Unix runtime evidence and Windows qualification/limitation are explicit. |
| A21 | Receiver and child memory under blocked output | Application buffers/connections remain within declared limits; measure FFmpeg buffers separately. No whole-object buffering disguised as bounded disk use. |
| A22 | Unsupported build or sink failure | Full conservative admission selected before spawn where policy permits; no in-place small-grant direct-file fallback. |

The four-viewer case is a sustained acceptance floor, not a promise of dozens
of encoders. Report CPU/hardware admission separately. Compare reservation
and actual retained bytes against the same workload on baseline; do not use
a 150 MB estimate as the result.

### 10.1 Commands and test naming

Use the existing compiler loop and add meaningful tests with the common
filter `scratch_sink_` in affected crates. The names do not exist yet; M1–M5
introduce them. Verify the filter executes the intended tests, including
real-FFmpeg tests rather than a collection of ignored entries.

```bash
# Establish this before any Rust edits; use the source-only cloud loop if needed.
rustup run 1.97.1 rustc --version
rustup run 1.97.1 cargo check --workspace --locked --all-targets

# Per-package compile/lint evidence, using the pinned compiler environment.
cargo fmt --all --check
cargo clippy --workspace --locked --all-targets -- -D warnings

# Proposed focused filters, once the implementation introduces the tests.
cargo test --locked -p plurxd --bin plurxd scratch_sink_ -- --test-threads=1
cargo test --locked -p plurx-core --features hiqlite-store --lib scratch_sink_ -- --test-threads=1

# Existing documentation contract after every plan/receipt/index change.
python3 -m unittest discover -s tests/operations -p test_docs_index.py
```

Pin `cargo` through the repository's toolchain configuration or run each
command with `rustup run 1.97.1`; a version printed by some other compiler is
not evidence. Record binary paths/version and test counts. Loopback binding
is necessary for these tests; a sandbox socket denial is not a passing test.
Final qualification includes the required broader unit/runtime surfaces;
the focused filter does not replace `make unit` or applicable promotion gates.

## 11. Delivery and review gates — qualify the exact tree

Preserve the shared checkout's unrelated changes. Start implementation in an
isolated worktree from verified current main. Carry this plan and its index
row into the effort together. Read [AGENTS.md](../../AGENTS.md),
[the development pipeline](../DEVELOPMENT_PIPELINE.md) and
[the agent compiler loop](../ci/AGENT-COMPILE-LOOP.md) at build time.

Use the configured repository/forge, not an assumed GitHub push remote.
Commit normally with the tracked hook. Archive committed source for a remote
compiler; transfer neither `.git` nor credentials. Run the focused regression
and affected compilation before pushing; CI must not be the first compiler.

Treat `Effort development gate` as blocking for task integration. At effort
completion freeze task merges, integrate current main, rerun against that
exact tree, and open the effort-to-main candidate. Main promotion requires
the current candidate's required gate and qualification receipt under AGENTS;
the pipeline document contains historical/amending workflow descriptions,
so inspect current workflow definitions rather than assuming a stale job name
is sufficient. Resolve a material gate discrepancy explicitly, never by
selecting the least demanding interpretation. A moved base invalidates older
qualification for the new tree.

M6 updates [PLAYBACK.md](../PLAYBACK.md) and
[OPERATIONS.md](../OPERATIONS.md) with the actual writer guarantee, capacity
failure behavior and diagnostic interpretation, plus the earlier R4 plan's
status where appropriate. Any new/changed doc is indexed in
[docs/README.md](../README.md) in the same commit. No feature toggle is added
for rollback: use the ordinary release rollback to the prior complete build,
settling/stopping owned attempts through lifecycle cleanup rather than
switching their writer mode in place.

## 12. Fable's review — decisions that must be explicit

Return **approve**, **approve with concrete amendments**, or **request
changes**, with numbered findings tied to sections and source symbols.
Distinguish a wrong invariant from an M1 experiment that the plan already
requires. Do not approve by assuming that HTTP support in documentation is
equivalent to passing the required output matrix.

| Review question | Proposed decision to accept or amend |
|---|---|
| R1. Is the sink the right next effort? | Try retaining FFmpeg's muxer with owned HTTP writes. M1 failure returns to design; it does not authorize a custom filesystem or muxer rewrite. |
| R2. Is the byte proof complete? | All rolling namespace writers/mutators and pins are covered; object accounting and submitted I/O lifetimes replace polling assumptions. Attack cancellation and scan races specifically. |
| R3. Can transport preserve HLS semantics? | M1 proves init/media/playlist order, URI handling, persistence, timeout, duplication and real publication consumption for every reachable recipe. |
| R4. Are the proposed buffering and credit quanta reasonable? | 64 KiB writes, 1 MiB optional growth, bounded upload/global memory; metadata reserve derived and enforced before switching sizing. Numeric values need evidence, not safety-factor language. |
| R5. Is the availability policy acceptable? | Short bounded capacity waits; proposed 5-second no-grant episode bound and existing earlier lifecycle deadlines; explicit terminal capacity failure when growth cannot be served. No promise that estimated admission guarantees uninterrupted playback for every source. |
| R6. Does growth priority protect active work sufficiently? | Growth precedes new starts, bounded round-robin upload service, no stolen issued credit, startup fails explicitly if it cannot reach publication. |
| R7. What constitutes platform completion? | Qualified supported Unix recipes use sink by default; Windows needs native runtime evidence for reduced grants. Any compatibility fallback remains conservative and visibly incomplete scope. |
| R8. Is the work boundary small enough? | Seven sequential packages, transport proven at conservative sizing before admission change; shared ledger/copy fixes only where required by demonstrated invariant failures. |

**Review ledger:** pending. Record Fable's disposition and each amendment
here before changing the status to approved. M1 feasibility, implementation,
runtime qualification and deployment are all unperformed at this document's
creation; none is implied by the design review.
