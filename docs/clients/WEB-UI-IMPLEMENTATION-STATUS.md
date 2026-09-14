# Web UI implementation — progress and decisions

**Status:** building · **Updated:** 2026-09-14 · **Branch:** `codex/ui-usability`

Executes the approved [usability audit](WEB-UI-USABILITY-AUDIT.md).
The [status page](WEB-UI-STATUS.html) is the visual companion to this ledger.
Work uses a fresh agent-owned clone; no user checkout is modified. Draft PR: http://192.168.4.7:3000/noirr/plurx/pulls/317.

## Delivery ledger

| Slice | State | Evidence / next action |
|---|---|---|
| Fresh clone and current main | Done | Base f80f6d2e includes DVR PR316 |
| Title pages and series continuation | Built, visual review pending | Shared viewing surface, bounded series continuation lookup |
| Home and search | Built, visual review pending | Continuation cards, recently recorded, exact matches and library scope |
| Channel creation | Built, visual review pending | Guided steps, named selections, preview invalidation |
| Activity and analysis | Built, visual review pending | Viewer cards, separate running/queued, scoped cause grouping |
| System, libraries and settings | Built, visual review pending | Node issues, scan evidence, scope and enable advisory |
| Phone and theme fidelity | Building | Compact web chrome; actual screenshots next |
| Native iPhone / iPad layouts | Building | Series continuation, recording selector, search shortcuts, readable cards; simulator inspection next |
| Native Android phone layouts | Source review complete | Playback action row is a follow-up; device visual review not yet performed |
| Apple TV | Building | User included TV; focus-visible recording cards and reachable recording filters |
| Adversarial PR review | Queued | Exactly one review before ready |
| Findings addressed | Queued | Record each disposition |
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

No unit tests have run for this implementation yet. Draft status prevents automatic
CI jobs. Visual evidence and the exact reviewed commit will be recorded here.
