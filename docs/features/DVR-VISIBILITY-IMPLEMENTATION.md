# DVR visibility — make recording state visible and explain what happened

**Status:** ready for Sol to implement; design review findings resolved in this
contract, product changes not implemented · **Written:** 2026-09-14 ·
**Executes:** the user's Live TV / Activity / Recordings design proposal and
[its adversarial review](DVR-VISIBILITY-REVIEW.md).

Read this document in order, then work through §9 one package at a time.
It extends the working DVR; it does not replace its recorder or scheduler.
The original renderings establish the visual direction. The corrected
wireframes, labels, state rules and capabilities below are authoritative.
Companion to the [documentation index](../README.md),
[development pipeline](../DEVELOPMENT_PIPELINE.md), and
[player input contract](../clients/PLAYER-INPUT-CONTRACT.md).

## 1. Start from the DVR implementation, not the old local checkout

**Verified source:** cached `origin/main` at
`5b0456058804ac2ced4d642c6d8c8d7de8321642`, containing the DVR merge
`aba140968` / Forgejo PR #294 and the schema-predecessor fix in PR #304.
The documentation checkout is older local `main` at `10f2afe60` and contains
unrelated uncommitted work. No production files were changed for this plan.
The user reports live recording now works; this document does not claim a
new hardware qualification.

Sol must fetch the intended main branch, record its SHA, and create an
isolated worktree from that base. Do not reset or switch the user's dirty
checkout. Re-verify every symbol below against the new base. A repair that
has already landed is retained as a regression case, not implemented twice.

```bash
git fetch origin                        # update the integration reference
git rev-parse origin/main               # record the implementation base
git show origin/main:crates/plurx-core/src/dvr.rs  # verify durable contracts
rustup run 1.97.1 rustc --version         # establish the compiler before Rust edits
```

Use the [source-only compile loop](../ci/AGENT-COMPILE-LOOP.md) if the pinned
compiler cannot run on the checkout host. This documentation-only session
does not need a Rust compiler run; Sol's implementation does.

### 1.1 Existing source seams

Paths in this table describe the verified newer commit, not files promised
to exist in the older documentation checkout. Inspect them with
`git show origin/main:<path>`; implementation line numbers may move.

| Source at the verified commit | Existing seam and its role |
|---|---|
| `crates/plurx-core/src/dvr.rs` | `DvrRecording`, `DvrState`, `DvrTransition`, `DvrStatePatch`, `DvrRecordingFilter`; durable airing identity and attempt metadata |
| `crates/plurx-core/src/store/mod.rs` | `DvrStore`; preserve public trait/backend parity |
| `crates/plurx-core/src/store/sqlite/dvr.rs` | SQLite transitions, progress writes and stop intent |
| `crates/plurx-core/src/store/hiqlite_dvr.rs` | Equivalent replicated operations and deterministic transactions |
| `crates/plurxd/src/live_tv/dvr.rs` | `DvrTransport`, `DvrSink`, `recording_activities`, `pump_tuner_fanout`, `dvr_recover`, `stop_sink`, `finish_row_with_facts`, `dvr_progress_loop`, library sweep |
| `crates/plurxd/src/http/dvr.rs` | DVR router, paginated recordings, schedule, rules, restore and stop/delete routes |
| `crates/plurxd/src/http/internal_activity.rs` | `ActivitySnapshot`, authenticated peer transport, single-flight reads, bounds and validation |
| `crates/plurxd/src/http/system.rs` | `local_activity`, `activity`, `activity_detail`; existing cluster aggregation |
| `crates/plurxd/src/web/index.html` | `renderActivityBody`, `paintActivityBody`, `dvrActivityRows`, `loadLiveTvDvr`, `liveTvLoadRecordings`, `liveTvDvrIndex`, `liveTvStageMarkup`, layout chrome and route dispatch |
| `clients/apple/Sources/DvrClient.swift` | `DvrAPI`, recording/status decoders, marks and existing action outcomes |
| `clients/apple/Sources/DvrRecordings.swift` | `DvrController`, `DvrRecordingsPanel`, scheduled/library/rules UI |
| `clients/apple/Sources/LiveTvView.swift`, `HomeView.swift` | Selected-player metadata, guide modes, root destinations and focus lifecycle |
| `clients/android/app/src/main/java/tv/plurx/app/livetv/` | `DvrApi.kt`, `DvrController.kt`, `DvrMarks.kt`, `DvrUi.kt`, `LiveTvScreen.kt`, `LiveTvGuideUi.kt` |

### 1.2 The first repair is an actual response-envelope defect

At the verified commit, `GET /api/v1/dvr/recordings` returns:

```json
{"rows": [], "next": "optional cursor; absent on last page"}
```

`renderActivityBody` instead tests `Array.isArray(recording)` and discards
that object. `liveTvLoadRecordings` makes the same mistake for saved
recordings. This provides a source-level explanation for an empty recording
section; it is not a claim that we inspected the user's running response.

Fix the decoder at those consumers first. Rules still return an array;
schedule still returns `{conflicts, rows}`. Do not change every list reader
to one invented universal envelope. A malformed response is an error, not
an empty list. Preserve the previous successful rows and show a stale
message when a read fails. Follow `next` through an explicit Load more action
for historical lists; never silently declare the first 100 rows complete.
The final active-capture source will be §5, not a first page of history.

## 2. Product decisions — one DVR, several ways to see it

1. **Recording status belongs to the programme.** Show a text-and-icon badge
   in the guide cell, On now row/card, selected-programme details and player
   metadata. A user should not have to locate a channel lower in the guide.
2. **A persistent recording indicator opens capture activity.** Desktop web
   uses its existing app chrome and Activity destination. Native clients use
   a compact recording-activity destination, not a new copy of the full
   server administration Activity page.
