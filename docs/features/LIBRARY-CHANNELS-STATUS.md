# Library channels status — what is built and what remains

**Status:** shipped on `main`; production collection-route correction
implemented in source and tracked by [#233](http://192.168.4.7:3000/noirr/plurx/issues/233)
· **Updated:** 2026-09-10 · **Correction base:** `cb76cc8a`

Companion to [FEATURES.md](../FEATURES.md) (what Plurx supports),
[PLAYBACK.md](../PLAYBACK.md) (finite-media delivery), and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (how this effort reaches
`main`) — this is the short answer to *where is Library channels, what is
proved, and what remains?*

## Progress — shipped feature, focused production correction

| Milestone | State | Evidence |
|---|---|---|
| M0 isolated base and compiler | complete | clean independent clone; current `main` at `cb76cc8a`; Rust 1.97.1 compiler loop established before Rust edits |
| M1 recipes, schedules, and durable storage | built; pinned workspace compile passed | one normalized evaluator and deterministic clock/order implementation; SQLite v55 and Hiqlite v36 entities/build state, authorization-at-write, 24-hour bounded idempotency, coherent catalogue snapshots, renewable claims, guarded publication, immutable-vector LRU, bounded pruning, and import census |
| M2 API and playback purpose | built; pinned workspace compile passed | bounded authenticated CRUD/opaque-preview/guide/resolve routes, idempotent rebuild/delete, pinned-occurrence finite-HLS starts, durable purpose binding, control-time authorization, and following-mode start/history isolation |
| M3 web | built; integration compile passed | responsive paginated browse/guide, three-step resumable editor, stable preview seed, admin management, fenced following playback, watch-from-start/return, and advisory Developer enablement |
| M4 Apple | built; iOS and tvOS compile passed | native paginated guide, three persisted tvOS layouts, iPhone/iPad resumable preview/authoring from navigation or title detail, finite playback control, server-monotonic following, and ordinary watch-from-start/return; build 132 |
| M5 Android | built; Android APK compile passed | native paginated guide, phone/tablet resumable authoring from navigation or title detail, three Google TV layouts, finite playback control, server-monotonic following, and ordinary watch-from-start/return; versionCode 81 |
| M6 promotion | complete | PR #231 merged as `cb76cc8a` after exactly one adversarial review and a green current-head Main promotion gate |
| M7 production collection route | implemented; promotion state is linked from issue #233 | deployed clients request `/api/v1/library-channels/`, which Axum 0.8 returns as 404 while the canonical no-slash collection route authenticates normally; the Store schema and list query are healthy; server compatibility plus corrected web, Apple build 133, and Android versionCode 82 are in the candidate |

## Production correction — route failure, not Store failure

The first production load exposed a client/server URL spelling mismatch. Web,
Apple, and Android used a trailing slash for collection list and create, while
the nested Axum 0.8 router exposes the canonical collection path without it.
On the deployed `cb76cc8a` server, the canonical request reaches authentication
and returns 401 without a token; the trailing-slash request returns 404. The
replicated schema is version 36 on the inspected nodes and the list query runs
successfully, so the former generic “cannot establish authoritative channel
state” message was not truthful evidence of a Store failure.

The correction moves all clients to the canonical no-slash route, retains the
first client spelling as a server compatibility alias during rollout, and
distinguishes route/transport failures from typed Store failures in the web
empty state. The compatibility alias changes no enablement or authorization
rule.

## Current decision — the merged Live TV guide is the UI seam

Forgejo `main` already contains the 2026-09-08 Live TV guide and client
layout work. Library channels reuse its guide presentation and preserve
its tuner controller as a separate source adapter. There is no parallel
layout implementation and no combined tuner/library transport.

The host default is Rust 1.98.0, but Rust 1.97.1 is installed through
`rustup`. Every Rust compile command for this effort explicitly uses the
pinned toolchain so Homebrew's default cannot silently become evidence.

## Enablement — explicit and advisory

Library channels are compiled into every ordinary build. An administrator
gets an enable control in Settings → Developer together with live readiness
facts: authoritative store availability, catalogue media with a successful
video probe, and client/server compatibility. Those facts explain likely
failure; they never override the administrator's choice or hide the feature.

The switch controls new resolve/session admission, not discovery or authoring.
This keeps the empty page and editor available while an administrator inspects
or repairs readiness, and it preserves a disabled channel definition without
pretending the readiness verdict has authority over the explicit switch.

## Promotion rule — review once, qualify once

Implementation commits accumulate on `effort/library-channels`. Only the
complete main-bound draft receives one adversarial agent review. The author
addresses that review without requesting a second pass, marks the pull request
ready, applies `fast-lane`, and merges only when the current head has a green
Main promotion gate. Full unit and device sweeps remain owned by the separate
test-maintenance process.

## Review closure — one pass, owned by the author

The one adversarial review was completed against the frozen draft. Its
findings were addressed in one author-owned repair pass: ordinary visibility
no longer inherits administrator management access; builds have durable
queued/building/ready/failed acknowledgement; catalogue and activation fences
are retried without publishing stale work; generation identity includes the
seed but excludes operational recipe fields; preview seeds and first-ten rows
are stable; Store outages and queue pressure have typed responses; and guide
clients consume every bounded page.

Following playback now binds its complete canonical worker recipe to the
durable request identity before producer placement, carries that purpose
through version-fenced ownership transfer, and rejects activation when the
pre-placement record is absent or different. Web, Apple, and Android use
finite playback control, pause safely at boundaries, expose ordinary
watch-from-start and return, preserve editor drafts, and separate guide focus
from the playing occurrence. The diagnostic surface adds channel/programme
activity plus bounded build, queue, catalogue, conflict, resolve, cache, and
unavailable-slot metrics.

The first promotion run then found four integration defects that compilation
could not prove. Ordinary VOD requests now treat an absent or JSON-null channel
purpose identically; channel refresh SQL binds placeholders in first-use order;
the replicated channel migration keeps trigger bodies intact inside one atomic
transaction; and a readdressed sole voter materializes a transferable database
snapshot before its recovery marker is cleared and peers may join. The static
schema, transaction, Store, import, readiness, downgrade, and UI inventories
are updated to describe the shipped surface instead of the pre-feature tree.
The later legacy migration shard also proved that adding v35 and v36 requires
admitting both v34 and v35 as migration sources before the corresponding
transactions can execute; the source allow-list and its direct boundary
contract now cover both steps.

## Evidence limits — compilation is not a device claim

- Pinned Rust workspace check and Clippy, iOS/tvOS generic-simulator
  compilation, and the Android debug APK build pass on the implementation
  head. Focused corrective contracts and the 78-capture UI baseline pass. The
  repository's full device and sweep suites were deliberately not run; the
  current-head Main promotion gate owns promotion evidence.
- Physical two-device convergence, tune-to-frame and transition latency,
  phone/tablet rotation and PiP, and Siri Remote/D-pad focus observations need
  the named devices. They are not inferred from compilation and remain the
  explicit follow-up acceptance record.
- Library channels never gate on a tuner, remote metadata service, AI key, or
  readiness verdict; only ordinary authentication and media permissions apply.
