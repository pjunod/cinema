# docs — where everything is, and what each file answers

The front door is the repo [README](../README.md): what plurx is, how to run
it, and the reading path for a newcomer. This page is the router for `docs/`:
the reference documents at this level, and one row per subject folder linking
that folder's own index, which lists every file in it, the question it answers
and its status. Read this page, then only the folder index you need.

**Projects / to-do:** [the project backlog](features/PROJECT-BACKLOG.md) is the
cross-project queue: unbuilt work, built work awaiting acceptance, unresolved
older plans and deferred decisions. Update its row when a project's state
changes.

Three tiers, and only three:

- **This directory** holds the reference set: the documents that describe
  plurx *as it is today*. A behavior change updates them in the same commit.
- **Subject folders** hold everything written *about* a piece of work — plan,
  reviews, handoffs, status tracker, diagnoses. One subject, one folder, one
  index.
- **[`archive/`](archive/README.md)** holds finished work outside any live
  thread. Nothing is deleted; it is out of the way, not gone.

Status words mean exactly this: **live** — maintained, describes the current
system · **open** — in progress or awaiting a decision · **built** — landed;
the doc records what and why · **done** — closed, kept for history ·
**superseded** — replaced by a later document. When an index row and a doc's
own `**Status:**` header disagree, the doc wins.

## Find it fast

| You want to know… | Read |
|---|---|
| What does plurx actually do? | [FEATURES.md](FEATURES.md) |
| How do I run it, configure Cinema sharing addresses, and read output? | [OPERATIONS.md](OPERATIONS.md) |
| What do I type? | [CHEATSHEET.md](CHEATSHEET.md) |
| How is it built, and why that way? | [ARCHITECTURE.md](ARCHITECTURE.md) |
| What endpoint do I call, and what authorizes it? | [API.md](API.md) |
| How do I build and operate the fixed-purpose Cinema notification broker? | [Broker README](../crates/plurx-notification-broker/README.md) — **open**; synthetic software evidence, provider/native acceptance outstanding. |
| How does a file become a stream? | [PLAYBACK.md](PLAYBACK.md) |
| Why is this title playing badly? | [PLAYBACK-TESTING.md](PLAYBACK-TESTING.md), then [streaming/](streaming/) |
| Why is a Dolby Vision title arriving as HDR10? | [streaming/DV-DELIVERY-FINDINGS.md](streaming/DV-DELIVERY-FINDINGS.md) |
| What is the cluster supposed to do when a node dies? | [cluster/](cluster/), starting at [CLUSTERING-PLAN.md](cluster/CLUSTERING-PLAN.md) |
| Why was the transport-recovery campaign red on main for so long? | [cluster/TRANSPORT-RECOVERY-RESOURCE-BASELINE.md](cluster/TRANSPORT-RECOVERY-RESOURCE-BASELINE.md) |
| What does its resource check assert now, and why? | [cluster/TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md](cluster/TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md) |
| Which button does what on which client? | [clients/PLAYER-INPUT-CONTRACT.md](clients/PLAYER-INPUT-CONTRACT.md) |
| Why is a title with PGS subtitles slow or refusing to start? | [clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md](clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md) · [review](clients/PGS-SUBTITLE-START-PATH-RCA-REVIEW.md) |
| Why does a first subtitle wait for a full read on one node? | [clients/SUBTITLE-CLUSTER-EXTRACTION-PLAN.md](clients/SUBTITLE-CLUSTER-EXTRACTION-PLAN.md) · [review](clients/SUBTITLE-CLUSTER-EXTRACTION-REVIEW.md) · [status](clients/SUBTITLE-CLUSTER-EXTRACTION-STATUS.md) |
| Why did subtitles refuse, not appear, or stop? | [clients/SUBTITLE-RELIABILITY-ASSESSMENT.md](clients/SUBTITLE-RELIABILITY-ASSESSMENT.md) · [handoff](clients/SUBTITLE-RELIABILITY-HANDOFF.md) · [physical verification](clients/SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT.md) |
| Why is there an error overlay while the picture is still playing? | [clients/PLAYBACK-SURFACE-CONTRACT.md](clients/PLAYBACK-SURFACE-CONTRACT.md) |
| Why does Live TV freeze a few seconds after it starts? | [features/LIVE-TV-START-STALL-AND-TVOS-PLAYBACK-SURFACE.md](features/LIVE-TV-START-STALL-AND-TVOS-PLAYBACK-SURFACE.md) |
| What is missing from the Apple / Android client? | [clients/APPLE-CLIENT-PARITY.md](clients/APPLE-CLIENT-PARITY.md) · [clients/ANDROID-CLIENT-PARITY.md](clients/ANDROID-CLIENT-PARITY.md) |
| How do I cut a release? | [RELEASING.md](RELEASING.md), then [PUBLISHING.md](PUBLISHING.md) |
| What does CI gate, and why did it fail? | [VALIDATION.md](VALIDATION.md) |
| How does work get from a branch to `main`? | [DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) |
| What is designed but not built, and what still needs finishing? | [Project backlog](features/PROJECT-BACKLOG.md) |
| What is being built right now? | [STATUS.html](STATUS.html) · [ROADMAP.md](ROADMAP.md) |
| What did we decide about X, and when? | the subject folder for X — plans and reviews carry dated `**Status:**` headers |


