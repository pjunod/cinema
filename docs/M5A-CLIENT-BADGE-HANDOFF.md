# M5a — the `DV P7 → DV P8` badge in the Apple and Android clients

**Status:** ready to build · **Executes:** PLAYBACK-CAPS-V2-PLAN.md milestone
M5a's client half, and MEDIA-BADGES-PLAN.md §2.3 · **Written:** 2026-08-31 ·
**Server and web are done** (PR #716); this is the last thing between M5a and
"finished".

You are adding one badge state to the two native clients. Nothing about the
stream changes — the server already delivers the converted media and already
tells you what it delivered. This is a display change and a small amount of
wire plumbing.

Read MEDIA-BADGES-PLAN.md §2.1–§2.3 before writing anything. §2.3's table is
the contract; the row you are implementing is the second one.

---

## 1. What changed on the server, and why you need a new field

plurx now converts Dolby Vision **Profile 7** to **Profile 8.1** while a copy
session streams. A Profile 7 disc remux is a base layer plus a dual-layer
enhancement layer that no consumer decoder has ever taken; 8.1 is that same
base layer with each RPU rewritten to describe a single layer. Nothing is
re-encoded — the picture is copied byte for byte and only per-frame metadata
changes.

The consequence for the badge: **a device that takes Profile 8 and not 7 now
gets Dolby Vision where it used to get the HDR10 base.** Apple TV and Safari
are exactly that device, and so is most of the Android fleet.

`delivered_dynamic_range` cannot express this. It answers `"dolby_vision"` for
both of these:

- a Profile 7 title **preserved** for a device that enumerates 7;
- the same Profile 7 title **converted** to 8.1 for a device that does not.

The grade really is the same. What differs is that in the second case the
profile on screen is not the profile on disk, and a badge reading `DV P7` for
both is describing the file rather than the picture.

So the wire gained one field:

```
delivered_dolby_vision_profile: u8 | absent
```

It is on **both** `POST /api/v1/files/:id/decision` and the session create
response (`POST /api/v1/files/:id/hls/sessions`), beside
`delivered_dynamic_range`, and it follows the same precedence rule: the
session's answer overrides the decision's the moment a session attaches,
because a burn or a forced rung produces a delivery the decision never
promised (MEDIA-BADGES-PLAN §3.2).

**Absent means "no answer", not "not Dolby Vision".** Three things produce it:
a delivery carrying no Dolby Vision at all (a transcode, a strip), a source
that never had any, and a library row scanned before the profile columns
existed whose label is the bare string "Dolby Vision". `delivered_dynamic_range`
beside it is the field that answers "is this Dolby Vision".

---

## 2. The badge state

| State | Condition | Visual | Example |
|---|---|---|---|
| **converted** | delivered grade == source grade, profile differs | **both halves lit** | `DV → DV P8` |

**Neither half dims**, and that is the whole reason this is its own state
rather than a spelling of the existing "different grade" one. The dimmed
source half means "this capability is unavailable". Here it is *active*: the
base layer is copied untouched and what reaches the device is Dolby Vision.
Dimming it would say the opposite of what happened.

The arrow is there because the profile changed, and a viewer comparing two
players deserves to know which one they are watching.

### Spelling

**Corrected 2026-09-01, after reading the code rather than trusting this
paragraph.** It previously said Apple and Android both label the source half
`DV` with no profile number. Android does. **Apple does not** —
`DynamicRange.sourceMark` already returns `"DV P\(profile)"` when it can read
one out of `hdrFormat`, and has shipped that way. Building from the old claim
would have produced a badge that disagreed with the same client's own detail
screen. So:

- **web** (done): `DV P7 → DV P8`
- **Apple** (done): `DV P7 → DV P8` — the source half already carries the
  number, and this state must not take it away
- **Android** (done): `DV → DV P8` — the source half is a bare `DV` there,
  which is the deliberate already-shipped decision MEDIA-BADGES-PLAN §2.3
  notes

The arrow half carries the number in every client, because that number is the
entire content of this state. `DV → DV` says nothing.

### Accessibility and detail text

- accessibility label: "Dolby Vision, playing as Dolby Vision Profile 8"
- long-press / info panel: "Dolby Vision Profile 8 — converted for this device"

Match the voice of the strings already there; do not invent a new register.

---

## 3. What to change

### Android

1. **Wire.** Add `deliveredDolbyVisionProfile: Int?` to whatever data class
   parses the decision and the session-start responses. Kotlin's serializer
   must treat the field as optional — an older server omits it, and a client
   that fails to parse the response over a missing optional field is a client
   that cannot play anything at all on that server.
2. **State.** Carry it beside the existing delivered-range value, wherever
   that lives, with the same session-overrides-decision precedence. **A
   session that reports a range must clear the profile when the response omits
   it.** This is not hypothetical: the legacy single-ffmpeg copy path (live-HLS
   recovery) serves the HDR10 base for a title the decision said would be
   converted, which is the *normal* first watch of a converting title, before
   its fragment index exists. A stale profile there paints `DV → DV P8` over
   HDR10.
3. **Badge.** In `dynamicRangeFact` (or whatever now owns the chip), add the
   converted branch: source grade `dolby_vision`, delivered grade
   `dolby_vision`, both profiles known, and they differ.
4. **Test it.** Whatever the badge's existing unit test is, add: converted
   (arrow, neither half dimmed), preserved same-profile (no arrow), server
   omitted the field (no arrow, exactly today's chip), and session-clears-a-
   decision's-profile.

### Apple

Same four steps. `Caps.swift` already sends `dvprofile=5,8`, so Apple TV is
already receiving converted streams — the badge is simply not saying so.
`DynamicRange.sourceMark` is where the chip is built.

`Decodable` with an optional `UInt8?` handles the missing field; verify that
against an actual older-server response rather than assuming.

---

## 4. What NOT to do

- **Do not claim Profile 7 in caps to avoid the conversion.** A device that
  genuinely decodes dual-layer and enumerates 7 gets the stream untouched and
  no arrow — that is already correct. A device that does not must not pretend.
- **Do not add a profile number to the source half** on Apple or Android. See
  §2's spelling note.
- **Do not derive the delivered profile client-side** from the source's
  profile plus "did the server say it converted". The server sends the answer;
  a second derivation is a second thing that can be wrong.
- **Do not dim anything in this state.**
- **Do not touch the caps document.** M5a's caps work (Android's
  `DolbyVisionProfileDvheDtb` → 7 mapping) already shipped.

---

## 5. Acceptance

On the two films MEDIA-BADGES-PLAN §11 names, item **6041** (Godfather, DV
P7) and item **6045** (Shawshank, DV P8):

1. **Apple TV 4K, DV output on:** 6045 shows `DV` lit with no arrow; 6041
   shows `DV → DV P8`, both halves lit, and the picture is Dolby Vision on the
   panel (the TV's own DV indicator lights).
2. **A DV-capable Android phone:** same two answers.
3. **An Android device that enumerates Profile 7:** 6041 shows `DV` lit with
   **no** arrow — it is being preserved, not converted.
4. **Either client against a server with `PLURX_DV_CONVERT=0`:** 6041 shows
   `DV → HDR10`, dimmed source half, exactly as it did before this work.
5. **Either client against a server too old to send the field:** every badge
   is exactly what it was. Check this by pointing the build at a node running
   `main` before PR #716.
6. **Forced 1080p rung on 6041:** `DV → SDR`. The badge follows the session,
   not the decision, and a transcode reports no profile.

Cell 5 is the one to actually run rather than reason about. It is the only one
that fails silently.