3. **Activity gives current captures first-class space.** Show useful
   progress and failures beside existing playback and background work.
   Do not collapse active analysis, scans, downloads or pre-transcoding to
   make the DVR example look cleaner.
4. **Recordings is a durable navigation destination.** Its primary tabs are
   Upcoming, Saved and Needs attention. Preserve Series rules, manual timer
   recording, skipped-airing restoration and reminders through explicit
   secondary entry points. Reuse the existing recordings library/player.
5. **State words require evidence.** A recording request is not capture,
   a running clock is not saved media, and a successful frontend request is
   not proof that the recorder is reachable.

**Non-goals:** no growing-file playback, rewind/chase play, new tuner
allocation policy, new series matcher, artwork host allowlist, recording
engine rewrite, reminder delivery changes, arbitrary per-airing padding or
retention editor, or device push notifications. These require independent
contracts. Preserve all existing operations while rearranging their UI.

## 3. Corrected layout contract

### 3.1 Live TV — keep the watched programme and selected airing distinct

```text
 plurx   Home  Live TV  Recordings  Activity      [● 2 recording]
 ──────────────────────────────────────────────────────────────
 Live TV
 ┌────────────────────────┐  WATCHING · 13.1 WNET   ● Recording
 │ Existing live player   │  Nature: Tidewater
 │ Same player instance   │  Programme 8:00–9:00 PM
 └────────────────────────┘  Capture ends 9:02 PM · includes padding
                            Recording window elapsed ━━━──────
                            1.18 GB written · Data written 2 s ago
                            [Full screen] [Recording details]
 Also recording · Evening Edition · ends 8:47 PM   [Activity →]
 ──────────────────────────────────────────────────────────────
 Existing guide / On now / Favorites, with recording text badges
```

The global indicator counts confirmed writing recordings, not occupied
tuners. Mixed status says `1 recording · 1 reconnecting`, or
`1 recording · 1 unconfirmed`. With only stale data, say `Status unavailable`
and retain the last-confirmed count inside the opened panel. A fresh,
complete zero result says `No active recordings` inside the panel; the
inactive chrome indicator may disappear. No endless pulsing animation.

**Identity:** use the existing exact `(channel_id, airing_start)` key for
guide joins. Use recording ID for details, mutations and stable row keys.
Neither title, channel display number, programme ID alone nor approximate
time overlap is a substitute. Watching and guide selection have separate
identities; selecting another cell must not relabel the watched programme.

When a channel advances to the next programme while the previous recording
continues its tail padding, the previous programme keeps its recording
badge. The new programme does not acquire one. The channel-level strip may
say `Recording Nature: Tidewater · end padding · until 9:02 PM`.
During head padding use `Recording early padding for Nature: Tidewater`.
Manual time-range captures get `Channel recording · 8:00–9:00 PM` in channel
context, without asserting that an entire named guide programme is covered.

Full-screen playback shows the current programme's badge with the existing
control overlay and hides it when those controls hide. New capture failure
may produce one brief non-modal status message when this app is foreground;
deduplicate by event ID. Do not repeatedly obstruct the picture or intercept
remote input. Watching remains independent of recording.

### 3.2 Activity — facts on the left, selected recording on the right

```text
 Activity                         DVR observed 2 seconds ago
 [Current capture problem, if any; otherwise no alert banner]
 ┌ Active recordings / pending captures ┐ ┌ Recording details ─────┐
 │ Nature: Tidewater       ● Recording  │ │ Airing + capture times │
 │ 1.18 GB written · window 40% elapsed │ │ Data last written      │
 │ Evening Edition         Starting    │ │ Attempts / known gaps  │
 │ Waiting for first bytes             │ │ Typed event history    │
 └─────────────────────────────────────┘ │ Technical details ▸    │
 Watching now · existing playback rows  │ [Stop recording…]      │
 Existing active background work        └────────────────────────┘
 Recent DVR outcomes · newest first             [View all →]
 Needs attention · bounded historical summary   [Review →]
```

Current capture problems precede historical failures. Preserve the existing
Activity node-failure notice and its wording for non-DVR data. A DVR failure
must not blank playback, and a playback failure must not blank DVR data.
Keep existing active background sections expanded. At most three historical
attention items appear in the Activity summary; the complete paginated set
lives in Recordings. Select by recording ID and preserve the selection,
open diagnostics and focus across refreshes.

Show bytes as **written**, not verified playable or crash-durable bytes.
The bar is **recording window elapsed**. Its denominator is the padded
capture window, not programme duration. It may reach 100% while the owner
is still finishing. Never put `24 min saved` beside a wall-clock bar.
Playable duration comes only from the finalized library media probe.

The event list uses actual recorded events (§6). Replace the conceptual
`First video saved` with `First bytes written`. Do not fill historical
gaps with invented sample timestamps or a polling failure labelled as a
tuner signal failure. If actor information is absent, omit it.

### 3.3 Recordings — preserve the tasks the existing panel already supports

| Destination | Contents and actions |
|---|---|
| Upcoming | Scheduled, conflict, programme-moved/stale and withdrawn airings; separate active summary; sort by capture start ascending with ID tie-breaker |
| Saved | `done` and `partial` recordings; stopped-early label where applicable; ordinary item navigation only after media linking; newest airing first using the existing cursor |
| Needs attention | Current conflicts, moved requests, capture/write failures, missed and unintentionally partial outcomes; reasons and available actions, never a generic alarm for every user stop |
| Series rules | Existing rule enable/edit/delete and matching/retention information; global reorder only where authorized |
| Manual recording | Existing channel + time range + title creation flow; no dependence on guide horizon |
| Skipped | Cancelled upcoming airings with explicit Restore; preserve cancellation suppression |
| Reminders | Existing reminder controls remain available from guide/details; do not fold reminder identity into recording status |

