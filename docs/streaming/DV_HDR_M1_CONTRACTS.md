# Dolby Vision M1 — typed intent, evidence and generation ownership

**Status:** design approved; additive implementation in progress ·
**Executes:** M1 of the [build handoff](DV_HDR_PROCESSING_BUILD.md) ·
**Written:** 2026-10-08 · **Inspected tree:** `b228a303e9e263abeb247e7fe5e1a01a50a90a06`

Read the build handoff and accepted M0 control documents first. This retained design
handoff answers where the new contract belongs and what it may promise. The
inspected branch and approved evidence remain read-only. Adversarial design
review approved scratch SHA `3cfee8664321101ba99efc76dca7f33da9cefb322e7ceecc6c8bc9b58e7b3980`;
Sections 1–7 retain the reviewed design as a historical contract; section 8
records the actual additive implementation and subsequent corrections. The
relative links and current status have been adapted for this repository. Implementation
is based on effort merge `03fa9d2d104122d40d168260e96647fd14884104`. Re-verify signatures
against the implementation task's current effort base before editing Rust;
establish the pinned Rust 1.97.1 compiler loop first.

## 1. M1 adds contracts and tests without selecting new playback routes

M1 owns a typed, test-only resolver and receipt validation primitives. It does
not add a producer, advertise a production backend, alter FFmpeg arguments,
change native/copy fallback policy, or award HDR10-E. Missing capabilities
resolve to existing behavior. Synthetic mechanism evidence never upgrades
itself to production-route qualification.

The accepted evidence establishes bounded arithmetic and association:

| Evidence | What may be represented | What remains unavailable |
|---|---|---|
| Structured/parsed GPU controls | Explicit polynomial/NLQ and matrix/PQ operations; exact consumed inputs | General creative target mapping and physical reference fidelity |
| CPU controls | Supplied standard-matrix integer reconstruction | General `rgb_to_lms` support; DoViBaker ignores it |
| Decoded-layer controls | Six 64×64 pictures per layer, no-reorder/B-frame cases, actual rational container PTS/duration, fresh raw RPU association | Seek, VFR, discontinuity and legitimate RPU reuse |
| Nonidentity authoring | Luma `1/16 + 3x/4`, identity chroma, bounded residual; corrected identity mapping on new encoded base; repeated-map negative rejected | General nonlinear adaptation, creative trims, DV container/profile conformance |

L1 tags in fixtures prove association. Their presence is not proof of creative
L1 target mapping. Full-render versus reconstruction equality is not evidence
of display mapping. Changed pixels are not a processing eligibility condition;
identity curves and zero residuals are valid controls. These distinctions must
survive in the Rust types and wire summaries.

## 2. Existing interfaces — preserve the owners already in the tree

Paths below are relative to the inspected repository root. They are existing,
not proposed APIs.

