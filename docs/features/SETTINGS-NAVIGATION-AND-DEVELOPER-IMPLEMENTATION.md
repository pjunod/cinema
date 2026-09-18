# Settings implementation — build the approved navigation and Developer layout

**Status:** open · approved design, production implementation pending ·
**Owner:** Sol implementation task · **Written:** 2026-09-09

Companion to [FEATURES.md](../FEATURES.md) (current behavior),
[API.md](../API.md) (settings and readiness endpoints), and the separate
[review remediation plan](../reviews/WEEKLY-ARCHITECTURE-SECURITY-IMPLEMENTATION.md)
(underlying security and playback corrections). This document is the complete
build contract for the web settings redesign Paul approved. It can be handed
to an implementation task without the conversation or a running preview.

## 1. Deliver the layout in one bounded UI change

**Paul's final constraints.** Finish this as a focused UI delivery. Do not build a
new settings framework, qualification service, policy engine, certification
workflow, or staged feature rollout. Readiness is advisory information: missing,
unmet, stale, or unobservable requirements must never disable a feature toggle,
reject its Save, or silently override an enabled choice. No receipt, benchmark,
fleet approval, or completed test sweep may become a new software enablement
prerequisite. An explicit enable/disable choice and readable requirements/status
in Developer are the maximum machinery needed for experiments. Keep the approved
regular-menu destinations for ordinary controls. Do not expand CI/CD for this
work; use the current fast lane exactly as documented.

The web administration UI should put ordinary feature configuration in its
normal settings section and make Developer a readable home for compatibility
controls and experiments. Paul also explicitly approved improving the expanded
**Readiness and device qualification** section: badges must not wrap into
vertical text, and evidence must be laid out consistently.

This is a web settings change. It does not implement the native Live TV layout
plan, library channels, or the behavior fixes from the review. It changes no
feature defaults, authorization, server configuration key, or protocol version.
Do not add backend enablement gates. If current code independently gates an
explicitly enabled feature on readiness evidence, identify it for removal in the
review task; do not add a cosmetic override that leaves the backend gate intact. Moving a control into Playback does not certify its feature
as production-qualified.

### 1.1 Treat the existing local implementation as a prototype

The design session edited these six files locally:

- [index.html](../../crates/plurxd/src/web/index.html).
- [settings-sections.test.js](../../tests/web/settings-sections.test.js).
- [page-read-budget.test.js](../../tests/web/page-read-budget.test.js).
- [cluster-membership.test.js](../../tests/web/cluster-membership.test.js).
- [FEATURES.md](../FEATURES.md) and [API.md](../API.md).

Those edits were not committed, merged, deployed, or reviewed through the
main-bound PR process. They were made on
`codex/redact-hiqlite-cancelled-responses`, whose HEAD was
`4d05857f07e7792c4c6e80ce237074b9567ecfb8`; that is not an appropriate assumption
about the current implementation base. The checkout also contains unrelated
CI, documentation, and validation work. Preserve it.

The optional local reference folder is `tmp/dev-settings-refresh/`. It contains
`index-before.html`, a generated `preview.html`, a sample-data preview generator,
and test logs. These are local design aids, not tracked dependencies. The
preview used real renderer functions and CSS with mocked API responses; it did
not exercise a production daemon or tuner. Reconstruct the preview if absent.
Do not ship its sample data, API mocks, preview banner, or localhost server.

Start from current intended `main` in an isolated checkout. Inspect the local
prototype diff if available, port only relevant changes, and resolve newer
implementation changes by the contracts below. Do not copy the entire dirty
working tree or treat prototype tests as current-head qualification.

## 2. Put each control in one predictable location

Keep all existing settings sections and their relative order, inserting Live TV
in Content after Metadata. Retain the Developer route for bookmarks.

| Group | Section / route | Contents relevant to this change |
|---|---|---|
| Content | Libraries · Metadata · **Live TV** (`#/settings/livetv`) | HDHomeRun configuration, enable/disable, readiness, owner recovery, programme guide |
| Playback | **Playback** (`#/settings/playback`) · Analysis | Quality switching, followed by existing streaming and browser defaults |
| Server | Maintenance · Users · System · **Cluster** (`#/settings/cluster`) | Existing cluster dashboard plus expandable automatic transport-recovery guidance |
| Outside | Integrations | Existing behavior |
| Developer | **Developer** (`#/settings/developer`) | Protocol compatibility, browser preparation override, decoder experiments, HLS comparison |

