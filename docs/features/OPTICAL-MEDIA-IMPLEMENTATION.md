# Optical media — Sol's build contract for DVD and Blu-ray playback

**Status:** executing on `effort/optical-media`; M0 feasibility and foundations
in progress; no optical code or hardware acceptance exists · **Written:**
2026-09-20 · **Rebased:** 2026-09-20 · **Implementer:** Codex · **Original
inspected checkout:** `0afefd92a` · **Implementation base:** `7911325407b9`.

> **Implementation correction, 2026-09-20:** feature readiness is advisory,
> following the current Developer-settings contract and the operator's explicit
> ruling. The saved optical enable choice is authoritative even when drive,
> helper, package, cluster-version, or physical-media checks are unmet. Those
> checks remain visible beside the control and operations fail with specific
> capability, authorization, ownership, or media-generation errors when their
> real prerequisites are absent. This correction supersedes older language
> below that called readiness an enablement gate or made optical a startup-only
> setting.

Companion to [PLAYBACK.md](../PLAYBACK.md) (the current delivery paths),
[ARCHITECTURE.md](../ARCHITECTURE.md) (storage and cluster boundaries), and
[WEB-SHELL-LAYOUT.md](../clients/WEB-SHELL-LAYOUT.md) (where the web app lives).
This document owns the first release's product scope, source contracts,
implementation order and acceptance. It is self-contained: the earlier
conversation's interactive mockup is inspiration, not a required build input.

Build milestone by milestone on one effort branch. Reuse the normal playback
controller and native players. If physical-disc seeking cannot satisfy the
existing VOD contract, record the failing case and resolve M0 before building
the dependent playback work. Do not silently introduce full-disc ripping,
a legacy live presentation, a second player, or fictitious file IDs.

The authoring task requested a handoff document, not implementation or a
deployment. This document creates no additional task and dispatches no agent.
When assigned the build, Sol should execute in that task without asking again
about routine choices settled here. Hardware availability and product-scope
changes are explicit decision points, not reasons to pause unrelated work.

## 1. Outcome and first-release boundaries

### 1.1 The experience to deliver

A user inserts a DVD-Video or standard Blu-ray into a drive on a plurx server,
sees it on Home, chooses a feature/episode/extra, and watches on web, Apple or
Android through the existing player. Chapters, seek, resume, audio selection,
subtitles, normal Stop and source-loss reporting must work. No automatic
playback, modal on insertion, or automatic import of the disc's bytes.

The initial host adapter is Linux, including explicit device/mount access
for the packaged server. This is a planning default: the user has not supplied
a drive model, host OS, or representative media. M0 records the actual test
hardware. Keep host integration behind an adapter and preserve builds on
other platforms; those platforms report host support unavailable until an
adapter is implemented and accepted. Apple clients are in scope even when
the drive host is Linux.

| Required in this effort | Deferred, with reason |
|---|---|
| Physical DVD-Video and standard Blu-ray title playback | UHD Blu-ray: separate drive/protection/HDR qualification |
| Movies, multiple titles, episode discs and extras | Original DVD menus and Blu-ray HDMV/BD-J menus: separate navigation/overlay runtime |
| Web, iOS/tvOS and Android/Android TV presentation | Client-attached drive sharing: requires native host integration |
| One active viewing session per drive | Multiple viewers: competing reads need a different admission/caching policy |
| Configured Linux drives, insertion/removal, authorized eject | Arbitrary network URLs, user-supplied device paths or remote mounts |
| Disc-specific progress and optional verified metadata match | Automatic movie/episode matching from a volume label |
| Readable media and explicit capability/protection failures | A blanket promise that all protected commercial discs play |
| Tiny authored disc-folder fixtures for testing | User-facing ISO/folder library import and full-disc copying |

Physical media is the objective. Folder fixtures alone cannot complete this
effort. Commercial protection requirements remain visible: collect the user's
representative discs if available, report compatibility per backend/build,
and do not describe unreadable retail media as supported.

### 1.2 Release invariants

1. A disc swap never makes an existing URL, request or worker read the new disc.
2. Exactly one viewing session owns a drive; all physical reads are scheduled
   under that ownership. Inspection and subtitle work cannot race playback.
3. File playback requests and persisted legacy recipes retain their meaning.
4. The decision response describes an executable title-delivery plan. It never
   offers direct HTTP ranges over an optical block device.
5. Progress belongs to a user, disc edition, title and angle. It survives
   removal/restart without assuming that another cut has the same timeline.
6. Source ownership and availability are enforced on the drive's node, including
   when the client reaches a different cluster ingress node.
7. No-drive and disabled-feature installations retain existing behavior and
   incur no periodic optical probing.
8. Unknown metadata, protection, duration and host availability remain unknown
   in the UI; neither a spinner nor a guessed film conceals an error.

## 2. M0 preparation — preserve the checkout and prove the source

### 2.1 Branching, compiler and workflow

The authoring checkout contains unrelated uncommitted review/streaming docs
and edits to the docs index. Do not stash, commit, discard or reformat them.
Use an isolated checkout from current main and carry only this document and
its index row into the implementation effort. Check whether an optical effort
already exists before creating one.

Use `effort/optical-media` for integration and `codex/optical-m<N>-<topic>`
for task branches, each based on the current effort. Shared playback/store
files make the independent-main exception inapplicable. Keep task merges
serial; the ownership matrix in §12 is integration responsibility, not an
instruction to dispatch parallel agents.

Follow [AGENTS.md](../../AGENTS.md), the
[compile-loop runbook](../ci/AGENT-COMPILE-LOOP.md), and the dated corrections
at the top of [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md). Before
editing Rust, establish this loop on the repository's pinned toolchain:

```bash
rustc --version                          # must report the repository pin
cargo --version                          # record the actual cargo
rustup show active-toolchain             # inspect rust-toolchain.toml too
cargo check --locked -p plurx-core -p plurxd --all-targets
```

At authoring time the pin is Rust 1.97.1. If unavailable locally, archive
committed source with `git archive` and use the documented compiler bridge.
Never send `.git` or credentials. An untracked build doc is not in HEAD's
archive: commit the intended handoff on the isolated effort before archiving.
Recheck the exact candidate after a base move.

The checked-in effort workflow is manual `workflow_dispatch`; pushing a task
does not automatically allocate its compiler jobs. Explicitly obtain the
required Effort development gate evidence and retain focused local results.
For main promotion, obey AGENTS' qualification-receipt requirement as well as
the dated pipeline correction: draft PR, required adversarial review, then
ready/current-candidate Main promotion gate. Do not mistake the main fast
lane's compilation for physical-disc or full qualification evidence. Read the
current workflows when executing; document any unresolved policy mismatch
instead of weakening a gate. A merge does not authorize a fleet deployment.