| Interface | Existing facts and proposed attachment point |
|---|---|
| `domain.rs::DolbyVisionFacts` | `profile`, `level`, `bl_compat_id: Option<i64>`; `el_present`, `rpu_present: Option<bool>`. No MEL/FEL classification or validity proof. Do not enrich a catalog boolean into accepted-frame evidence. |
| `playback/mod.rs::RenderCaps` | `dv_strippable`, `dolby_vision_p5_render`, `hdr10_passthrough`, `hdr10_max_height`, `dolby_vision_convert`. Keep these old fields/semantics. New processing context is separate. |
| `decide(file: &MediaFile, profile: &DeviceProfile, node: &RenderCaps) -> Decision` | Existing device/force/subtitle and delivered-range authority. Native-supported DV wins before enhancement selection. |
| `dolby_vision_converts_to_p81(...) -> bool` | Requires existing conversion permission, numeric source eligibility, client profile 8 and no native profile 7. Reuse this permission/eligibility decision; the new FEL preference cannot enable conversion. |
| `dvconvert::convert_length_prefixed(sample: &[u8], nal_length_size: u8, out: &mut Vec<u8>) -> Result<Converted, DvConvertError>` | Base-copy metadata conversion remains fallback. `Converted.rpus` counts rewritten RPUs, not accepted display pictures or reconstructed residuals. |
| `dvpipe::Converter::{for_init, convert, report}` | Post-mux fMP4 conversion owns container/timing behavior. Its `Report` describes discarded EL and first-RPU source classification. Do not insert it as an encoder input or reinterpret it as FEL receipt. |
| `store::PlaybackPlanningSnapshot` | `file`, `probe_json`, `settings`, `generation: i64`; one source/settings read. This generation is planning-input revision, not output-generation ID. |
| `DecodeFacts`, `DecodeSourceIdentity`, `DecodeCacheIdentity`, `PlanSourceBinding` | Reuse fact selection and separate continuity/cache identities. Descriptor binding verifies held bytes; catalog identity (`id,size,mtime`) shares artifacts across producers. |
| `ResolvedTranscode` in `transcode/decode.rs` | Private validated plan; `plan_digest()`, `cache_identity()`, `observed_source_identity()`, `source_binding()`, output/codec contracts. Future DV semantic plan belongs here, constructed through validation. |
| `Recipe::new(digest: &PipelineDigest, plan: &ResolvedTranscode, audio_copied: bool)` | Hash feeds `plan_digest` and namespace. `PipelineDigest` currently contains FFmpeg build text. Do not introduce a second competing DV cache key. |
| `ProducerHealthReceipt`, `GenerationManifest` | Decode-health receipt is a different observation. Manifest already digests its object inventory and optional producer-health field. A future DV receipt must bind the same published bytes, not replace decode health. |
| `vodgen::Generation` and `vod/generation.rs` | Existing immutable init/timeline contract and producer ownership. `convert_dolby_vision` is old base conversion. New processing failure must not silently change it mid-run. |
| `manager/plan.rs::{resolve_movie_plan_from_probe, resolve_movie_plan_with_facts, effective_recipe, candidate_recipe_digest}` | Daemon chooses source/worker snapshots and passes validated semantics to core. Keep profile-5 safety and current graph qualification separate. |

## 3. Proposed additive core contract — validated values, not booleans

Add `crates/plurx-core/src/transcode/dv_processing.rs`, exported by
`transcode/mod.rs`. Core owns grammar, validation, deterministic semantic
identity and pure selection. Daemon owns observation, process lifecycle,
worker proof collection and persistence. Private fields/validated constructors
prevent a deserialized or hand-built receipt from becoming trusted directly.

