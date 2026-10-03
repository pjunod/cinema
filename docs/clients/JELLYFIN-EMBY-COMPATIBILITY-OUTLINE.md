# Jellyfin and Emby compatibility — the whole effort and its boundaries

**Status:** open · proposal for review; implementation has not started ·
**Written / revised:** 2026-10-02 · **Original source:** `4d7257019` ·
**Review source:** `f1f1390f1` · [Review and disposition](JELLYFIN-COMPATIBILITY-REVIEW.md)

Companion to [CLIENTS.md](../CLIENTS.md) (the existing client strategy) and
[JELLYFIN-COMPATIBILITY-BUILD.md](JELLYFIN-COMPATIBILITY-BUILD.md) (the first
executable build plan). This document answers what the complete compatibility
effort includes, what comes first, and when to extend it. Give the separate
Jellyfin build document to Opus for technical review. This outline does not
claim that any third-party client already works with Plurx.

## 1. Objective — use existing apps with a Plurx server

Expose enough of the Jellyfin and, subsequently, Emby server protocols for
selected, unmodified apps to browse and play Plurx libraries. Plurx continues
to own authentication, cataloguing, media decisions, watch state and clustered
delivery. Compatibility is a new client-facing translation layer over those
services, alongside the native API and existing Plex façade.

The release promise is a table of tested **app version × platform × operation**.
An API implementation is not evidence that every app in an ecosystem works.
The full effort is bounded by the phases below; it is not a commitment to
reimplement every endpoint or every server-management feature.

## 2. The first release — Jellyfin movies and television

The first required clients are **Infuse 8.5 or later on tvOS** and the
**official Jellyfin for Android TV app**. Findroid is optional phone direct-
play coverage; it cannot qualify transcoding. The TV app is distinct from
Jellyfin's Android mobile web wrapper. J0 pins actual releases and hardware,
then captures Infuse's user-selected transcode flow and Android TV HLS/remux.
See the [review disposition](JELLYFIN-COMPATIBILITY-REVIEW.md) for the corrected
client selection and source evidence.

The minimum useful experience is manual server connection, username/password
login, libraries, movies, shows/seasons/episodes, artwork, search, resume,
watched/unwatched state, direct play, a compatible remux/transcode route,
audio-track choice, text subtitles and correct stop/cleanup behavior. A forced
transcode must be proved in a client that actually requests it; a successful
Infuse direct-play run does not satisfy that requirement.

The [Jellyfin build plan](JELLYFIN-COMPATIBILITY-BUILD.md) contains the exact
contracts, proposed source ownership, milestone order and physical acceptance
matrix. It is the only detailed implementation plan in this initial package.

## 3. Architecture — one media server, separate protocol adapters

```text
 Infuse / Android TV / later Jellyfin apps     selected Emby apps
                    |                               |
          Jellyfin HTTP adapter              Emby HTTP adapter
                    +---------------+---------------+
                                    |
                  shared compatibility model and identity
                                    |
             Plurx auth / catalogue / watch / media services
                                    |
               Store + session coordinator + media workers
```

The adapters may share DTO primitives and mapping code where measured
semantics agree. They must retain separate protocol versions, route behavior
and client acceptance records. Do not turn an Emby difference into a global
Jellyfin behavior change merely because both clients send an `X-Emby-*`
header.

Use one durable catalog and one watch-state authority. Do not run a second
Jellyfin or Emby server against Plurx's data, create a second scanner, proxy
through an independently managed server, or duplicate the transcode planner.
Those alternatives introduce state reconciliation and deployment work that
this façade is intended to avoid.

## 4. Phases — expand only after the preceding contract is measured

| Phase | Deliverable | Exit evidence | Dependency |
|---|---|---|---|
| P0: protocol and lifecycle spike | Pinned Jellyfin reference, client traces, browse/play/resume prototype and an honest external-client lifecycle design | Both clients connect; at least one renders media; unknowns and revised estimate recorded | This proposal's review |
| P1: useful Jellyfin release | Movies/TV contract in the separate build plan | Complete two-client matrix, focused regressions, exact-candidate qualification | P0's trace and lifecycle decisions |
| P2: broader Jellyfin clients | Individually add phone apps, a second Apple client or other requested apps | Each new app/version earns its own matrix row and retained fixtures | P1; choose clients by actual demand |
| P3: Emby feasibility | Authentication, server checks, entitlement behavior, browse and playback traced with named Emby apps | An explicit supported-app proposal; genuine blockers distinguished from missing endpoints | Reusable P1 services; no assumption that Emby is a drop-in alias |
| P4: useful Emby release | Separate adapter over the same Plurx services, for the selected apps | The same playback/security/cluster checks as P1 plus Emby-specific checks | P3; separate detailed plan reviewed before implementation |
| P5: optional capability packages | Live TV/DVR, music, downloads, discovery, remote control or web-client support | A bounded plan and per-capability client acceptance | Demand and traces; not prerequisites for P1 |
| P6: continued compatibility | Repeatable checks against supported app/server protocol versions | Published supported matrix, failures triaged on upstream changes | Starts with P1 and continues |

P2 and P3 are separable after P1. Their implementation order should follow the
apps the user actually wants, not the size of either upstream API.

