# Library channels status — what is built and what remains

**Status:** single review addressed · corrective fast-lane rerun pending · **Effort:**
`effort/library-channels` · **Updated:** 2026-09-10 · **Base:** `7fabfbf2`

Companion to [FEATURES.md](../FEATURES.md) (what Plurx supports),
[PLAYBACK.md](../PLAYBACK.md) (finite-media delivery), and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (how this effort reaches
`main`) — this is the short answer to *where is Library channels, what is
proved, and what remains?*

## Progress — one frozen candidate will receive the expensive gate

| Milestone | State | Evidence |
|---|---|---|
| M0 isolated base and compiler | complete | clean independent clone; current `main` merged at `7fabfbf2`; Rust 1.97.1 compiler loop established before Rust edits |
| M1 recipes, schedules, and durable storage | built; pinned workspace compile passed | one normalized evaluator and deterministic clock/order implementation; SQLite v55 and Hiqlite v36 entities/build state, authorization-at-write, 24-hour bounded idempotency, coherent catalogue snapshots, renewable claims, guarded publication, immutable-vector LRU, bounded pruning, and import census |
| M2 API and playback purpose | built; pinned workspace compile passed | bounded authenticated CRUD/opaque-preview/guide/resolve routes, idempotent rebuild/delete, pinned-occurrence finite-HLS starts, durable purpose binding, control-time authorization, and following-mode start/history isolation |
| M3 web | built; integration compile passed | responsive paginated browse/guide, three-step resumable editor, stable preview seed, admin management, fenced following playback, watch-from-start/return, and advisory Developer enablement |
| M4 Apple | built; iOS and tvOS compile passed | native paginated guide, three persisted tvOS layouts, iPhone/iPad resumable preview/authoring from navigation or title detail, finite playback control, server-monotonic following, and ordinary watch-from-start/return; build 130 |
| M5 Android | built; Android APK compile passed | native paginated guide, phone/tablet resumable authoring from navigation or title detail, three Google TV layouts, finite playback control, server-monotonic following, and ordinary watch-from-start/return; versionCode 79 |
| M6 promotion | corrective candidate in qualification | exactly one adversarial review completed; fast-lane qualification exposed and directly covered promotion inventories, ordinary-session ownership, replicated schema parsing and migration-source admission, channel publication parameters, and sole-voter readdress snapshot boundaries; runner disk/tmpfs faults were repaired without changing the candidate; next is the current-head main-only fast-lane rerun and merge |

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
