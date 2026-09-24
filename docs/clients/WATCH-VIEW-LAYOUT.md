# Watch view layout — the page beside and below the picture

**Status:** built · **Design:** Fable option D, chosen 2026-09-23 ·
**Written:** 2026-09-23

Companion to [WATCH-AND-BROWSE-IMPLEMENTATION.md](WATCH-AND-BROWSE-IMPLEMENTATION.md)
(how the retained web player and its browser came to exist and what still
gates the native versions) and [WEB-SHELL-LAYOUT.md](WEB-SHELL-LAYOUT.md)
(where the files live). This is *what the page shows once a movie or episode
is playing, and why it is laid out that way*.

## What changed and why

The first watch view carried three things over from the modal player that
read as afterthoughts once the player sat on a page: chapters were numbered
grey tiles with no pictures, every media fact was behind a "Title details"
button, and the wide state squeezed the title into a caption box under the
player while a page-level "Close player" sat next to the player's own ✕.
Four layouts were rendered and compared (project record: *Watch view redesign
options*, 2026-09-23); the one built is B's chapter rail with A's ledger,
both collapsible.

The rules that came out of it:

- **One Close.** The ✕ in the player bar closes the player. The page heading
  is the same breadcrumb the item page shows, so the page still says where
  it is without a second button.
- **Media facts render on the page.** Video, audio tracks, subtitle tracks,
  delivery mode and file are a ledger beside the picture (compact) or under
  it (wide). Nothing is behind a button.
- **Chapters have pictures**, and the chapter rail sits under the picture as
  part of the player rather than a grid somewhere below it.
- **Unbounded rows fold.** A file can carry a dozen audio or subtitle tracks
  and thirty chapters. Folded, a track row shows the selected track and a
  count; folded, the rail is one line. The folds remember themselves.

## The page, state by state

```
compact (default)                          wide ("Larger")
┌───────────────────────┬───────────────┐  ┌───────────────────────────────────────┐
│                       │ NOW PLAYING   │  │                                       │
│        picture        │ Title         │  │               picture                 │
│                       │ meta · left   │  │                                       │
│                       │ badges        │  ├───────────────────────────────────────┤
│                       │ synopsis      │  │ Chapters  · Chapter 4 · 8 · 0:33/2:00 ▴│
├───────────────────────┤ ───────────── │  │ ▬▬▬▬▬▬▬▬▬▬▬▬▬░░░░░░░░░░░░░░░░░░░░░░░░ │
│ Chapters · Ch 4 ·   ▴ │ Video …       │  │ [01][02][03][04][05][06][07][08]      │
│ ▬▬▬▬▬▬░░░░░░░░░░░░░░ │ Audio  ● En ▾ │  ├──────────────────────┬────────────────┤
│ [01][02][03][04][05]› │ Subs   ● Off▾ │  │ NOW PLAYING          │ Video …        │
│                       │ Delivery      │  │ Title · meta         │ Audio  ● En ▾  │
│                       │ File          │  │ badges · synopsis    │ Subs   ● Off ▾ │
└───────────────────────┴───────────────┘  └──────────────────────┴────────────────┘
```

**Compact** is the stage grid: the picture and, under it, the chapter rail,
in a 2fr column; the title panel with the ledger in a 1fr column of at least
300 px. **Wide** stacks: picture, rail, then a two-column band with the title
block on the left and the ledger on the right. Under 550 px the band is one
column. Which state you are in is the player's existing Larger / Smaller
control and is remembered per browser (`plurx_watch_player_size`).

Episodes keep the episode browser (`#watch-lower`, cards or rows, season
picker) under everything; the rail is hidden for them because episode files
rarely carry chapters worth a strip.

## The chapter rail (`#watch-rail`)

**What it shows:** a header line — `Chapters · <current chapter title> ·
N chapters · position / total` and a fold chevron — then a ruler and a
strip. The ruler is one bar whose segments are each chapter's share of the
runtime, separated by 2 px gaps, with the playhead drawn across it as an
accent fill, so it is a chapter map of the whole film at a glance. The strip
is one row of 132 px thumbnails that scrolls sideways past what fits; each
tile carries the chapter number top-left, the start timecode bottom-right,
and the chapter title under it. The current chapter has an accent frame, an
accent title and a progress bar along the bottom of its tile; the strip
scrolls itself to keep the current chapter in view (horizontally only — it
never scrolls the page under a viewer).