### 2.2 Record the existing interfaces before changing them

These declarations were inspected in the authoring checkout. Re-verify them
at build time; this is a map of contracts, not a patch to paste verbatim.

| Existing interface | Consequence |
|---|---|
| [`domain.rs`](../../crates/plurx-core/src/domain.rs): `MediaFile { id: i64, item_id: i64, path: PathBuf, size: i64, mtime: i64, ... }` | File identity and media facts are coupled; split/reuse facts without inventing a file row. |
| [`playback/mod.rs`](../../crates/plurx-core/src/playback/mod.rs): `pub fn decide(file: &MediaFile, profile: &DeviceProfile, node: &RenderCaps) -> Decision` | Factor the common facts/decision seam and add source delivery constraints. |
| [`scan/probe.rs`](../../crates/plurx-core/src/scan/probe.rs): `pub async fn probe(path: &Path) -> Result<ProbeResult, ProbeError>` | Optical probe input needs format/title/angle arguments before `-i`. |
| [`transcode/mod.rs`](../../crates/plurx-core/src/transcode/mod.rs): `TranscodeExecution { source_path: PathBuf, ... }` | Audit every input construction, including second inputs and subtitle paths. |
| [`transcode.rs`](../../crates/plurxd/src/transcode.rs): `SessionRequest { file_id: i64, playback_id: String, request_id: Option<String>, ... }` with `deny_unknown_fields` | A new source cannot be appended to old persisted/wire envelopes without version handling. |
| [`http/hls.rs`](../../crates/plurxd/src/http/hls.rs): `StartResponse` contains `session_id`, `playlist_url`, `duration_ms`, `start_seconds`, `media_origin_ms`, `vod`, `ladder` | Reuse the start/control contract, including timeline origin and actual delivery facts. |
| [`http/watch.rs`](../../crates/plurxd/src/http/watch.rs): progress is item-keyed and coalesced | Add disc-specific storage while reusing ordering/durability semantics. |
| [`http/error.rs`](../../crates/plurxd/src/http/error.rs): `ApiError::Typed`, `TypedRetry`, `TypedDetail` | Use existing flat machine-readable errors; do not add an incompatible envelope. |
| [`config.rs`](../../crates/plurx-core/src/config.rs): startup config; runtime settings live in Store | Device paths remain node-local configuration; user grants belong in Store. |

Read the current [playback lifecycle contract](../playback-control/PLAYBACK-LIFECYCLE-IMPLEMENTATION.md),
[surface contract](../clients/PLAYBACK-SURFACE-CONTRACT.md),
[input contract](../clients/PLAYER-INPUT-CONTRACT.md), and
[subtitle assessment](../clients/SUBTITLE-RELIABILITY-ASSESSMENT.md) before
altering client or producer behavior.

### 2.3 Physical reader and VOD feasibility gate

Use the configured executables, including the actual Jellyfin FFmpeg build
selected by [Dockerfile](../../Dockerfile). Probe capabilities and retain
build/version output. A version number alone is not evidence of compiled
DVD/Blu-ray support. Check the demuxer/protocol help, execute a short decode,
and require an observed decoded frame; some FFmpeg help failures exit zero.

```bash
"${PLURX_FFMPEG:-ffmpeg}" -version
"${PLURX_FFMPEG:-ffmpeg}" -hide_banner -h demuxer=dvdvideo
"${PLURX_FFMPEG:-ffmpeg}" -hide_banner -h protocol=bluray
"${PLURX_FFPROBE:-ffprobe}" -version
```

With a specifically identified test drive/disc, exercise title enumeration,
first-frame decode, a forward seek, backward seek, chapter jump, track switch
and cancellation. Record drive model, host, firmware when available, media
format/protection status, exact binary, commands, durations and outcomes.
Use operator-selected hardware for eject/removal tests; do not eject an
unrelated mounted drive merely to populate evidence.

**Critical proof:** selected titles must work through the current VOD producer,
index/segment timeline and seek-demand model without reading the entire disc
first. `SessionRequest` comments explicitly disallow newly admitted legacy
live presentations. Prove copy and transcode paths, DVD cell and Blu-ray clip
boundaries, correct `media_origin_ms`, and nonzero-start playback. A successful
`ffplay` or first frame does not establish this contract.

If the packaged demuxer cannot support the required seek/index behavior,
record a minimal failure and implement a bounded title-aware seek/reopen
adapter. Re-run the same cases. Full-title materialization before playback
is a product/storage change requiring a revised plan; do not quietly ship it
as instant disc playback. Hardware absence leaves physical acceptance open;
continue independent domain, fixture, UI and Store work without inventing a
passing receipt.

## 3. Source contract — identity is not a path

### 3.1 Domain types and input lowering

Introduce the source domain under a new `crates/plurx-core/src/optical/` module
and a shared playback-source seam. The following is the proposed semantic
shape; use repository ID newtypes and derives where appropriate:

```rust
enum PlaybackSourceRef {
    File { file_id: i64 },
    Optical {
        owner_node_id: String,
        drive_id: String,
        media_generation: String,
        disc_id: String,
        title_id: String,
        angle: u32,
    },
}

enum OpticalTitleLocator {
    Dvd { title_number: u32 },
    Bluray { playlist_number: u32 },
}

enum ResolvedInput {
    File { path: PathBuf },
    Dvd { path: PathBuf, title_number: u32, angle: u32 },
    Bluray { path: PathBuf, playlist_number: u32, angle: u32 },
}
```

Public title IDs are opaque and resolve to inspected locators. Bound angles
against the title's reported choices; normalize each backend's numbering
explicitly. Never let a user-supplied title number become an arbitrary path.
Build argv with typed arguments; do not serialize shell command strings.
Input lowering is shared by ffprobe, copy/transcode, index and permitted
subtitle consumers. A builder test must cover input-option placement before
each relevant `-i`, whitespace in paths and multiple inputs.

Extract media facts from file identity so the decision engine can consume the
same codec, track, dimension, duration, HDR and bitrate facts for both kinds.
Keep `decide(&MediaFile, ...)` as a compatibility adapter if that reduces
unrelated churn. Source constraints must participate in delivery planning;
compatible elementary streams still need title demuxing and packaging.

### 3.2 Four identities with different lifetimes

| Identity | Construction/lifetime | Used for |
|---|---|---|
| Drive ID | Logical server/node ID plus configured drive ID | Finding the physical resource; never content identity |
| Media generation | Fresh unguessable token per insertion, uncertain-change event or daemon restart | Fencing opens, ejects, requests and callbacks |
| Disc ID | Versioned canonical fingerprint of navigation structures and content evidence | Metadata/resume recognition across reinsertion |
| Output identity | Generation + disc/title/angle + existing byte-affecting recipe fields | Preventing cached output from crossing source/selection boundaries |