| Proposed type | Exact content / invariant |
|---|---|
| `DvDestination` | `Hdr10`, `Profile81`; native DV returns `KeepExisting(NativeCompatible)` before construction. Destination follows the existing delivery decision, not a source badge. |
| `DvPreferences` | `hdr_processing: bool`, `fel_reencode: bool`, `conversion_permitted: bool`, `planning_input_generation: i64`. Nonnegative revision; absent new switches false. Old conversion default remains unchanged/on. |
| `DvSourceFacts` | Existing `DolbyVisionFacts`, selected absolute video index, `DecodeSourceIdentity`, `DecodeCacheIdentity`, `PlanSourceBinding`, parser identity, `DvRpuState`, `DvElKind`, exact observed metadata-subset evidence. Catalog-only facts are provisional, not producer acceptance. |
| `DvRpuState` | `Unknown`, `Absent`, `PresentUnvalidated`, `ValidatedFresh`, `ReuseUnsupported`, `Malformed`. `ValidatedFresh` has bounded parser/subset evidence bound to source stream, continuity epoch and an explicit sampled-frame or interval coverage set. It never certifies unsampled frames or a whole title. Every emitted frame must independently satisfy current raw-RPU/predicate acceptance, including later curve/reuse changes. |
| `DvElKind` | `Unknown`, `None`, `Mel`, `Fel`; unknown never becomes MEL/none. Existing converter's default `EnhancementLayer::None` is not input evidence for this type. |
| `DvOperation` | `RepresentationNormalization`, `BaseNormalization`, `PolynomialReshape`, `MmrReshape`, `LinearNlqResidual`, `RpuColorConversion`, `TargetMapping`, `CreativeTrimApplication`, `MetadataAdaptation`, `Bt2020NclConversion`, `Main10Encoding`, `P81ContainerSignaling`. Parsed metadata and actual application remain distinct. |
| `DvIntermediateDomain` | `NormalizedBaseVdrComponents`, `ReshapedBaseVdrComponents`, `ReconstructedVdrComponents`, `BaseBt2020PqRgb`, `ReconstructedBt2020PqRgb`, `TargetMappedBt2020PqRgb`, `LimitedBt2020NclYuv420P10`. Domain transitions/operation order are explicit; RGB already reconstructed cannot be reshaped again without an intentional, separately proved transform. |
| `DvMetadataSubset` | Named, revisioned contract plus exact parser/recipe predicate digest and parsed metadata levels. Initial names: `SyntheticP7IdentityLinearNlqV1`, `SyntheticP7AffineLumaLinearNlqV1`, `SyntheticP81IdentityV1`. Every predicate carries supplied matrix coefficients/depths/curve/NLQ constraints, no trims, fresh-RPU requirement and bounded raster/frame envelope. No `all_dv` variant. |
| `DvBackendIdentity` | SHA-256 executable, parser/library inventory, graph/patch and ABI contract; OS/architecture, driver/device identity when output depends on them. Validate lowercase 64-hex digests. Node name, probe time and observer counts are not byte semantics. |
| `DvCapabilityEvidence` | Identity above, schema/revision, supported source predicates/operations/transitions, exact decoder+encoder pair, output contract, target-policy identity, resource/timing envelope and evidence digest; `SyntheticControl` or `ProductionRoute` scope. Never construct `ProductionRoute` from a successful build/filter list. |
| `DvTargetPolicy` | Named/revisioned policy, integer peak/black millinits (`0 <= black < peak <= 10_000_000`), gamut, mapping algorithm+canonical parameters, precision/dither/resampling and static metadata policy. Fixed rendition versus measured display provenance explicit. M0 synthetic MaxCLL/mastering constants are unmeasured, not production content facts. |
| `DvStrategy` | `BaseRpuToHdr10`, `FelRpuToHdr10`, `ReconstructedProfile81`; existing native/base/converted-copy paths are returned as unchanged existing decisions. Each new strategy requires its entire operation chain and target/output contract. |
| `DvProcessingPlan` | Source/cache binding, captured preferences, strategy, capability identity+contract revision, exact ordered operations/domains, source predicate, target/output contract and bounded fallback order. Private, immutable validated plan. No post-observation setters. |
| `DvSelection` | `KeepExisting { reason: DvFallbackReason }` or `Selected(DvProcessingPlan)`. A test-only resolver can exercise synthetic scopes; production selection rejects them. |

An operation set alone is insufficient: reshape followed by residual/color
conversion is different from applying the same set twice. Preserve a validated
ordered graph, with predicates attached to the source/domain edges. Metadata
levels present in the source cannot satisfy an operation that was never applied.

Proposed pure interface (names provisional; no implementation is claimed):

```rust
pub fn resolve_dv_processing(input: &DvResolutionInput)
    -> Result<DvSelection, DvContractError>;
```

`DvResolutionInput` contains the existing delivery decision, selected worker
capability snapshot, source facts, preferences and target contract. M1 tests
use an explicit test-only synthetic-evidence constructor. Native compatibility
bypasses enhancement only when the existing decision actually delivers native
DV copy; client DV capability alone never overrides forced encoding, resolution
constraints or required subtitle burn. No runtime caller
switches to a new producer in M1. P5 remains on the existing required renderer
path; absence of an HDR10-compatible base cannot enter compatible-base fallback.