`livetv` is deliberately letters-only to fit the existing route parser. Do not
introduce an unregistered `live-tv` settings route or change the viewing route
`#/live-tv`. Both the desktop rail and compact horizontal navigation derive
from `SET_GROUPS`/`SET_TABS`; do not maintain a second hardcoded list.

### 2.1 The exact control ownership

| Control | Destination | Existing setting / local contract | Save and effect |
|---|---|---|---|
| Tuner address, owner, session limit, output height | Live TV | Existing `live_tv_*` configuration and generation | Save configuration; does not enable playback |
| Enable/disable Live TV | Live TV | `live_tv_enabled` plus generation | Explicit action; preserve existing drain and fencing behavior |
| Previous-owner physical-fencing attestation | Live TV | Existing `live_tv_fenced_owner` object | Existing explicit attestation; never infer fencing from unreachability |
| Guide source, XMLTV URL, look-ahead | Live TV | `live_tv_guide_source`, `live_tv_xmltv_url`, `live_tv_guide_hours` plus generation | Own Save; preserve draft on error |
| Prepare quality changes | Playback, **Quality switching** card | `prepared_quality_handoff` | Own Save; next eligible quality change; keep **experimental** badge |
| Advertise playback control protocol v1 | Developer, Compatibility | `playback_control_protocol_v1` | Own Save; advertisement for new sessions |
| Allow a second player | Developer, Compatibility | Existing `preparedHandoffEnabled` / `setPreparedHandoff` browser storage | Local, automatic save; next reporter exchange |
| Require a producer health receipt | Developer, Decoder experiments | `decoder_health_qualified_artifacts` | Own Save; preserve next-start/restart semantics and actual policy state |
| Automatic decoder recovery | Developer, Decoder experiments | `automatic_decoder_recovery` | Own Save; preserve immediate/new-attempt semantics |
| Typeless sliding HLS | Developer, Delivery experiments | `hls_typeless_sliding` | Own Save; new sessions only |
| Cluster transport recovery | Cluster | Automatic capability, not an enable setting | Informational guidance and readiness; no fabricated switch |

Server controls remain administrator-only. A browser-local override changes only
that browser; it must not change another viewer or a server-wide setting. No
readiness result may disable a control or silently reset a saved choice.
Structural backend validation, generation conflicts, and existing security
refusals still apply.

## 3. Build a compact Developer page with useful detail

Use the application's existing theme variables, embedded typography, card
primitives, spacing, and responsive navigation. No new UI framework or theme.

```text
 Developer
 Compatibility controls, experimental playback and device diagnostics.

 [ Live TV ↗              ] [ Playback ↗        ] [ Cluster ↗          ]
 [ Tuner and guide        ] [ Quality/defaults  ] [ Health and recovery ]

 Compatibility
   Playback control protocol                 [compatibility]
     [checkbox] Advertise protocol v1
     ▸ Client readiness
     Save                                      Saved / Unsaved changes

   Second player in this browser             [this browser]
     [checkbox] Allow a second player
     Saved automatically; device memory/decoder cost and Playback link

 Decoder experiments
   Verified decode artifacts                 [actual policy state]
     [checkbox] Require a producer health receipt [preview]
     Restart and cache-impact summary
     ▸ Cache impact and diagnostic evidence
     Save

   Automatic decode recovery                 [enabled / disabled]
     [checkbox] Enable recovery [experimental]
     Short CPU-cost and effect summary
     ▸ Recovery limits and diagnostic evidence
     Save

 Delivery experiments
   HLS delivery comparison                   [experimental]
     [checkbox] Typeless sliding HLS
     Save
```

The three destination tiles are ordinary accessible links. They tell returning
users where controls moved. They must work with keyboard navigation, browser
history, and a narrow viewport. Do not make the whole page a disclosure widget;
controls, saved state, costs, and action buttons stay visible.

