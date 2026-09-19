# Web UI usability audit — what to improve after DVR

**Status:** open design review · **Observed:** 2026-09-14 · **Scope:** web UI

Companion to [UI layouts](UI-LAYOUTS-IMPLEMENTATION.md) and the
[DVR visibility work](../features/LIVE-TV-DVR-VISIBILITY-STATUS.md).
This review identifies the next usability improvements and their acceptance
criteria. It is a proposal, not an implementation receipt or a claim that
all clients have been inspected.

## The next pass should make the next action obvious

The strongest opportunity is consistent hierarchy. A viewer should quickly
find the right title and continue it. An administrator should quickly see
what needs attention, its effect, and the next useful action. Both should
be able to reveal the technical evidence without losing their place.

Keep the artwork, themes, layout choices, rich diagnostics, and useful
administrative tables. Extend the DVR pattern: recognizable content,
explicit state, nearby actions, and detail when selected.

### Evidence and limits

Live inspection used the desktop web application at `lab6:32400`, reporting
build `v0.3.0-2481-gb59e421d`. Source inspection used commit
`482a757c5c50e1beedb1e795d7089f07398a3cf0`, the DVR fidelity branch.
These are different snapshots; re-check findings against the intended
implementation base. Live observations below are dated observations,
not enduring counts or a diagnosis of the underlying services.

The audit changed no server settings, created no channel, and started no
playback. A temporary 390 × 844 browser viewport was restored afterward.
All concepts use illustrative state. Proposed success states must be backed
by server evidence when implemented.

| Surface | Coverage | Main opportunity |
|---|---|---|
| Home | Live desktop + source | Readable continuation and early browse entry points |
| Movie library | Live desktop + source | Scoping, density and useful filters |
| Search | Live query and result selection + source | Exact matches, type grouping and context |
| Film detail | Live desktop + source | Put watching ahead of maintenance |
| Series detail | Live desktop and phone | Continue the series directly |
| Live TV / Recordings | Earlier live and fixture review; DVR branch checked | Preserve the new DVR fidelity work |
| Library channels | Live empty state and unsaved editor | Guided creation with meaningful preview |
| Activity | Live desktop + source | Consistent status and useful priorities |
| Analysis workspace | Live desktop + source | Separate queued/running and group problems |
| Settings: Libraries / Playback | Live desktop + source | Useful outcomes and clear setting scope |
| Settings: System / Cluster | Live desktop + source | Promote impact and operational readiness |
| Metadata / Integrations / Reader | Source only | Follow-up candidates, not verified visual defects |
| Active player and recovery states | Source and existing contracts only | Preserve established controls; run a dedicated scenario pass |
| Native Apple / Android | Not inspected in this pass | Separate device and remote-control review required |

## Priority and build order

P1 means a misleading state or a missing core action. P2 means a substantial
usability improvement. P3 means secondary refinement. Effort is relative:
small is localized presentation; medium crosses a few surfaces; large needs
new aggregation or interaction contracts. These are not schedule estimates.

| ID | Priority | Change | Effort / dependency |
|---|---|---|---|
| U01 | P1 | Correct contradictory Activity and analysis messages | Small–medium; verify status semantics |
| U02 | P1 | Continue a series from its own page | Medium; share existing next-episode selection |
| U03 | P2 | Reorganize film and episode details around playback | Medium; mostly existing data |
| U04 | P2 | Make channel creation a real guided flow | Medium; existing recipe and preview APIs |
| U05 | P2 | Give Home readable continuation and browse shortcuts | Medium; mostly existing data |
| U06 | P2 | Extend Activity's DVR hierarchy to all work | Medium–large; reconcile scopes and freshness |
| U07 | P2 | Turn the analysis workspace into a useful work queue | Medium–large; grouped causes may need API work |
| U08 | P2 | Improve search and library browsing | Medium; exact-match ranking and new facets need validation |
| U09 | P2 | Keep content and primary actions visible on phones | Medium; verify each layout and theme |
| U10 | P2 | Make system warnings explain impact and recovery | Medium; affected-content mapping may need API work |
| U11 | P3 | Clarify settings scope and simplify advanced sections | Small–medium; retain independent save behavior |
| U12 | P3 | Make library outcomes and imported recordings legible | Medium; title metadata may need ingestion work |

