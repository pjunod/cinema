# Playback surface review response — the presenter gives up the player

**Status:** done — review answered and contract v2 produced 2026-09-13 · **Answers:**
[PLAYBACK-SURFACE-CONTRACT-REVIEW.md](PLAYBACK-SURFACE-CONTRACT-REVIEW.md)
(PR #274 @ `c2deda1`) · **Produces:** contract v2 in
[PLAYBACK-SURFACE-CONTRACT.md](PLAYBACK-SURFACE-CONTRACT.md)

The review's verdict was *reject*, on one thesis: the proposal transferred
recovery authority to presentation. That was true of v1 and it was a design
error, not a wording one — a presenter that pauses the player is a recovery
actor, and the doc's own first sentence said the overlay should be a
projection. v2 removes every side effect from the presenter; the recovery
owner (the client's existing ladder) stops the player when it declares
exhaustion and the presenter renders that. Most of the eleven findings fall
out of that one change; the rest are corrections taken as written or, in
three places, narrowed with evidence.

Every finding was re-verified against the source before it was dispositioned.
"Taken" means the contract changed as the review asked; "narrowed" means the
finding holds with a smaller scope than stated; "refuted" means the source
does not support it.

| # | Severity | Disposition | What changed in v2 |
|---|---|---|---|
| 1 | blocker | **taken** | `stalled` is gone. `exhausted` is raised only by the recovery owner after its ladder and budgets are spent and after it has stopped the player; a readiness deadline with rungs left is `recovering` (§3.0, §3.1, source row 14). Keep waiting is defined per client as the owner's own re-arm (web `armStall`, Apple `retryAfterPlaybackFailure` — which resets `recoveryReopenBudget` and only that, as the review says — Android `stallReopenBudget` + `sessionlessStallRecoveryUsed`), and the doc says so rather than implying a universal budget reset. |
| 2 | blocker | **taken** | Faults carry two identities, `attached` (media generation) and `intent` (viewer request); a fault with an intent is retired only by that intent settling or being superseded, never by another picture moving (§3.1, §3.4). Evidence is each client's *existing presentation proof* — web `playbackProgressTick.moved` with its frame-counter requirement, Apple the periodic observer gated on `!isChangingStream` plus first-frame proof for an unestablished item, Android `presentedVideoFrame`/`realPosition()` on the current epoch — not a bare 500 ms clock. AirPlay/PiP are declared by platform flags; a hidden page samples nothing. Refusals hls.js recovered from within one attach are retired by the recovery (`FRAG_LOADED`), not by the next fatal. M6 commit/rollback ownership is in the implementation doc §4.6. |
| 3 | blocker | **taken, by removal** | `surfaceHold` no longer exists; there is nothing for the presenter to acquire or release. The web owner's exhausted sites call `pausePlaybackInternally` + `stopPlayerTimers`, which is the owner's own cancellation path, and the control snapshot is untouched because `wantsPlayback` is untouched. After a disagreement nothing is released because nothing was held (§3.2). |
| 4 | blocker | **taken** | §3.3 is now an ordered table with first-match precedence and explicit contexts (start · attached · pending change). Change-create 503 → `refused` (row 7) beats start-create 503 → `preparing` (row 6). Successor abandonment is log-only (row 18). 409 is `vod_source_rescan_required` (row 4), not device supersession. `parseStreamFailure` grows `position_ms`. Terminal wording keeps the transport-class exception (row 1, citing `PlayerController.swift:5327` / `Controller.kt:657`). 410 plays out buffered media as `recovering` and becomes `stopped` when the owner stops (row 9, D1's shape). Create-retry deadline and identity, the shared hls.js retry budget with position preserved, and a finite-timeline rule for `BEHIND_LIVE_WINDOW` are written into M5's constraints (§7). Added sources: 401/403 (row 3), the VOD refusal family `vod_disabled` / `vod_index_pending` / `vod_engine_unattested` / `vod_source_unsupported` / `vod_transcode_unavailable` / `vod_subtitle_burn_unavailable` (rows 4–6, from `hls.rs:3755-3785`), decision/create timeouts and cancellation during a change (row 7), Android target-deadline exhaustion (row 2). |
| 5 | should-fix | **taken** | §2.2 now states that AVFoundation surfaces only `errorStatusCode` for media refusals and the Apple adapter classifies on the code plus the session status poll — it never claims a body it cannot have. `PlurxAPI.check` gains `.refused(status:code:message:position_ms:)` **and keeps `.http(status)`** for bodiless answers so `isSessionExpired` (`AppModel.swift:288`) and existing matchers keep working; the implementation doc §4.3 pins that with a test. |
| 6 | should-fix | **taken** | Actions are a property of the fault (`keep_waiting · retry · close · force_transcode · sign_in`), set by the owner; a demoted fault keeps its actions and its banner does not expire while an action is valid (§3.1, §3.2). The input contract's `failed` state is "a blocking surface with actions is up" and the surface × class × input cross-product is pinned by a test (§4, implementation §5.4). |
| 7 | should-fix | **taken** | Fixture cases are ordered event sequences with timestamps, identities, `player_stopped`, actions and expected surfaces at named times (§3.5); a blocking class without `player_stopped` is a fixture *error*, not a surface. `refused_progress_ms` is defined as continuous presenting on `attached`. Ledger rows carry `session_id` and `attempt`, not only the client-local generation (§5). |
| 8 | should-fix | **taken** | Scope is declared in §7: the web Library Channels path is *in* (it shares the renderer); `LibraryChannels.swift`, `OfflinePlayerScreen.kt`, `OfflineDownloadManager.swift` and the readers are out; Apple offline shares the model with `exhausted` unreachable. The fence scans writes only (assignment, `+=`, bulk assign, class add/remove/toggle — `classList.remove` included), adds `playbackNotice` to the Android patterns, and explicitly does not police *reads* of raw errors in ladders and diagnostics; positive and negative fence fixtures are required (§4, implementation §5.5). |
| 9 | should-fix | **narrowed** | Correct on both counts: `live_presentation_removed` (`transcode.rs:17115`) means every HLS session is VOD and Android seeks those natively, so the copy-HLS variant is retired; and `bounded_progressive_media_origin` (`stream.rs:55-70`) falls back to the *requested* start after its budget, which hides rather than removes the mismatch. §6.1 is rescoped to progressive remux, records the Google TV run as inconclusive (paused transport; the sessionless-second-stall sentence, not the target-deadline one; no `playback_target_timeout` captured), and M6 is gated on a measured achieved-origin vs first-frame delta, not on the reading. The prepared-successor compensation (`successorAttachPositionMs`) is cited as the shape of the eventual fix, not as proof of the defect. |
| 10 | should-fix | **taken** | M2's acceptance names the delivery (a fresh VOD transcode that never readies — the `shouldBoundFreshStartReadiness` path, walking the ladder first) and the exhausted rung; M3's disagreement test uses `realPosition()` advance on the same epoch, not `onIsPlayingChanged`; M1's injected fatal specifies ≥ 20 s buffered; M4's allowed outcomes are enumerated per recipe in the implementation doc §7, and a `surface_disagreement` passes only when its ledger row names a documented exception. |
| 11 | should-fix | **taken, two narrowed** | §2 corrected bullet by bullet: 35 web call sites; the fatal → buffer → reopen sequence is reachable, not universal; malformed refusal bodies are not recorded; VOD/direct seeks are native and paint nothing (the false claim); `fail()` is the writer that lacks a pause, the five terminal writers pause first, failover readiness errors are swallowed; the readiness deadline is scoped to `seekWhenReady` and `shouldBoundFreshStartReadiness` (fresh VOD transcodes included — my "never a cold transcode" was wrong); post-attach sites clear `failed` not `playbackError`; established-HDR retry precedes terminal on both natives; the Android watchdog restart needs foreground and no pending seek; the "no test" claim is narrowed to Android; the photo proves the overlay and its transparency, and the watchdog chain is stated as plausible, not logged. **Narrowed:** 2.3.6 — the cited `AppleClientTests` assertion is Apple's; the Android claim stands as narrowed. 2.3.7 — the photo was never offered as proof of the chain; the sentence now says what it proves. |

Two things the review did not ask for but v2 does, because the ownership
change made them necessary: the recovery owner's *one* new obligation (stop
before raising a blocking fault) is written down as the only behaviour change
in M0–M4, with the five Apple sites that already satisfy it named; and the
silent black-picture exhaustion the review surfaced under finding 2 is given
a surface (row 15) rather than left as a case the evidence rule merely
tolerates.

Recommended disposition of the review's verdict: the reject was earned by v1
and is answered by v2. The implementation doc is written against v2 and
carries the review's acceptance corrections as its own acceptance checks.