### 3.1 Initial synthetic graphs are exact, ordered contracts

These graphs exist only through test-only synthetic constructors. `DecodedBl`
is native decoded YUV with explicit representation normalization; it is not an
already reshaped VDR picture. Each edge validates its input/output domain.

| Synthetic strategy | Ordered pixel transitions | Separate metadata contract |
|---|---|---|
| `BaseRpuToHdr10` mechanism | Decoded BL → representation normalization → `NormalizedBaseVdrComponents` → declared identity/affine polynomial → `ReshapedBaseVdrComponents` → supplied RPU matrix/PQ conversion → BT.2020 PQ RGB → explicit BT.2020 NCL/range/chroma conversion → Main10 HDR10 | Fresh supported source RPU; no target mapping or creative trim claim. EL is explicitly not processed/dropped where present; this is not reconstructed RGB. |
| `FelRpuToHdr10` mechanism | Same normalization/polynomial → `ReshapedBaseVdrComponents` → exactly once supported linear NLQ with accepted same-frame EL → `ReconstructedVdrComponents` → supplied RPU matrix/PQ conversion → `ReconstructedBt2020PqRgb` → explicit NCL/range/chroma conversion → Main10 HDR10 | Fresh supported source RPU per frame. No target mapping/trim claim. |
| `ReconstructedProfile81` mechanism | Same full reconstruction through `ReconstructedBt2020PqRgb` → explicit NCL/range/chroma conversion → new Main10 base | Separate pixel-independent metadata edge: source P7 RPU plus reconstructed-base contract → `To81` + remove source mapping/residual → parsed identity-mapped residual-disabled P8.1 RPU associated with that new base. Elementary insertion/control only; container conformance remains unavailable. |

`BaseBt2020PqRgb` names the first row, so dropping FEL cannot be mislabeled
reconstruction. `Bt2020NclConversion` and `RepresentationNormalization` are
distinct operations. The broad `BaseNormalization`
variant describes a future profile-normalization backend and is unconstructible
in initial tests. MMR, creative trims, target mapping and P8.1 container
signaling are also unconstructible; enum presence is vocabulary, not support.
HDR10 mechanism rows have an explicitly unmapped working/output target policy,
not a physical-display qualification. Production HDR10-E still has no admission.

Reject duplicate reshape/residual/color-conversion edges, missing required
residual/color conversion, target mapping on a wrong domain, and metadata that
retains the original nonidentity curve on the reconstructed destination.
The independent affine wrong-curve control supplies the repeated-mapping
negative; identity-only controls cannot prove that guard. Do not implement a
generic nonlinear adaptation rule from this single affine recipe.

### 3.2 Production admission is absent in M1

M1 provides no trusted production-qualified registry entries and no constructor
that grants `ProductionRoute` authority. Production selection and HDR10-E
qualification therefore refuse all new candidates. A syntactically valid
payload, digest, scope label or private-field deserialization cannot create
that authority. Test-only synthetic constructors always retain synthetic scope;
relabeling serialized `SyntheticControl` as `ProductionRoute` must be rejected.

A later registry must be owned by daemon qualification/published worker facts,
with explicit reviewed operation-chain/output/timing/target/resource predicates
and exact executable/dependency identities. Define `DvProofValidity` separately
from byte identity: observed identity/snapshot revision, observation time,
expiry (or identity-bound non-expiring policy), invalidation reason and required
runtime continuity checks. Expiry affects admission, not output bytes/cache
identity. An absent, expired or invalidated observation is ineligible; no build
success substitutes for registry admission. M1 tests identity invalidation and
expired observations using synthetic authority only.

## 4. Receipts — count display-frame work and bind published intervals

