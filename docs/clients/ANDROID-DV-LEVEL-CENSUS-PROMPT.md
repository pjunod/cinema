# Android Dolby Vision level census — what each device's decoder actually admits

**Status:** open · **For:** a session with the Android devices · **Written:**
2026-09-26 · **Decides:** whether a Dolby Vision *level* check is worth
building server-side

Companion to
[../streaming/ANDROID-DV-CONVERSION-RCA-AND-FIX.md](../streaming/ANDROID-DV-CONVERSION-RCA-AND-FIX.md)
(why a capable Lenovo received HDR10) and
[../streaming/DV-STRIP-TRIAL-PROBE.md](../streaming/DV-STRIP-TRIAL-PROBE.md)
(the strip probe). This one gathers the evidence that decides a third,
unbuilt change.

**Why this exists.** plurx promises Dolby Vision to a client by *profile*
only: `CapsPolicy.kt:265-268` maps the decoder's `CodecProfileLevel.profile`
constants to `dvprofile=4,5,8` and drops the `level`; the server matches
`dolby_vision_profiles.contains(profile)` (`plurx-core/src/playback/mod.rs`)
and never compares the file's `dv_level` to anything. Silo's server checks
the file against the client's maximum level (pixel rate, width, bitrate).
That check only pays off if some device we own reports a level *below* what
our 4K discs need (UHD Blu-ray Profile 7 is level 6–7: 3840×2160 at 24–30 fps,
≤ 70–130 Mb/s high tier). An Apple TV 4K is level 9 (UHD60) and gains
nothing; a tablet whose decoder stops at FHD levels would today be promised
4K DV Profile 8 and fall into recovery. Nobody has looked.

Paste this whole document into the session with the devices. Report per
device; do not merge devices into one verdict.

## What you need

- Every Android device that runs the plurx client: at minimum the Lenovo
  tablet (TB322FC), any Android TV / Google TV box, and any phone.
- `adb` to each, or the ability to sideload one small APK.
- Ten minutes per device.

## Procedure

**1. Dump the Dolby Vision decoders and their profile/level pairs.**

Preferred, no build needed — the `dumpsys` route:

```bash
adb -s <serial> shell dumpsys media.player | grep -i -A20 "dolby"   # some vendors
adb -s <serial> shell dumpsys media.codec 2>/dev/null | grep -i -B2 -A30 "video/dolby-vision"
```

If neither prints profile/level pairs (most don't), run this in a
`MainActivity` of a throwaway app, or in the plurx client's Developer
screen if one is handy, and copy the log lines:

```kotlin
val list = MediaCodecList(MediaCodecList.ALL_CODECS)
for (info in list.codecInfos) {
    if (info.isEncoder) continue
    for (type in info.supportedTypes) {
        if (type != "video/dolby-vision") continue
        val caps = info.getCapabilitiesForType(type)
        for (pl in caps.profileLevels) {
            Log.i("DVCENSUS", "${info.name} ${info.isHardwareAccelerated} profile=0x${pl.profile.toString(16)} level=0x${pl.level.toString(16)}")
        }
        val vc = caps.videoCapabilities
        Log.i("DVCENSUS", "${info.name} maxW=${vc.supportedWidths.upper} maxH=${vc.supportedHeights.upper} bitrate=${vc.bitrateRange.upper} fps4k=${vc.getSupportedFrameRatesFor(3840,2160)}")
    }
}
```

```bash
adb -s <serial> logcat -d -s DVCENSUS
```

**2. Decode the constants.** Profiles: `0x20` = Profile 5 · `0x80` =
Profile 7 · `0x100` = Profile 8 · `0x10` = Profile 4. Levels: `0x1` HD24 ·
`0x2` HD30 · `0x4` FHD24 · `0x8` FHD30 · `0x10` FHD60 · `0x20` UHD24 ·
`0x40` UHD30 · `0x80` UHD48 · `0x100` UHD60 · `0x200` UHD120 · `0x400`
8K30 · `0x800` 8K60. Record the *highest* level per profile per decoder.

**3. Record what the display says.** `Display.getHdrCapabilities()`
supported types (`HDR_TYPE_DOLBY_VISION` = 1) and, on TV boxes, the
current output mode (`adb shell dumpsys display | grep -i "mode\|hdr"`).

**4. One playback per device**, the same 4K Profile 7 title on each, Auto
quality; note the overlay's Method, delivered range and DV profile, and
whether it played, stalled or errored in the first 60 s.

## Report format

| Device (model, Android version, plurx versionCode) | Decoder | HW | Profiles → highest level | maxW×maxH · max bitrate · fps@4K | Display DV | 4K P7 title result |
|---|---|---|---|---|---|---|
| Lenovo TB322FC · 14 · 129 | `c2.dolby.decoder.hevc` | yes | P5 → ?, P8 → ? | | | |

## What decides it

- Every device reports UHD24 (`0x20`) or higher on Profile 8 → **no level
  check needed**; the profile match is sufficient for this fleet, and
  the finding is filed in this document as closed.
- Any device reports only FHD levels on Profile 8, or a `maxW×maxH` below
  3840×2160 → **build it**: Android sends `dvlevel=<max per profile>`
  beside `dvprofile`, the server keeps the file's `dv_level` (already
  stored) and refuses to *promise* DV when the file's level exceeds the
  claim, falling to the existing strip/reencode ladder with a reason that
  names the level. Silo's table of level bounds
  (`internal/playback/plan_v3.go:1834-1860`) is a fair reference for the
  server side when `dv_level` is missing from an old scan.
- A device that *plays* the 4K P7 title while *reporting* a lower level is
  also a result: the level constants are vendor-filled and sometimes lie
  downward, which is an argument for advisory-only use of the claim.
