# Web UI implementation — progress and decisions

**Status:** merged as 55a2fce0; native follow-up open · **Updated:** 2026-09-14 · **Branch:** `codex/ui-usability`

Executes the approved [usability audit](WEB-UI-USABILITY-AUDIT.md).
The [status page](WEB-UI-STATUS.html) is the visual companion to this ledger.
Work uses a fresh agent-owned clone; no user checkout is modified. Draft PR: http://forge.lan:3000/noirr/plurx/pulls/317.

PR317 is now merged. New device reports are tracked in the
[native layout follow-up](NATIVE-LAYOUT-FOLLOWUP.md); the ledger below preserves
the original implementation and review history.

## Delivery ledger

| Slice | State | Evidence / next action |
|---|---|---|
| Fresh clone and current main | Done | Base f80f6d2e includes DVR PR316 |
| Title pages and series continuation | Built; fixture inspection | Shared viewing surface, bounded series continuation lookup |
| Home and search | Built; fixture inspection | Continuation cards, recently recorded, exact matches and library scope |
| Channel creation | Built; fixture inspection | Guided steps, named selections, preview invalidation |
| Activity and analysis | Built; fixture inspection | Viewer cards, separate running/queued, scoped cause grouping |
| System, libraries and settings | Built; fixture inspection | Node issues, scan evidence, scope and enable advisory |
| Phone and theme fidelity | Verified narrow layouts | Classic/Catalog/Theater at 390 × 844; light mode also inspected |
| Native iPhone / iPad layouts | Simulator review | iPhone Home and iPad Home/series inspected; resolved episode visible; widened iPad secondary labels |
| Native Android phone layouts | Source review complete | Playback action row is a follow-up; device visual review not yet performed |
| Apple TV | Simulator inspected | Home recording shortcut, activity cards and remote focus verified; larger recording text; navigation-title contrast corrected |
| Adversarial PR review | Complete | One source review of 41f6960c; four P2 findings and one completion gap |
| Findings addressed | Complete | R1–R5 dispositions below; final iOS/tvOS compilation and focused visual checks complete |
| Fast lane | Not started | Run once after review and fixes |
| Merge to main and cleanup | Queued | Merge only reviewed current head with green fast lane |

## Decisions made without waiting

- Scope expanded at the user’s request to native mobile layouts, especially
  iPhone and iPad. Apple TV is included at the user’s request.
- Keep new layouts enabled by default. Developer enablement information is
  advisory and never disables a control because a readiness condition fails.
- Preserve existing authorization and operational safety checks. They are
  not rollout feature gates.
- Reuse available APIs. New server-side discovery facets and inferred impact
  counts are not prerequisites for the approved layouts; never invent facts.
- Keep one draft main-bound PR for the shared web shell, so one adversarial
  review and one final fast-lane run cover the integrated change.
- Follow the September 13 pipeline correction and the user's instruction:
  no repeated unit tests during development. Browser inspection and syntax
  checks remain part of implementation. Report any required rerun separately.

## Verification

Local web syntax checks pass. iOS and tvOS compile with Xcode 26.6 / 17F113.
Browser inspection at 390 × 844 found and addressed clipped theater navigation
and a crowded recording indicator; the iPhone simulator has reached Home using
synthetic fixture data. Native device coverage is tracked in the [mobile audit](MOBILE-UI-USABILITY-AUDIT.md).

No unit tests ran during implementation. The candidate will be marked ready once
review fixes are committed; the single fast-lane result and merge revision are
recorded on PR317 and the live status page.

## Single adversarial review and dispositions

Reviewed original implementation commit `41f6960c`. No P1 or confirmed
security finding. Exactly one review was requested; subsequent work addresses
its findings rather than commissioning another review.

| Finding | Disposition |
|---|---|
| R1: first typed character became the suggested channel name | Generate the name after leaving content, using the complete subject |
| R2: duplicate Live TV sessions selected the first detail row | Disambiguate fallback keys within the snapshot |
| R3: native continuation could choose an older season or missing file | Match current hub progress by parent identity; compare fallback progress across seasons by watch timestamp; use an available file and refresh after watched changes |
| R4: movie/episode with no file had no explanation | Add an explicit no-media-file state; this was a completion gap, not a new regression |
| R5: Home recording colors used unsupported tone names | Reuse Saved's warning/saved mapping, including stopped-early captures |

The restored agent clone matches the pushed implementation. Temporary local
state was lost during the session; no user checkout was used to recover it.
Apple build 159 compiles for both simulator targets. No unit suite has run.

## Final visual coverage and limits

The three actual web layouts were inspected at 390 × 844 with synthetic
content; Classic was also checked in light mode. The iPad series page shows
the resolved episode and complete secondary-action labels. Apple TV remote
input reaches the Home recording shortcut and moves between activity cards
with a visible outline; the final larger recording typography was inspected.
The last navigation-title contrast adjustment compiled for both Apple targets.

`make apple-build` passes for build 159 with Xcode 26.6 (17F113);
`scripts/js-check` and `git diff --check` pass. No Rust source changed.
Fixture gaps (watch-duration fields, artwork and library previews) are not
production failures or evidence of live playback. Android remains source-only;
large-text, player, offline recovery and further native editor scenarios remain
explicit follow-ups in the mobile audit. Merge does not deploy a server image
or publish the Apple application.

## Promotion attempt

PR317 became ready at candidate `28bb4723`. Forgejo did not emit a ready
event, so reopening the unchanged PR triggered run 2081. It stopped at
`history-check` before any unit tests: the corrective client commit lacked
its required `tests/client-fixes.toml` entry. The follow-up adds that entry
and one retained channel-name regression. A second workflow attempt is
required; no test suite is being repeated. The retained channel test belongs
to the web suite and is not claimed as executed by the smaller fast lane.