## The reference set

Maintained documents describing the current system. Everything here is
`live`.

| File | The question it answers |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | How plurx is built, and why — the diagrams and the founding decisions. |
| [API.md](API.md) | Every HTTP endpoint, the credential it takes, and what comes back. There is no OpenAPI document; this is the specification. |
| [FEATURES.md](FEATURES.md) | Everything plurx does, exhaustively, including what it deliberately refuses to do. |
| [OPERATIONS.md](OPERATIONS.md) | Running it day to day, and what every output means. |
| [CHEATSHEET.md](CHEATSHEET.md) | What to type, in what order. |
| [PLAYBACK.md](PLAYBACK.md) | How a file becomes a stream — the end-to-end delivery path. |
| [PLAYBACK-TESTING.md](PLAYBACK-TESTING.md) | Turning a playback failure into a reproducible matrix. |
| [CLIENTS.md](CLIENTS.md) | The client strategy and the platform matrix — what runs native, what runs web. |
| [INTEGRATION.md](INTEGRATION.md) | Every seam with Curator, and how to prove each one works. |
| [SECURITY.md](SECURITY.md) | What plurx protects, and what it leaves to the network. |
| [VALIDATION.md](VALIDATION.md) | The functionality-point system: what CI gates and how impact is selected. |
| [REQUIREMENTS.md](REQUIREMENTS.md) | The product requirements, and the decision behind each. |
| [ROADMAP.md](ROADMAP.md) | The phases, and what each one ends with. |
| [RELEASING.md](RELEASING.md) | One version for the whole workspace — how it moves. |
| [PUBLISHING.md](PUBLISHING.md) | TestFlight, the App Store, and Google Play. |
| [BENCHMARKING.md](BENCHMARKING.md) | Measuring plurx against Plex without grading different tests. |
| [DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) | Fast effort branches, deliberate qualification — how work reaches `main`. |
| [STATUS.html](STATUS.html) | The pinned status page: what is deployed, on which nodes and clients. |


A status record that also lives at this level:

| File | Answers | |
|---|---|---|
| [DECODER_SELECTION_RECOVERY_STATUS.md](DECODER_SELECTION_RECOVERY_STATUS.md) | The live execution ledger for that plan: what is merged, what was tested, and what remains unsafe. | open |

Assets and generated records also live at this level:
[`img/`](img/) (README screenshots) · [`apple-builds/`](apple-builds/README.md)
(one note per Apple build, read by `validation/apple_build.py`) ·
[`mockups/`](mockups/) · [`evidence/`](evidence/).

## Subject folders

One row per folder; the folder index lists every document in it. Counts are
index rows by status as of 2026-10-17 — the folder index is authoritative.

| Folder | What it holds | Rows |
|---|---|---|
| [playback-control/](playback-control/README.md) | The playback control rewrite: explicit demand, one owner per stream, prepared handoffs. | 27 open · 22 built · 13 done · 10 superseded |
| [streaming/](streaming/README.md) | Delivery: VOD presentation, capability negotiation, Dolby Vision, decoder selection, stall investigations. | 1 live · 121 open · 19 built · 25 done |
| [cluster/](cluster/README.md) | Phase 4 clustering: replication, membership, recovery, and replicated-failure diagnoses. | 23 open · 5 built · 9 done |
| [clients/](clients/README.md) | Apple, Android and the web player: parity, the input contract, subtitles, overlays, layouts. | 7 live · 70 open · 28 built · 19 done |
| [server/](server/README.md) | The HTTP layer, scanning, images, auth and observability. | 7 open · 2 built |
| [performance/](performance/README.md) | Where the seconds go: performance rounds and the video-quality programme. | 4 open · 1 built · 7 done |
| [ci/](ci/README.md) | The build, the gate, and the agents that use it. | 4 live · 16 open · 3 done |
| [features/](features/README.md) | Whole capabilities planned end to end, including the project backlog. | 4 live · 42 open · 11 built · 6 done |
| [reviews/](reviews/README.md) | Pull-request and effort review records. | 2 live · 10 open · 7 done · 1 superseded |
| [archive/](archive/README.md) | Finished work, kept for history. | 2 rows |

## Adding a document

Put it in the subject folder its work belongs to and add a row to that
folder's `README.md` in the same commit — not here. This page changes only for
a new reference document or a new subject folder. Give the document an H1
with an em-dash tagline, then a `**Status:**` line carrying an absolute date.
A plan or handoff that an agent will execute also states what it executes and
what it must not touch.

This index is kept honest by `tests/operations/test_docs_index.py`: every
Markdown file under `docs/` is listed on this page or in its folder's index, a
folder index lists only its own folder, this page links every folder index,
every link resolves, and nothing in the repo points at a `docs/` path that
does not exist.