Build U01 first. Then U02/U03/U09 as one coherent title-page slice, followed
by U04 and U05/U08. Extend operations with U06/U07/U10 once their data
contracts are explicit. Keep changes reviewable; this is not authorization
for a single broad rewrite.

## Findings and proposed behavior

### U01 — Make status claims agree with their evidence

**Observed:** Activity said “No libraries yet,” while Settings listed eight.
The source renders that message when `d.scans.length` is zero. An empty
scan list does not establish that no libraries exist.

Activity also said the index queue was idle while showing 95 working and
3,672 recent attention items. The analysis workspace showed queued rows
under the working summary, including rows with nine-day ages. The snapshots
prove a confusing presentation; they do not prove the scheduler is broken.

**Proposed:** Use “No scans running” for an empty scan list. Only claim no
libraries when the library response confirms zero. Separate queued, actively
running, recent finished, and recent failed counts. Every summary states its
scope and observation time. Explain different historical windows locally;
never force them into an apparently comparable row of totals.

**Acceptance:** Fixtures with configured libraries and no scan work never
render “No libraries.” A queue-only fixture never says jobs are running.
Unavailable and stale responses never become zero or healthy. Summary,
filters and detail agree about the same job state and time window.

**Source:** `paintActivityBody`, `analysisVerdictLine`, `analysisSummaryCard`
and `analysisLiveProgress` in the [web shell](../../crates/plurxd/src/web/index.html).

### U02 — Put “Continue series” on the series page

**Observed:** Shameless showed “39 of 134 watched.” Its prominent actions
were Make a channel, Mark series watched, and Mark series unwatched.
The user had to select a season and find the episode to keep watching.

**Proposed:** Put a readable continuation block directly under the synopsis:
episode identity, watched progress, time left, and Resume. If no episode is
in progress, show the next eligible episode with Play. Keep season navigation
below it; show watched counts by season. Move bulk watched actions and
Make a channel into a secondary action menu.

**Acceptance:** Home and series detail select the same continuation target
for the same user and snapshot. Handle never started, partially watched,
all watched, missing episodes, unavailable files, and multiple versions.
Do not infer the next episode from watched count alone. A continuation
failure leaves a usable season browser and a clear message.

### U03 — Give film and episode pages a playback hierarchy

**Observed:** reference film G's Resume and Start over actions shared space with
Make a channel. Refresh artwork sat beside the title. Video/audio/subtitle
inventories, HLS capability, file information, another pair of track
selectors, analysis actions, and a raw conversion error filled the page.
It was difficult to understand how the error affected watching.

**Proposed:** Title, synopsis, key metadata, and Resume/Play form the first
block. Show one compact Audio/Subtitles selector strip. Put Mark watched,
Make a channel and artwork actions in More. Fold the technical inventory
into Media information. An administrator-only preparation notice explains
the verified impact, with an action and expandable raw evidence.

**Acceptance:** One clear primary playback action and one set of pre-play
track selections. All existing diagnostics remain reachable. A failed
preparation job must not label the title unplayable unless the playback
contract says so; source resolution must not masquerade as delivered
quality. Non-admin users see no maintenance controls.

### U04 — Guide channel creation from choice to preview to save

**Observed:** The new-channel editor started with a narrow textarea, many
rules, raw included/excluded IDs, and a red name error before reaching the
name step. Its preview showed zero eligible titles and thousands excluded
before the user had made a meaningful choice. Refresh preview, Next and
Save competed for attention.

**Proposed:** Three steps: Choose content → Playback → Name and create.
Use a full-width subject field and concrete starter choices. Render selected
titles as named chips with removal actions. Keep metadata rules and IDs in
advanced details. Before a preview exists, explain what the user will see;
afterward show matching titles, duration before repeats, and exclusion
reasons. Mark a preview out of date when its recipe changes. Place Create
channel only on the final step.

**Acceptance:** Opening the editor shows no validation errors. Validate the
current step on progression and all required fields on final submission.
Changing content cannot leave an old preview looking current. Zero matches,
loading, stale preview and failed preview have distinct presentations.
Back preserves inputs; keyboard users can complete the flow. A save failure
keeps the draft and explains the failed operation.

