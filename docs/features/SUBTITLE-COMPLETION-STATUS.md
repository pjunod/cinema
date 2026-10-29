# Subtitle completion — one inventory for unfinished subtitle work

**Status:** ACTIVE · repair reviewed · all 21 focused regressions pass · deployed acceptance incomplete

**Date:** 2026-10-25 · **Updated:** 2026-10-29 · **Owner:** Codex /root · **Branch:** `codex/subtitle-artifact-admission`

The current repair is [PR #1332](http://forge.lan:3000/noirr/plurx/pulls/1332).
All 21 focused subtitle/storage cases and 2,129 policy cases have passed;
unchanged passing evidence remains retained. Native qualification exposed two
test interaction failures: Meridian setup steered vertically at a vertical
rail, and successful diagnostic captures consumed the live controls' idle
interval before Select. The fixture corrections preserve their assertions and
failure captures. Authenticated per-case native reuse is being completed so
only failed or missing journeys repeat. The web lane also retains its three
Node unit commands from authenticated original execution with unchanged inputs;
JavaScript parsing and TypeScript qualification remain active. Final independent
review approves both receipt changes. A green final gate remains required; the PR records the live counts and owner. The maintainer deploys,
then translation publication and visible generated/translated captions on the
physical TV still require deployed acceptance.

[PR #1309](http://forge.lan:3000/noirr/plurx/pulls/1309) merged as
`f9074711`, and the maintainer has now deployed `10f45ae5`, which contains it.
The running service is healthy with managed schema v2. A fresh Tiny English
model transfer (77,704,715 bytes), pinned hash verification and speech
self-test passed in about 26 seconds. A fresh request against the installed
Marian model passed its three-cue inference check in about 20 seconds; that
request reused existing model files.

Actual German-to-English subtitle translation still exposed a queue blocker.
The exact extracted WebVTT artifact exists and matches the requested input
hash, but the translation job shares the background media-reader slot with
unrelated extraction/indexing work. Its current worker can fall back to media
extraction on a cache miss, so simply removing that reservation would be unsafe.

The follow-up separates acquisition from artifact-only inference, uses a
distinct job kind that older workers cannot claim, and adds append-only
managed v3 / SQLite v121 resource upgrades. Legacy jobs keep their original
reader fence and explicit Retry reacquires the original verified input.
Implementation, the pinned workspace/all-target compiler check, normal
format/Clippy/syntax hooks and final adversarial review pass. The review found
one native pagination-status issue, which is fixed and covered by the new
methods. The original 18 focused cases pass: seven new/expanded core, four daemon,
three Web, two tvOS and two Android cases. Two additional changed managed-v3
fixtures pass against real replicated stores, and the strict clock-header
selector regression passes. This includes a real 40-second surround decode. Previously passing cases remain retained across main updates.
The only integration conflict was adjacent changelog entries; both are kept.
The unrelated sharing update does not change subtitle source or invalidate
these passes. The repair PR records its current fast-lane result and merge state; a green
gate remains required before merge.
[Tracking issue #1329](http://forge.lan:3000/noirr/plurx/issues/1329) records
ownership and the current repair branch; the PR carries gate/merge readiness.

A second confirmed deployed client failure occurs when Create returns
`operation_stopped` for the exact stopped recipe without an operation handle,
leaving clients unable to offer the existing explicit Resume. The candidate
returns that validated handle, adopts only matching read-only observation,
and retains explicit fenced Resume.

Fresh-file deployed acceptance now identified a separate concrete decoder
failure. Source verification of the 5.72 GB test file succeeds, the current
worker and speech-model receipt match, and browser playback advances through
eight minutes. Requested ranges fail during decoding with
`source_clock_unsupported`, leaving no committed cues. Exact source probing
passes. A bounded decoder replay identifies the root cause: FFmpeg surround
side-data continuation messages share the clock filter name but carry no frame
timestamps. The parser treats them as malformed clock records. Those metadata
messages also exhaust its byte cap before the window ends. The correction
selects frame records before bounded retention, ignores metadata in clock
parsing, and keeps malformed frame/overflow refusals and process ownership.
Actual timestamp continuity and the requested anchor pass without loosening
source-clock validation. The owned acceptance operation was stopped and its
test tab closed after recording the failure; the user tab is preserved. The
candidate reports current terminal range errors, including later failure
pages, instead of leaving the UI saying it is still processing.

On-the-fly generation and physical caption rendering remain unverified.
The selected Apple TV responds through its existing Companion pairing and
Cinema launches. An earlier 2160p HEVC remux route stalled near 90 seconds; manual playback
advanced, and the fresh-file Auto route advanced beyond eight minutes.
This is not a successful moving-playback or native-rendering acceptance result.
The maintainer owns deployments; no agent deployment or production database
mutation has occurred.

Companion to the [subtitle generation plan](SUBTITLE-GENERATION-USABILITY-IMPLEMENTATION.md)
and [progressive translation plan](PROGRESSIVE-SUBTITLE-TRANSLATION-PLAN.md).

## Earlier completion and qualification evidence

The earlier implementation was built in one owned clone and batched in
[PR #1292](http://forge.lan:3000/noirr/plurx/pulls/1292), owned by Codex /root.
The PR records current qualification results, merge blockers and the landing
receipt. Independent review findings, including final worker and native lifecycle deltas, are resolved. The user owns deployment;
no agent deployment is authorized. Physical Apple TV acceptance follows that
deployment and is distinct from server/runtime or simulator evidence.

| Area | Current implementation | Qualification and acceptance |
|---|---|---|
| On-the-fly captions | Shared progressive worker supports same-language transcription and speech-to-English translation, selected audio ordinal and actual media clock. | Review and focused regressions pass; real advancing-clock qualification produced useful captions at 13.3/14.6 seconds and complete 95-second coverage at 47.7/63.0 seconds on two CPUs. Final gate is tracked on PR #1292. |
| Existing subtitle translation | Pinned local multilingual-to-English Marian model, durable eight-cue slices, bounded partial storage, explicit stable-ID adoption. | Production multilingual self-test and durable-slice regressions passed; broader human accuracy remains acceptance evidence. |
| Apple/iOS/tvOS | Setup, three caption operations, synchronized rendering, Stop/seek/audio/auth ownership and Shared read-only delivery implemented. App and test targets compile. | 25 affected simulator methods pass; physical Apple TV acceptance follows user deployment. |
| Android | Equivalent caption operations and Shared reader; direct downloaded caption attachment repaired. Main and JVM test sources compile. | 47 affected JVM tests pass; device acceptance follows user deployment. |
| Web | Speech/text setup and controls, explicit track adoption, attachment handoff and Shared reader integrated. | 73 focused protocol/UI cases pass; deployed acceptance remains attributable separately. |
| Source/B sharing | Opaque grant-scoped catalog/manifest/segments, snapshot retention, read-only source verification. No Shared viewer inference authority. | Focused grant, retention and restart regressions passed; deployed acceptance is separate. |
| Landing | PR #1292 is the owned integration and qualification record. | Current check results, blockers and merge receipt are maintained on the PR. |

The historical inventory below records the starting point; it is not a claim
that the newly implemented operations remain absent.

## Advancing-playback correction — 2026-10-26

The user explicitly requested this correction before merge. The real35second
fixed-position Small test passed both speech tasks, canonical readback and
complete coverage. Its measured first publication was123/137seconds and full
completion177/194seconds on two CPUs. Its constant-position heartbeat did not
prove ordinary advancing playback. A later source audit found the manager
cancelled running work when its core fell behind the current scheduling
window; normal advancement could therefore repeatedly abandon inference.

The correction separates queued-window admission from claim-bound continuous
playback authority. Requests carry an explicit traversal UUID; seeks replace
it, while normal advancement retains it. Stop, expiry, source/producer changes
and resource limits remain authoritative. Implementation and bounded review
are complete, including expiry-gap revocation and native in-flight mutation
handoff corrections. The three exact-shape schema tests, 47 affected Android
tests, 25 Apple TV simulator methods and 73 focused web cases have passed;
passing cases are retained and only failed fixture cases were rerun.

The real advancing-playback qualification exposed another setup blocker before
playback began: Base recognition failed the existing speech-quality check.
That check remains intact and the unqualified Base progressive choices are
removed. Small retains its original profile identities, model bytes and
decoding parameters. The generic pinned engine disabled x86 vector instructions;
a reviewed statically linked AVX2 executable now accompanies the compatible
baseline. Runtime CPU selection keeps the existing held-descriptor execution
boundary and introduces no plugin loader. Both executables build, strict Rust
construction checks pass, and all 17 packaging/verifier tests pass.

The real moving-playback qualification passed for both Small tasks on the
95-second source with selected audio ordinal 2, after integration of main
`167af4616` and the reviewed engine-identity ownership correction. English translation first
became useful at 13.325 seconds and completed all four immutable cores at
47.709 seconds; same-language German first became useful at 14.628 seconds
and completed at 63.039 seconds. Ordinary 1x playback advanced through
72.711/73.040 seconds. Canonical leased readback observed useful current-core
captions repeatedly in both the 30–60 and 60–90 second bands (9/3 English
observations and 8/5 German observations). All eight range jobs and both real
Install/Verify jobs succeeded. No competing owned compiler work ran during
this qualification. The selected static engine SHA256 is
`3beb7ff9ca5660306022a1acd0a9c30df431f1fff860722632997076415adbc4`.

The full private receipt is retained outside compiler caches with the local
runtime log and source review. This proves the controlled source, worker and
readback path; physical Apple TV acceptance follows the user's deployment.
PR #1292 records the current-head main fast lane and landing receipt.

Final policy preflight corrections are reviewed: bounded knowledge-context
negotiation, explicit records for the four real-runtime fixtures and a typed
Android caption-task chooser with unchanged wire values. All eight failed or
affected policy checks and Android compilation pass; the final fast lane and
merge remain tracked on PR #1292. The unadmitted ignored-test budget stays 38.


## Final ownership and main integration — 2026-10-26

The ownership inventory prompted an additional explicit source review. It found
that cancellation of a text engine-identity request released serialization
while its blocking hash continued. The fix moves the permit and cache
publication into the hashing worker, accepts only nonempty regular executable
files up to 2 GiB, and reads at most the accepted length plus one byte. Four
focused regressions pass, including cancellation with retained serialization
and cache publication, bounded growth, changed identity, and invalid input.
The single inventory method passes with all seven reviewed counts; its other
15 structural categories remain unchanged.

Main `167af4616` added the published v115 playback-handoff schema. Integration
preserves that ordinal and appends the subtitle schemas at v116–v119, with
matching replay guards. A real v115-to-v119 fixture preserves handoff codes and
tokens; exact legacy and replicated caption upgrades also pass. The shared
admission constructor includes main's extension field. Independent source
review and all three formal correspondence checks are clear. Native and web
caption source is unchanged; existing passing cases are retained. The PR
records qualification of the integrated candidate and its final gate.

## Final adversarial review — 2026-10-26

Reviewed candidate `b7a475198439ed7e5af452d1fad226f14af549cc`, based on
`087e836d3`; request changes, six P2 findings and no established P0/P1.
The independent review covered server/core and text-model boundaries, with
separate Apple and Android/web readers. It executed no tests.

| Finding | Owner | Disposition |
|---|---|---|
| Apple cancelled mutation leaves controls busy; stale Stop status | Apple owner | Resolved: action serial and cancellation/status guards; compiled regression definitions. |
| Apple caption adoption precedes proven subtitle-off handoff | Apple owner | Resolved: accepted attachment proof, lifecycle/intent guard and preserved incumbent. |
| Android late text Use overrides newer caption choice | Android owner | Resolved: capture and recheck shared caption-selection intent. |
| Android partial failed range uses operation Retry | Android owner | Resolved: immutable range/revision retry targets, bounded failure pages and explicit ambiguity resolution. |
| Android transient reads end polling and can starve demand renewal | Android owner | Resolved: bounded concurrent read/renewal children, exact replay and lease expiry. |
| Text cue IDs accepted at 256 bytes but stored at 128 | Integrator | Resolved: shared 256-byte bound; parser and durable-publication regression definitions. |

The independent reviewer closed all six findings by bounded source review. An additional installer integration blocker was found during preparation: the shared downloader accepted only Whisper URLs. The fix admits only exact immutable compiled text-catalog entries and preserves the existing HTTPS/redirect checks; bounded source review also closed this issue. Fresh publisher download and production multilingual inference subsequently passed. No physical Apple TV
or deployed acceptance is implied by static review or compilation.

## Runtime qualification — 2026-10-26

The isolated Linux compiler/runtime target passed 295 focused daemon tests,
19 core translation/slice contracts and four new weight-loader safety tests.
Passing cases are retained; failures are rerun individually. Core commands
used `--features hiqlite-store`; this does not claim a deployed replica run.

Four real durable Install/Verify jobs qualified both speech tasks using the
pinned whisper.cpp1.9.2 engine, small model and German fixture. The worker used
its real decoder/inference and receipt publisher; the private SQLite receipts
survived reopen. This proves the fixture and setup/runtime path, not arbitrary
movie quality or realtime throughput.

Fresh Marian bundle download and SHA verification succeeded. Inference exposed
a concrete format mismatch: the publisher's authenticated PyTorch file uses
legacy protocol1001 serialization, while Candle's normal PTH loader expects a
ZIP archive. The pinned adapter now reads raw little-endian tensor data through
a compiled, authenticated layout and the verified held file. Production never
parses pickle. The developer-only generator checks the full model hash before
restricted metadata inspection; it reproduced the same 256-storage/258-tensor
layout twice. Bounds, aliases, cancellation and changed-file tests pass, and
production French/German/Spanish-to-English inference passed.

The actual progressive worker qualification uses a finite multi-audio clip
and canonical leased readback; its result and the final fast lane are recorded
on PR #1292. Device, broader language accuracy, long-form and deployed evidence
remain separately attributable; none is implied by these fixtures.

## Closing the remaining implementation gaps

The finite multi-audio worker run exposed a flat120second inference budget:
its35second window timed out while the shorter tail succeeded. The pinned
engine processes30second acoustic blocks; the reviewed correction assigns
120seconds per validated block, bounded to240seconds by the registered40second
context window. Decoder, resource admission, claim renewal and cancellation
retain their existing limits. The failed actual worker case must pass again.

Linux package staging now derives a readable FLEURS notice from the canonical
fixture metadata and includes the complete CC BY4.0 license in both package
formats. Caption rendering reconciles cue identities instead of clearing the
entire track on every manifest refresh. Bounded review closed both changes;
the packaging regression and two cue-delta/reselection regressions passed.

Atomic operation completion and worker reassignment passed bounded review.
Completion requires all expected immutable core segments with exact bounds
and accepted manifest revisions. Reassignment retains viewer demand and
cached segments while replacing the preparation job identity; old jobs cannot
publish after a move back to the original node. The SQLite117 and replicated
upgrade accept only the recognized previous trigger shape; unknown schemas
remain refused. Focused authority, replay, gapped/completed coverage, cached
rebinding and300-window lease regressions passed. The actual replicated-store
upgrade test also passed; this remains separate from fleet deployment.

All35 selected Apple TV unit cases and52 focused Android caption cases are
covered by passing results. Failed/affected fixture cases were reviewed and
rerun individually. Native caption owners now track their network tasks,
including captured demand cleanup, and expose awaitable retirement before a
transport closes. This closes a concrete test transport lifecycle race
without adding sleeps or changing synchronous player retirement. It is
simulator evidence, separate from physical Apple TV playback.

The actual multi-audio progressive-worker execution and final main fast lane
have separate qualification records. Their exact candidate and current result
are recorded on PR #1292, which is also the authoritative landing record.
Source review and compilation alone do not establish their success.

## Scope and initial inventory

| Work area | Existing contract or evidence | Initial state and unresolved work |
|---|---|---|
| Offline subtitle generation | [Generation usability implementation](SUBTITLE-GENERATION-USABILITY-IMPLEMENTATION.md); [recognition qualification](SUBTITLE-RECOGNITION-QUALIFICATION.md) | Whole-file background transcription and complete WebVTT publication exist. This does not generate captions progressively during playback. Android direct acquired/generated caption attachment, broader cluster/native/long-form quality, full post-merge CI and Linux package lifecycle are explicitly open in the generation plan. |
| Progressive same-language generation during playback | [Generation usability implementation §1.1](SUBTITLE-GENERATION-USABILITY-IMPLEMENTATION.md#11-what-on-the-fly-means-here) | Not implemented by the offline workflow: decode and inference run on the complete finite file, then publish. Live speech recognition and partial-caption publication are called out as a separate extension. Scope and contracts for this extension remain to be inventoried. |
| Progressive translation during playback | [Translation plan](PROGRESSIVE-SUBTITLE-TRANSLATION-PLAN.md); [translation ledger](PROGRESSIVE-SUBTITLE-TRANSLATION-LEDGER.md) | First accepted route is web direct playback, one audio stream, explicit spoken language. Native clients, multiple audio streams, encoded/remux delivery, fleet, long-film and broader human-quality qualification remain open. The plan's initial exclusions also include translation of existing subtitle files. |
| Existing subtitle-file translation | [Translation plan §1.1](PROGRESSIVE-SUBTITLE-TRANSLATION-PLAN.md#11-the-initial-scope-has-a-narrow-output-contract) | Explicitly outside the first progressive-translation build. No implementation or acceptance contract has yet been inventoried here; determine the intended input formats, language/model behavior, persistence and client delivery before assigning implementation status. |
| Model setup and runtime qualification | [Setup repair status](SUBTITLE-SETUP-REPAIR-STATUS.md); [translation ledger](PROGRESSIVE-SUBTITLE-TRANSLATION-LEDGER.md); [recognition qualification](SUBTITLE-RECOGNITION-QUALIFICATION.md) | Managed setup and admission repairs exist, but a 2026-10-25 translation Install flow error was reproduced and repaired in source; this inventory has not established deployed or current-tree acceptance. An earlier worker reported `output_invalid` with unresolved cause. Packaged translation-specific readiness and measured profile gates remain distinct from setup UI completion. |
| Deployment and fleet acceptance | [Generation usability implementation](SUBTITLE-GENERATION-USABILITY-IMPLEMENTATION.md); [translation ledger](PROGRESSIVE-SUBTITLE-TRANSLATION-LEDGER.md) | No deployment or fleet acceptance was established by this inventory. Prior ledgers explicitly distinguish local/package evidence from deployed state; gather exact candidate, platform, worker, model and receipt evidence before closing. |
| Existing subtitle delivery and reliability | [Subtitle reliability completion](../clients/SUBTITLE-RELIABILITY-COMPLETION.md); [cluster extraction status](../clients/SUBTITLE-CLUSTER-EXTRACTION-STATUS.md); [Apple native subtitle handoff](../clients/APPLE-NATIVE-SUBTITLES-HANDOFF.md) | These are canonical adjacent ledgers for established WebVTT readiness, native rendering, extraction and cluster work. Their remaining physical-device and fleet acceptance is not yet reconciled into this cross-cutting inventory. |

## Source-backed boundaries

- Offline generation publishes only after complete-file inference and VTT
  validation; its existing usability work does not promise playback-time
  captions.
- Progressive translation's accepted route is bounded to direct web playback,
  one audio stream and an explicit spoken language. The documented exclusions
  are not evidence those routes are impossible; they remain work to scope and
  build under the user's broader request.
- Model installation, model testing, feature enablement, and playback
  acceptance are separate states. A downloaded/cached model or successful
  setup request does not by itself qualify translation.
- The translation ledger records a setup response defect and an unresolved
  `output_invalid` from another worker. It states that no translation
  enablement was saved and no application was deployed for that incident.
- The subtitle reliability completion ledger covers established caption
  delivery behavior and retained client evidence. Its acceptance does not
  establish progressive generation, file translation or fleet-wide
  qualification.

## Inventory limits

This first pass read the repository instructions, knowledge skill, documentation
router and feature index, and the heads or relevant sections of the linked
subtitle ledgers. An initial sandbox refusal was resolved through the authorized owned-clone access path. Scoped server knowledge queries returned no curated coverage or conflicts; no absence of obligations is inferred. Source-level contracts, all delivery/audio
routes, current deployment state and fleet receipts have not yet been audited.
Those uncertainties stay open until traced to source and evidence.

## Work ownership

| Owner | Work | State |
|---|---|---|
| Codex /root | Integration, web adapter, text translation engine/setup, status and final landing | Implementing; one batched PR after final adversarial review |
| progressive_server_map | Shared progressive task/audio/clock/admission contracts and worker | Implemented; first source snapshot compiles; full integration pending |
| apple_completion_map | iOS/tvOS typed acquisition, synchronized text rendering, selection and setup | Implemented all three operations; iOS/tvOS app and test targets compile only |
| android_completion | Android typed acquisition, player-owned text rendering, selection and setup | Implemented all three operations; main and test sources compile only |
| GPT-6 Luna | Bounded documentation and Android source inventory | Initial inventory complete; no test execution |
| Codex /root | Existing-file translation requirements and implementation | Explicit text MT adapter implemented; speech inference is not reused for text |
| Codex /root | Model packaging, runtime profiles and final qualification | Model pinned; final runtime evidence pending. User owns deployment. |


## Integration and verification

All owners work in the independent task clone. File ownership is exclusive: the server owner holds progressive core/store/runtime and narrow shared admission changes; native owners hold their client trees; root holds web, the text-MT adapter and integration metadata. No unit or acceptance tests run during construction. The complete candidate receives one adversarial review; findings are addressed, then the required fast lane and named regressions run, retaining successes and rerunning only failures. Compile and lint checks remain available during construction. No main promotion or live acceptance is claimed until its exact-candidate evidence is recorded.

## Current construction evidence

The initial shared progressive Rust snapshot compiled on pinned Linux Rust1.97.1. Native iOS/tvOS app and test targets and Android main/JVM test sources compile; no tests have executed. Apple construction targets compiled at build276; the current merge candidate uses build277 after reconciling main. Android versionCode182 is a construction counter, not release qualification. The text-MT engine, pinned multipart model bundle and durable eight-cue job slices are implemented and undergoing integrated compilation. New text partials stay in bounded authoritative storage; the queue checkpoint holds counts/references, not subtitle text. Native text translation completion never changes caption selection; explicit Use refreshes caption identity before ordinary selection. Physical device, packaged model, long-film, fleet and deployment evidence remains open.

## Current-main integration — 2026-10-26

The final landing candidate incorporates the HTTPS work from main `656a85540`.
Subtitle producer-incarnation admission and fragment replay/source fencing both
remain in the shared queue; the schema stays append-only through v119. The
combined API has 403 routes. Nine Apple caption continuation checks now use
the existing named Attempt fences with unchanged viewer-action scopes; all
16 focused census checks and six affected simulator methods pass. Integrated
source review, affected checks and the exact-head fast lane are tracked on
PR #1292. Existing real inference evidence remains separately attributable.
