# Live TV DVR and reminders — the options, with renders

**Status:** accepted by Paul 2026-09-13 · **Written:** 2026-09-13 · **Verified against:**
`main` `311683bc` (the tree the proportions PRs #268/#269/#271 landed in) ·
**Renders:** `docs/mockups/live-tv/dvr-*.png`, generated from the canvas
["Live TV DVR and Reminders"](https://claude.ai/code/artifact/9dadbbdf-929d-4bb1-8c69-7ad8d7cc2e7e)
(the editable source)

Companion to [LIVE-TV-GUIDE-AND-UI-PLAN.md](LIVE-TV-GUIDE-AND-UI-PLAN.md)
(the page these features live on) and
[HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md) (what the tuner
engine has proved on hardware). This is *the choices for recording and for
"tell me when it starts"*, each with what it costs, so one round of rulings
turned into the implementation document after the accepted rulings. Section
numbers exist so the decision record can cite them (`§2.3`, `§4.2`).

Two things shape every option below, and they are worth stating before the
menu, because they decide how far any of it can go:

1. **The guide horizon is the DVR's horizon.** The cache is bounded to
   4–72 h (`live_tv.rs:71–73`, default 24) and the comment on the constant
   says why: *four hours is what the free HDHomeRun tier actually answers*.
   A DVR that can only see four hours ahead is a "record what's on"
   button, not a scheduler. A 14-day guide comes from either the
   Silicondust DVR subscription (their `DeviceAuth` guide answers a
   `Start=` parameter days out) or an XMLTV grabber (Schedules Direct, or a
   free zap2xml-style feed). Both are already the configured sources
   (`GuideSource` at `live_tv.rs:176`); what changes is the cache: it has
   to hold days, not hours, and it has to persist across restarts so a rule
   set at 9 pm still has a guide at 3 am. §2.1 prices this.
2. **Programmes have no identity today.** `LiveTvProgramme`
   (`live_tv/guide.rs:101–118`) carries start, end, title, episode, synopsis,
   image, air date and filters — no series id, no programme id. The
   HDHomeRun entry parser (`guide.rs:349–361`) drops `SeriesID` on the
   floor, and XMLTV's `episode-num` is reduced to an `SxxEyy` string
   (`xmltv_ns_episode`, `guide.rs:913`). A one-off recording or reminder
   can key on `(channel_id, start)`; a *series* rule needs a stable id or
   an honest title match. §2.2.

---

## 1. What "DVR" and "reminders" mean here

**DVR:** you mark a programme in the guide and plurx records it on your
tuner into your library, where it plays like anything else, on every client.
It includes: one-off recordings · series rules · a schedule with conflicts
you can see and resolve · a recordings shelf · retention · and the same
attribution you asked for everywhere — a recording is a row on Activity
with a reason and a Stop.

**Reminders:** you mark a programme and every client you have open tells you
when it is about to start, with *Watch* and *Record* right there; phones
tell you even when the app is closed.

**Not in scope, deliberately** (each would be its own decision): pausing
live TV / a time-shift buffer (§2.4 says how the capture choice keeps the
door open) · commercial detection · recording protected ATSC 3.0 (the
engine cannot open it; `HDHOMERUN-LIVE-TV-STATUS.md`) · recording from a
library channel (that is your own media, already on disk).

---

## 2. DVR — the four decisions

### 2.1 Guide horizon: how far ahead can you schedule?

| Option | Horizon | Series identity | Cost | Verdict |
|---|---|---|---|---|
| **G1 · Free HDHomeRun guide** (today) | ~4 h | none | nothing | Record-what's-on only. Fine as the *floor* every install has. |
| **G2 · HDHomeRun DVR subscription** | 14 days | `SeriesID` + `ProgramID` in every entry | ~$35/yr, one setting | The zero-grabber path; the same `DeviceAuth` fetch (`guide.rs:426`) with `Start=` paging. **Hardware verification needed** for the exact host/paging — the guide plan already left that open. |
| **G3 · XMLTV** (already a source) | whatever the grabber gives, typically 7–14 d | `dd_progid` / `xmltv_ns` episode ids when the grabber supplies them | a grabber you run + often Schedules Direct (~$35/yr) | Best data; most moving parts. |

**Recommendation:** build for G2 *and* G3 (they are the two sources that
exist), keep G1 working as the floor. Either way the guide cache changes
shape: **persisted, day-scale, refreshed in a rolling window** — a
`live_tv_programmes` table keyed on `(channel_id, start)` with
`series_id`/`programme_id` columns that are nullable, because G1 never
fills them. Memory-only is what the 72-hour ceiling is protecting; on disk
the ceiling becomes 14 days and the bound becomes rows per channel. This is
the same cache the "guide is empty until I press refresh" complaint needs
(draft PR #273 proposes the background refresh); the DVR's only additions
to it are the horizon and the two id columns — build it once.

### 2.2 Series rules: how do you say "every new Kitchen Table"?

| Option | Matches on | Works with | Failure mode |
|---|---|---|---|
| **S1 · Series id** | `series_id` from G2/G3 | G2, G3 with ids | none worth naming — this is what the ids are for |
| **S2 · Title, normalised, channel-locked** | `lower(trim(title))` + channel | G1, G3 without ids | "Kitchen Table" and "Kitchen Table: Holiday Special" are one show or two, depending on the network's whim |
| **S3 · Both, id preferred** | S1 when the guide has an id, S2 otherwise, and the rule remembers which | everything | the rule's match mode is visible in the UI so you know why it fired |

**Recommendation: S3.** A rule stores `match: series_id | title`, the
matched value, an optional channel lock, `new_only`, `keep` (all · last N ·
until watched · N days), and start/end padding. **New-only** means
`original_air_date` within 7 days of the airing — the one signal every
source gives (`guide.rs:381`); XMLTV's `<new/>`/`<previously-shown/>` and
HDHomeRun's `Filter` tags are upgrades when present. Rules have a priority
order (drag to reorder) because §2.3 needs one.

### 2.3 Tuners and conflicts: who loses when four is not enough?

The FLEX 4K has four tuners (`HDHOMERUN-LIVE-TV-STATUS.md`, discovery row);
viewers and recordings draw from the same pool through the same owner node
(`LiveTvConfig.owner_node_id`, `live_tv.rs:148`). Three policies:

| Policy | A recording needs a tuner a viewer holds | A viewer needs a tuner a recording holds |
|---|---|---|
| **C1 · Recordings never pre-empt** | recording is *missed*, logged, and shown as a conflict | viewer gets `tuner_capacity` as today |
| **C2 · Reserved tuners** | as C1, but recordings may only use `tuner_count − reserve` (setting, default 1) | at least `reserve` tuners are always there for live viewing |
| **C3 · Viewer may pre-empt** | as C2 | the client shows "recording Kitchen Table on tuner 3 — stop it and watch?"; a confirmed stop is attributed to that user |

**Recommendation: C2, with C3 as the confirm path** — one Stop for a
recording (`DELETE /dvr/recordings/{id}`, new), reachable from the
Activity row and from the tuner-full error. Activity's existing Stop
(`DELETE /activity/sessions/{id}`, `http/mod.rs:138`) is transcode/VOD only
(`system.rs:4338–4353`) and the Live TV rows there have none
(`liveTvActivityRows`, `index.html:13995`); the recording row gets its own.
Nothing takes hardware silently in either direction, which is the rule you
set for the cache producer. Recording-vs-recording conflicts
resolve by rule priority, and the losing airing is *kept in the schedule*
marked "won't record — no tuner", so the conflicts view (render
`dvr-web-schedule`) can show you what to drop.

### 2.4 Capture: what actually writes the file?

| Option | How | Quality | Reuse | Tuner cost |
|---|---|---|---|---|
| **K1 · Copy the broadcast** | one tuner GET on `/auto/v{n}` (`live_tv.rs:2118`), `ffmpeg -c copy` into `.ts` on the DVR root, sidecar JSON from the guide | exactly the broadcast (MPEG-2 1080i, or HEVC on ATSC 3.0) | the original-quality delivery table already decides how each player gets that (`LIVE-TV-ORIGINAL-QUALITY-IMPLEMENTATION.md` §, *copy where the player can, convert only what cannot play*) | one tuner per recording |
| **K2 · Record the live session's HLS** | tee the segments an active viewer session already produces | whatever that viewer negotiated — 720p H.264 on a browser | none of the VOD pipeline; a second player format | zero extra, but only while someone is watching |
| **K3 · Silicondust RECORD engine** | run `hdhomerun_record` on a NAS; plurx indexes its folder and pushes rules through `api.hdhomerun.com/api/recording_rules` | broadcast copy | none of ours; a second daemon, its own storage layout, and it needs G2 | theirs |

**Recommendation: K1.** It is the smallest engine that gives full quality
and it is the same philosophy the original-quality work already shipped:
the file on disk is the broadcast; conversion happens at play time per
player, where the code for that already lives. Two implications worth
ruling on now:

- **The DVR root must be a mount every node sees.** You have four plurx
  nodes on the same media mounts (`/20t`, `/8tb`, `/8t-2`, `/media`,
  `/mnt/nas` NFS); the owner node writes, any node serves. The setting is
  a container path, like every library root.
- **Playback while still recording** is a second milestone: it needs the
  VOD path to accept a growing input. K1 keeps that possible (a growing
  `.ts` is exactly what a time-shift buffer is); K2 makes it free but at
  session quality. Decide later; nothing in K1 closes it.

K2 is worth naming because it is tempting: it is *not* free once you ask
"what if nobody is watching", which is the whole point of a DVR.

### 2.5 Where recordings live in the library

| Option | Shape | Metadata | Verdict |
|---|---|---|---|
| **L1 · A `Recordings` library kind** | like `home` (`LibraryKind`, `domain.rs:17`): the disk is truth, a sidecar JSON per recording carries title · episode · synopsis · channel · airing time · image URL from the guide, no provider lookups | from the guide, verbatim | one scanner branch, no false TMDB matches for *Eyewitness News* |
| **L2 · Write into a Shows library** | `Show/Season 01/Show - S01E02 - Title.ts`, let the existing scanner (`scan/parse.rs:84`, `SxxEyy`) and provider enrich | provider, when it matches | great for real series, wrong for news · sports · movies · local |
| **L3 · L1 with "file into Shows"** | L1 by default; a per-rule or per-recording action moves a finished recording into a Shows root with L2 naming | both | later, if wanted |

**Recommendation: L1**, with L3 as the obvious follow-on. The recordings
shelf on the Live TV page (renders `dvr-tv-recordings`, `dvr-web-recordings`,
`dvr-phone-recordings`) reads the same rows; a recording that is still in
progress shows as such and, until §2.4's second milestone, is not playable.

### 2.6 Attribution and stopping — non-negotiable, so it is not an option

Every recording is an Activity row: `kind: "record"`, label *Recording
Kitchen Table*, detail *7.1 WABC · rule "Kitchen Table (new)" · tuner 2 ·
24 min left · 1.9 GB so far*, and a Stop (`DELETE /dvr/recordings/{id}`).
A stopped recording keeps what it captured and says who stopped it. The
scheduler that starts recordings is a loop on the tuner-owner node beside
`guide_refresh_loop` (`live_tv.rs:3206`) — owner-only by construction, so
exactly one node in a cluster starts captures without a second lease.

### 2.7 Settings, and the Developer-tab enable section

Nothing is gated. The switch is `live_tv.dvr_enabled`; the Developer tab
lists what should be true first, each row `Met` / `Unmet` / `Unobservable`
from `GET /developer/readiness` (`http/developer.rs:106`), advisory only,
exactly as the existing rows work (render `dvr-web-developer`):

| Requirement | How the daemon answers |
|---|---|
| DVR root set and writable from the owner node | statx + a write probe of a temp file — `Met`/`Unmet` |
| Free space on the DVR root ≥ the floor (setting, default 50 GB) | `Met`/`Unmet` with the number |
| Guide source is on and the horizon is > 24 h | `Unmet` on G1 with "4 h — record-what's-on only", `Met` on G2/G3 |
| Tuner reserve < tuner count | `Met`/`Unmet` from the snapshot |
| Every node mounts the DVR root | `Unobservable` on a single node; `Met`/`Unmet` from peer capabilities in a cluster |

The free-space floor is not a gate either: it is what the scheduler checks
before *starting* a recording, and a recording that would breach it is
kept in the schedule marked "won't record — disk", the same shape as a tuner
conflict. Running out of disk under a recording is destructive to the
cache and the transcodes on the same mount; refusing to start is the honest
alternative to failing halfway.

---

## 3. Reminders — two decisions

### 3.1 What a reminder is

A row per user: `(channel_id, start, title, lead_minutes)`, default lead
5 min, set from any guide cell on any client (renders `dvr-tv-guide`,
`dvr-phone-programme`). Identity is `(channel_id, start)` with the title as a
guard: if the guide refresh moves or renames the airing, the reminder
follows it when the title still matches and is flagged when it does not.
A reminder that fires while a recording rule already covers the airing says
so ("recording on tuner 2"). Reminders are the user's, not the install's —
you and another account on the same server get your own.

### 3.2 Delivery: how does it reach you?

| Channel | Reaches | Needs | Verdict |
|---|---|---|---|
| **R1 · In-app overlay** | every client that is open — web, iPhone/iPad, Android, **and both TVs**, which have no notification centre worth the name (tvOS has none; Android TV heads-ups are minimal) | one poll on the guide/status cadence already running; a banner with *Watch* · *Record* · *Dismiss* (render `dvr-tv-reminder`) | **the floor** — the only thing that works on the TVs at all |
| **R2 · Local notifications on phones** | iPhone/iPad and Android with the app closed | `UNUserNotificationCenter` (nothing in the Apple client uses it yet) and `NotificationCompat` (Android already has a channel for offline downloads, `OfflineTransferJobService.kt:142`, and `POST_NOTIFICATIONS` in the manifest); the client schedules them from the server's reminder list on launch, foreground and every change; the server keeps the list, so a reminder set on the TV shows up on the phone | **cheap and no infrastructure** — but a phone that has not opened the app since the reminder was set does not know about it |
| **R3 · Outbound webhook** | anything you already notify with — **Home Assistant** (its companion app is on every phone and can hit the TVs), ntfy, a generic JSON POST | one settings URL through the approved-URL pattern (`approved_artwork_url`, `http/comingsoon.rs:534`; `approved_guide_url`, `guide.rs:76`), fired by the same job that starts recordings | **cheap, covers the gap in R2**, and doubles as *recording started / finished / failed* |
| **R4 · Apple push (APNs)** | iPhone/iPad lock screen, app closed, no dependence on the phone having synced | an APNs auth key (`.p8`) uploaded in settings; plurxd talks HTTP/2 to APNs itself; device tokens hang off the per-device `tokens` rows (`hiqlite.rs:199`, `device` column) | real push, fully self-hosted, but a second credential and a push entitlement in the build |
| **R5 · Android push (FCM)** | Android lock screen | a Firebase project and its `google-services.json` in *your* build | a third-party dependency for what R2 + R3 already cover; **UnifiedPush** is the self-hosted alternative if this ever matters |
| **R6 · Web Push** | browsers, tab closed | a service worker in the embedded UI, VAPID keys plurxd generates, **and HTTPS** — push is refused on a plain-HTTP origin, which is what `media1:32400` is | not until the servers speak TLS |

**Recommendation: R1 + R2 + R3 in the first milestone.** R1 is the
TV story and it is one overlay per client; R2 is the phone story and it is
the platform's own alarm clock; R3 is one URL and it reaches everything
else you own through Home Assistant. R4 is the one worth adding when you
want the lock screen without opening the app — it is self-hosted and
nothing in R1–R3 has to change to add it. R5 and R6 wait for a reason.

---

## 4. The UI — what the renders show

Everything sits on the Live TV page that shipped; the proportions from
`LIVE-TV-PROPORTIONS-IMPLEMENTATION.md` are unchanged (620 px list beside a
1116×628 picture · 74 px grid rows · 30/22/20/18 px type on tvOS · web
`.lt-row`). What is added:

| Render | Shows |
|---|---|
| `dvr-tv-guide.png` | The TV guide with a **focused future cell**: the details pane gains *Record* · *Record series* · *Remind me* beside *Watch*; cells carry a small red dot (scheduled) or bell (reminder); the toolbar gains a **Recordings** segment |
| `dvr-tv-reminder.png` | The **overlay** (R1) over whatever is on the TV: "Kitchen Table starts in 5 min · 7.1 WABC" with *Watch* · *Record* · *Dismiss*; auto-dismisses at start |
| `dvr-tv-recordings.png` | The **Recordings** segment on TV: Library · Scheduled · Rules chips; a recording in progress with its tuner and a Stop; the same 620 px list rules |
| `dvr-web-guide.png` | The web grid with the details popover and the same four actions; scheduled/reminder marks on cells |
| `dvr-web-recordings.png` | Web Recordings: Library shelf, then Scheduled with a **conflict** row ("won't record — no tuner · move rule up or drop"), then Rules with priority order and keep/padding |
| `dvr-web-activity.png` | The Activity row a recording produces, with Stop — §2.6 |
| `dvr-web-developer.png` | The Developer-tab DVR enable section with the five requirement rows — §2.7 |
| `dvr-phone-programme.png` | The phone programme sheet: Watch · Record · Series · Remind, padding and keep in a fold |
| `dvr-phone-reminder.png` | The phone's own notification (R2/R4) with *Watch* and *Record* actions, and the in-app banner under it |
| `dvr-phone-recordings.png` | Phone Recordings list with the in-progress row |

Two conventions the renders adopt, for a ruling:

- **Record is one press, series is two.** *Record* on a cell records that
  airing with the default padding. *Record series* opens the rule editor
  with sane defaults (new only · keep all · −1/+2 min) already filled.
- **Marks on cells are quiet.** A 6 px red dot for scheduled, a bell for a
  reminder, a red progress underline for recording now; no text, because a
  grid cell is 74 px tall and the title is the point.

---

## 5. What one ruling decides

Answer these and the implementation doc writes itself:

1. Guide horizon: **G2**, **G3**, or both (recommended both, G1 as floor).
2. Series matching: **S3** (recommended).
3. Tuner policy: **C2 + C3 confirm** (recommended); the reserve default.
4. Capture: **K1** (recommended); play-while-recording now or later.
5. Library shape: **L1** (recommended), L3 later.
6. Reminder channels: **R1 + R2 + R3** (recommended); R4 now or later.
7. The two UI conventions in §4.

What you need on the hardware side before any of it: for G2, the
subscription on the FLEX 4K and one guide fetch with `Start=` to confirm the
paging shape (the guide plan's M1 left the host to hardware; this adds one
parameter). That is a GPT-session job and §6 has the prompt.

---

## 6. The one thing to verify on hardware (GPT prompt)

> On the FLEX 4K (device `1040A1B2`, LAN), read `http://<tuner-ip>/discover.json`
> and take `DeviceAuth`. Then fetch, with curl and `--compressed`, the
> HDHomeRun guide **twice**: once as
> `https://api.hdhomerun.com/api/guide?DeviceAuth=<auth>` and once with
> `&Start=<unix time 48 hours from now>`. For each response report: HTTP
> status, whether it is gzip, the number of channels, for channel 7.1 the
> earliest and latest `StartTime`/`EndTime` (as local times), and whether the
> entries carry `SeriesID`, `ProgramID`, and a `Filter` array (paste one
> entry verbatim, with `ImageURL` shortened). If the second call returns
> nothing beyond ~4 hours, say so — that is the free tier. Do not paste
> `DeviceAuth` into the report; replace it with `<auth>`.

---

## 7. Where this sits in the pipeline

If the rulings come back, the next document is
`LIVE-TV-DVR-IMPLEMENTATION.md` in the shape of the proportions one: two or
three independent PRs (server engine + web · Apple · Android), each a draft
with one adversarial review, one full-suite run after the fixes, merged
from a fast-lane label; Apple build and Android versionCode bump when client
sources change; nothing gated beyond the Developer-tab switch in §2.7.