Use distinct visible section headings. Technical detail lives in native
`details`/`summary` disclosures, initially closed. Preserve the exact meaning of
observed artifact evidence and recovery behavior inside them, including the distinction
between a saved request, an effective policy, uncovered paths, and a pending
restart. Shorter copy must not imply that enabling verified artifacts validates
all decoders or that recovery guarantees a seamless picture.

## 4. Make expanded readiness readable at every width

### 4.1 The required expanded structure

The Quality switching card appears after the existing player-defaults card and
before Streaming. Keep its toggle and save outside the disclosure. Its expanded
body has this structure:

```text
 ▾ Readiness and device qualification
 These readings describe this server. Missing evidence does not disable the setting.

 SERVER READINESS
 ─────────────────────────────────────────────────────────────────
 Control protocol                                             [met]
 Description of the requirement and when it takes effect.

 Server preparation                                  [not reported]
 Description of capacity, preparation and cancellation requirements.
 │ The server's observed evidence, when provided.

 Throughput measurement                                  [supported]
 Reporting exists; this does not prove a particular session has enough throughput.

 DEVICE QUALIFICATION
 ─────────────────────────────────────────────────────────────────
 Client handoff                                     [not observable]
 What Apple, Android and web must demonstrate on a physical device.
 │ Observed evidence or the explicit limit of what the daemon can see.

 Fleet validation                                  [not observable]
 Consecutive handoffs, memory stability and measured fallback interruption.
 │ Observed evidence.

 Missing qualification does not disable the feature or override the saved choice.
```

The static throughput-support row is implementation information, not proof that
any particular stream or device has passed qualification. Label it accordingly;
do not invent a green runtime measurement from the existence of code.

### 4.2 One row structure for every readiness item

Use a dedicated readiness-row component/class structure rather than reusing
`.tog`, which was designed to lay out a label and a checkbox. The prototype's
use of `.tog` allowed the status pill to shrink until “met” became three
vertically stacked letters.

A row has four parts: title · nonshrinking status · description · optional
observed evidence. Use the same structure in Playback, Developer protocol
readiness, and moved Cluster readiness.

Recommended starting CSS, adjusted only to fit current theme conventions:

```css
.devcheck { padding: 16px 0; border-top: 1px solid var(--line); min-width: 0; }
.devcheck-head { display: flex; align-items: flex-start; gap: 8px 16px; }
.devcheck-head > strong { flex: 1 1 0; min-width: 0; line-height: 1.5; }
.devcheck-status { flex: 0 0 auto; white-space: nowrap; }
.devcheck-status .pill { display: inline-flex; white-space: nowrap; line-height: 1.4; }
.devcheck-description, .devcheck-evidence { margin: 6px 0 0; line-height: 1.65; }
.devcheck-evidence { padding-left: 10px; border-left: 2px solid var(--line); }
.devcheck-evidence:empty { display: none; }
```

Titles can wrap; statuses cannot. Descriptions span the row beneath the header,
not a narrow column beside the pill. Long evidence, binary names, and technical
identifiers must wrap without widening the card. At very small text widths,
stack the header deliberately if needed; never shrink a badge into vertical
text. Use roughly 13 px titles, 12–13 px descriptions, and 11 px group labels in
the existing scale. Text must remain usable with browser zoom and larger fonts.

Use semantic group headings and an actual native summary control. Preserve
visible keyboard focus, sufficient theme contrast, and textual statuses; color
alone cannot convey readiness. Do not nest disclosure text in a checkbox label.

### 4.3 Render facts without promoting guesses

| Input state | Visible meaning | Control behavior |
|---|---|---|
| Request pending | Checking | Existing saved choice stays editable |
| `met` | Requirement observed as met | No automatic enablement |
| `unmet` | Requirement not met | Advisory; Save still possible |
| `unobservable` | Not observable from this daemon | Never relabel as passed or failed |
| Item absent in a valid response | Not reported | Preserve saved choice |
| Request failure | Unavailable, with a bounded readable reason | Preserve the form and allow retry |
| Unknown status | Unknown / not reported, never green | Preserve the form |

Retain the existing readiness keys and `data-devstat`/`data-devev` bindings, or
migrate both rendering and asynchronous patching together. Escape backend
strings; insert evidence as text, not trusted markup.

