# Settings navigation and Developer — implementation status

**Status:** review addressed; promotion gate required · **Owner:** Codex implementation task · **Started:**
2026-09-09

Companion to [FEATURES.md](../FEATURES.md) (current settings behavior),
[API.md](../API.md) (settings and readiness endpoints), and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the main-bound pull
request workflow) — this page answers what is built, reviewed, and proved for
the settings navigation and Developer-page redesign.

## Delivery state — one bounded web change

| Phase | State | Evidence |
|---|---|---|
| Isolated implementation branch | complete | `codex/settings-navigation-developer`, based on Forgejo `main` at `4cef0da7` |
| Navigation and control ownership | complete | Live TV owns tuner and guide cards · Playback owns quality switching · Cluster owns transport guidance · Developer owns compatibility and experiments (as delivered; since 2026-09-28 Developer holds only unfinished features — see decisions 5 and 6) |
| Readiness layout and responsive treatment | complete | Shared nonshrinking rows · neutral static throughput support · closed native disclosures · deliberate stacking below 360 CSS px |
| Interaction and stale-response correctness | complete | Independent save payloads · returned-value badges · Live TV route fencing · card-local repaint and sibling-draft preservation |
| Portable UI structure golden | complete | 78 deterministic captures · 7,536 structural facts · settings and Developer drift accepted across all three layouts at desktop and mobile widths · no console or page errors |
| Current-reference documentation | complete | `FEATURES.md` names each control owner and advisory semantics · `API.md` names all three readiness consumers and the moved fencing action |
| Actual-app visual evidence | complete | Isolated daemon at `127.0.0.1:32419` · 1280, 880, 390, and 320 CSS px · light and dark · 640 CSS px as the 200% responsive equivalent · expanded readiness on desktop and phone |
| Adversarial review | complete | The one permitted review reported five findings; all five are addressed on the draft branch without a re-review |
| Fast lane | required | PR #228 is the authoritative record for current-head attempts and corrections; runs begin only after the one review is addressed |
| Merge to `main` | required | Requires a green Main promotion gate on the current head |

No separate unit, integration, browser, simulator, emulator, recovery,
playback, package, or smoke suite has run for this branch. The main-bound
workflow deliberately defers those suites; fast-lane attempts and any
current-head corrections begin only after the single adversarial review is
addressed.

## Actual-app evidence

The branch was compiled and served as an isolated single-node installation on
loopback-only ports. A disposable local admin account was used; no production
data or existing plurx installation was touched.

**Screenshot refresh, 2026-09-15:** the linked images now show `main` at
`39625f9fe`, rebuilt with Rust 1.97.1 and a disposable synthetic library for
the public documentation. They retain the previous image dimensions and use
noirr cinema branding. Prepared-quality readiness is now on Developer; the
existing `settings-playback-readiness-expanded-*` filenames are retained for
link compatibility. The original branch validation narrative below is a
historical record; these replacement images do not repeat those interaction
tests.

| Surface | Evidence |
|---|---|
| Developer, desktop | [1280 px dark](../img/settings-developer-1280-dark.png) · [1280 px light](../img/settings-developer-1280-light.png) |
| Developer, responsive | [880 px](../img/settings-developer-880-dark.png) · [390 px](../img/settings-developer-390-dark.png) · [320 px](../img/settings-developer-320-dark.png) |
| Developer, zoom pressure | [640 CSS px](../img/settings-developer-200-percent-equivalent.png), the layout-equivalent viewport for a 1280 px window at 200% browser zoom |
| Control ownership | [Playback quality](../img/settings-playback-1280-dark.png) · [Live TV tuner and guide](../img/settings-live-tv-1280-dark.png) · [Cluster recovery](../img/settings-cluster-recovery-1280-dark.png) |
| Expanded readiness | [1280 px](../img/settings-playback-readiness-expanded-1280-dark.png) · [390 px](../img/settings-playback-readiness-expanded-390-dark.png), including long evidence and status pills |