`done` does not automatically mean `Complete`: inspect
`stopped_by_user_id`, `late_start_s` and `gap_s`. An intentional early stop
says `Stopped early`; an unintended gap remains visible even if the user
subsequently stops. Before `item_id` and `file_id` are linked, say
`Recorded · preparing playback`. Active recordings never offer Play.
Absence of a known media duration renders `Duration unavailable`, not zero.

For the first release, omit the mockup's speculative `Find another airing`
button. The existing guide remains reachable. Candidate search can follow
later with a real identity/matching contract. Omit arbitrary `Edit recording`
menus for APIs that do not exist. Show rule settings through the existing
rule editor; show other capture settings read-only.

Desktop web adds canonical `#/recordings` and preserves the old Live TV
recordings tabs as aliases/deep links to the appropriate destination. Keep
ordinary `#/item/<id>` media navigation. The Activity destination remains
`#/activity`, with a recording-ID selection parameter parsed independently
of route name. Route changes must preserve the existing dock/player host
contract and must not start a second live session.

### 3.4 Native, compact web and remote navigation

| Surface | Required treatment |
|---|---|
| Desktop web, all shipped layouts/themes | Existing chrome and tokens; no replacement app shell. Two-column Activity when space allows, stable master/detail selection. Include Classic, Catalog, Theater, Deck and Guide where registered on the implementation base. |
| Phone web / iOS / Android | Selected recording opens a detail page or sheet immediately; do not append it below a long list. Back/Close restores the originating row and scroll position. Keep existing future-guide navigation instead of merely hiding future columns. |
| Tablet | Use a split view only while both panes fit at the current text size; otherwise use the phone detail navigation. |
| Apple TV / Google TV: Guide + preview | Badge beside preview title and on each exact airing; summary above the guide; remote path to recording details. |
| TV: Guide over picture | Status in existing metadata/control region and guide cells; do not add an always-on opaque panel. |
| TV: Channel browser | Badge on On now card and selected programme metadata; recording activity opens a focused sheet/page. |
| Native root navigation | Add Recordings using the current root navigation conventions. The compact capture-activity destination is reachable from the Live TV summary and Recordings. No new server-admin Activity tab is required. |

Focus order: recording summary → existing player controls → guide toolbar →
guide rows. Opening details records the originating recording/airing key.
Back returns there; if it disappeared, choose the nearest surviving row,
then the section heading/control. Preserve the existing one-owner remote
input routing. Never retune as a side effect of opening DVR details.

Use real tabs and associated panels, labelled progress elements and existing
modal confirmation components. Recording states use text plus icon, not
colour alone. Screen readers announce state transitions, not changing bytes
and clocks every poll. Support 320 CSS pixels, landscape phones, 200% text,
long programme names, reduced motion and visible keyboard/remote focus.

## 4. State and measurement contracts

### 4.1 Preserve durable state; add observation alongside it

Keep the existing SQL/serde state vocabulary unchanged:

```text
 scheduled · conflict · withdrawn · stale · recording
 done · partial · failed · missed · cancelled · deleted
```

`stale` here means a changed guide programme; it does not mean a failed
network refresh. Add a separate runtime observation with phase, health,
attempt identity and freshness. Do not add `starting`, `reconnecting` or
`finishing` to `DvrState` merely to paint a badge; older native decoders use
closed enums.

The shared presentation reducer consumes durable row, runtime sample,
server time/freshness, media-link state and permissions. Implement equivalent
pure reducers on web, Apple and Android against the same fixture corpus.

| Evidence, in precedence order | Primary label | Rule |
|---|---|---|
| Durable terminal state | Recorded / Incomplete / Failed / Missed / Skipped / Deleted / Stopped early | Terminal row wins over any older active sample; derive media readiness separately. |
| Durable pending `conflict`, `stale`, `withdrawn` | No tuner / Programme moved / Withdrawn | Show the actual reason; no capture implication. |
| Accepted durable stop request on `recording`, without confirmed finishing | Stop requested | Keep visible; acceptance is not a stopped recorder. Add unavailable-observation qualifier if needed. |
| Fresh owner phase `finishing` | Finishing | Requires a live owner observation of closure/assembly; reaching end time is insufficient. |
| Missing, expired, unsupported or fenced observation for durable `recording` | Status unavailable | Retain last-confirmed facts with their age. Do not count as confirmed recording. |
| Fresh owner phase `reconnecting` | Reconnecting | The recorder has entered retry/recovery; no inference from browser connectivity. |
| Fresh owner phase `starting` | Starting · waiting for first bytes | The owner has accepted/created the attempt but no successful sink write exists yet. |
| Fresh owner phase `writing`, successful write age ≤10 s | Recording · data being written | Confirms recent application writes only, not decoded content quality. |
| Fresh phase `writing`, last write age >10 s | Recording · no recent data | Mark a capture-health warning. Do not claim retry until owner reports retry. |
| Durable `scheduled`, start time in the future | Scheduled | Scheduled is a plan, not capacity guaranteed forever. |
| Durable `scheduled`, start time reached but no start observation | Waiting to start | Never show Recording from the clock alone. |

Unknown future observation phase/health values map to `Status unavailable`
without failing an entire native response. Disabled DVR is separate from
observation failure; saved recordings remain accessible when disabled.

### 4.2 Runtime facts and their producers

New fields are proposed interfaces, not claims about the existing API.