`DvFrameKey` is source stream identity plus continuity epoch and rational
presentation timestamp/duration, with a display ordinal retained for diagnostics. Rational values use reduced signed `i64` numerator/positive denominator,
checked arithmetic and explicit unknown timestamps. No floating-point joins.
Unknown/ambiguous timing fails enhanced acceptance. Do not manufacture 1/24
when actual container durations are 41 ms and PTS gaps alternate 41/42 ms.

Initial association rejects duplicate/ambiguous BL or EL timing; a local
display ordinal is diagnostic only and cannot legitimize equal-PTS matches.
A future association identifier needs separately qualified source-grounded
evidence. PTS can be signed; known duration must be strictly positive.
`DvDurationProvenance` is `StoredContainer`, `DeclaredFixtureInterval`,
`DecoderInferred` or `Unknown`; retain the observed value and provenance.
Decoder API duration is not automatically container/source truth; inferred or
unknown duration cannot satisfy an initial production timing contract.

A producer-owned bounded `DvFrameAcceptance` binds the consumed BL/EL payload
identity, fresh raw RPU identity, parsed recipe/subset, frame key, operations
actually applied, intermediate/output contract and acceptance/rejection.
Fresh metadata and accepted legitimate reuse require distinct evidence states;
initial subset rejects reuse. Parsed side data surviving a missing raw RPU
cannot satisfy fresh metadata acceptance. Zero NLQ residual can still be an
accepted/applied EL; it does not require changed pixels.

`DvProcessingReceipt` contains schema, plan/recipe digest, output-generation
and attempt IDs, backend/source identity, interval/object-shape binding,
observation completeness, output contract, actual operation counts, per-stage
counts and terminal/fallback reason. Stage counters use checked `u64`. They summarize evidence; they cannot grant
qualification independently. An interval derives acceptance from unique,
once-only terminal per-frame records for exactly its emitted display set.
Every emitted frame must satisfy the same frame-key RPU/EL/ordered-operation
chain; unrelated stage successes cannot be joined by counts. Duplicate events
and rejected-frame events never inflate success. A nonempty served object
requires a nonempty, complete coverage set; zero-frame, partial and sampled
receipts cannot qualify that object/interval. Reject missing/extra frame keys
and overlap across independently settled interval records.

The summary equations are:

- Display frames: `seen = accepted + rejected`; `emitted <= accepted`.
- RPU frame decisions: fresh accepted · reuse accepted · rejected · missing;
  mutually exclusive over seen display frames. Reuse accepted is zero initially.
- EL frame decisions: accepted · rejected · missing · not required; mutually
  exclusive over seen frames. FEL strategy cannot emit missing/rejected EL.
- Per-operation applied counts cover emitted frames when the plan requires
  that operation. Counts are frame applications, not raw NAL totals.
- Residual dropped frames and unprocessed metadata levels are explicit. An
  attempted reconstruction failure cannot retain a successful FEL application
  count in the effective successor's receipt.

A bounded interval accumulator emits receipts per retained part/object range;
no unbounded per-movie frame log is required at playback. Retain detailed
frame hashes in diagnostic/replay artifacts, and bind interval receipts to
published object hashes/shape through the existing manifest/retained-part
machinery. Those SHA-256 records provide corruption/identity checks, not proof
against a writer who can replace bytes and recompute every digest. Publication
must retain the existing trusted producer/worker boundary; no new signature or
security guarantee is claimed here. Completed receipts cannot assert complete observation after EOF
error/cancellation. A prepublication first-frame receipt is explicitly partial;
it is never recycled as complete-file evidence.

`DvFallbackReason` is typed, with bounded human detail kept separate: disabled
preference · native compatible · conversion forbidden · unsupported source
profile/base · unknown EL kind · absent/malformed/reused RPU · unsupported
metadata subset/trim/matrix · missing/misaligned EL · stale backend proof ·
unqualified source binding · unsupported decoder/encoder/geometry/cadence ·
unqualified target/resource envelope · invalid output/base contract · backend
failure/cancellation · unsupported receipt schema. These are new DV reasons,
not replacements for `playback_control::FallbackReason` or decode fault enums.