Manual actual-app exercises also confirmed that:

- saving prepared quality changes updates only that card and renders the
  returned enabled/disabled state;
- changing and saving the guide source preserves an unsaved tuner-address
  draft; and
- leaving Live TV while a saved-configuration check is pending keeps the
  destination route on Developer when the response completes.

The in-app browser harness cannot set browser chrome zoom directly. Its 640
CSS px viewport exercises the same responsive width as a 1280 px window at
200%; the evidence names this limitation instead of claiming a native zoom
gesture was performed.

## Promotion-lane corrections

The main-bound lane began only after the sole adversarial review was addressed.
Its current-head attempts found and closed three branch-owned evidence gaps;
one unrelated test flake and one runner-capacity refusal were handled without
changing product behavior.

| Signal | Disposition |
|---|---|
| Retained decoder copy and history evidence | Restored the concrete producer-health explanation and recorded explicit `regressions.d` coverage |
| Intended settings DOM drift | Regenerated the portable golden: 78 captures and 7,536 structural facts with no console or page errors |
| Host-dependent decoder path counts | Render each measured path set inside one stable code container; the golden now records the same evidence shape on macOS and Linux |
| Rust deadline test | One unrelated timing assertion failed after 2,147 passes; the unchanged Rust suite passed on the next current-head attempt |
| VOD browser runner capacity | Job stopped before test execution at its 45 GiB disk preflight; idle `gha-lab4-general-02` was pruned with Docker's unused-data cleanup and the repository's bounded Cargo-cache pruner, leaving 48 GiB free |

PR #228 remains the authority for the final current-head Main promotion gate
and merge result; this document records causes and repairs without predicting a
result that does not yet exist.

## Sole adversarial review

The draft pull request received exactly one adversarial agent review. The
author addressed every finding directly; no second review or approval pass was
requested.

| Finding | Disposition |
|---|---|
| Off-route Live TV writes left the shared Settings cache stale | Cache the returned snapshot before suppressing route-local DOM work |
| Guide save and refresh failures could target a detached error node | Re-resolve the owning error slot after each awaited request |
| An older quality-save response could erase a newer edit | Track each card's draft revision and leave newer edits visible and unsaved |
| Guide readiness could be mistaken for validation of an unsaved draft | Label the persistent status and response with the saved source and look-ahead |
| Visual evidence omitted expanded readiness | Add desktop and phone captures with long evidence and nonshrinking status pills |

## Decisions — preserve operator choice

1. **Readiness remains advisory.** Missing, unmet, stale, failed, or
   unobservable evidence will never disable a toggle, reject its Save, or
   silently replace a saved choice. Existing input validation, authorization,
   generation conflicts, and real operation failures remain intact.
2. **Every control has one owner.** Live TV owns tuner, enablement, fencing,
   and guide configuration · Playback owns prepared quality switching ·
   Cluster owns automatic transport-recovery guidance · Developer owns
   compatibility controls and experiments that are not yet fully active or
   tested. The decoder controls were Developer's until 2026-09-28; they now
   belong to Playback → Advanced server delivery (decision 6).
3. **The prototype is a reference, not a base.** The implementation starts
   from current Forgejo `main`; only relevant ideas are ported from the local
   prototype, and unrelated work in the original checkout is left untouched.
4. **No feature gate is added.** Experiment toggles stay explicit and
   editable. Developer explains what safe enablement needs and which evidence
   the daemon can currently observe.