```rust
// Names are normative; types may reuse established timestamp/ID wrappers.
struct DvrCaptureObservation {
    recording_id: String,
    channel_id: String,
    airing_start: i64,             // Unix seconds; exact airing key
    owner_node_id: String,
    config_generation: i64,
    serving_generation: u64,
    attempt: i64,
    phase: String,                 // starting | writing | reconnecting | finishing
    observation_age_ms: u64,
    last_write_age_ms: Option<u64>,
    first_write_at_ms: Option<i64>, // Unix ms, current attempt; absent before write
    attempt_bytes_written: u64,
    prior_attempt_bytes: Option<u64>,
    write_bps: Option<u64>,        // successful sink-write bytes per second
    reason_code: Option<String>,   // bounded vocabulary, never raw stderr
}
```

Record successful-write time and byte count only after `write_all` succeeds.
This is application-write evidence; do not claim it establishes playable
video or fsync durability. A disk-write error belongs to that sink, not
every sink sharing the channel. Keep transport and sink counters separate:
the current transport `delivered` counter increments for each sink write,
so it is not a valid measure of source/network bitrate.

Use monotonic clocks for age and bitrate on the owner. Snapshot capture is
cheap in-memory work, outside filesystem I/O and long-held registry locks.
Maintain a short 5-second successful-write rate window; return null before
two samples in the same attempt exist. On attempt change reset the rate
window. Label the rate `Write rate`, not `Incoming bitrate`.

At recovery, inspect prior attempt sizes once through the existing file
recovery path and retain the baseline in the current sink observation.
Never add the durable `bytes` field to the current attempt counter: it can
describe that same attempt. `total_bytes_written` is reported only when
the prior-attempt baseline is known. Otherwise expose current-attempt bytes
and label them `This attempt`. Do not stat files on each public poll.
Normal retries preserve the total; if a part is actually missing, disclose
the loss rather than hiding it behind a client-side maximum.

The capture-window bar is clamped
`(server_now - capture_start) / (capture_end - capture_start)` for a positive
span. A zero/invalid span produces no bar. That computation is always
labelled elapsed window, never percent captured. Airing and capture times
are distinct; display local timezone and dates where a window crosses
midnight or a DST boundary. Padding belongs in the capture end label.

### 4.3 Freshness is about the recorder, not the HTTP response

An authoritative runtime observation expires after **20 seconds**. The owner
reports observation age; the receiving server adds measured cache residence
and transport time; the client adds elapsed monotonic time since receipt.
Do not subtract wall clocks on two different machines. Reject impossible
timestamps/negative ages or generation mismatches as unavailable evidence.

Freshness and health are independent: a fresh sample may truthfully report
no writes for 15 seconds. The 30-second durable `last_progress_ms` write is
useful crash-recovery metadata, not a two-second heartbeat. Do not increase
replicated writes to once per poll or per chunk to animate the interface.
Keep the existing capture timeout/retry policy; this project observes it.

## 5. One bounded read model reaches every frontend

### 5.1 API additions and backward compatibility

Add `GET /api/v1/dvr/overview` for shared foreground status. Add an optional
`dvr` member to `/api/v1/activity/detail` using the **same collector and
projection**. Preserve every existing Activity member and the old compact
Activity response shape. Do not parse human-readable Activity strings.

```json
{
  "version": 1,
  "server_now_ms": 1789403040000,
  "availability": "complete",
  "runtime_supported": true,
  "observation_age_ms": 1200,
  "counts": {
    "recording": 0, "starting": 0, "reconnecting": 0,
    "finishing": 0, "unconfirmed": 0, "attention": 0
  },
  "active_total": 0,
  "active_truncated": false,
  "active": [],
  "next_capture_start": null,
  "diagnostics": null
}
```

This is a complete idle response. `availability` is `complete`, `partial`, or
`unavailable`. Counts are null, not zero, when the durable read itself fails.
`runtime_supported=false` means an older owner, not an idle recorder.

Each active projection includes recording ID, exact airing identity, title,
channel labels, durable state/reason code, airing/capture times, stop intent,
last-confirmed bytes, current observation if available, derived display
state, and capability booleans. Avoid shipping the entire database row with
paths into the new overview. Byte/count fields have explicit null semantics.
The selected detail reads its durable row through the existing ID endpoint
plus §6 history; normalize these responses in one client controller.

**Bounds:** overview returns at most 64 active projections, with exact
independently computed counts, `active_total` and `active_truncated`.
This is a response bound, not an engine limit: overlapping/manual sinks can
exceed physical tuner count. On truncation show `Showing 64 of N` and open
the paginated active list; never claim the whole set is visible. Regular
recording/event pages default to 50 and cap at 100. Do not fetch all saved
recordings to calculate a badge.

### 5.2 Owner collection and partial cluster results

Extend internal `ActivitySnapshot` with optional DVR observation data using
serde defaults. An omitted field means unsupported. An explicitly present
empty collection from the current authoritative owner means no live sinks.
Reserve up to 64 KiB for DVR inside the existing 256 KiB peer response budget
without removing the Live TV/analysis reservations. Enforce both the
64-row ceiling and actual serialized byte size, including JSON escapes;
stop before either limit and mark truncation. Carry exact observed
sink count and truncation; validate every field, string length and nested
array. Reuse existing signed peer authentication, two-second deadline and
single-flight request gates. A public household token never authorizes an
internal snapshot request.

Read the configured owner through that existing peer client and cache; an
overview does not start a new full-cluster scan or contact the tuner.
Join runtime observations to replicated durable recordings by ID, then
check current owner/config generation and serving generation. Serving
generations are local to a node; do not numerically compare them across
different owners. The receiver establishes current owner/config, and the
owner must attest that its local serving authority is current.

During handoff choose the current authoritative observation, not the newest
wall-clock timestamp or largest byte count. Never double-count old/new
owner attempts. Keep a durable `recording` row visible as unconfirmed if the
owner cannot be reached, has old software, or omits the row in a truncated
snapshot. A complete owner snapshot with no worker does not prove that the
durable recording is done; label it unconfirmed until the owner reconciles.
Use the terminal durable state when it arrives.