**HDR10-E readout:** only a validated, integrity-bound, recognized receipt from the trusted
producer/publication path for the effective
generation and served interval, matching its plan/output/backend/source and
confirming a qualified DV operation chain, can report enhanced HDR10. A plan,
source flag, saved setting, synthetic receipt or legacy decode-health receipt
cannot. FEL contribution is separate from metadata processing; native/P8.1
labels remain Dolby Vision. Future views clear the enhancement summary before
showing any successor lacking matching evidence. M1 only tests this predicate. The effective generation must still be active;
a terminally failed/retired generation cannot keep the current enhancement
summary even if an earlier interval succeeded.

## 5. Identity and lifecycle — freeze policy, replace generations on failure

Keep descriptor continuity outside cache identity. Extend the future semantic
`ResolvedTranscode` with `Option<DvProcessingPlan>` and feed its canonical
semantic digest into `plan_digest`; a missing field preserves legacy behavior.
The digest includes byte-changing strategy/graph/metadata adaptation/target,
source subset, decoder/encoder contract, output/domain/precision policy and
backend identity. Do not hash node names, planning revision, counters or
receipt outcomes. Preference bits matter only where they change selected
semantics; requests with the same effective old fallback can share old bytes.
Existing recipe/presentation namespaces continue composing that single plan.
Do not add `Hdr10Enhanced` to `OutputGrade` (currently `Sdr`/`Hdr10`): HDR10-E
is processing evidence for the same HDR10 transport. New P8.1 needs a separate
DV output/signaling contract; `OutputGrade::Hdr10` alone cannot assert it.

Do not equate `ArtifactQualification::HealthQualified` with DV-qualified.
New enhanced artifacts eventually need a separate versioned DV receipt
requirement/namespace component, even if decode-health enforcement is off.
Legacy caches cannot be relabeled enhanced. Peers unaware of the new contract
are eligible only for old routes. M1 tests these rules; M3 performs integration.

Capture source/settings/selected-worker capability/target once for a generation.
New settings affect new decisions; they cannot mutate a running generation.
Before publishing, failure can select the next policy-permitted candidate under
a new plan/recipe/generation. After publishing, an unmet operation retires the
old generation and restarts through the existing session transition protocol
at a verified timeline anchor; it cannot splice base-only output into enhanced
segments. The candidate-attempt budget and circuit-break state belong to one logical
playback recovery episode across all successor generations. Each candidate is
attempted at most once in that episode; creating a successor never resets the
budget. Only an explicit new playback request or a separately qualified reset
may replenish it. Model an exhausted episode as a bounded existing fallback
or terminal result, never an A → B → A loop. M1 validates this state without
starting a producer.
Seeking requires fresh association/continuity checks and cannot reuse a stale
receipt merely because the source/catalog identity still matches.

## 6. Ownership and focused implementation checks

| Task | Likely files | Acceptance |
|---|---|---|
| M1 contracts only | New `transcode/dv_processing.rs`, `transcode/mod.rs` export; minimal shared imports | Pure types/validation and test resolver compile on pinned toolchain; old runtime callers unchanged |
| M2 producer | Daemon `producer_spawn.rs`, `vod/generation.rs`, `vodgen.rs`, transcode manager | Owned decode/render/encode children, bounded queues/permits; real observations required |
| M3 planning/identity | `transcode/decode.rs`, `recipe.rs`, manager `plan.rs`/`candidates.rs`, manifest/retained receipt validation | One canonical plan, selected-worker proof, incompatible cache refusal and generation restart |
| M4 settings/reporting | `store/mod.rs` keys, system/developer APIs, existing playback views and client surfaces | New switches default-off/save without capable worker; HDR10-E only from effective receipt |

