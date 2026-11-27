# Playback info — a live "This shot" row: decision

**Status:** done — decided no, not now · **Decided:** 2026-11-25 · **Owner:** Claude, for Paul (review open as ask in effort `E-DE100CBAEC`)

The Playback info rows for Dolby Vision metadata and the display target
(`dv_metadata`, `display_target` in
[`playback-info-fields.json`](../../tests/playback/playback-info-fields.json))
left one row out: the current shot's brightness, its L1 minimum, average and
maximum, as LibreELEC CB1 shows it. The rows plan made that row a separate
decision, taken only after measuring what a per-shot timeline would cost. This
records the measurement and the decision.

## What was measured

One complete Dolby Vision title from the library, read once and read-only
through the server's download endpoint, as a stream copy of its video. No
picture was decoded and nothing was written. Every RPU was parsed, and the
`dolby_vision` crate the server already vendors read each one's L1 block and
scene-refresh flag.

| Title | Grade | Length | RPUs | Parsed | Shots (scene refresh) | Shots (L1 change) | Most shot starts in any 5 s |
|---|---|---|---|---|---|---|---|
| The Beekeeper (file 93) | Profile 7, FEL, 2160p, 24 fps | 1 h 45 min | 151 384 | 151 384 | 2 068 | 2 046 | 12 |

The average shot is **3.05 s**: 6 314 s spread over 2 068 shots.

## What a "This shot" row would cost

- **Storage:** one record per shot holds a start frame (u32) and L1 min, average and maximum (3 × u16), 10 bytes. That is **about 21 KB per title** in binary, or about 66 KB as JSON. JSON would not fit the 64 KiB bound on title-scan output, so the record would need a compact encoding. Storage is not the obstacle.
- **Wire:** control exchanges run every 5 s. Carrying the shots that start before the next exchange costs **at most 12 records, about 120 bytes** (1.6 records on average). That fits easily inside the 16 KiB control body. Bandwidth is not the obstacle either.

## Why the answer is no

Measuring changed the question. The obstacle is correctness, not cost:

1. **A value refreshed once per exchange would be wrong most of the time.** Shots average 3.05 s and the exchange runs every 5 s, so a single "current shot" value would usually describe a shot that has already ended.
2. **Being right means scheduling values on each player's own clock.** The server would send upcoming shots with start times, and each client would switch values against its media clock, across seeks, rate changes, prepared-successor commits and media-origin offsets. That is new timing code in four clients (web, Apple, Android, the Linux player), for a diagnostic row. The two existing rows only render text the server wrote.
3. **What a viewer would learn from it is already said.** The `Display target` row gives the title's peak and what the stream was mapped for. Per-shot values matter for checking tone mapping frame by frame, which is a lab job.

## What would change the answer

- A real need to inspect per-shot mapping on a viewer's device, rather than in the lab.
- A player clock contract that clients already share for other per-time server data. The timed objects of the subtitle overlay are one possible example. A "This shot" row could then reuse it instead of adding its own.

If either arrives, the numbers above are the budget: store about 21 KB per title in a compact encoding, and send at most 12 shots per exchange.