The existing `/dvr/status.slots.recording` counts recording rows, despite
the tuner-oriented name. Do not treat it as occupied tuners. New admin
diagnostics must report distinct `recording_sinks` and
`recording_transports`, both sampled from the authoritative owner. Do not
promise that stopping one sink frees a shared transport. Show storage only
when sampled on the configured owner with known freshness; otherwise
`Storage unavailable`, never frontend-local free space as the DVR capacity.

### 5.3 Shared polling, guide refresh and stable rendering

One cache/controller per authenticated server/profile owns DVR observations.
Views subscribe; badges, cards and detail panels do not create their own
timers. A profile/server/token change clears the cache and increments a
request generation; late responses from the old scope are discarded.

| Condition | Refresh policy |
|---|---|
| Foreground Live TV, Recordings or Activity, or known active/pending capture | Overview every 5 s; single-flight, 4 s overall client timeout |
| Other foreground page with no known active capture | Overview every 30 s; refresh at next known capture start, then use 5 s while awaiting observation |
| Activity page | Its normal detail poll carries DVR; suppress a duplicate overview request |
| Hidden/background app | Stop periodic UI reads; reminders retain their existing separate behavior |
| Resume, reconnect, successful mutation | Immediate refresh; cancel obsolete request or queue one coalesced successor |
| Repeated read failure | Back off 5 → 10 → 20 → 30 s; stale expiry still occurs at 20 s; explicit Refresh remains available |
| Selected detail history | First page on open; incremental events every 5 s while active, otherwise after mutation; older pages only on user request |
| Visible guide marks / schedule | Refresh at most every 30 s and after mutations; no guide-provider refetch for DVR status |

The existing native comments that marks never change except on a user action
are wrong for recording start/finish and cross-device edits; replace that
assumption. Rebuild the mark index once per changed response, not per cell.
Overlay the current overview onto marks; when a previously active ID leaves
a complete overview, invalidate/reload the visible mark page rather than
leaving a red badge indefinitely. While that read is pending, show unknown
status instead of asserting continued recording or silently removing intent.

Extend the schedule read additively with optional `from`, `to`, repeated
`channel_id`, `after` and `limit`. New clients request the visible guide
window: maximum 24 hours and 64 channels per request, 100 rows per page,
ordered `(capture_start, id)` ascending, with `next` and total conflict count
for that scope. Preserve legacy unfiltered behavior for older clients.
Include terminal/cancelled marks in this filtered projection where needed
to clear obsolete status; their exact airing keys remain unchanged. Load
additional visible pages on demand and explicitly mark unqueried regions;
an absent row in a partial page is not a negative recording answer.
The full Upcoming destination pages its own date range independently.

Backend collection is shared/coalesced for concurrent requests; cache only
the unprivileged raw collector within its bounded lifetime and apply
per-request permissions/attention acknowledgment afterward. Do not share
personalized responses across users. No filesystem scan, per-row user-name
query, per-row peer call or guide request is allowed in the poll path.

## 6. Durable event history and attention

### 6.1 Add a bounded ledger; webhooks are not the history database

Use an append-only migration after the implementation base's current schema
version. Do not assume the old checkout's version or reuse the predecessor
number fixed in PR #304. Both Store backends implement the same operations.

The proposed schema is deliberately separate from the recorder state enum:

```sql
CREATE TABLE dvr_event_heads (
  recording_id TEXT PRIMARY KEY REFERENCES dvr_recordings(id) ON DELETE CASCADE,
  next_sequence INTEGER NOT NULL CHECK (next_sequence >= 1),
  latest_attention_sequence INTEGER NOT NULL DEFAULT 0,
  latest_attention_at_ms INTEGER,
  history_started_at_ms INTEGER NOT NULL,
  history_has_gap INTEGER NOT NULL DEFAULT 0 CHECK (history_has_gap IN (0,1)),
  pruned_through_sequence INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE dvr_events (
  recording_id TEXT NOT NULL REFERENCES dvr_recordings(id) ON DELETE CASCADE,
  sequence INTEGER NOT NULL CHECK (sequence >= 1),
  event_id TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL,
  occurred_at_ms INTEGER NOT NULL,
  attempt INTEGER,
  actor_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
  reason_code TEXT,
  facts_json TEXT NOT NULL CHECK (length(facts_json) <= 4096),
  PRIMARY KEY (recording_id, sequence)
) STRICT;
CREATE INDEX dvr_events_recent ON dvr_events(occurred_at_ms DESC, event_id DESC);

CREATE TABLE dvr_attention_acks (
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  recording_id TEXT NOT NULL REFERENCES dvr_recordings(id) ON DELETE CASCADE,
  through_sequence INTEGER NOT NULL CHECK (through_sequence >= 0),
  acknowledged_at_ms INTEGER NOT NULL,
  PRIMARY KEY (user_id, recording_id)
) STRICT;
```

Allocate `sequence` transactionally from the per-recording head and increment
only on a new event. Supply UUID/time from the application, once per logical
event, and reuse its ID on retries. No SQL randomness, `unixepoch()` or
AUTOINCREMENT on voters. Maintain heads when pruning so sequence values
are never reused. Order one recording's history by sequence, not wall-clock
time, because owners may have skewed clocks.

Update attention sequence/time only for a new actionable occurrence, in the
same transaction. Media linking must not resurface a reviewed failure.
Preserve those summary fields after event pruning so attention counts do
not depend on retaining every history row. Maintain the history start/gap/
prune fields as durable provenance; pruning the first event must not make
an old recording look fully instrumented.