For fingerprint v1, hash format, canonical ordered navigation/control-file
bytes, relevant clip/file lengths and bounded content samples selected during
inspection; persist the algorithm/version and evidence completeness. Include
DVD IFO structure or Blu-ray index/playlist/clip-info structure, not just the
volume label or longest duration. Bound physical reads in the inspector. A
disc which cannot produce the required evidence gets an insertion-only ID
and cannot inherit durable resume automatically. Treat conflicting evidence
as a different disc; never overwrite a known identity from weaker evidence.

This fingerprint is a recognition scheme, not proof of every media byte.
Do not reuse output across insertions in v1. Partition optical cache/index
namespaces by generation and version; include title, angle, track selection
and the existing plan digest. Do not bump the file-recipe namespace gratuitously
or change cache identity for every existing movie.

On removal/restart, revoke every old generation's reader/output capability.
Already downloaded client bytes cannot be recalled, but the server must not
open new reads or admit new sessions from the old generation. Buffered playback
may briefly continue while the single playback surface reports the source
loss; cancel work, preserve progress and suppress automatic retry loops.

### 3.3 Session compatibility and mixed versions

Normalize file and optical start requests into one internal source-aware
session request. Preserve the public file endpoints and their existing
serialization; never use `file_id = 0`, negative IDs, fake paths or a dummy
database file. Audit all uses of file/item IDs in control, recovery,
telemetry, reservations, subtitle URLs, activity, desired selection and
session persistence before calling this migration complete.

Keep legacy file recipe decoding byte-compatible. Use an explicitly versioned
optical recipe with a source discriminator and route it only to nodes
advertising `optical_v1`. All cluster nodes participating in optical routing
must understand that recipe before the feature becomes available. Nodes
without optical support must not scan/adopt/parse optical payloads as legacy
file requests. If existing durable storage cannot segregate/skip those rows
safely, add a versioned payload table keyed by session ID rather than relying
on unknown-field tolerance the old type does not have.

No optical failover to a node without the drive. An ingress relay is allowed;
producer adoption elsewhere is not. Same-node process restart still changes
media generation and invalidates sessions; resume creates a new session only
after reinspection. Show an incompatible mixed cluster as an unmet advisory
requirement beside the enable control. A start routed through an incompatible
owner still fails with a typed capability error because the old node cannot
execute the recipe. Downgrade requires draining optical sessions before
replacing participating nodes.

## 4. DiscManager — one authority for all drive operations

### 4.1 Node-local configuration and discovery

Proposed node-local drive configuration:

```toml
[optical]
inspection_timeout_seconds = 30
inspection_output_limit_bytes = 4194304

[[optical.drives]]
id = "media-room"
name = "Media room drive"
device = "/dev/disk/by-id/replace-with-actual-optical-drive"
mount_root = "/mnt/optical/media-room"
```

The paths above are placeholders, not a detected device. Configured IDs must
be unique per node. Validate a read-only optical device and the canonical
mount root; reject arbitrary files, symlink escapes and writable-media actions.
The manager must recheck device identity when opening, not only at startup.
Use OS insertion/mount events with debouncing and a bounded fallback poll
only for enabled configured drives. Do not repeatedly wake idle drives with
full probes. The 30-second timeout and 4 MiB reply cap are initial engineering
bounds; M0 may adjust them with measured reasons and tests.

Separate physical-drive operations behind an adapter: discover configured
device, observe media, inspect/open under a permit, and eject. Fake adapters
must support deterministic swap, removal, hung-read and restart injection.

### 4.2 Explicit state machine

```text
disabled / unsupported-host
              |
              v
empty -> inspecting -> ready -> reserved -> playing
           |            |          |          |
           v            |          +----------+--> ready (stop/failed start)
     unreadable         |
                        +------------------------> ejecting -> empty

any present state -- removal/change --> revoke generation -> empty/inspecting
any state -- host loss --> unavailable (advertisement expires)
```

`ready` requires a readable title and supported backend; unknown protection
does not become supported by default. `unreadable` carries a typed reason.
Reservation is acquired atomically with expected generation/title validation.
It is owned by user + playback ID + request identity + session lifecycle,
not by an HTTP connection. Tie expiry to the existing bounded session lease
and cleanup semantics; do not add an unrelated keepalive protocol.

Repeated identical create requests return the existing session/result.
Reusing a request ID with a different payload is a conflict. Another viewer
gets Drive busy. A same-player seek or selection replacement transfers the
reservation under the existing ordered control operation; releasing the old
worker must not release a newer reservation. Fence each callback by media
generation and reservation/worker epoch.

Every failure between reserve, spawn, index and session publication releases
the reservation after readers are stopped. Cancellation/timeout of an HTTP
request must either settle an admitted session or run the established cleanup;
it must not leave an invisible drive owner.

### 4.3 All readers obey admission

One viewing session does not mean one unsupervised reader per subsystem.
Probing, indexing, FFmpeg, chapter thumbnails, bitmap/text subtitle extraction,
analysis and prewarming all consume the same physical resource. Start with
one active physical read job. Serialize required preparation, then playback;
disable optional thumbnail/analysis/background indexing while playing.

For track/quality changes, prefer already materialized bounded data. When a
second physical read would be required, stop and acknowledge the incumbent
reader before opening its replacement; report preparation/rebuffering through
the existing controller. Do not present a prepared seamless handoff when the
drive cannot support it. If the current protocol has no correct serialized
replacement path, add a source-concurrency capability and server-owned
transition before enabling optical changes. The client must not invent one.

A blocked kernel read may outlive a helper's termination deadline. Keep the
drive quarantined/busy until the OS confirms closure/device removal; never
grant a second owner because a timer elapsed. Bound retries and surface a
drive reset/operator action when necessary. Test with a fake hung worker.

### 4.4 Inspection helper and format adapters

Use a small versioned helper process for C-library inspection; it is a local
implementation component, not a new remotely administered service. A request
contains an allowlisted local input, operation and expected generation from
the manager. Its JSON response contains schema version, format, protection
facts, fingerprint evidence, titles, locators, streams, chapters, durations
and diagnostics. Limit output, strings and entry counts; retain bounded
stderr, classify errors, and terminate/reap on cancellation. No BD-J or
disc-supplied executable code is run for title inspection.

Prefer libdvdread/libdvdnav for DVD structure and libbluray for Blu-ray.
Map their title/playlist IDs to explicit input options. Do not concatenate
VOBs or pick a single M2TS clip as the feature. Preserve branching/playlists
and alternate angles. Deduplicate identical title structures conservatively;
never delete choices solely because durations match.