**Folded**, the strip and the full ruler go, and a shorter ruler with the
same segments and playhead moves into the header line, so a folded rail is
still a chapter map at one line of height.

**Clicking a tile** seeks to that chapter's start through the same
`seekTo()` the transport uses. Clicking the chevron folds or unfolds the
rail; the choice is stored per browser in `plurx_watch_folds` (`rail`,
default open).

**Where the pictures come from:** `GET /api/v1/files/{id}/chapters/{n}/thumb`
(next section). The page loads them two at a time, in order, and only once
playback has been accepted, so eight image requests never take the browser's
connections to the origin while the stream is starting; a failed load is
retried once after four seconds (a 503 means both of the node's extraction
slots were busy). A thumbnail that cannot be had — the switch is off, the
file has gone, ffmpeg failed — leaves the tile as the numbered tile it was
before, so the rail degrades to what it used to be rather than to a broken
image.

## Chapter thumbnails (`/api/v1/files/{id}/chapters/{n}/thumb`)

**What it is:** one 320 px-wide JPEG per chapter, made the first time a
watch page asks for it and kept under the runtime cache at
`<runtime cache>/chapter-thumbs/<file id>-<size>-<mtime>/<n>.jpg`. The
second request for the same chapter is a file read. The cache key names the
file's identity, so a replaced file gets fresh thumbnails and the old ones
become an orphan directory (there is no cap and no sweep; removing
`chapter-thumbs` under the runtime cache reclaims everything). The URL the page
uses carries `?v=<mtime>` for the same reason on the browser side, and the
response carries an `ETag` and `private, max-age=604800`.

**How it is made:** one ffmpeg, `-ss <chapter start + 2 s, capped at the
chapter midpoint> -i <source> -map 0:v:0 -frames:v 1 -vf scale=320:-2`, JPEG
quality 4, on the input side of the seek so the demuxer jumps to the nearest
keyframe rather than decoding from the start. CPU only; no GPU is touched. At
most two extractions run on a node at a time; the others wait up to 20 s for
a slot and are told 503 after that; each extraction is bounded to 15 s and
2 MiB. A request that queued behind the one that made a thumbnail finds it
on disk when it gets its slot; two requests that hold both slots for the same
chapter both extract, and the atomic rename makes the second a no-op. A
failed extraction leaves a `<n>.fail` marker beside the thumbnails and the
route answers 404 from it for an hour without running ffmpeg again.

**Why on request and not in the analysis queue:** the analysis queue is the
replicated, leased, target-node machinery behind the fragment index, and a
third component there is a schema and metrics change on every voter. The
thumbnail is a few seconds of CPU per chapter, needed only for the film
somebody is watching, and worth nothing for the films nobody opens — so
there is no background producer, no library sweep, and nothing that runs
unless a page asks. That is also what keeps it honest under the standing
rule that hardware work must be attributable from inside the product: the
only work is the request in front of it, and the Developer tab counts every
one.

**HDR sources** come out flat: the frame is scaled and converted to 8-bit
without tone mapping. The thumbnail is a locator, not a reference image;
tone mapping would triple the cost of every extraction for a 320 px picture.

## The ledger

Five rows, beside the picture in compact and in the band in wide:

| Row | What it shows | Folds? |
|---|---|---|
| Video | codec · width×height · bit depth · SDR/HDR/DV · bitrate | no |
| Audio | one chip per track, language · codec · channels · title; the playing track is accent with a dot | yes |
| Subtitles | an `Off` chip then one chip per track, language · format · forced/SDH · title | yes |
| Delivery | the VOD/Live HLS chip and its one-line reason, as the item page shows it | no |
| File | filename in monospace; container · size · duration | no |