Add Store operations for atomic intent/transition-plus-event, idempotent
observation event append, paginated reads, acknowledgment and bounded prune.
Extend the existing `insert_dvr_airing_if_absent`, `request_dvr_stop`,
`transition_dvr_recording` and media-link transaction seams; do not write an
event after an unrelated successful commit and claim atomic history. A lost
conditional transition writes no event. Protect observation appends with
recording ID, attempt and current owner fence, so an abandoned worker cannot
publish a current event. Stop and progress keep their disjoint column writes.

| Kind | Emission point; meaning |
|---|---|
| `scheduled` | Successful new airing intent; requester/rule attribution if known |
| `schedule_changed`, `conflict`, `withdrawn`, `cancelled`, `restored` | Accepted reconciliation/user transition; no duplicate per scheduler tick |
| `attempt_started` | Attempt file/worker successfully established; not proof of video |
| `first_bytes_written` | First successful nonempty sink write in this attempt, once |
| `capture_interrupted` | Owner-observed read/write failure, bounded reason code |
| `retry_started` | Owner recovery actually starts another attempt |
| `stop_requested` | First accepted stop intent; repeats return the same intent |
| `finishing` | Owner begins closing/assembling a capture |
| `finished`, `failed`, `missed` | Terminal state persisted with reason, bytes and known gaps |
| `media_linked` | Library item/file IDs attached, ordinary playback becomes available |
| `deleted` | Recording delete intent accepted; do not claim filesystem purge already completed |

Progress samples are not persisted as events. First-write/write-error
observations are latched in the sink and drained outside the chunk I/O path;
the terminal path drains pending facts before its terminal transaction.
Never make tuner reads wait on a webhook or event-history HTTP request.
If a process dies before a latched observation persists, expose a history
gap after recovery rather than synthesizing the missing event. Persist the
instrumentation start/version and any known gaps in bounded ledger facts;
reads return `history_complete=false` where appropriate. Older recordings
say `Detailed history was not collected for this recording` and may show
only explicitly labelled durable-row facts.

### 6.2 Read, retention and acknowledgment contracts

Add `GET /api/v1/dvr/recordings/{id}/events?before=&limit=`. It returns
`{rows, next, history_complete, truncated_before_sequence}`. Newest sequence
first, page size 50/cap 100; `before` is an exclusive sequence cursor.
An optional mutually exclusive `after` cursor returns ascending new events
for incremental refresh with an explicit next cursor. Never silently drop a
burst larger than one page. Query boundaries and malformed cursors have
typed 400 errors on this new API.

Retain events for **30 days**, with a **100,000-row server-wide ceiling**.
The owner prunes at most 1,000 rows per maintenance pass, retaining recent
events first and exposing truncation metadata. Retention does not delete
recordings or media. Do not delete event-head/ack state while recording
tombstones still need sequence continuity. Bound and safely render event
text; unknown kinds receive a neutral readable fallback.

Add `POST /api/v1/dvr/recordings/{id}/attention/ack` with
`{"through_sequence": 17}`. Authenticate the user, validate that sequence
belongs to the recording, and monotonically advance only that user's ack.
Opening a row alone does not acknowledge it; provide `Mark reviewed` for
historical outcomes. A newer event can surface again. Active conflicts or
ongoing capture problems stay in Needs attention until resolved, even if
their explanation was read. Historical acknowledged items remain in Saved
or history; acknowledgment never stops or deletes anything.

Add `GET /api/v1/dvr/attention?after=&limit=` returning
`{rows, next, total}` with the same per-user projection as overview attention
counts. Unresolved current conditions come first; historical outcomes use
`(latest_attention_at_ms, recording_id)` descending. Page size defaults to
50/caps at 100. An opaque cursor carries section, exclusive key and the
first-page upper watermark. New occurrences refresh the first page instead
of being injected into pages already traversed. Clients deduplicate by
recording ID and refresh counts after acknowledgment. For pruned events,
validate acknowledgment against the retained recording head and attention
sequence rather than requiring a deleted event body. Refuse a sequence
beyond that recording head.

Partial/failed/missed rows predating the ledger still appear. Their detail
can offer Mark reviewed using an explicit `legacy_outcome` baseline event
created idempotently by the acknowledgment transaction; label its timestamp
as review-baseline creation, not original failure time. Do not fabricate a
timeline of earlier events. Current partial outcomes with unintentional
gaps remain actionable; intentional stops alone do not raise an alarm.

## 7. Actions, permissions and failure behavior

### 7.1 Reuse the existing mutation endpoints

| User action | Existing API / required behavior |
|---|---|
| Record this programme | `POST /dvr/recordings` with `channel_id` and exact `airing_start`; use the returned row, including an already-existing cancelled/terminal row |
| Manual channel recording | Same POST with existing `capture_start`, `capture_end`, `title` fields |
| Stop active recording | `DELETE /dvr/recordings/{id}` without `delete_file`; 202 means Stop requested, not complete |
| Skip pending airing | Same DELETE; preserve suppression and expose Restore |
| Restore skipped airing | `POST /dvr/recordings/{id}/restore`; display server refusal if airing is past |
| Delete saved media | Separate named confirmation; DELETE with `delete_file=1` only for that explicitly confirmed operation |
| Rule editing / reorder | Existing rule endpoints and their existing authorization |
| Play recorded media | Ordinary item navigation, only after linked item/file readiness |

Bind confirmations to immutable recording ID and programme/channel/time,
not the current guide selection. Stop confirmation says `Stop recording
Nature: Tidewater? Any captured portion will be kept. Watching continues.`
Do not promise a saved file if no bytes exist. Disable duplicate submit
while in flight. A timeout preserves uncertainty; refetch the same row
before offering a retry. A 202 acknowledgment stays pending across views
until durable/owner evidence changes.