The suggested main feature is a heuristic with reasons and confidence. If
several substantial titles compete, show a chooser; do not equate longest
with correct. For episode discs, list titles with duration and tracks and
allow manual episode matching. Store any explicit choice for that fingerprint.

## 5. Store contract — progress without fabricated library files

Add an `OpticalStore` trait integrated into the existing Store family, with
SQLite and Hiqlite implementations and backend-neutral contract tests.
Reuse migration conventions and parameter validation; choose the next actual
migration number at build time. Do not ship an empty default implementation.

The following schema is the proposed logical contract. Split SQL statements
as required by the existing backend migration APIs. Foreign-key names for
users/items must be verified against the current schema before migration.

```sql
CREATE TABLE optical_discs (
    disc_id TEXT PRIMARY KEY,
    fingerprint_version INTEGER NOT NULL,
    fingerprint_evidence_json TEXT NOT NULL,
    format TEXT NOT NULL CHECK (format IN ('dvd', 'bluray')),
    volume_label TEXT,
    display_title TEXT,
    metadata_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE optical_titles (
    disc_id TEXT NOT NULL,
    title_id TEXT NOT NULL,
    locator_json TEXT NOT NULL,
    facts_json TEXT NOT NULL,
    chapters_json TEXT NOT NULL,
    duration_ms INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    matched_item_id INTEGER,
    match_kind TEXT CHECK (match_kind IN ('movie', 'episode', 'extra')),
    PRIMARY KEY (disc_id, title_id),
    FOREIGN KEY (disc_id) REFERENCES optical_discs(disc_id)
);

CREATE TABLE optical_progress (
    user_id INTEGER NOT NULL,
    disc_id TEXT NOT NULL,
    title_id TEXT NOT NULL,
    angle INTEGER NOT NULL CHECK (angle >= 1),
    position_ms INTEGER NOT NULL CHECK (position_ms >= 0),
    duration_ms INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    watched INTEGER NOT NULL DEFAULT 0 CHECK (watched IN (0, 1)),
    audio_selection_json TEXT,
    subtitle_selection_json TEXT,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (user_id, disc_id, title_id, angle),
    FOREIGN KEY (disc_id, title_id) REFERENCES optical_titles(disc_id, title_id)
);
```

Store JSON as bounded versioned documents validated at the boundary, not an
unrestricted dump of native library structures. Upsert a complete inspection
transactionally; a failed partial scan cannot erase known titles or progress.
Device paths, handles, lease tokens and current generation do not belong in
these durable disc tables. Availability is a short-lived owner advertisement,
with owner validation authoritative at open.

Publish availability through the existing suitable peer-state mechanism, or
add a bounded in-memory peer DTO: initially refresh at most every 5 seconds
and expire after 15 seconds without an owner update. Use local monotonic
receipt time for expiry, not comparisons between host wall clocks. Refresh
on meaningful insertion/state changes as well, with debounce. Do not turn
availability heartbeats into replicated database writes every 5 seconds.
After expiry, preserve durable disc metadata but report the drive unavailable.

Required operations: upsert inspection, read disc/title list, write explicit
metadata match, read/update per-user progress and track preferences, and remove
that user's history using existing deletion policy. Reuse the existing
coalescer's monotonic ordering, terminal flush and watched threshold rules;
do not introduce clock-order races between old and new sessions. Authenticate
progress against its session/user and known title timeline. Reject negative,
non-finite/out-of-range input rather than trusting a client duration.

Disc progress is authoritative for resuming this edition. An explicit movie
or episode match may propagate completion/scrobble through existing policy,
but do not overwrite another file edition's resume position or scrobble an
extra as the movie. A deleted matched item becomes an unmatched disc title,
not lost optical history. Add user/item cleanup to both Store backends.

## 6. HTTP and client contracts

### 6.1 Endpoints and authorization

New endpoints live under `/api/v1/optical`. Routes below are the build target;
record any necessary naming adjustment in this document before client work.
Apply existing bearer/session authentication, profile restrictions, body
limits, and node-to-node peer authentication.

| Method and suffix | Response/behavior |
|---|---|
| `GET /drives` | Authorized drive DTOs with state, owner, capabilities and current insertion; no host paths |
| `GET /drives/{drive_id}/disc` | Current generation/disc summary and title summaries, or explicit empty state |
| `GET /discs/{disc_id}/titles/{title_id}` | Durable title detail: tracks, chapters, matching and requesting user's progress |
| `POST /drives/{drive_id}/titles/{title_id}/decision` | Expected insertion plus existing caps/preferences; same delivery semantics as file decisions |
| `POST /drives/{drive_id}/titles/{title_id}/sessions` | Expected insertion plus existing start intent; returns the normal StartResponse |
| `POST /discs/{disc_id}/titles/{title_id}/progress` | Session-bound position/selection update with angle; authoritative durable/queued acknowledgment |
| `PUT /discs/{disc_id}/titles/{title_id}/match` | Admin-only explicit match/unmatch to a permitted movie/episode/extra |
| `POST /drives/{drive_id}/eject` | Admin-only generation-checked eject; optional exact-session stop-and-eject |

Provide a finite optical source DTO to the existing client playback adapter;
source type chooses decision/start/progress URLs, while the normal player
owns rendering, controls, telemetry and teardown. Source changes do not
create a second singleton player or second playback surface.

Proposed node-local device configuration is separate from replicated per-user
grants. Introduce `optical.play` as an explicit user grant, default false for
non-admins and true for admins; admin-only eject/match avoids a larger new
role system. Matched titles must also pass existing library/content
restrictions. Unmatched/unknown-rated discs are unavailable to restricted
profiles unless the existing policy explicitly allows unrated media. Test
authorization at every entry, not just list filtering.

### 6.2 Wire examples and races

Illustrative drive response; opaque IDs are examples, and `capabilities`
describes the actual owner's build rather than a requested feature:

```json
{
  "id": "node-a:media-room",
  "name": "Media room drive",
  "owner_node_id": "node-a",
  "state": "ready",
  "capabilities": {"dvd_titles": true, "bluray_titles": true},
  "disc": {
    "id": "optical-v1:example-digest",
    "media_generation": "opaque-insertion-token",
    "format": "bluray",
    "display_title": "Meridian",
    "metadata_status": "confirmed",
    "suggested_title_id": "title-1"
  }
}
```

Session creation extends the existing public start-body parser through a
source adapter; preserve current `start`, `audio`, `subtitle_burn`, caps and
intent ordering semantics. This example is deliberately a subset, not a
replacement for the existing complete selection envelope:

```json
{
  "expected_disc_id": "optical-v1:example-digest",
  "media_generation": "opaque-insertion-token",
  "angle": 1,
  "playback_id": "stable-player-identity",
  "request_id": "unique-create-attempt",
  "start": 1458.0,
  "audio": 0
}
```

Decision does not acquire a long-lived playback lease. Session creation must
repeat source freshness, title, policy, output capacity and owner checks;
then atomically reserve and revalidate at input open. IDs are opaque bounded
strings; validate length, numbers, title/angle and selection membership.
Selected stream indices refer to the inspected logical title's probe result,
not arbitrary clip-local indexes. A reinspection that changes stream mapping
invalidates the old selection token instead of silently selecting a different
language.

| HTTP | Stable error code | Client action |
|---|---|---|
| 409 | `optical_media_changed` | Refresh disc; never retry the old title automatically |
| 409 | `optical_drive_busy` | Show in-use state; no automatic takeover |
| 409 | `optical_request_conflict` | Request ID was reused with a different payload |
| 409 | `optical_eject_conflict` | Active session changed; refresh before offering stop/eject |
| 415 | `optical_format_unsupported` | Show unsupported media; no codec retry loop |
| 422 | `optical_protection_unsupported` | Show protected-disc capability failure |
| 503 | `optical_owner_unavailable` | Show drive host offline |
| 503 | `optical_reader_unavailable` | Admin diagnostics explain missing/failed reader capability |
| 503 | `optical_read_failed` | Offer bounded manual retry; retain underlying error in diagnostics |

Keep normal 400/401/403/404 meanings. Render typed errors through the existing
surface mapper; do not parse English messages to decide recovery behavior.

Idle eject requires `expected_disc_id` and `media_generation`. Stop-and-eject
also requires the exact active session ID and `stop_active: true`; under the
manager's serialized transition, validate all fields, revoke admission, stop
and confirm reader closure, then eject. A newly started session or inserted
disc cannot be stopped/ejected by an older request. Busy means busy until
closure is proven; do not release a lock before issuing the hardware command.

Use conditional polling initially: refresh visible Home/disc state every
5 seconds, back off on failures, and stop polling when hidden/signed out.
Avoid adding a permanent timer in every render. Reuse the web router's timer
ownership and native lifecycle cancellation. An existing suitable event
channel may replace polling, but it is not required for this release.

## 7. Playback pipeline — preserve the existing controller

### 7.1 Input/output and timeline

```text
OS media observation -> DiscManager -> bounded inspection -> Store metadata
                            |
client decision/start -> authorize + generation check + drive reservation
                            |
                    resolve title/angle input
                            |
                  common media facts + caps
                            |
                  existing copy/transcode VOD
                            |
                   existing session/HLS relay
                            |
                   existing client player
                            |
                disc progress + normal telemetry
```

Prefer video copy when compatible; re-encode audio only when required.
Transcode video for actual decoder, geometry, interlace or subtitle needs.
Do not treat a compatible codec as permission for direct file transport.
Preserve the existing delivered-versus-source HDR/audio fact distinction.

Audit these source users explicitly: regular-file metadata/open checks,
descriptor identity, ffprobe, keyframe/index builders, copy inputs, transcode
inputs, subtitle secondary inputs, PGS overlay/VTT extraction, thumbnail and
analysis jobs, reservations, prewarm, replacement, retry, recovery/adoption,
activity and progress. Record the disposition of each in M3 evidence.

Each title has a logical timeline beginning at zero. Normalize DVD cells,
branching and Blu-ray clips into that timeline. Store chapter times with
accuracy/availability; do not promise exact DVD markers before the necessary
index exists. Playback may begin once the current VOD contract has sufficient
evidence; lazy refinement must never shift an already published timeline.
Unknown duration is not zero duration or permission to mark a title watched.

Prove backward and forward seeks, near-end seeks, out-of-range rejection,
cell/clip joins and resume after reinsertion. Honour `media_origin_ms` and
the existing source-to-player offset. Do not pass a nonseekable pipe into a
consumer that depends on arbitrary input seeks. If exact chapter markers
need slow preindexing, expose preparation honestly and do not start an
uncoordinated full read during playback.

### 7.2 Audio, subtitles and DVD picture geometry

Reuse track selection policy and stable source-stream identities. Validate
commentary/default/forced flags and language fallback; never use display-list
position as the FFmpeg stream identifier. Account for DVD subpicture palette
and forced display, Blu-ray PGS, audio layouts, and changes across clip joins.

PGS/native overlay or burn decisions remain server-owned. The subtitle path
must use the same logical title and timestamp origin as video. If extraction
would compete with a reader, serialize it or use in-pipeline processing and
existing bounded staging; do not secretly read the full disc before play.
Unsupported combinations get a clear disabled option/reason, not a selected
track that produces no subtitles.

DVD tests must include anamorphic aspect ratio, 4:3 and 16:9, PAL/NTSC, genuinely
interlaced content and telecined content. Do not hardcode deinterlacing for
every MPEG-2 stream. Preserve field/frame cadence and audio sync across seeks.
Standard Blu-ray scope does not remove the need to probe stream facts; do not
infer SDR or a codec purely from the physical format badge.

### 7.3 Scratch, failures and resource ownership

Use existing scratch reservations/budgets for read-ahead, indexes and selected
subtitle artifacts. No unlimited disk spool. A 60-second window at 40 Mb/s is
approximately 300 MB before overhead; calculate admission from actual source
facts and existing budgets. Do not bake that illustration in as a new quota.
Exhaustion must return the existing bounded preparation/resource behavior,
clean up partial output and keep the drive reservation lifecycle correct.

On removal, atomically invalidate generation, cancel queued reads, stop the
producer, settle the existing control/surface state and flush latest permitted
progress. Do not reinterpret removal as a decode-margin failure and retry at
lower quality. A genuinely retryable read failure may offer one user-triggered
retry; each attempt is bounded and rechecks insertion identity.

Instrument source type/format, owner, drive, reservation and generation,
inspection latency, time to first frame, seek latency, read failures,
scratch bytes, stale-request rejection and cleanup duration. Keep public
responses free of device paths and keys; sanitize disc labels in logs/HTML.
Reuse Activity and Playback info instead of adding a parallel dashboard.

## 8. UI build contract — implement the three surfaces

### 8.1 Home and navigation

Use the current product display name (`APP_NAME`, currently cinema), existing
themes, typography, cards and layout registry. Add a source badge and a drive
name; do not hardcode the mockup's noirr colors into all themes.

```text
Home

Inserted disc
+-----------+---------------------------------------------------+
| artwork   | Blu-ray · Media room drive                        |
|           | Meridian                                          |
|           | 1h 58m · 1080p · English 5.1                     |
|           | [Resume at 24:18]  [Explore disc]                 |
+-----------+---------------------------------------------------+

Continue watching
...
```

