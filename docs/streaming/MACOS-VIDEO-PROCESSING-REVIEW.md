# macOS video processing review — three execution contracts corrected

**Status:** review complete; author corrections applied, not independently
re-approved · **Reviewed:** 2026-10-07 · **Original verdict:** request changes ·
**Reviewer:** independent Codex agent `macos_adversarial_review` ·
**Source inspected:** `890bca0fc186d00f7b10a379cd7dd2f1a37dc37c`.

Companion to the [design](MACOS-VIDEO-PROCESSING-DESIGN.md) and
[implementation plan](MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md). This records
one adversarial documentation/source review requested by Paul. It is not
an Opus review, a runtime qualification receipt or an implementation result.
The original reviewer did not edit files. The author applied the corrections
below after receiving the findings; there was no second approval pass.

## 1. Reviewed inputs and verdict

The reviewer inspected the full documents, local planner/recipe/recovery/
cluster seams, and primary Jellyfin HEVC-decoder and Metal-filter sources.
Original draft SHA-256 values identify the reviewed text before correction:

| Document | Original SHA-256 |
|---|---|
| Design | `d02b78bdc0b5711bcb1bfd5326639b13881bdeebefb2537e2ec6258556186e03` |
| Implementation | `554ce12acde4cd734b97642e794bac7624f9ea9eb8e326f1ada04a885d43354e` |

**Verdict:** request changes to the execution contract. The experiment is
justified; no performance result is required to approve M0/M1. Three gaps
need correction before implementation, with the Dolby enforcement requirement
specifically attached to E1. All three findings were rated P2; no P0/P1
finding was reported.

The line references below identify the original draft, not line numbers in
the revised documents. Section references point to the corrected contract.

## 2. R1 — P5 metadata must be enforced in the executing graph

**Priority:** P2 · **Original references:** design lines 207 and 416;
implementation line 381 · **Disposition:** accepted and corrected in design
§5.1 and implementation E1 (§9.1).

**Finding:** planning flags and a startup sample prove neither metadata
continuity nor strict behavior inside the running filter. A P5 source may
pass initially and lose effective Dolby metadata after a seek, frame
transition or decode/transfer defect. The pinned Metal filter's `apply_dovi`
means use metadata when present. Without it, PQ/HLG can proceed through
ordinary mapping with exit zero. That can produce incorrect pixels without
triggering the proposed failure recovery.