**Chips are the track picker.** While the title is playing, clicking an audio
chip calls `switchAudio()` and a subtitle chip calls `setSub()` — the same
functions the player's ≡ and CC menus call — so the page and the menus can
never disagree; the ledger re-renders whenever the player's selection
changes. A chip for a track the player did not offer for this playback (a
stream the decision left out) renders as plain text with a title saying so.
Before playback is accepted the chips are plain text and the accent one is
the file's default.

**Folded** (the default), a track row is the selected chip plus a summary
chip — `1 more` for audio; `3 available · English, English SDH, Español` for
subtitles, at most three names then `+N` — and a chevron. Open, every chip
is inline and the chevron folds it back. Fold state is per browser in
`plurx_watch_folds` (`audio`, `subs`, default folded). Video, Delivery and
File are one line each and never fold.

## Settings → Developer → Chapter thumbnails

The switch `chapter_thumbnails` (settings API field; store key
`playback.chapter_thumbnails`; default on) is the enable path. Off answers
the thumbnail route 404, runs no ffmpeg, and the rail shows numbered tiles;
what is already cached stays on disk and serves again when the switch comes
back. The card's rows are advisory and never turn the switch:

- **ffmpeg can decode a frame** — the configured ffmpeg answered `-version`
  at startup.
- **The runtime cache has room** — the same free-space verdict the subtitle
  store uses (1 GiB or 2 % of the filesystem, whichever is larger).
- **What this process has extracted** — made, served from cache, failed,
  running now and refused-while-off since the process started, plus the
  count and bytes on disk under the cache directory.

## Where the code is

| Piece | File |
|---|---|
| Page markup and state (mount, layout, folds host) | `crates/plurxd/src/web/player/watch-presentation.js` |
| Title panel, ledger, chip wiring, rail, chapter marking | `crates/plurxd/src/web/detail/watch-browser.js` |
| Styles (`.watch-*`) | `crates/plurxd/src/web/app.css` |
| Thumbnail route, extraction, cache, counters | `crates/plurxd/src/http/chapter_thumbs.rs` |
| The setting on the wire | `crates/plurxd/src/http/system.rs` (`chapter_thumbnails`) |
| Developer card (server rows / web card / save) | `crates/plurxd/src/http/developer.rs` · `web/pages/settings-developer.js` · `web/pages/settings-playback.js` |
| Browser acceptance (shipped assets, intercepted server) | `tests/web/watch-and-browse.browser.cjs` |

## Decisions taken without Paul in the room (2026-09-23)

Each is easy to reverse; they are recorded so they can be looked over.

1. **Extraction on request, not a queue component or a library sweep.** See
   the thumbnail section for why. The cost is that the first open of a film
   makes its thumbnails while you watch (two at a time, a few seconds each);
   the benefit is nothing ever runs for a film nobody opens.
2. **The switch defaults on.** Matches how stored PGS tracks shipped and
   Paul's rule against gating features; the Developer card is where it is
   turned off, with the counters beside it.
3. **Rail open, track rows folded, on first visit.** The renders Paul chose
   showed that state; both choices are remembered per browser once changed.
4. **Folded subtitle summary shows up to three names, then `+N`.** Names
   tell you whether opening the row is worth it; past three they truncate.
5. **The ⓘ Playback info button in the player bar still opens the title
   dialog in compact and wide** (it now carries the ledger too, with working
   chips). With the facts on the page it is redundant there, but it is the
   transport's button and the Playback Surface Contract fixture pins the
   transport row.
7. **Episodes keep a "Play next" button** in the title block, where the old
   caption had it; the episode grid below is the other way to the next one.
6. **Native clients are untouched.** Their watch layouts are gated on
   separately reviewed owner designs (WATCH-AND-BROWSE-IMPLEMENTATION §2).

## Non-goals

- No thumbnail backfill for the library and no admin "make all thumbnails"
  action. Both would be a background producer, which is exactly what this
  design avoids; if one is ever wanted it belongs in the analysis queue with
  its own row on the Analysis page.
- No sprite sheets or scrubbing previews on the timeline. A per-chapter
  frame is a locator; per-second previews are a different feature with a
  different cost.
- No tone mapping of HDR frames.