If recording finishes while Stop confirmation is open, the old DELETE may
return `delete_file_required`; explain that recording has finished and
refresh. **Never retry Stop with `delete_file=1`.** If a pending skip races
capture start, refresh the actual row and show its current action rather
than blindly reporting cancellation. No optimistic removal of a running
row. Owner failure keeps a pending request visible; it does not authorize
the client to kill a process or invent a terminal outcome.

### 7.2 Preserve actual household policy and constrain new diagnostics

At the verified base, authenticated users can read shared recordings,
create one-offs, stop/skip/delete and restore them. Rule creation is
authenticated; editing/deleting a rule requires its owner or an admin;
global rule ordering requires an admin. Reminders are user-scoped. Do not
silently substitute a new owner-only policy for household recording actions.
Re-verify these guards on the implementation base.

New projections return `can_stop`, `can_skip`, `can_restore`, `can_delete`,
`can_edit_rule`, `can_reorder_rules`, and `can_view_diagnostics`, calculated
server-side. These are UI guidance, not authorization tokens; every mutation
still enforces its route policy and current state. Non-admin capture views
must remain accessible without routing users into unavailable admin pages.

Normal views omit filesystem roots/paths, raw user IDs, host addresses,
internal peer URLs and raw error text. Show a safe programme-level reason.
Admin diagnostics can add recorder name, attempt and transport counts, write
rate and sampled storage. Do not imply a physical tuner number unless the
engine actually provides one. Prefer an opaque transport ID when that is
the only identity available. Apply privacy filtering on the server, not
by hiding fields in CSS. Preserve `Cache-Control: private, no-store`.

## 8. Regression matrix — evidence required for the resulting behavior

Use deterministic clocks and fake sink/peer/store observations. Do not make
tests sleep for freshness timeouts or require the real tuner for unit proof.

| Case | Required assertion |
|---|---|
| API page envelope / second page / malformed body | Activity and Saved decode `rows`, preserve `next`, expose malformed/error state instead of empty |
| Same title, different channels; rerun | Badge joins exact airing; actions target correct recording ID |
| Head/tail padding and channel rollover | Previous airing remains marked; next programme is not falsely recording |
| Manual timer | Channel/timer context appears without claiming exact guide coverage |
| Start request and delayed owner start | Scheduled → Waiting to start → Starting → Recording follows evidence |
| Clock progresses but bytes do not | Window bar may advance; never render saved duration or healthy-write claim |
| Two attempts | Rate resets; known totals combine distinct attempts once; unknown baseline remains unknown |
| Sink write error with shared channel | Only failed sink is unhealthy; surviving sink continues; tuner count stays transport-based |
| Non-owner frontend | Displays configured owner's capture, not empty local registry |
| Missing/old/truncated owner response | Unconfirmed rows survive; null/partial counts are not presented as zero |
| Handoff and late response | Old owner/generation sample cannot win or double-count; terminal durable state wins |
| Stop vs completion / duplicate Stop / timeout | Never deletes media; pending intent survives; explicit reconciliation |
| Finalization / library scan delayed | Finishing and Preparing playback are distinct; no active-file Play |
| User-stopped `done` | Stopped early, not misleading green Complete; unintended gap still shown |
| Event retries and failed conditional transition | One event per accepted logical operation; no event for rejected transition |
| Crash before event drain / older recording | History gap/unavailable message, no fabricated first-write event |
| Pruning / pagination / sequence after pruning | Bounded reads; no sequence reuse; incremental pages lose no events |
| Attention acknowledgment | Per-user, monotonic; new event resurfaces; no media/state mutation |
| Permissions / hostile title or reason | Capabilities and route enforcement agree; no path leakage/XSS |
| Existing background work | Analysis/scans/downloads remain visible while DVR is active |
| Navigation and timers | One observation loop; hidden cancellation; stale old-profile response discarded |
| Mobile / TV / keyboard | Immediate detail navigation, focus return, accessible confirmation; no retune |
| Existing DVR tasks | Rules, manual recording, skipped restore and reminders remain reachable |

## 9. Implementation packages for Sol

Use `effort/dvr-visibility` as the temporary integration branch, with focused
task branches based on its current head. One owner edits the monolithic web
file at a time. Preserve the user's uncommitted work. This handoff authorizes
implementation planning; it does not itself launch Sol or deploy changes.

### S01 — repair the blank recording lists

**Change:** correct web recordings-envelope decoding, explicit pagination,
and separate empty/error/stale states in Activity and Saved. Keep the rules
array decoder unchanged. Add a focused fixture using the actual endpoint
shape, including `next`. Audit native page decoders for the same assumption.

**Acceptance:** a recording page with one row displays it; a second page is
reachable; failed refresh keeps stale rows. Run the existing Live TV web
suite plus new envelope cases. No Rust change is necessary if the base still
has the observed client defect. Do not claim this gives two-second telemetry.

### S02 — publish trustworthy capture observations

**Change:** add sink write timestamps/rate sampling, phase observation and
attempt byte baselines (§4). Extend the bounded internal snapshot and shared
collector, then add overview and Activity projection (§5). Preserve old DTO
members and decode absent/new fields safely. Add request/snapshot bounds,
null semantics and server-side diagnostic filtering.

**Acceptance:** deterministic cases prove no writes ≠ healthy recording;
non-owner reads show the correct capture; owner loss/handoff/truncation
retain uncertainty; shared sinks do not inflate tuner count. Check schema
and both native decoder compatibility before pushing. No per-chunk database
write and no new HTTP request to the tuner are introduced.

### S03 — record lifecycle history and attention