Render this only for authorized users with a present disc. Multiple drives
become a shelf; discovery/reading states use a compact placeholder. No empty
optical shelf when disabled or no discs are present. Expose a Discs route
when optical is configured/authorized, with small empty-drive rows for users
who deliberately visit it; do not add a permanent nav item for everyone.

Build Home model data in the established model seam and support classic,
catalog and theater renderers. Prevent a late disc refresh from rerendering
the active player, stealing search focus, or resetting a TV focus position.

### 8.2 Disc detail and matching

```text
< Back        Disc in Media room drive                  [Eject]

[poster]  Meridian
          Blu-ray · 1h 58m · 1080p
          [Resume] [Play from start]

Titles & extras       Chapters       Audio & subtitles
------------------------------------------------------------
Suggested feature        1h 58m       12 chapters     [Play]
Title 2                    24m                       [Play]
Title 3                    12m                       [Play]
```

Feature suggestions are labelled suggestions until confirmed. Generic title
numbers/durations are legitimate final UI when metadata is unavailable.
Do not invent chapter names. Include format, tracks and duration accuracy
where it affects a choice. Persist audio/subtitle preference per title/user.
Do not conflate the format badge with delivered quality.

An admin can match/unmatch a title to an existing movie or episode; show the
chosen item before saving, and reject mismatched type/access. Use existing
metadata/artwork infrastructure; no new metadata provider is required.
Extras may remain unnamed and must not inherit the movie's watch completion.
Full collection browsing of absent discs is deferred, but a known resume
entry may say Insert disc and must never offer a working Play action.

### 8.3 TV, mobile and player integration

TV uses a large inserted-disc card with Resume/Play and Browse disc. Initial
focus is Resume; Back returns to the invoking shelf and restores focus. Disc
contents are native focusable rows/sheets, not desktop tabs shrunk to TV.
Removal leaves a clear message and an actionable Back; never trap focus on a
disabled Play control. Eject is secondary, admin-only, with explicit active
session consequences.

Phone/tablet stack artwork and details responsively. Keep primary actions
reachable without horizontal scrolling. Use platform touch target and type
conventions. Audio/chapter controls open through the established player menus.
The existing player owns fullscreen, PIP, watch-and-browse, remote keys, stats
and playback surface. Test feature reach through the common source adapter.

| State | Text/action contract |
|---|---|
| Inspecting | Reading disc; no invented percentage; Back remains usable |
| Ready | Play/Resume plus browse |
| Ambiguous | Choose a title with durations/tracks |
| Busy | Drive in use; reveal other viewer/device information only if permitted |
| Protected/unreadable | Specific capability/read failure; no endless spinner |
| Removed | Disc removed; saved progress and reinsertion guidance |
| Owner offline | Drive host unavailable; preserve metadata, disable Play |
| Eject active | Confirm exact active session consequences; stale confirmation conflicts |

Web scripts remain plain scripts in one global scope. For each new script,
change its file, WEB_ASSETS, shell tag and layout-map row together. Keep
`"use strict";` and load order. Add HTTP responses to the appropriate API DTOs
in both native clients; use native model/view patterns, not an embedded mockup.

## 9. Packaging, compatibility and operator controls

Optical is an explicit runtime opt-in in Developer settings. Disabled
installations do not probe configured drives or require the helper/libraries to
run the daemon. The enable action itself is never gated: package, helper,
device, permission, cluster and physical-media requirements are shown beside
the control as advisory readings. An enabled installation with an unmet
requirement reports that specific capability failure when the affected
operation is requested. Keep unsupported-host stubs compiling so this work does
not break Windows/macOS release targets. A missing helper is a capability
failure, not a daemon crash.

For Linux containers, document the exact mapped optical device and read-only
mount arrangement required by the accepted backend, including required device
ioctls. Do not use a blanket privileged container as the default. A read-only
filesystem mount does not automatically authorize drive control. Keep the
normal service non-root where feasible; implement narrowly scoped OS/device
permissions. Avoid persisting or replicating device credentials/key material.

Verify FFmpeg build options and library notices using the repository license
checks. FFmpeg's DVD demuxer requires libdvdnav/libdvdread build support and
provides no decryption; libbluray alone does not cover many AACS/BD+ discs.
Record supported combinations and failures. Do not auto-download protection
keys, change drive region settings, or claim original menus as title playback.
Dependency choice/redistribution must pass existing project policy before
packaging; a subprocess boundary is not by itself a license conclusion.

The Developer enable section shows the explicit optical switch plus configured
drive label/owner, package/helper availability, device permissions, reader
capabilities, cluster compatibility, disc state and bounded diagnostic reason.
Each requirement says what safe operation needs and whether this installation
currently meets it, but no row disables the switch or rewrites the saved choice.
Eject/match controls belong in normal optical administration and on disc detail;
raw paths appear only in authorized diagnostics. Disabling optical drains active
optical sessions through normal session control, stops observation, and opens no
new drive; it does not require a daemon restart. Shutdown must stop and flush
optical sessions before releasing handles. Drive stop/eject operations do not
stop ordinary file/Live TV sessions.

## 10. Milestones — each PR ends with observable evidence

### M0 — source feasibility and executable contracts

**Build:** compiler loop, backend capability inventory, real-drive spike,
authored small fixtures, fake drive adapter and a recorded input/seek decision.
Specify the helper schema and source-concurrency integration after tracing the
current VOD consumers. Include physical compatibility results where possible.

**Acceptance:** one DVD and one Blu-ray produce correct first-frame and
bidirectional/chapter seek results through the intended source path; the VOD
proof in §2.3 is explicit. Hardware gaps remain open. Record the decision and
actual commands/results in §14 before proceeding with dependent M3 work.

### M1 — domain, persistence and backward compatibility

**Build:** source types/common facts, disc/title/progress Store contracts,
SQLite and Hiqlite schema/implementations, source identity/cache namespace,
versioned optical session payload strategy and legacy serialization adapters.
Add explicit user grant and cleanup semantics using existing user storage.

**Acceptance:** backend-neutral persistence round trips and migration tests;
same-label different discs do not collide; old file recipes still decode and
serialize correctly; no fake file IDs; restricted profiles fail closed.

### M2 — Linux lifecycle, inspector and drive admission

**Build:** configured drive adapter, bounded helper, generation changes,
inspection publication, manager state machine, serialized reader admission,
reservation cleanup, polling/events, and authorized eject primitives.

**Acceptance:** deterministic concurrent-create, cancellation, swap-during-open,
swap-during-inspection, restart, stuck-reader and stale-eject tests. Demonstrate
exactly one active read and reservation transfer without a release race.

### M3 — API and existing VOD integration