Preference expectations are explicit: `hdr_processing` governs HDR10
processing independently of `conversion_permitted`; disabling P7-to-P8.1
conversion does not disable HDR processing. P8.1 FEL reconstruction requires
both `fel_reencode` and `conversion_permitted`; `hdr_processing` does not
enable or disable that P8.1 choice. Old base-copy P8.1 conversion still follows
its existing permission and numeric/client eligibility. No switch overrides
an existing native-copy/force/subtitle delivery constraint.

New keys are exactly `playback.dolby_vision_hdr_processing` and
`playback.dolby_vision_fel_reencode`; public fields match the proposal. Persist
in M4 using the existing planning snapshot, whose `playback.*` changes already
advance the input revision. Do not add readiness-based Save rejection.

Proposed meaningful M1 regressions (names become PR fields only after written):

| Case | Failure it must catch |
|---|---|
| All eight preference combinations across native/P81/HDR10 destinations | New switch overriding old conversion/native/force/subtitle policy |
| No production evidence, wrong worker identity, unknown schema or expired proof | Builds and M0 fixture success becoming runtime eligibility |
| Unknown EL; fresh raw RPU missing with stale parsed side data; reuse; source/profile/trim/matrix change | False classification/application evidence |
| Affine original → reconstructed base → identity adapted curve versus repeated curve | Plan allowing the same reshape to be applied twice |
| Zero residual and identity RPU with complete acceptance | Changed-pixel eligibility incorrectly refusing valid application |
| Overflow, typed enum/unknown fields, invalid rationals/duration provenance, duplicate/ambiguous BL/EL timing, inconsistent stage counts | Receipt acceptance from malformed or contradictory observations |
| Missing-RPU emitted frame, duplicate frame, unrelated per-stage successes, zero-frame nonempty object and partial coverage | Vacuous/disconnected counters becoming interval qualification |
| Synthetic payload relabeled production | Self-asserted scope becoming production eligibility/HDR10-E |
| Receipt source/backend/generation/interval/object-shape mismatch | Stale HDR10-E or artifact qualification surviving a replaced generation |
| A → B → A and repeated successor generation | Recovery episode budget cannot be reset by generating another successor |
| Semantic digest mutation versus execution/revision/counter mutation | Cache collision or unnecessary cross-producer identity split |

Run the focused new core test module on Rust 1.97.1, plus existing recipe
`execution_only_fields_are_not_part_of_the_plan_or_recipe_identity` and
`planned_v4_recipe_hash_is_a_golden_fixture`, and playback native/conversion/
forced-original regressions. These existing tests must remain green because
M1 promises no old route/cache changes. Required `check`, formatting and
Clippy evidence must describe the final task branch, not this scratch design.
No new regression command is claimed to have run in this design pass.

## 7. Boundaries that stay explicit after M1

Do not expose general P8.1 conformance, creative trim support, independent DV
picture fidelity or realtime performance as true/false inferred from mechanics.
Keep unsupported/unavailable evidence as explicit status or null. General
nonlinear metadata adaptation, MMR and nonstandard matrices, target-display
mapping, legitimate reuse, seek/VFR/discontinuity and full production resource
qualification remain separate work. M0 proves enough to describe operations;
it does not provide a production capability advertisement.


## 8. Additive implementation — exact source, no production authority

The actual contracts live in
[`dv_processing.rs`](../../crates/plurx-core/src/transcode/dv_processing.rs),
with focused unit checks in
[`dv_processing/tests.rs`](../../crates/plurx-core/src/transcode/dv_processing/tests.rs).
They are exported as `transcode::dv_processing`; no existing playback, plan,
recipe, producer or API calls them. The default preferences preserve the old
conversion permission and keep both new preferences off. No settings are
persisted in M1. No production registry entry or production/source-timing
qualification constructor exists.