## 5. Features that deserve separate decisions

| Capability | Why it is outside the first build | What a later plan must settle |
|---|---|---|
| Jellyfin web and web-wrapper apps | These depend on the web application as well as HTTP endpoints | Asset delivery, base URLs, API breadth, packaging and upstream license obligations |
| Official Emby apps | Some playback/features depend on app unlocks or server Premiere status | Whether each normally licensed app can operate against the façade; no fabricated entitlement responses |
| Live TV/DVR | Tuner ownership, guide/channel IDs, live session maintenance and recording rules add a different lifecycle | Channel/EPG mapping, live open/close, pause, resource ownership and recording API scope |
| Music/books/photos/home videos | Different hierarchies and metadata expectations | Separate type mappings, filters and playback/read semantics |
| Offline downloads/sync | Download authorization, offline watch conflicts and device state go beyond online VOD | Expiration, conflict resolution, revocation and actual app workflows |
| Casting/remote control/SyncPlay | Device discovery and bidirectional commands require more than playback reports | Target identity, command ownership, WebSockets and receiver behavior |
| LAN discovery | Useful convenience, not necessary for manual connection | Logical cluster identity, advertised reachable address and duplicate-node behavior |
| Native-equivalent recovery | Third-party apps do not execute Plurx's prepared-action protocol | Which failures the server can hide and which require a client reconnect |
| Server administration/plugins | This effort exists to consume media, not expose a second admin interface | Only revisit for a concrete, authorized workflow |

The official [Jellyfin Android project](https://github.com/jellyfin/jellyfin-android)
describes its web-client integration. [Emby's feature matrix](https://support.emby.media/support/articles/Premiere-Feature-Matrix.html)
documents app and Premiere dependencies. Those are scope differences to
measure, not reasons to promise universal compatibility.

## 6. Work estimate — planning ranges, not delivery dates

Assume one experienced engineer familiar with Plurx, reachable test devices,
an isolated reference Jellyfin instance and the repository's working compiler
loop. Human review, unavailable hardware and upstream client changes can add
elapsed time without adding the same amount of coding work.

| Scope | Cumulative planning estimate | Main uncertainty |
|---|---|---|
| P0 feasibility spike | 4–6 engineer-days | Client request flow and external playback lifecycle |
| P0 + P1 useful Jellyfin support | 5–9 engineer-weeks | Capability constraints, seeking, subtitles and cluster session binding |
| Broader Jellyfin support | 8–12 engineer-weeks | Which clients; whether web assets are required |
| Substantial Jellyfin + Emby app coverage | 3–6 engineer-months | Emby-specific behavior, supported platforms and chosen optional packages |

These ranges are cumulative and must not be added together. Full upstream
feature parity has no estimate here. P0 ends by replacing the first-release
estimate with one based on captured requests and a demonstrated lifecycle.
The revised contract uses passive VOD, excludes
rolling recovery, routes unindexed copy immediately to a valid remux/encode
alternative or refusal, and permits encoded VOD without an index when duration
and recipe are valid. Only transient startup outcomes get a bounded wait.
Renegotiation retains one producer per player instance. It also adds new compatibility-only watch-edit
fencing and token-only logout; these are work, not existing guarantees.
Ongoing maintenance remains necessary after the first release.

## 7. Delivery and product controls

Artwork policy is explicit in the build: J0 records credential carriers and
settles whether the façade needs anonymous item images. A UUID is not access
control. Native image auth stays unchanged, and streams/subtitles require
authenticated entry or an issued media capability.

Use the large-effort workflow from [AGENTS.md](../../AGENTS.md) and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md). The initial
implementation starts from then-current main, without unrelated local work,
on `effort/jellyfin-compat`; task branches start from its current head and
target it. Shared auth, routing, storage and playback files overlap, so the
independent-file exception does not apply.

The unfinished feature has one explicit switch in Settings → Developer.
Readiness is advisory and says which acceptance evidence is missing. It
cannot disable the switch, reject Save or override the saved value. After
the Jellyfin release qualifies, the permanent enable/disable control moves
to the appropriate connection/server settings section. Follow the existing
[Developer lifecycle](../features/SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md).

Supporting a new protocol does not authorize changing native playback policy,
relaxing auth, accepting invented render evidence or claiming an untested
client works. Each adapter uses the same resource and storage ownership rules
as the native API.

## 8. Review package and decisions

Opus approved the architecture and requested contract corrections; the
[review and disposition](JELLYFIN-COMPATIBILITY-REVIEW.md) records the changes.
The [re-review](JELLYFIN-COMPATIBILITY-REREVIEW.md) approves starting J0; the
[Jellyfin build document](JELLYFIN-COMPATIBILITY-BUILD.md) incorporates R1–R8.
Publish this indexed package as its own draft docs PR from main before J0.
J0 must still prove the selected client flows, idempotent lazy creation,
copy/encode fallback, transient startup deadlines and long-pause recovery. The proposed persistent state
must remain the minimum necessary for identity, fencing and native routing.

Decisions about Emby entitlement compatibility, serving Jellyfin web assets,
Live TV and native-equivalent recovery remain future-phase decisions. They
must not silently expand the first Jellyfin build.