**Build:** routes/errors, optical source normalization, decision constraints,
typed input for all read consumers, remux/transcode, subtitles, seek, progress,
ordered replacement, scratch admission and single-source terminal reporting.
Record the disposition of every source consumer in §7.1.

**Acceptance:** fixture-based HTTP-to-session playback with copy and transcode,
forward/backward/chapter/resume cases, audio/subtitle changes and a removal
failure. Existing file playback regressions pass. A pipe that only starts but
cannot seek does not pass. No production UI is advertised before this works.

### M4 — cluster routing and capability rollout

**Build:** owner advertisements with expiry, authenticated internal forwarding,
drive-node placement, optical recipe compatibility diagnostics, typed refusal on
an incompatible execution node, and explicit no-adoption semantics for absent
physical sources. The diagnostics advise the enable choice; they do not gate it.

**Acceptance:** start/seek/stop through another ingress; owner dies while
starting/playing; stale advertisements fail; peer auth errors fail; old nodes
cannot parse/adopt optical sessions; unrelated file failover remains valid.

### M5 — web Home, disc detail and operator UI

**Build:** source-aware player adapter, Home shelf, Discs route, detail/title/
chapter/track views, metadata matching, progress and admin diagnostics/eject.
Add the authoritative enable control and advisory requirement rows to the
Developer tab. Update all web registration contracts and each layout renderer.

**Acceptance:** browser interactions for all §8 states, keyboard/tab focus,
320px through desktop layout, all shipped themes/layouts, no duplicate timers,
and real playback through the existing surface. Capture sanitized screenshots.

### M6 — Apple and Android parity

**Build:** shared wire models, source adapter, native Home/detail/TV navigation,
chapter/track selection, progress, source-loss and control error mappings.
Follow existing build-number and generated-project conventions.

**Acceptance:** affected iOS/tvOS/Android compilation and focused client model/
surface tests; physical TV remote focus, track changes and resume; phone/tablet
layout and fullscreen/PIP paths where supported. Compile-only evidence is not
device acceptance. Preserve existing ordinary file playback on each platform.

### M7 — packaging, full acceptance and promotion

**Build:** accepted helper/library packaging, least-required device access,
operator instructions, capability diagnostics, validation catalog/routing
inventory changes, rollback/drain notes and final evidence.

**Acceptance:** installed-package tests with real DVD/Blu-ray hardware, full
matrix in §11, required main-promotion evidence for the frozen current tree,
and no open release-blocking row in §14. Update FEATURES/API/OPERATIONS/PLAYBACK
and platform parity docs as behavior lands. Promotion and deployment are
separate actions under the existing workflow.

## 11. Verification — fixtures, hardware and regressions

### 11.1 Required matrix

| Area | Required cases and pass condition |
|---|---|
| Identity | Swap A→B at the same device/mount; same label, different structure/content; all A requests rejected |
| Restart | Same disc across restart; metadata/resume recognized, old generation/session refused |
| Admission | Two viewers race; one succeeds, one busy; repeated identical request returns the winner's session |
| Ownership | Old release/callback cannot cancel successor; timeout between reserve/spawn/publish leaks nothing |
| Inspection | Malformed structure, oversized reply, missing helper, timeout and blocked read have bounded outcomes |
| DVD | Single/multi-title, episode disc, PAL/NTSC, 4:3/anamorphic, interlace/telecine, cell/chapter/angle transitions |
| Blu-ray | Multi-clip playlist, alternate playlists, episode disc, angle/clip changes; correct logical timeline |
| Tracks | Multiple languages, commentary, default/forced subtitles, DVD subpictures and PGS, track switch after seek |
| Seeking | Cold nonzero start, forward/backward, chapter, near-end, repeated rapid seeks, invalid offsets |
| Protection | Supported readable media succeeds; unsupported protection differs from permission and read failure |
| Resource bounds | Scratch full, budget exhausted, reader hangs; no unbounded spool, duplicate reader or leaked process |
| Cluster | Remote ingress works; owner loss reports source unavailable; no read on an unrelated node |
| Authorization | No grant, restricted profile, unknown rating, unauthorized eject/match, expired playback capability |
| HTTP | Forged generation/title/angle, stale decision, idempotency collision and arbitrary path rejection |
| Removal | During inspection/start/seek/play/subtitle work; one terminal source-loss outcome and saved progress |
| UI | All states on web/mobile/TV; focus retained; no endless spinner, fabricated metadata or silent takeover |
| Regression | File copy/transcode/seek/subtitles/resume, Live TV, existing session replacement and file failover |

Use authored content and a reproducible tiny DVD/Blu-ray structure builder
for routine tests. Keep copyrighted feature content out of the repo. Physical
tests are opt-in and explicitly identify the test device; they must fail or
report skipped without hardware, never synthesize a passing hardware receipt.

### 11.2 Commands and nonzero test execution

Existing checks to use as applicable to each milestone:

```bash
cargo fmt --all --check
cargo check --locked -p plurx-core -p plurxd --all-targets
cargo clippy --locked -p plurx-core -p plurxd --all-targets -- -D warnings
cargo check --locked -p plurx-core --features hiqlite-store --all-targets
node tests/web/asset-layout.test.js
node tests/web/asset-load.test.js
node tests/web/player-dom.test.js
node tests/playback/player-input-contract.test.js
python3 -m unittest discover -s tests/operations -p test_docs_index.py
make validation-lint
make apple-build                         # when Apple sources change
make android                             # when Android sources change
```

Add focused suites named with an `optical_` prefix in Rust and
`tests/web/optical.test.js` for web behavior; these are planned tests, not
existing evidence. Once created, run and verify a nonzero expected test count:

```bash
cargo test --locked -p plurx-core optical_
cargo test --locked -p plurxd --bin plurxd optical_
node tests/web/optical.test.js
```

Run new Store contracts through both the SQLite and actual Hiqlite contract
harness, not merely a build with `hiqlite-store`. Record the exact harness
invocation and pass count. Add affected regressions for existing playback
from the routing inventory rather than relying only on tests of new code.
Use the repository's required-count wrapper where appropriate so a misspelled
filter cannot pass with zero tests.

Add a bounded opt-in optical hardware runner with machine-readable output;
specify device, format/title, case list and output directory explicitly. Its
schema records source SHA, toolchain, engine/helper versions, drive/host,
fixture identity, per-case outcome, latency, cleanup and observed failures.
The runner must not log credentials or export disc content. Measure startup
and seeks for diagnosis; choose release latency budgets from M0 observations,
record them before final qualification, and do not retrospectively loosen
them to make a failed candidate pass.

## 12. Ownership and integration order

New paths below are proposed. Existing paths are links to inspect before
editing. Shared files are touched serially across milestones.