### U05 — Make Home answer “What can I watch now?”

**Observed:** Small portrait cards truncate continuation titles, episode
labels and remaining time. A single Next up item occupies a large mostly
empty row. Movies, TV and Books entry points appear well below several
rails. Recently added content repeats again in category previews.

**Proposed:** Show three or four comfortable landscape continuation cards
with full episode context, progress and an explicit Resume action. Put
Movies, TV and Books browse shortcuts near the top. Keep a compact Next up
section for series without an in-progress episode. Add a cohesive Recently
recorded rail using the DVR metadata contract. Reduce repeated rails and
label Coming soon as future releases, distinct from available content.

**Acceptance:** At 1,024px a representative long title, episode and time left
remain readable. At phone width the first continuation action appears
without scrolling through a poster hero. Do not remove titles automatically
because only a few minutes remain. Preserve explicit watched-state control.
No Coming soon tile implies the file is available before that is known.

### U06 — Apply the DVR Activity pattern to viewing and other work

**Observed:** Now playing was empty while a separate Live television section
contained an active session. Several idle job categories consumed large
boxes. Useful work and failures were spread among unrelated sections.

**Proposed:** Organize around Happening now, Needs attention, and Recent.
Within Happening now, distinguish Watching, Recording and Background work.
Watching cards show title/programme, viewer, device and playback position.
Selecting any job reveals its relevant details. Collapse idle categories
into a compact summary with a link to their workspace. Preserve job type
filters for administrators.

**Acceptance:** A Live TV viewer appears in the viewing total exactly once.
A recording does not count as a viewer. Session identity, local-node scope,
cluster scope and stale observation are explicit. Polling preserves focus,
selection, expanded details and scroll. Existing DVR cards remain intact.

### U07 — Make analysis problems reviewable in groups

**Observed:** The workspace puts large recent-window counters above thousands
of rows. Queued rows include pipeline hashes, raw node IDs and multiple
identifiers before the useful explanation. Finding a shared cause requires
reading individual rows.

**Proposed:** Separate Running, Queued and Needs attention. Keep a table for
bulk inspection, with title, stage, owner name, age and last useful progress.
Put pipeline IDs in the detail pane with Copy details. Group attention by a
stable failure category and offer Review affected items. Present Retry only
when supported, with explicit selection and scope.

**Acceptance:** Group counts come from the relevant full result set, not the
current page. Unknown owners retain a clearly labelled ID fallback. Queued
age is not labelled runtime. Unsupported retries have an explanation.
Selection and pagination survive polling. Re-check aggregation requirements
before promising a grouped view using only the current response.

### U08 — Make search and library grids explain what matched

**Observed:** Searching “shameless” returned a film, the exact series, and
an episode from another series in a flat grid, with no explanation of the
other matches. The Movies view showed 418 items with All as its page size;
titles were heavily truncated and acceptance media appeared among films.

**Proposed:** Prioritize exact title matches, then group by Movies, Series,
Episodes and Books. Show episode titles and parent series together; include
a match reason only when the search contract provides it. Add a visible
library scope, active filter chips and Clear filters. Make the existing
poster-density preference easier to reach. Let administrators explicitly
choose which libraries appear in normal browsing.

**Acceptance:** Search retains query and navigation context on return.
Empty results suggest changing scope or terms. Do not silently exclude a
library based on its name. Genre/duration facets require metadata coverage
and server behavior checks; they are follow-up features, not assumed data.

### U09 — Reduce phone chrome and bring actions above oversized artwork

**Observed:** At 390 × 844, the series page's navigation and status area took
about the first 200px, followed by a large portrait poster. The title began
well down the viewport; playback was absent. Horizontal overflow was also
visible in the deployed build. The DVR fidelity branch already contains a
navigation containment fix; validate it before creating duplicate work.

**Proposed:** Use compact mobile navigation with reachable overflow entries,
a search control that expands on demand, and one concise activity summary.
On title pages, pair a smaller poster with title metadata and put the primary
action immediately below. Keep plot and technical detail expandable.

**Acceptance:** Check 390px and 320px, enlarged text and long localized labels.
No page-level horizontal scrolling; guide/table overflow stays inside its
own region. Touch targets reach 44px where practical. All navigation remains
reachable. Test classic, cinema and compact layouts in light and dark.

