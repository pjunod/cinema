# Subtitle completion — one inventory for unfinished subtitle work

**Status:** open · final qualification

**Date:** 2026-10-25 · **Updated:** 2026-10-26 · **Owner:** Codex /root · **Branch:** `effort/subtitle-completion`

Companion to the [subtitle generation plan](SUBTITLE-GENERATION-USABILITY-IMPLEMENTATION.md)
and [progressive translation plan](PROGRESSIVE-SUBTITLE-TRANSLATION-PLAN.md).

The complete implementation is being integrated in one owned clone and one
batched PR. Owner: Codex /root. Merge state: review findings resolved and independently checked; final qualification is starting. No fast-lane or unit/runtime success is claimed yet. The user will deploy the merged result. **No agent
deployment is authorized.** Target acceptance includes the server and Apple TV;
compile evidence alone does not establish physical playback or model accuracy.

| Area | Current implementation | Remaining qualification |
|---|---|---|
| On-the-fly captions | Shared progressive worker supports same-language transcription and speech-to-English translation, selected audio ordinal and actual media clock. | Final review, focused regressions, model/runtime and playback evidence. |
| Existing subtitle translation | Pinned local multilingual-to-English Marian model, durable eight-cue slices, bounded partial storage, explicit stable-ID adoption. | Real model self-test, final regressions and accuracy evidence. |
| Apple/iOS/tvOS | Setup, three caption operations, synchronized rendering, Stop/seek/audio/auth ownership and Shared read-only delivery implemented. App and test targets compile. | Final tests and Apple TV physical acceptance after user deployment. |
| Android | Equivalent caption operations and Shared reader; direct downloaded caption attachment repaired. Main and JVM test sources compile. | Final tests and device acceptance. |
| Web | Speech/text setup and controls, explicit track adoption, attachment handoff and Shared reader integrated. | Final protocol/UI regressions and playback acceptance. |
| Source/B sharing | Opaque grant-scoped catalog/manifest/segments, snapshot retention, read-only source verification. No Shared viewer inference authority. | Final authorization, retention and restart regressions. |
| Landing | Construction compilation in progress; no completion PR published yet. | Reconcile current main, commit, final adversarial review, run final checks once, merge green candidate. |

The historical inventory below records the starting point; it is not a claim
that the newly implemented operations remain absent.

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

The independent reviewer closed all six findings by bounded source review. An additional installer integration blocker was found during preparation: the shared downloader accepted only Whisper URLs. The fix admits only exact immutable compiled text-catalog entries and preserves the existing HTTPS/redirect checks; bounded source review also closed this issue. Real fresh download and inference remain required final checks. No physical Apple TV
or deployed acceptance is implied by static review or compilation.

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