For prepared handoff, bind `server_preparation_is_real`,
`client_two_player_handoff`, and `fleet_receipt` under
`prepared_quality_handoff`. The protocol row uses the saved protocol setting.
Developer's protocol evidence uses `playback_control_protocol_v1` /
`clients_report`. Keep the existing transport-recovery requirement keys when
moving that guidance.

## 5. Preserve data loading, edits, and navigation

### 5.1 Update registry, dispatch, and manifest together

The implementation entry point is [index.html](../../crates/plurxd/src/web/index.html).
Search by symbol rather than using prototype line numbers.

| Area | Symbols / contract |
|---|---|
| Navigation | `SET_GROUPS`, `SET_TABS`, `settingsRouteTab`, `settingsTab`, `setSettingsTab` |
| Data loading | `SETTINGS_MANIFEST`, `SETTINGS_ENDPOINTS`, `loadSettingsTab` |
| Dispatch | `settingsPanel`, `playbackPanel`, new or equivalent `liveTvPanel` |
| Evidence | `devReq`, `devReadinessPill`, `devReadinessEvidence`, `applyDeveloperReadiness` |
| Composition | `developerPanel`, `clusterPanel`, prepared-quality card helper, transport-guidance helper |
| Card state | `markSetCard`, `setCardSaved`, `setCardFoot` |
| Live TV | Guide draft, source-change, save, refresh, readiness, owner-recovery handlers |

Required data should remain small:

| Section | Required | Secondary |
|---|---|---|
| Live TV | Settings | No new prerequisite endpoint required merely to render the form |
| Playback | Settings | Existing developer readiness endpoint |
| Developer | Settings | Existing developer readiness endpoint |
| Cluster | Existing cluster roster | Existing operations status plus developer readiness |

Reuse existing caching and request coalescing. A slow readiness response must
not delay an editable form. Patch evidence nodes instead of rerendering the
whole page when readiness arrives. Preserve the cluster's existing dialog,
focus, and staged store/paint safeguards. Keep expanded disclosures stable
across background evidence updates.

### 5.2 Every asynchronous Live TV handler must follow the new route

Change route-current checks from Developer to `livetv` for guide readiness,
manual guide refresh, configuration writes, tuner readiness, and error paths.
A request started on Live TV may finish after navigating away: it must not
repaint another section or re-enable an unrelated button. Keep abort/deadline
behavior and stale-authentication handling from the current API wrapper.

Retain generation-based writes. A settings conflict must show a useful error
and preserve the entered values. Guide readiness describes saved configuration;
do not silently present it as validation of an unsaved XMLTV draft.

### 5.3 Save only what the card shows

Separate the former shared `saveDeveloper` payload. Protocol Save writes only
`playback_control_protocol_v1`; Quality switching Save writes only
`prepared_quality_handoff`. Preserve independent verified-artifact, automatic
recovery, guide, and experimental-HLS saves. Browser capability has no server
Save button.

Render the returned saved state, not the attempted value. The Quality switching
badge must update on success; an unchecked toggle next to “Enabled” is a
failure. Save buttons track clean, dirty, submitting, failed, and saved states.
An error leaves the draft correctable and the button usable.

Do not erase another card's edits during a save or source-selector repaint.
Specifically test changing the guide source while tuner fields are dirty, and
saving tuner configuration while the guide URL/look-ahead is dirty. The local
prototype still uses broad settings repaints in some Live TV paths; tighten
those paths or preserve complete drafts during production implementation.
Do not copy that limitation into the finished build.

## 6. Implement in four reviewable steps

1. **Port navigation and ownership.** Add Live TV, move the two cards, move the
   prepared server control, retain the browser override in Developer, and put
   transport guidance in Cluster. Wire manifests and callbacks in the same
   change so no route is temporarily broken.
2. **Apply the visual system.** Destination tiles, grouped Developer sections,
   collapsed details, consistent readiness rows, and server/device groups.
   Preserve disclosure accessibility and theme behavior.
3. **Finish interaction correctness.** Independent save payloads, authoritative
   saved badges, stale-response guards, and sibling-draft preservation. Keep
   runtime enablement policy unchanged.
4. **Validate and document the final behavior.** Update existing current
   references, capture screenshots from the actual app, and assemble the
   implementation evidence described below.