**Evidence:** the [Jellyfin v8.1.3-1 Metal filter patch](https://github.com/jellyfin/jellyfin-ffmpeg/blob/v8.1.3-1/debian/patches/0046-add-vf-tonemap-videotoolbox-filter.patch)
conditionally reads `AV_FRAME_DATA_DOVI_METADATA` and otherwise accepts
SMPTE2084/HLG. The required-side-data field in
[decode.rs](../../crates/plurx-core/src/transcode/decode.rs) describes a plan;
it does not validate every frame inside FFmpeg.

**Author correction:** E1 now requires a proved strict mode or a minimal
reviewed patch enforcing required effective Dolby metadata at the renderer
boundary. Define effective metadata to include legitimate decoded reuse,
with correct frame association, rather than requiring a new RPU in every
packet. Missing/invalid required state must fail before the affected frame
reaches the encoder; existing recovery owns any replacement. Bind strict
mode and patch revision into the recipe.

Tests now explicitly cover initial absence, midstream loss, valid reuse,
stale seek state, reordered frames and ordinary HDR10/HLG controls. If this
execution contract cannot be supplied, retain the existing P5 route. This
is an executable compatibility requirement, not an offline-review gate on
the user's saved setting. SDR/HDR10 experiments can proceed independently.

**Still to prove during implementation:** the strict boundary exists in the
selected executable; rejection occurs before affected output publication;
valid reuse is not rejected; recovery does not splice differently processed
bytes into the prior artifact.

## 3. R2 — pipeline names are diagnostic, not remote execution commands

**Priority:** P2 · **Original references:** design line 482; implementation
line 294 · **Disposition:** accepted and corrected in design §5.4 and
implementation M3 (§6).

**Finding:** the draft claimed old workers decline unknown graph names through
existing compatibility handling. Current snapshot/offer validation accepts
bounded strings, and eligibility uses broad capability facts. Adding a new
enum to a new binary cannot change an old peer's interpretation.

**Evidence:** [media_pool.rs](../../crates/plurxd/src/media_pool.rs),
`snapshot_is_bounded`, `offer_is_bounded` and `snapshot_can_answer`; and
[media_sessions.rs](../../crates/plurxd/src/media_sessions.rs),
`RemoteStartRequest`, which carries presentation/session requirements rather
than a required processing graph. This establishes a false proposed
mechanism, not an observed unsafe graph transplant in today's runtime.

**Author correction:** choose the existing worker-local resolution model.
Pipeline strings on the wire remain diagnostic; detailed Mac graph classes
stay local. A worker must resolve and accept the actual requested
presentation/decoder constraints using its own current capabilities and
saved preference. A coarse advertisement is a prefilter, never execution
proof. An old worker may correctly use its incumbent graph for a presentation
that a new worker delivers with Metal.

M3 now explicitly tests old-to-new and new-to-old dispatch, unsupported
presentation refusal, capability-generation changes between offer and start,
and takeover onto a different recipe. Actual worker plan/recipe identity
controls output and cache lookup. Replanning cannot append new bytes to the
old artifact. If a required remote graph constraint becomes necessary, it
needs an explicit versioned envelope/protocol amendment, not an opaque string
under the existing version.

**Still to prove during implementation:** offer/start revalidation reaches
the actual acceptance seams; stale capability/recipe data cannot grant a
cache hit or authorize an incompatible presentation; takeover maintains its
existing publication and lease fences.

## 4. R3 — a clean install needs provisioned smoke fixtures

**Priority:** P2 · **Original references:** design line 377; implementation
lines 267 and 457 · **Disposition:** accepted and corrected in design §4.5
and implementation M1/M3/M6 (§4/§6/§10).

**Finding:** the draft required compressed fixtures but assigned neither
shipping nor generation. The offline benchmark harness could leave a developer
Mac prepared while a fresh install stays permanently unavailable. Restart
and reprobe do not fix an absent input. Unbounded first-start generation would
also violate the intended responsiveness contract.

**Evidence:** the current [pipeprobe](../../crates/plurxd/src/pipeprobe.rs)
generates and caches its own HDR10 fixture. It is not a provisioner for the
proposed collection of Mac graph classes.

**Author correction:** M1 owns a small, redistribution-safe synthetic smoke
corpus plus generator/provenance and a versioned hash/expectation manifest.
M3 embeds and loads it; M6 verifies the release on a clean offline install.
Initial aggregate compressed size budget is 2 MiB, revised explicitly if
adequate fixtures do not fit. Preparation writes verified embedded bytes
asynchronously with a separate 5 s deadline and atomic cache publication;
there is no runtime network fetch or fixture encoding.

Corrupt cached bytes are recoverable from the embedded copy. Disk/permission
failures are reported separately and can be retried after repair. Invalid
embedded data is a packaging failure. Private quality media stays outside
the runtime dependency. Later graph classes must ship their own lawful
fixtures before their runtime capability is advertised.

**Still to prove during implementation:** clean offline start, no cache,
corrupt cache, invalid manifest, failed writes, cancellation and successful
reprobe; a developer benchmark directory must not be required on the host.

## 5. Premises that survived the adversarial pass

- Shared memory removes the discrete PCIe-copy premise; conversion, copying
  and synchronization can still cost time. Measuring rather than assuming
  their importance is appropriate.
- Hardware HEVC reconstruction can coexist with CPU Dolby metadata parsing.
  The pinned decoder attaches Dolby side data and the Metal renderer consumes
  it; this motivates an experiment rather than proving a supported tuple.
- The four-way decoder/renderer comparison isolates useful changes, including
  hardware decode feeding the current CPU renderer.
- Saved preference, runtime compatibility and offline qualification are
  substantially separated; none is a substitute for the others.
- Effective recipe identity, implementation attestation, immutable running
  plans, grade-preserving fallback and segment-boundary tone-map checks cover
  important correctness risks.
- Reusing the existing recovery owner is credible. The existing
  [prepublication retry](../../crates/plurxd/src/transcode/rolling/prepublication.rs)
  already supports a renderer fallback while retaining the hardware encoder.

## 6. Verification and limits

The author ran the four existing documentation-index tests and whitespace
checks and verified local links in all three documents. The new review record
is indexed alongside the design and implementation plan. No Rust code,
FFmpeg package, daemon setting or media file changed.

The reviewer did not benchmark hardware, reproduce the dated local FFmpeg
inventory, inspect every distribution patch or establish GPU/driver behavior.
The author corrections are documentation changes, not proof that their future
implementation works. The original verdict remains request changes on the
original draft; this record reports the fixes without claiming independent
approval of the revised text.