### U10 — Put system impact and actionable warnings first

**Observed:** System exposed a missing recording path within a long storage
throughput section and a failed tone-map reference alongside encoder facts.
“0 active streams” described the local node while the header showed a live
session elsewhere. Cluster had useful readiness information, but below
large replication details, with repeated raw IDs and maintenance actions.

**Proposed:** Start System with verified issues and scope: This node versus
Cluster. Show the affected path, last observation and a link to its settings.
Keep measurements under expandable capability/storage detail. On Cluster,
put readiness and exceptions before protocol counters. Keep dangerous
maintenance controls secondary, with existing preconditions and confirmation
behavior preserved. Replace UUID-first prose with a resolved node name.

**Acceptance:** No warning disappears when details collapse. A missing path
is not claimed to affect all recordings without destination mapping.
Unavailable capability evidence remains unknown. Readiness keeps its existing
quorum semantics, freshness and refusal reasons. No synthetic health score.

### U11 — State who each setting affects

**Observed:** Playback mixes defaults for every player, server delivery
parameters and browser-only autoplay. Some text explains internal rollout
conditions. Per-card save/dirty state and the grouped sidebar already work
as useful organizing patterns.

**Proposed:** Label scope and effect beside each section: Your browser,
All players, or This server; indicate when the change takes effect. Keep
common preferences first and technical tuning in Advanced. Replace internal
rollout prose with the consequence a user needs to decide. Retain per-card
Save and Saved rather than introducing an ambiguous global save.

**Acceptance:** A user can identify who is affected before editing. Saving
one section does not discard another section's unsaved input. Validation
appears beside its field; saving and failed saving remain visibly distinct.

### U12 — Show library outcomes and clean recording identities

**Observed:** Library rows mostly say idle, which gives little information
about their last useful result. Imported recordings appear in Home with
timestamp-like titles or initials placeholders, and can resemble duplicate
series and episode entries.

**Proposed:** Preserve the compact library table, adding last scan result
and a clear attention link where supported. Represent recorded programmes
with programme title, episode, channel and date as separate fields. Prefer
actual artwork with a consistent intentional fallback. Link to the saved
recording rather than forcing the user to understand its file naming.

**Acceptance:** Unknown last-scan information is labelled unavailable.
Separate multiple recording destinations instead of silently merging or
removing libraries. A title with no guide identity remains usable without
invented metadata. Series containers and playable recordings are distinct.

## Reading the concept layouts

The companion interactive concepts illustrate Home, series continuation,
Activity and the first channel-creation step. Artwork is represented by
title placeholders; programme, progress and job data are illustrative.
Concept navigation connects only the illustrated screens. Keep all existing
application destinations reachable in a real implementation. Playback and
server actions in the concepts describe their intent without executing it.

The concepts were inspected at 1,024px and 390px browser widths. This is
layout evidence for the proposals, not acceptance evidence for application
code. Full implementation still needs the state and theme matrix below.

## Guardrails for the next implementation

1. **Reuse state contracts.** The shared continuation target, recording
   observation and playback surface contracts must drive every projection.
   New layouts must not create a second meaning of Resume or Recording.
2. **Preserve rich evidence.** Move diagnostic detail into reachable drawers
   and disclosures. Keep real failure reasons and existing administrative
   safeguards; visual simplification must not erase operational truth.
3. **Separate proposals from backend facts.** New failure grouping, affected
   item counts, ranking and discovery facets need explicit contracts before
   their mockup examples become promised behavior.
4. **Prove visual fidelity.** For each slice, list the named design elements
   and attach screenshots of the actual shipped HTML against representative
   fixtures. A passing compile or screenshot of the proposal does not prove
   the implementation matches it.
5. **Exercise states, not just the happy screen.** Include empty, loading,
   unavailable, stale, error, long-title, many-items and non-admin states.
   Verify mouse, keyboard, focus restoration and phone layouts. Assert real
   actions and shared state semantics in focused tests.

Do not rewrite the playback engine, change cluster membership policy,
redesign native clients from web assumptions, or ship new discovery APIs as
incidental styling work. The player and ebook reader need their own active
interaction pass before making concrete redesign claims about them.