| Owner milestone | Files/area | Boundary |
|---|---|---|
| M0 | Fixtures, spike/evidence tooling; this document's decisions | No production enablement |
| M1 | New core optical/source modules; [`domain.rs`](../../crates/plurx-core/src/domain.rs), [`store/mod.rs`](../../crates/plurx-core/src/store/mod.rs), SQLite/Hiqlite optical slices, user grants | Durable identity and compatibility |
| M2 | New daemon optical module/helper and OS adapter; [`config.rs`](../../crates/plurx-core/src/config.rs), daemon startup/state | Device authority and reader ownership |
| M3 | New `http/optical.rs`; [`http/mod.rs`](../../crates/plurxd/src/http/mod.rs), [`http/stream.rs`](../../crates/plurxd/src/http/stream.rs), [`http/hls.rs`](../../crates/plurxd/src/http/hls.rs), core [`transcode/mod.rs`](../../crates/plurx-core/src/transcode/mod.rs), [`decode.rs`](../../crates/plurx-core/src/transcode/decode.rs), [`recipe.rs`](../../crates/plurx-core/src/transcode/recipe.rs), daemon [`vodencode.rs`](../../crates/plurxd/src/vodencode.rs), [`vodgen.rs`](../../crates/plurxd/src/vodgen.rs), [`transcode.rs`](../../crates/plurxd/src/transcode.rs), [`playback_control.rs`](../../crates/plurxd/src/playback_control.rs) | Single controller and source-aware VOD |
| M4 | [`media_sessions.rs`](../../crates/plurxd/src/media_sessions.rs), [`internal_media_sessions.rs`](../../crates/plurxd/src/http/internal_media_sessions.rs), cluster capability/placement adapters | Owner routing, not physical-source replication |
| M5 | New web disc module; [`pages/home.js`](../../crates/plurxd/src/web/pages/home.js), [`detail/preplay-selection.js`](../../crates/plurxd/src/web/detail/preplay-selection.js), player source/lifecycle adapters, layouts, router, settings, asset registration | Existing player and all web layouts |
| M6 | Apple [`PlurxAPI.swift`](../../clients/apple/Sources/PlurxAPI.swift), [`HomeView.swift`](../../clients/apple/Sources/HomeView.swift), [`DetailView.swift`](../../clients/apple/Sources/DetailView.swift); Android [`PlurxApi.kt`](../../clients/android/app/src/main/java/tv/plurx/app/data/PlurxApi.kt), [`HomeScreen.kt`](../../clients/android/app/src/main/java/tv/plurx/app/ui/HomeScreen.kt), [`DetailScreen.kt`](../../clients/android/app/src/main/java/tv/plurx/app/ui/DetailScreen.kt), new disc views/tests | Native source adapters and focus |
| M7 | Docker/package/device instructions, license notices, validation catalog and reference docs | Reproducible installed behavior and promotion |

Preserve the web asset four-way registration and add every new/moved document
to [the docs index](../README.md) in the same commit. Update
[playback routing inventory](../../tests/playback/routing-decisions.toml) and
its companion playback documentation when a new decision fork is introduced.
Do not broaden this work into a general media scanner or player rewrite.

## 13. Upstream facts and design decisions

These sources were checked while preparing the original concept; re-verify
against the exact packaged versions in M0:

- [FFmpeg DVD-Video demuxer](https://ffmpeg.org/ffmpeg-formats.html#dvdvideo):
  libdvdnav/libdvdread input, explicit title selection, device/image/folder
  support, separate slow preindex, and no built-in decryption.
- [FFmpeg Blu-ray protocol](https://ffmpeg.org/ffmpeg-protocols.html#bluray):
  playlist/angle/chapter selection. Selecting the longest default playlist
  is not a reliable product-level main-feature decision.
- [VideoLAN libbluray](https://images.videolan.org/developers/libbluray.html):
  playlist/navigation/menu library; commercial AACS/BD+ media needs more than
  this library. Its menu capability does not create an HLS menu UI.
- [libbluray disc capability fields](https://videolan.videolan.me/libbluray/structBLURAY__DISC__INFO.html):
  distinguish detected protection, available implementation and handled media.

The typed source, exclusive reader, Linux-first host, conservative output
namespace, conditional polling, grants and milestone split are this plan's
engineering decisions. They are not claims of existing plurx behavior.

## 14. Execution ledger and final acceptance

Update this section in each task PR. Evidence names the exact SHA, command,
case count and result; a link to CI alone does not establish hardware behavior.
Store larger sanitized results under the existing evidence directory and link
them only after they exist. Do not label a milestone built because its code
compiles when its acceptance cases remain unexecuted.

| Milestone | State at handoff | Evidence / blocker |
|---|---|---|
| M0 source/VOD proof | partial | [2026-09-20 evidence](../evidence/OPTICAL-M0-2026-09-20.md): pinned compiler and typed source/helper contracts established; local FFmpeg lacks both optical inputs, lab nodes were unreachable, and no identified drive exists on the reachable host. Physical DVD/Blu-ray VOD proof remains open. |
| M1 domain/Store | built | Commit `b9032504`; [M1 evidence](../evidence/OPTICAL-M1-2026-09-20.md). Common source facts, managed optical decision constraints, versioned session/output identity, both Store backends, per-user progress, matching cleanup and explicit `optical.play` grants are implemented. A live multi-node Hiqlite round trip remains M4 acceptance, not a claim of this focused receipt. |
| M2 device lifecycle | open | No implementation |
| M3 playback/API | open | Depends on M0 source/VOD decision |
| M4 cluster | open | No implementation |
| M5 web | open | Concept only; full UI contract is in §8 |
| M6 native | open | No implementation |
| M7 package/qualification | open | Physical-media and current-tree qualification required |

Before enabling release support, verify: accepted source/seek decision; both
formats on actual hardware; all required lifecycle/authorization/concurrency
cases; web and native playback; packaged backend capabilities; no regressions
in ordinary files; complete operator instructions; current-candidate gates
and qualification receipt. Document any unsupported protected media, hosts
or track combinations explicitly. A narrower completed milestone may merge
within the effort, but it must not be presented as the completed feature.

Copyable assignment for Sol:

> Implement this optical-media effort milestone by milestone, starting with M0.
> Preserve unrelated work and use an isolated checkout with one optical effort
> branch. Establish the pinned compiler loop before Rust edits. Follow the
> contracts and ownership in this document, record exact evidence in §14,
> and keep physical-hardware gaps visible. Reuse the existing playback/VOD
> controller; do not fake file IDs, silently rip discs, or revive a legacy
> live path. Continue independent implementation when hardware is unavailable,
> but do not claim physical-disc acceptance or promote an incomplete feature.
