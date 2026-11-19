# Developer tab cleanup — plan for review (2026-11-18)

Owner: Claude (claude-opus-5-5, session 01BCWL9xwkar1pfxbk97l2fh).
Verified against: origin/main @ 8630ee43.

## Problem

Settings → Developer (web: `crates/plurxd/src/web/pages/settings-developer.js`,
`developerPanel()`) holds 27 cards. Most are finished features held there only
by an unrun acceptance bullet (`devGraduation(...)` "Leaves Developer when…").
Each card carries several paragraphs of readiness rows and graduation prose.
Paul's ruling (2026-11-18): an unrun acceptance protocol doesn't mean a feature
isn't done. Developer is for genuine in-development features only, shown as
name + on/off, with any detail collapsed by default. Apple
(`LiveTvDeveloperView`, `SettingsView.swift` Developer section) and Android
(`LiveTvDeveloperScreen.kt`, `SettingsScreen.kt` Developer section) have the
same problem.

## Decisions (Paul, 2026-11-18)

| # | Item | Setting key(s) | Decision |
|---|---|---|---|
| 1 | Indexing while serving | — (readiness only) | Card removed. Behavior already automatic. |
| 2 | Applied view | — | Card removed. |
| 3 | Create plan | — | Card removed. |
| 4 | Processed DV upgrade | — | Card removed. |
| 5 | Diagnostic ZIP reports | — | Card removed. Reporting stays in Help and the player. |
| 6 | Playback restart recovery | `/developer/source-recovery` | Readout moves to Cluster → Troubleshooting, collapsed. |
| 7 | Adaptive Auto quality | `playback_auto_abr` | Always on. Switch removed. |
| 8 | Fit Auto to display | `playback_display_aware_auto` | Always on. Switch removed. |
| 9 | Network priors | `playback_network_priors` | Always on (part of Auto). Switch removed. |
| 10 | CODECS on SDR master playlists | `playback_sdr_master_codecs` | Always on. Switch removed. |
| 11 | Shared libraries | `sharing_enabled` | Moves to Settings → Sharing, same key. |
| 12 | Companion remotes | `cinema_remote_control` | Moves to Remote settings, same key. |
| 13 | Background invitations | `cinema_remote_invitations` | Moves to Remote settings, same key. |
| 14 | Jellyfin client compatibility | `jellyfin_compatibility_enabled` | Moves to Integrations, same key. |
| 15 | HTTPS | https endpoints | Moves to System. |
| 16 | Dolby Vision processing | `dolby_vision_hdr_processing` | Playback toggle, same key. |
| 17 | FEL preservation (P8.1) | `dolby_vision_fel_reencode` | Playback toggle, same key. |
| 18 | Dolby Vision F+ curves | refit per-platform switches | Playback toggle(s), same keys. |
| 19 | Display-targeted HDR10-E / SDR | `dolby_vision_display_target` | Playback toggle, same key. |
| 20 | Complete-output preparation | `vod_output_preparation` | Server-wide selector in Settings → Playback, **plus a per-playback override in the player to turn it off for that playback (new)**. |
| 21 | Rolling output retention | `vod_rolling_retention` | Always on. Switch removed. |
| 22 | Content-aware encoding | `content_aware_encoding` | Always on. Switch removed. |
| 23 | Reordered VOD frames | `vod_reorder_frames` | Always on (normal x264 VOD recipe). Switch removed. |
| 24 | Mac video processing | `macos_video_processing_enabled` | Stays in Developer. |
| 25 | Mac HEVC output | `macos_hevc_output_enabled` | Stays in Developer. |
| 26 | Unverified HEVC copy | `hevc_unverified_copy` | Stays in Developer. |

(Cards 27+ — subtitle transcription/translation — are already routed to
Maintenance / Playback by `settingsRouteTab` and aren't part of this change.)

## Build

1. **Always-on items (7–10, 21–23):** the daemon stops reading the stored key
   and treats it as on. Remove the key from the settings GET/PUT DTOs
   (`crates/plurxd/src/http/system.rs`). A stored `0` is ignored. Leave the
   key in place for old clients or delete it with a one-time migration,
   whichever the store convention prefers. Remove the client toggles.
   `vod_reorder_frames` is a `u8` (B-frame count), so "always on" means a
   fixed value of 2.
2. **Moved items (11–19):** same keys and endpoints. Only the card's rendering
   location changes (`settings-playback.js`, `settings-system.js`,
   `settings-remote.js`, `sharing-management.js`, Integrations panel). Each
   moved card renders as title + one-line description + switch. Any readiness
   or detail goes in a `<details>` that starts collapsed. All
   `devGraduation(...)` prose is deleted.
3. **Removed cards (1–6):** delete the card functions. The
   `/developer/readiness` items that only fed them may stay server-side if
   Cluster/Activity uses them; otherwise remove them. Rename the endpoint so
   nothing keeps "developer" in the name for non-developer data.
4. **Developer page (24–26):** one row each (name, one line, switch), with a
   collapsed Details holding readiness rows and "Check compatibility again".
   No graduation prose. The `setdestinations` nav goes too.
5. **Item 20 per-playback override:** a player control "Prepare a reusable
   copy: on/off for this playback" sent with session create or as a session
   control. It defaults to the server-wide setting. Only "off" needs honoring
   per session.
6. **Native clients:** Apple and Android Developer screens get the same
   treatment. Sharing/remotes move to their existing Sharing/Remote settings.
   Device-local developer switches (e.g. refresh-rate match) are judged the
   same way. A client source change forces a version bump.
7. **Docs:** update `docs/` references to Developer-tab locations, plus the
   settings index row.

## Questions for the reviewer

- Does any "always on" item (7–10, 21–23) have a known open defect or a
  resource risk that makes forcing it on wrong? Name it concretely: code
  path, defect id, or doc line. A missing acceptance run doesn't count.
- Does any removed card (1–6) carry a control or operator action that would
  be lost?
- Is any key in the moved set read by something that assumes it's only
  reachable from Developer (e.g. a readiness gate, admin-only check, client
  copy)?
- Anything missed in the inventory (web, Apple, Android)?
- Is item 20's per-playback override shaped right for the session API?