This is a bounded independent UI delivery unless current code requires a
larger integration effort. Coordinate with the review-remediation task before
both edit playback functions or shared settings code. Merge one into current
main and rebase the other; do not overwrite the other's hunks.

## 7. Acceptance and evidence

### 7.1 Functional acceptance

- All settings routes deep-link correctly, survive reload, and obey Back/Forward.
- Both navigation forms expose Live TV; Developer's destination links land in
  the correct section. Ordinary users cannot enter administrator settings.
- Controls occur once in their intended destination and retain saved defaults.
- With readiness pending, unmet, absent, or failed, enable and Save still work.
  The next feature attempt is not rejected solely for missing qualification.
  Exercise the backend behavior as well as the enabled checkbox.
- Each Save sends only its card's fields; no missing-element access occurs
  because a related control now lives on another page.
- Failed saves and guide source changes preserve this card's and sibling cards'
  drafts. Successful saves update the effective/saved display appropriately.
- Delayed readiness patches only evidence, with no lost edits or collapsed
  disclosure. Delayed Live TV responses cannot mutate a different page.
- Enabling/disabling and owner recovery preserve the existing backend contract.
  Missing readiness remains advisory in both UI and backend; enabling a feature
  must not be blocked by qualification evidence. Authentication, invalid inputs,
  unavailable physical resources, and actual operation failures still report their
  normal result; they are not feature-certification requirements.

### 7.2 Visual acceptance

Inspect Developer, Live TV, Playback, and Cluster with real app rendering.
For the readiness section, inspect both collapsed and expanded states, including
all statuses and long evidence. Check at 1280, 880, 390, and 320 CSS-pixel widths,
plus 200% zoom and representative existing themes.

No document-level horizontal overflow; navigation may scroll inside its own
strip. No vertically wrapped badges, title/status collisions, clipped error
text, unreadable evidence, or controls hidden behind long descriptions. The
phone layout may wrap titles but keeps status pills intact. Verify keyboard
opening/closing and focus after saves or patches.

### 7.3 Relevant commands and evidence interpretation

```bash
node tests/web/settings-sections.test.js
node tests/web/page-read-budget.test.js
node tests/web/cluster-membership.test.js
node tests/web/live-tv.test.js
python3 -m unittest discover -s tests/operations -p test_docs_index.py
```

Also run the repository's embedded-JavaScript/static checks appropriate to the
final branch. New tests should exercise ownership, payloads, asynchronous
updates, or rendering behavior rather than pinning paragraphs of copy.

The prototype recorded 21 settings, 42 page-loading, 144 cluster, and 39 Live TV
JavaScript cases passing, plus the docs-index check. Its inline scripts parsed,
and the expanded sample-data view had no horizontal overflow at a 375 px
content viewport; all five badges were 21 px high. Those are historical design
observations, not acceptance for Sol's final branch. Re-run relevant checks and
record the actual head, screenshots, and limits of the test environment.

## 8. Finish through the current repository workflow

Follow [AGENTS.md](../../AGENTS.md) and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md). Keep the disabled commit
hooks disabled. If the final scope touches Rust, establish the pinned compiler
loop before editing; this UI plan does not require Rust behavior changes.

Update current docs and functionality-point ownership in the same commit as
applicable changes. A corrective test can supply direct regression evidence;
otherwise add the required historical-evidence record. A web-only change does
not advance native build counters, but any added native scope must follow the
mobile-version rules.

Open a main-bound PR as draft, request exactly one adversarial agent review,
address its findings, mark ready, apply `fast-lane`, and merge only after the
current head's **Main promotion gate** is green. Do not request a second review.
Full test suites remain owned by the separate sweep; targeted implementation
verification and visual evidence do not redefine the pre-merge gate. Deployment
is a separate action and must not be inferred from this handoff.

**Stop point.** Once the approved four settings surfaces, save behavior, and
readiness rendering pass the focused checks, finish the PR and stop. Do not pull
the wider security roadmap into this UI change.

**Sol's final deliverable:** implementation commits and PR, current-head check
results, actual-app desktop/mobile screenshots including expanded readiness,
and a short list of any remaining backend or device limits. The layout is done
when the interaction and visual acceptance above pass, not merely when the
prototype patch applies.
