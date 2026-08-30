# Playback caps v2 — M0 measurements

**Status:** measured 2026-08-30 · **Executes:** [PLAYBACK-CAPS-V2-PLAN.md](PLAYBACK-CAPS-V2-PLAN.md)
§7 M0 · **Node:** nuc4 (`192.168.4.8:32400`) on `v0.2.7-2417-g4ba8bb48`

Companion to [PLAYBACK-CAPS-V2-PLAN.md](PLAYBACK-CAPS-V2-PLAN.md) (what to
build and in what order) — this is *what was true before any of it was built*,
so every later milestone argues from numbers rather than from the report that
started it.

Read §1 first: it is the answer to the question that opened the whole
investigation, and it is not the answer anyone expected. §2 is the library
census, §3 the live decision probe, §4 the node's own capabilities, §5 the
browser facts, and §6 what all of it changes about the plan.

Every figure here was taken through the HTTP API on 2026-08-30 with an admin
bearer token. Nothing was measured through the database directly, so the
counts are per **file row as the API reports it**, which is the same row
`decide()` reads.

---

## 1. The report was from Chrome, and Chrome cannot play Dolby Vision

Paul's report was "a lot (if not all) media that is Dolby Vision is playing at
HDR10 or lower now", on a Mac with a Pro Display XDR. The browser was
**Chrome** (confirmed 2026-08-30).

Chrome decodes HEVC but has never decoded Dolby Vision, at any profile, on any
platform, whatever the display reports. There is also no macOS client —
`clients/apple/project.yml` declares `plurx-iOS` and `plurx-tvOS` only — so a
Mac is necessarily in the web player. HDR10 is therefore the *ceiling* for
every Dolby Vision title on that machine, and plurx handing over an HDR10 base
layer is the correct answer rather than a regression.

This retires the premise of the investigation and re-ranks the plan:

| Milestone | What it does for this report |
|---|---|
| M5a / M5b — Profile 7 → 8.1 | **Nothing.** Converting a P7 disc to single-layer P8.1 makes it playable as Dolby Vision on Safari, Apple TV and DV-capable Android. Chrome refuses it either way. |
| M4 — grade negotiation | **This is the fix.** The "or lower" half is every transcode of every HDR source, all of which tone-map to SDR today. On Chrome those transcodes happen for ordinary reasons — a height cap, burned subtitles, the quality menu. |
| M1 — index every identity | Fixes a real outage for Safari / Apple TV / Android, not for Chrome. |

Paul's ruling 2026-08-30: **do M4 next**, then return to the plan's order.

---

## 2. Library census — the movies libraries

Walked through `/api/v1/items/{id}` for every item in the `movies`-kind
libraries: **392 files**. The `shows` libraries expand to ~4,400 episodes and
are a separate sweep; nothing in §6 depends on them.

**By dynamic range:**

| `files.hdr` | Files |
|---|---|
| (none — SDR) | 267 |
| `dolby_vision` | 84 |
| `hdr10` | 41 |

**By Dolby Vision profile** — this is the census the plan asks for:

| `files.hdr_format` | Files |
|---|---|
| `Dolby Vision · Profile 8 (HDR10-compatible)` | 40 |
| `Dolby Vision · Profile 7 (HDR10-compatible)` | 33 |
| `Dolby Vision · Profile 5` | 10 |
| `Dolby Vision · Profile 8` | 1 |

