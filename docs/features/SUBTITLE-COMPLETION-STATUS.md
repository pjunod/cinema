# Subtitle completion — one inventory for unfinished subtitle work

**Status:** deployed acceptance incomplete · reviewed repair in PR #1309, qualification running

**Date:** 2026-10-25 · **Updated:** 2026-10-28 · **Owner:** Codex /root · **Branch:** `codex/subtitle-live-acceptance`

PR #1303 landed as `4be5ff26`; the maintainer deployed a later main containing
that work. The live media worker reports revision `f2120bd9`. A fresh text-model
download and three-cue production inference test succeeded. The installed Small
speech model also passed its same-language caption test. These setup results do
not yet prove captions render during playback or on the physical Apple TV.

Live acceptance found concrete blockers. Text-model installation incorrectly
shares the media-reader pool with library metadata work, causing a long setup
queue before any download begins. Progressive range demand also needs the
existing reserved live-reader slot under its durable authority predicates.
The paused web panel's Refresh status action does not refresh choices or server
time before an operation exists, leaving Start unable to recover after its
four-minute action clock expires. Foreground existing-subtitle extraction also
tries CPU admission once without registering live wait pressure, so background
work never receives the cooperative yield signal. Active progressive inference
also needs that signal before acquiring the shared heavy-worker lane. The fixes are implemented in [PR #1309](http://forge.lan:3000/noirr/plurx/pulls/1309),
owned by Codex /root. Final adversarial review and the normal compile/lint hook
passed. All 18 focused Rust cases passed, including real three-voter contracts
and FFmpeg extraction. Formal source mappings and managed-v2 fixture inventories
are corrected. Compatible passing cases are retained individually; only failed
cases were rerun. Preflight retained 2,104 compatible Python passes and reran
the two failed ownership-inventory methods successfully. Four type-only DOM
casts resolve the remaining web compilation diagnostics.

The main fast lane's live-identity guard now follows its documented head-only
execution policy; adversarial review and the new identity regression passed.
Source closures, authenticated origins, exact PR head and readiness checks
remain required. Current-main integration also changes the whole-file hash of
a mapped server source without changing its selected worker behavior. The
reviewed mappings are being rebound before final qualification; model
properties, bounds and authority claims remain unchanged.
No production database edits, service restart or agent deployment has occurred.

The physical Apple TV responds through its existing Companion pairing;
installed-app discovery and Cinema launch both succeeded. Its Xcode connection
remains unavailable and actual native caption rendering is unverified. Cinema's
server receiver registrations remain revoked despite the locally enabled
phone-control setting; this does not prevent Companion control. The maintainer
owns deployment of the reviewed repair, followed by deployed acceptance.

Companion to the [subtitle generation plan](SUBTITLE-GENERATION-USABILITY-IMPLEMENTATION.md)
and [progressive translation plan](PROGRESSIVE-SUBTITLE-TRANSLATION-PLAN.md).

The implementation is built in one owned clone and batched in
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