**Change:** add §6 migration and both backend implementations; connect
intent/owner/media-link transitions atomically to events. Add first-write
latching, explicit history-gap treatment, history endpoint, bounded
maintenance and per-user acknowledgment. Extend validation catalogs for new
operations/tasks/timers using existing repository procedures.

**Acceptance:** event retry, conditional transition, owner fencing,
replication determinism, stop/progress race, pagination/prune and per-user
ack tests pass on the relevant backend suites. Confirm no history operation
blocks the capture pump. Legacy history displays its limitation explicitly.

### S04 — integrate the web layouts and shared controller

**Change:** implement §3 with existing theme/layout tokens. Shared DVR state
feeds chrome, player title, guide cells, Activity and Recordings. Add the
permanent destination and compatibility aliases. Bind real confirmations
and allowed actions; retain background Activity sections and every old DVR
task. Use staged DOM updates or equivalent keyed rendering to preserve
focus; do not copy the prototype's whole-panel `innerHTML` refresh pattern.

**Acceptance:** the web cases in §8 pass, including idle → active without
navigation and cross-device mutation. Capture desktop/phone, light/dark,
all registered layouts, stale/error/empty states and the stop dialog.
Verify request counts and that detail navigation does not restart playback.

### S05 — integrate Apple layouts and navigation

**Change:** extend `DvrClient.swift` decoders and `DvrController` with one
foreground observation lifecycle. Add the same reducer contract, immediate
recording detail navigation, Recordings root destination, current-status
summary and exact-airing marks in all three television layouts. Preserve
native reminder scheduling, player ownership and focus routing.

**Acceptance:** iOS and tvOS compile; shared state fixtures and focused
Live TV/DVR tests pass; simulator captures cover phone/tablet/TV. A physical
Siri Remote walkthrough must prove detail/back focus and that changing
layouts or opening DVR details does not retune. Record device/build evidence;
do not label a simulator run physical acceptance.

### S06 — integrate Android layouts and navigation

**Change:** extend `DvrApi`, `DvrController`, `DvrMarks`, `DvrUi`, Live TV
metadata and root navigation with the same read model and action semantics.
Scope refresh to lifecycle-aware coroutine ownership; preserve stable
StateFlow and remote-focus keys. Preserve reminders and the one movable
player surface across the three TV modes.

**Acceptance:** Android compile plus focused DVR mark/UI/reducer tests pass.
Exercise a real or instrumented D-pad path through recording details and
Back; distinguish emulator proof from physical Google TV evidence.

### S07 — verify the integrated tree and ship through the current workflow

**Change:** integrate current main before final evidence, update the feature
and API/operations documentation on that base, add implementation screenshots
and actual run receipts, then perform the code review required by the
repository's current workflow. The review recorded here is a design review;
it is not a review of code Sol has not written yet.

**Acceptance:** all §8 cases have a named test or device observation;
appropriate checks ran against the actual candidate SHA. No missing native
or cluster evidence is represented as passing. Follow the current
`AGENTS.md` first, then the current development-pipeline correction and CI
workflow files. At the inspected newer base, marking a PR ready starts its
fast lane and the `fast-lane` label is obsolete; do not copy the older
checkout's label instructions. Do not resolve workflow conflicts by silently
skipping gates required by the implementation session's governing rules.
Merge/build/deploy are distinct actions; retain the repository's required
promotion evidence and do not infer deployment from a merge.

### 9.1 Concrete verification entry points

These are runnable existing commands or explicitly proposed new test names.
Sol must record the exact commands actually run, pass counts and candidate
SHA; none of the implementation checks below were executed for this doc.

```bash
python3 -m unittest tests.operations.test_docs_index # docs/index coherence
node --test tests/web/live-tv.test.js               # existing web regression suite
rustup run 1.97.1 cargo check -p plurxd --all-targets # compiler before pushing
rustup run 1.97.1 cargo clippy -p plurxd --all-targets -- -D warnings
rustup run 1.97.1 cargo fmt --all -- --check
rustup run 1.97.1 cargo test -p plurx-core dvr        # existing store/core DVR cases
rustup run 1.97.1 cargo test -p plurxd dvr            # existing daemon DVR cases
make apple-build                                    # both Apple targets compile
```

Add a focused web suite `tests/web/dvr-visibility.test.js` for the new
controller/reducer/envelope cases; then run it with `node --test`. Add
`DvrVisibilityTests` to the existing Apple test targets and use the project's
`xcodebuild` schemes with `-only-testing` on the actual discovered target
name and configured simulator destination. Do not run all simulator suites
just to discover the target name. The Android commands, from
`clients/android`, are:

```bash
./gradlew --no-daemon :app:compileDebugKotlin
./gradlew --no-daemon :app:testDebugUnitTest --tests 'tv.plurx.app.livetv.Dvr*'
```

Extend the existing cluster Activity test coverage with an owner/non-owner
DVR case and run the smallest named test via its existing integration test
binary. The full filesystem paths and test filters belong in the package's
evidence once the tests exist, not as guessed claims in this handoff.

### 9.2 Final observable acceptance

From a non-owner frontend, schedule a short unprotected airing. Leave the
guide idle and observe Waiting to start/Starting/Recording without reload.
Confirm the programme, global summary, Activity and a second client agree
within one normal refresh interval. Change the watched channel: capture
continues and the new programme is not falsely badged. Open Activity, observe
written bytes and actual lifecycle events, then stop the named recording.
The UI shows Stop requested before terminal confirmation; playback of the
watched channel continues. The recorded portion becomes playable only after
library linking. In an isolated test fixture, prove loss/retry and missing
owner states without risking an unrelated household recording.

**Done means:** the user can identify what is recording without scrolling
the guide, tell whether writes are progressing, understand a partial or
failed result, and reach the appropriate existing DVR action on web, phone
and television. The first blank-list repair alone does not complete this
effort.