5. **Developer is a waiting room, not a home (Paul, 2026-09-28).** A fully
   active option has no reason to be in Developer. A feature enters Developer
   while it is not fully active or not fully tested, and every Developer card
   says what it is waiting on. When that lands the card leaves: to its proper
   settings section if a permanent enable/disable makes sense, otherwise the
   toggle comes out and the feature is simply on. See
   [the Developer lifecycle](#developer-lifecycle--every-card-graduates).
6. **Chapter thumbnails and both decoder controls graduate as permanent
   Playback settings (Paul approved graduating them, 2026-09-28; the
   destination is this change's call and Paul can overturn it).**
   - *Chapter thumbnails* → Settings → Playback, after the player defaults
     and the browser-local card.
     A permanent switch makes sense: every first open of a film costs an
     ffmpeg seek per chapter, and off is how an operator stops that on a
     CPU-poor node. Default unchanged (on).
   - *Verified decode artifacts* → Playback → Advanced server delivery.
     Turning it on or off renames cached transcodes on covered paths and
     takes a restart; the decoder plan (§9) treats it as the operator's
     fleet-wide upper bound. That is a lasting operator decision, so it
     stays a switch rather than becoming "just on". Default unchanged (off).
   - *Automatic decode recovery* → Playback → Advanced server delivery. Each
     recovery spends CPU on a software decode, and an operator containing a
     misbehaving GPU or driver needs to be able to say yes or no to that;
     the plan again treats it as operator policy. Default unchanged (off),
     so no settings migration is needed and no stored choice moves.
   Making either decoder control default on is the alternative. It would
   need a migration that keeps an explicit stored `0`, and it is Paul's to
   choose.

## Developer lifecycle — every card graduates

Paul's rule, 2026-09-28: *"If it's a fully active option, then there's no
reason to have it in the dev tab. If it's still not fully active or tested or
whatever, then it goes in the dev tab. But everything in the dev tab
eventually moves out of it and into a proper place in settings if it makes
sense to have an enable/disable for it permanently, else they just come out
of the dev tab when the feature is done and ready to just use."*

It sits beside the standing rule that there are no feature gates: Developer
entries carry advisory readiness and safety information that never gates
enablement.

**Clarification, Paul, 2026-09-30:** an explicit Developer switch belongs
only where manual enable/disable has a meaningful purpose. Unfinished
implementation or acceptance alone is not a reason to add one. Internal
correctness fixes and automatic infrastructure can show read-only facts and
remaining evidence; advisory readiness still never disables a real control,
rejects Save or overrides its saved choice. A finished meaningful control
graduates to its proper settings section; an automatic feature's advisory
card leaves Developer when its acceptance is complete. K-06's clock
measurement retains its accepted no-switch design under this clarification.

| Stage | What happens | Where it is recorded |
|---|---|---|
| Enters | A feature that is not fully active or not fully tested gets a Developer card: its explicit switch (or, for automatic behaviour, an explanation), advisory readiness, and one **Leaves Developer when … Then …** line | `devGraduation(waitingOn, then)` in `web/pages/settings-developer.js` |
| Waits | The line names the plan acceptance or board evidence still owed, not a vague "qualification" | The owner plan and its workboard row |
| Graduates, permanent switch | The switch moves to its proper settings section; the Developer card is deleted in the same change | The plan's execution log and this page's audit |
| Graduates, no switch | The toggle is removed and the behaviour becomes the default; a server-side default change carries a settings migration that keeps an operator's explicit choice, plus a test | Same |

Where the line says **Paul chooses**, the destination is his call at
graduation time. `tests/web/settings-sections.test.js` fails when a card
rendered by `developerPanel` does not carry the line.

### Audit, 2026-09-28

Decision key: **(a)** not done, stays and states what it waits on; **(b)** done,
graduates in this change; **(c)** unclear, stays and is listed for Paul.

| Card (web Developer unless marked) | Switch | Owner row / plan | Owner status | Evidence still owed | Decision |
|---|---|---|---|---|---|
| Enable Live TV (web, and native Developer on Android, iOS and tvOS) | `live_tv_enabled` | L-02, L-03 ([HDHOMERUN-LIVE-TV-PLAN](HDHOMERUN-LIVE-TV-PLAN.md)) | merged: code; acceptance open | L-02's leader-restart, cold/warm-start (L6) and scratch-fault (L9) fleet prompts; L-03 M2 capacity offer and caption-positive M4 | (a) stays → Settings → Live TV; the native cards print the web's line word for word and, like the web card, draw every `/live-tv/readiness` row as Met / Not met beside the switch, advisory only |
| Durable cluster work | `vod_index_cluster_cache`, `cache_produce_mins` | [DURABLE-WORK-QUEUE-STATUS](../cluster/DURABLE-WORK-QUEUE-STATUS.md) | merged M1–M3, E0–E3 (`82df7f59e`); deployed in `55aa430fd` to all four nodes on 2026-09-27 | none: the plan calls the queue infrastructure and OPERATIONS says there is no fleet receipt to obtain | **(b) graduated: card removed.** Its two switches were copies of the permanent ones in Analysis (`an-enabled`) and Maintenance (pre-transcoding cadence) |
| Cluster media placement | `cluster_media_pool_enabled`, `cluster_session_takeover_enabled` | [CLUSTER-MEDIA-POOL-PLAN](../cluster/CLUSTER-MEDIA-POOL-PLAN.md) | built P0–P8 | physical-device corpus; ten-second takeover budget (§8.8–§8.9) | (a) stays → Settings → Cluster |
| Local catalogue reads | `bounded_replica_reads` | K-04 | merged: M0–M3 server; web read-after echo and on-by-default preference shipped in `5ba02212f`, deployed | §5.1 lab readout, rolling-upgrade check | (a) stays → Cluster, or removed (Paul's choice) |
| Shared storage budgets | storage domains | [DURABLE-WORK-QUEUE-STATUS](../cluster/DURABLE-WORK-QUEUE-STATUS.md) | as durable work: deployed | none | **(b) graduated: moved to Settings → Libraries** beside the roots it names; it is the only editor for the mapping, so it moves rather than goes |
| CODECS on SDR master playlists (added 2026-10-04) | `playback_sdr_master_codecs` (`playback.sdr_master_codecs`), default off | S-10 ([HONEST-MASTER-PLAYLIST](../streaming/HONEST-MASTER-PLAYLIST.md#the-sdr-codecs-developer-switch)) | SDR `CODECS` implemented; off restores the pre-S-10 master | Apple TV and iPhone device check that every SDR variant is still offered with `CODECS` printed (§5.4) | (a) stays → default on and the switch is removed |
| Unverified HEVC copy | `hevc_unverified_copy` | [HEVC-COLOR-CORRUPTION-RCA-AND-FIX](../streaming/HEVC-COLOR-CORRUPTION-RCA-AND-FIX.md) | proof-before-stripping containment `87ca67c0e` deployed in `55aa430fd` | complete-scan proof for every HEVC title VOD copies | (a) stays → removed, or Playback escape hatch (Paul's choice) |
| Jellyfin client compatibility | `jellyfin_compatibility_enabled` | [Jellyfin build](../clients/JELLYFIN-COMPATIBILITY-BUILD.md) | J0/J1 integrated; J2 under implementation | frozen-candidate catalog, artwork, playback, tracks, watch and cluster qualification | (a) stays → Settings → Integrations |
| Live TV deinterlace cadence | `live_tv_deinterlace_output` | S-08 | merged: M1–M4 | M5 media1 QSV/VAAPI qualification | (a) stays → Settings → Live TV |
| Portable cluster backup | `backup_*` | K-01 | merged: M1–M4; amd64 container smoke passed in run 3205 | M4 arm64 in-lane leg; M5 loss drills, physical restore, RPO/RTO | (a) stays → Settings → Cluster |
| Seek scratch accounting | none (automatic) | [SEEK-SCRATCH-RESERVATIONS-STATUS](../streaming/SEEK-SCRATCH-RESERVATIONS-STATUS.html) | merged and deployed | physical acceptance | (a) stays → card removed |
| Cluster clock observation (added 2026-09-30) | none (automatic; meaningful-control clarification above) | [K-06 measurement](../cluster/CLOCK-SKEW-MEASUREMENT-IMPLEMENTATION.md) | measurement implementation; release review and gate pending | identified measurement fleet receipt, separate enforcement and its acceptance | (a) stays → read-only diagnostics move to Settings → Cluster |
| Adaptive Auto quality | `playback_auto_abr` | A-04, A-05 | A-04 blocked: incomplete D3 matrix; A-05 unclaimed | Safari, HDR and Apple/Android physical traces; native controllers | (a) stays → Playback toggle, or removed (Paul's choice) |
| Fit Auto to display | `playback_display_aware_auto` (`playback.display_aware_auto`) | [Combined Auto plan](../streaming/DISPLAY-AWARE-AUTO-QUALITY-PLAN.md) | source implementation in progress; qualification open | source-grade worker proofs, display-fit candidates and physical runtime recovery | (a) Playback toggle if permanent choice remains useful, otherwise removed |
| Prepared quality handoff | `prepared_quality_handoff` | [QUALITY-SWITCH-CONTINUITY-BUILD](../playback-control/QUALITY-SWITCH-CONTINUITY-BUILD.md) | in execution | M2-Android, M1-web, M3; physical-client fleet receipt | (a) stays → Settings → Playback |
| Second player in this browser | browser-local | same | same | same | (a) stays → moves with prepared handoff |
| Refuse a subtitle segment that failed | `subtitle_not_ready_503` | [SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT](../clients/SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT.md) | open | AVPlayer, Media3 and hls.js observations | (a) stays → toggle removed, refusal becomes default |
| PGS subtitle overlay | `pgs_overlay` | [APPLE-PGS-OVERLAY-ACCEPTANCE](../clients/APPLE-PGS-OVERLAY-ACCEPTANCE.md) | open | Apple and Android physical acceptance | (a) stays → Settings → Playback |
| Parallel playback subtitle ranges | none (automatic) | K-09 | merged: M0–M5 | fleet evidence of a peer range exchange | (a) stays → card removed |
| Stored PGS tracks | `subtitle_stored_sources` | K-09 | merged: M0–M5 | fleet and device evidence | (a) stays → Settings → Maintenance |
| Share stored subtitle tracks | `subtitle_cluster_sources` | K-09 | merged: M0–M5 | fleet evidence | (a) stays → Cluster, or removed (Paul's choice) |
| Backfill subtitle tracks | `subtitle_backfill` | K-09 | merged: M0–M5 | fleet evidence | (a) stays → Settings → Analysis |
| Chapter thumbnails | `chapter_thumbnails` | [WATCH-VIEW-LAYOUT](../clients/WATCH-VIEW-LAYOUT.md) | built; on by default; in the deployed main | none the plan names; the fleet observation its Developer line waited on was never recorded and was waived by Paul's approval | **(b) graduated to Settings → Playback** (Paul, 2026-09-28; decision 6) |
| Verified decode artifacts | `decoder_health_qualified_artifacts` | [DECODER_SELECTION_RECOVERY_STATUS](../DECODER_SELECTION_RECOVERY_STATUS.md) | M0–M7 merged | fleet qualification, which the plan calls optional | **(b) graduated to Playback → Advanced server delivery**, default off (Paul, 2026-09-28; decision 6) |
| Automatic decode recovery | `automatic_decoder_recovery` | same | M0–M7 merged; off by default | a matched hardware/software retained pair, which is advisory | **(b) graduated to Playback → Advanced server delivery**, default off (Paul, 2026-09-28; decision 6) |
| Android: Match television refresh rate | display cadence | D-01 | merged: M1–M3 | M0 three-TV measurement, M4, M5 HDMI | (a) stays → Paul chooses: Settings → Playback, or removed |
| Android: Prepared replacement | device-local | as prepared handoff | as above | as above | (a) stays → moves with prepared handoff to Settings → Playback |
| Apple: Bounded pause/resume | device-local | M4 item 4 (hold/resume barriers) | built | matched physical Apple TV latency | (a) stays → Paul chooses: Settings → Playback, or removed |
| Apple: Prepared quality handoff | device-local | as prepared handoff | as above | as above | (a) stays → moves with prepared handoff to Settings → Playback |
| Android: Release startup profile | none (advisory status) | D-03 M10 ([ANDROID-CREDENTIAL-EXPOSURE-AND-RELEASE-BUILD](../clients/ANDROID-CREDENTIAL-EXPOSURE-AND-RELEASE-BUILD.md) §5.10) | tooling merged (build 136) | the Baseline Profile measured on the Lenovo release APK: five-run cold-start and first-frame medians with and without it (§6) | (a) stays → entry removed; the profile ships in every release build and has no switch |
| Android and Apple: Library channels, Recording, HDHomeRun Live TV, Programme guide | server settings | web already moved these to Settings → Live TV | — | none: finished settings | **(b) moved in PR #602** (Paul: "fix it") to a native Settings → Live TV screen on Android, iOS and tvOS, in the web's order, reached like Developer; saves and readiness unchanged. The Live TV enable stays in native Developer, as on the web |

Two graduated on 2026-09-28, after the PR's one adversarial review showed
durable cluster work deployed with no fleet receipt to wait for: the Durable
cluster work card was removed (its switches already live permanently in
Analysis and Maintenance) and the Shared storage budgets editor moved to
Settings → Libraries. No other card's plan or board row records complete
acceptance evidence. The server still reports the `durable_cluster_work`
readiness item at `GET /api/v1/developer/readiness`; the web no longer renders it.
Three more graduated the same day at Paul's word: chapter thumbnails to
Settings → Playback, and verified decode artifacts and automatic decode
recovery to Playback → Advanced server delivery (decision 6). Their saves,
defaults and advisory rows are unchanged; they no longer carry a
graduation line because they are no longer in Developer.
Since PR #602 every native Developer row prints its
**Leaves Developer when … Then …** line too — Android: Enable Live TV, Match
television refresh rate, Release startup profile, Prepared replacement; Apple:
Enable Live TV, Bounded pause/resume, Prepared quality handoff.
`LiveTvSettingsPlacementTest` (Android) and `LiveTvTests` (Apple) pin which
screen draws each server Live TV card and that every native Developer entry
carries its line.

## Remaining limits — evidence must name what it cannot prove

- Daemon readiness cannot certify physical first-frame handoffs, fleet memory
  stability, or measured fallback interruption when those facts are not
  reported. The UI must label them **not observable**, not pass or fail.
- Static throughput support proves the reporting path exists; it does not
  prove that a particular stream, client, or network has enough throughput.
- Guide readiness describes saved configuration. It must not be presented as
  validation of an unsaved XMLTV draft.
- The final current-head gate and merge result remain on PR #228; historical
  prototype observations are not evidence for this branch.

### Video-quality additions, 2026-10-03

The [video-quality programme](../performance/VIDEO-QUALITY-PROGRAM.md) adds two
unfinished-feature cards in PR #766. Both switches save the requested value
regardless of readiness. Their runtime validation still rejects a stale source,
invalid fragment or unusable encoder result; that never changes the saved choice.

| Card | Saved key | Advisory readiness | Graduation |
|---|---|---|---|
| Measured per-title encoding | `transcode.content_aware_encoding` | Selected encoder, explicit rate override, software quality capability and scorer availability; the current implementation measures bounded SDR software-x264 samples. | Record representative-title quality/cost evidence for supported builds; move to Playback if a permanent choice remains useful. |
| Reordered VOD frames | `playback.vod_reorder_frames` (0 or 2) | Software x264 support and remaining compression/client presentation evidence. Other families retain their current recipes. | Record the compression and client matrix; remove the switch if this becomes the normal recipe, otherwise move it to Playback. |

Next-episode metadata preparation uses the existing autoplay preference.
HLS acknowledgement batching is part of normal delivery ownership. The plain
VAAPI HDR graph publishes its measured capability through system diagnostics.