Current public interfaces include `resolve_dv_processing(&DvResolutionInput)`,
`DvGraph::new`, `DvSourceFacts::new`, `DvTimestamp::new`, `DvDuration::new`,
`DvIntervalObserver::{new,record_decoded,accept,emit,settle}`, and
`DvRecoveryEpisode::{attempt,record_failure}`. Validation returns
`DvContractError`; the production resolver always retains the existing route.
The internal synthetic constructor/selection paths are unit-test-only.

The implementation adds an independently declared fixture
`DvIntervalExpectation`: source/stream/epoch/evidence binding, exact bounds and
complete expected in-interval frame/duration set. It is never derived from the
observed emitted subset. `settle` additionally receives a shared
`DvIntervalSettlementLedger`; overlapping settled ranges in the same source
continuity epoch fail. Genuine accepted preroll stays outside the served set;
omitting an interior expected picture cannot be reclassified as preroll.
Initial bounds must coincide with the source trace; a declared 200 ms target
between source pictures remains unqualified. These are bounded synthetic
contract checks, not a new seek or presentation policy. The separately reviewed
[timeline follow-up](DV_HDR_TIMELINE_CONTROLS.md) retains actual VFR/seek/epoch
mechanics; the initial decoded-layer bundle alone does not establish them.

The semantic digest includes the selected absolute video stream. Descriptor
continuity, sample coverage, observation revision, expiry and counters are
excluded. No shipping recipe is changed. Fresh raw-RPU events bind their own
frame key; destination metadata binds the source RPU and reconstructed-base
identities. Parsed/present L1/L6 levels remain explicitly unprocessed by the
creative-mapping stage. Each emitted frame needs its own exact predicate and
once-applied operation chain; sample evidence cannot certify later pictures.

The initial resolver admits only the known FEL P7 predicates; MEL/none are
refused before a plan is selected. `BaseRpuToHdr10` remains graph vocabulary
and a separately validated mechanism shape, not a selectable qualified base
predicate in M1. Declared source and observed BL/EL shapes/representations
match the exact 64×64 capability; later-frame and enhancement mismatches fail.
Recovery failure transitions require an active attempt and exhaustion remains
persistent even for an unattempted candidate.

The initial predicates admit only the supplied standard matrices/depths,
identity/affine polynomial curves, exact linear NLQ controls and fresh metadata.
MMR, broader matrices, creative trims and general nonlinear adaptation remain
unsupported. Whole-RPU parsing, actual pixel processing and evidence collection
belong to later graph/producer work; typed producer declarations are not a
commercial Dolby oracle or a production qualification receipt.

`fail(reason)` stores the reason in the observer, but a failed observer's
`settle` returns `IncompleteCoverage`. M1 emits no failed receipt or retained
failure-reason telemetry; every successful receipt has
`terminal_failure = None`. M2/M3 own persistence and reporting of failed work.

**Verification state:** on effort base `03fa9d2`, the pinned Rust 1.97.1 loop
passed 25 contract tests, six existing compatibility tests, core all-target
checking and warnings-denied Clippy. The explicit rustup toolchain was used;
the host's default compiler is not equivalent evidence. With that toolchain
on `PATH` and an isolated `CARGO_TARGET_DIR`, the commands were:

```sh
rustc +1.97.1 --version
cargo +1.97.1 test -p plurx-core --features hiqlite-store --lib --locked transcode::dv_processing
cargo +1.97.1 check -p plurx-core --features hiqlite-store --all-targets --locked
cargo +1.97.1 clippy -p plurx-core --features hiqlite-store --all-targets --locked -- -D warnings
```

The compiler reported `rustc 1.97.1 (8bab26f4f 2026-07-14)`. The six named
compatibility regressions cover unchanged execution/recipe identity, the v4
recipe golden fixture, conversion negotiation, incompatible bases, forced
original playback and transcode conversion reporting. The task pull request
retains their exact invocations and results. Documentation-index checks and
infrastructure-name checks passed. Actual-diff approval and landing are tracked
in the [canonical ledger](DV_HDR_PROCESSING_STATUS.md); these compiler/test
results do not confer production eligibility.