**Files unclaimable by any client (plan edge E2): zero.** Every Dolby Vision
row carries a `Profile N` in its label, so the profile-less-label defect the
plan budgets M2 against does not exist in this library. M2 is still worth
building — a label is a bad place to keep a fact, and the Profile 7 conversion
needs `el_present_flag`, which no column carries — but it has **no backlog to
clear**, and its acceptance check ("the count reduces to the no-DOVI-record
residue") is already satisfied at zero.

**How to read this:** 33 Profile 7 files is the entire addressable market for
M5a and M5b, and 50 Profile 5/8 files are the ones M1 unblocks. The single
biggest group is the 267 SDR files, which none of this touches.

---

## 3. The VOD index is almost entirely unbuilt

| `vod_index_status` | Files | Of the 84 DV files |
|---|---|---|
| `pending` | 332 | 75 |
| `indexed` | 45 | 9 |
| `unsupported` | 15 | 0 |

**88% of the movies library has no fragment index at all.** This is a larger
fact than the one M1 was written for. M1 fixes a file that is indexed for the
wrong pipeline; this says most files are indexed for no pipeline.

Two things follow. First, the plan's M1 acceptance check — "after one index
pass, `SELECT COUNT(*) … WHERE file_id=<id>` returns 2" — has to be run against
a file that has actually been through a pass, not an arbitrary title. Second,
the backlog is the thing to watch after M1 ships: `INDEX_MAX_PER_PASS` is 4
identities per 15-minute tick, so 332 pending files (≈400 identities once
Dolby Vision titles ask for two) is on the order of **25 hours of wall clock**
before the library is current, and M1 makes that number bigger, not smaller.

**How to read it if the number does not fall:** check `vod_index_mins` is not
zero, then check the pass is not being preempted — `pretranscode_worker_idle`
returning false ends a pass immediately, and a node that is always transcoding
never indexes.

---

## 4. The node can do everything the plan assumes

`GET /api/v1/system` on nuc4:

| Field | Value | What it means for the plan |
|---|---|---|
| `dovi_rpu` | `true` | The `dovi_rpu` bitstream filter is present, so Dolby Vision can be stripped on the way out. A `false` here would have turned every strip into a re-encode and every DV title into SDR — the first thing to check on any "DV plays as SDR" report. |
| `dovi_reshape` | `true` | The Profile 5 RPU renderer is proven. |
| `dovi_passthrough` | `true` | The tonemapx passthrough graph runs. M4's `RenderCaps::hdr10_passthrough` starts from this. |
| `dovi_passthrough_qsv` | `false` | The QSV variant is not proven, so M4's HDR10 rung is software-encoded on this node until it is. |
| `ffmpeg_version` | `7.1.4-Jellyfin` | Above the 7.1 floor `dovi_rpu` needs. |

---

## 5. The live decision probe

The reported title resolves to file **5323**,
`Resident.Evil.Welcome.to.Raccoon.City.2021.2160p.BluRay.REMUX.HEVC.DTS-HD.MA.TrueHD.7.1.Atmos-FGT.mkv`
— 51.6 GB, HEVC Main 10, 3840×2160, 64 Mb/s, `Dolby Vision · Profile 7
(HDR10-compatible)`, TrueHD 7.1 Atmos first audio track.

Three capability strings against `GET /api/v1/files/5323/decision`:

```bash
# Safari: HDR, DV profiles 5 and 8, PQ presentation proven
&hdr=1&dv=1&dvprofile=5,8&dvhls=1&hdr10t=1&maxheight=2160
# Chrome: HDR and PQ presentation proven, no DV claim
&hdr=1&hdr10t=1&maxheight=2160
# Chrome on an SDR display: no HDR claim at all
&maxheight=2160
```

| Client | `method` | `preserve_dolby_vision` | `delivered_dynamic_range` | `vod_indexed` |
|---|---|---|---|---|
| Safari | `remux` | `false` | `hdr10` | `false` |
| Chrome | `remux` | `false` | `hdr10` | `false` |
| Chrome, SDR display | `transcode` | `false` | `sdr` | `false` |

Reasons, identical for the first two: `container mkv not browser-native` ·
`audio codec truehd unsupported` · `Dolby Vision metadata removed for this
device; compatible HDR base kept`. The third adds, first in the list: `HDR
(dolby_vision) presentation was not proven by this client; tone-mapping to
SDR`.

**How to read it.** Three of the plan's edges are visible in one table:

- **The Profile 7 rule is doing exactly what it says.** Safari advertises
  `dvprofile=5,8` and the file is 7, so the exact-match test fails and the DV
  layer is stripped to its HDR10 base. This is the designed answer, and it is
  why M5a exists.
- **E3 is real and it is one flag wide.** Dropping `hdr10t`/`hdr` turns the
  same request into an SDR transcode. Every HDR source that transcodes for any
  reason lands here.
- **E1 is real on this title.** `vod_indexed: false` even for the *stripped*
  identity, which is the one the background pass builds — so this file is part
  of the 88% in §3, and both its identities are missing rather than one.

---

## 6. What this changes

1. **M4 moves ahead of M2 and M3** (Paul, 2026-08-30). It is the only
   milestone that changes what the reporter sees.
2. **M2 has no backlog.** Zero profile-less labels means the migration is
   structural work for M5, not a repair. Build it, but do not expect the
   census to move.
3. **M6 is reporting, not repair.** `localStorage["plurx_decode_limits"]` was
   `{}` on the reporting browser — no learned decode limit is routing anything
   to a transcode today. The mechanism is still worth surfacing (it is
   invisible to the server by construction, and it is a month-long effect when
   it does fire), but it is not a live cause here.
4. **The index backlog is the thing to watch after M1.** 88% pending is a
   bigger lever on "does VOD HLS work at all" than the identity keyspace M1
   fixes, and M1 adds to it.

## 7. Still open

- `PLAY_CAPS` from the reporting browser — specifically `maxheight`. A 1080
  there would mean every 4K title is transcoded before Dolby Vision is even
  consulted, which would put a fourth cause ahead of everything in §6.
- The `shows` libraries (~4,400 episodes) are not censused. Nothing in §6
  depends on them; the numbers to add are the profile split and the
  `vod_index_status` split.
- `grep -c 'temporary live-HLS recovery'` on the plurxd log, which the plan
  asks for as the in-the-wild count of E1. Not reachable through the API.
