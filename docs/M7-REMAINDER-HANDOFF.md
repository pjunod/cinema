# M7 remainder — subtitle readiness, bounded materialization, seek coalescing, burn-join, prewarm

**Status:** ready to build · **Executes:** the unbuilt half of
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md) §13.8
(handoff §6 item 8) · **Written:** 2026-09-01 · **Baseline:** `main` at
`e1876cce` (`v0.3.0`)

## 1. Orientation — read this, work like this

Item 8's first half merged as PR #700 from
[CONTENT-ANALYSIS-INDEX-HANDOFF.md](CONTENT-ANALYSIS-INDEX-HANDOFF.md):
exact timeline annotations with provenance and manual override, the
lease-fenced analysis queue, its operator surfaces, and its instrumentation.
This document is the plan for everything §13.8 asks for that #700 did not
build: subtitle readiness through `DeliveryView` (§3), bounded forward
subtitle materialization (§4), seek coalescing and cancel-by-sequence (§5),
and joining burn changes into an existing successor action (§6). The
fifth piece — the marker-destination prewarm whose metric reads a permanent
zero — split into its own plan,
[MARKER-PREWARM-HANDOFF.md](MARKER-PREWARM-HANDOFF.md), per Paul 2026-09-01;
§7 here is the pointer. Section 2 is what exists today, verified against the tree; §8 is the
guardrails; §9 the milestones; §10 the repo mechanics that bite.

Read, in order: §13.8 of the protocol plan (the specification this executes),
then §2 here (the current behaviour you are changing), then the milestone you
are building. Work milestone by milestone; each ends in a runnable command or
an observable fact. Do not start a later milestone while an earlier one's
acceptance is red — M2 depends on M1's field, M3's latch is what M2's
production hangs off.

**The standing instruction, restated from the scoping brief:** the acceptance
criteria in §13.8 — quoted in full in §8.1 — are already written and are not
negotiable. If a design cannot satisfy one of them, stop and say which and
why rather than restating it more weakly. The same applies to the guardrails
in §8.2: an executing agent that finds a guardrail blocking the "obvious"
implementation has found the point of the guardrail.

Line numbers below are from `e1876cce`. **Re-verify every quoted contract
against the tree at build time** — this file moves.

---

## 2. What exists today — verified 2026-09-01

### 2.1 The subtitle path, end to end

Two routes serve text subtitles
([`crates/plurxd/src/http/mod.rs:298,314`](../crates/plurxd/src/http/mod.rs)):

- `GET /api/v1/files/{id}/subs/{subtitle}` →
  [`stream::subtitles_vtt`](../crates/plurxd/src/http/stream.rs) (`:1824`):
  the whole track as one VTT, for a native `<track>`. Awaits
  `ensure_vtt_bytes` — the full extraction — before answering. Rejects
  bitmap subtitles with a 415-shaped message worth preserving verbatim.
- `GET /api/v1/hls/{session}/subs/{index}/{segment}` →
  [`hls::subtitle_vtt`](../crates/plurxd/src/http/hls.rs) (`:6051`): one
  HLS segment. This is the route the remainder is about.

The HLS media playlist for a subtitle track
(`subtitle_media_playlist`, `hls.rs:6890`) mirrors the **video** playlist
segment for segment — one `segNNNNN.vtt` per video `#EXTINF`, whole timeline
advertised up front. The segment handler then:

1. races `read_cached_vtt` against the response publication deadline;
2. on a cache **hit**, slices the whole-track sidecar down to the segment's
   window with `slice_webvtt`, shifted by `media_origin_seconds` (the GOP-lead
   comment at the call site explains why the *session's* origin and not the
   requested offset — do not change that);
3. on a **miss**, fires `warm_vtt` and answers `WEBVTT\n\n` with `no-store`.

So serving is already windowed. **Materialization is not**: `read_cached_vtt`
returns bytes only once the *entire* sidecar exists, and producing it
([`crates/plurxd/src/subtitles.rs`](../crates/plurxd/src/subtitles.rs)
`:256`) is one ffmpeg pass over the whole source — minutes on a large MKV
over a NAS. The empty-segment fallback exists because AVPlayer gives a
subtitle segment about two seconds and blocks the muxed video while it
waits; the comment at `hls.rs:6051` is the constraint the whole design
lives under. Read it in full before touching anything.

The result is the user-visible defect: the first minutes of a large file
play with no subtitles, and the client learns they have arrived only by
re-fetching a `no-store` segment on its own schedule. Nothing tells it when.

### 2.2 The sidecar cache machinery (`subtitles.rs`)

Worth knowing because M2 layers in front of it rather than replacing it:

| Mechanism | Value at `e1876cce` |
|---|---|
| Cache key | `(file id, stream index, size, mtime)` via `vtt_name` |
| Sidecar cap | `MAX_SIDECAR_BYTES` = 8 MiB |
| Extraction timeout | `EXTRACTION_TIMEOUT` = 600 s |
| Dedup | one flight per cached path (`extractions()` / `warmups()`) |
| Negative memo | `NEGATIVE_TTL` = 120 s, `MAX_NEGATIVE_ENTRIES` = 128 |
| Cache prune | `MAX_ENTRIES` = 256, trim to `TRIM_TO` = 224 |

Two racing misses write identical bytes to a temp name renamed into place;
the loser's rename is a no-op. Keep that property in anything you add.

### 2.3 The control protocol facts you build on

[`crates/plurxd/src/playback_control.rs`](../crates/plurxd/src/playback_control.rs):

- `DeliveryView` (`:653`) is the per-response delivery projection. It is
  `#[serde(deny_unknown_fields)]`, so a new field is a protocol change.
  `producer_decision` is the precedent to copy exactly:
  `#[serde(default, skip_serializing_if = "Option::is_none")]`, and its doc
  comment's discipline — absence means *"not classified here", never
  "healthy"* — because an older peer relaying a response will not carry it.
- `PlaybackDemandSnapshot` (`:1573`) is the bounded set of client facts
  accepted with one control **sequence**: `position_ms`, `seek_target_ms`,
  `buffered_through_ms`, `render_state`, and the full `ClientSelection`
  including `subtitle: { mode, track }`. Its own doc comment says it exists
  precisely so "later quality, subtitle, and handoff work" does not
  reconstruct state from media fetches. This programme is that later work.
- Sequences are totally ordered per control epoch and stale sequences are
  rejected. That ordering is the cancellation authority §5 needs — nothing
  new has to be invented to know which seek is newest.

Seeks on remux/transcode restart the server-side stream at the new offset —
the web player says so in as many words
([`crates/plurxd/src/web/index.html:9043`](../crates/plurxd/src/web/index.html))
and tokens its own seeks (`_seekToken`, `:7442`) so only the last attaches.
The **server** has no equivalent: every restart it is asked for starts work.

### 2.4 What #700 and #726 already provide

- Timeline annotations with provenance/confidence and permanent manual-win,
  on both store backends (`store/timeline_annotations.rs`,
  `store/hiqlite_timeline_annotations.rs`), presented as one index beside
  the packed `FragmentIndex`.
- The analysis queue and its API (`http/mod.rs:214–225`): force, jobs,
  retry, cancel, summary; per-node coverage; `plurx_analysis_queue_depth`.
- Staged generations (#726): `media_session_preparations` and the four
  `MediaSessionStore` methods — `prepare_media_session`,
  `staged_media_session_for_playback`, `commit_media_session_preparation`,
  `abort_media_session_preparation` — a compare-and-swap prepare/commit/abort
  over a preparation ledger, **one preparation slot per playback**, on both
  backends. The predecessor is recorded at prepare time and never re-read at
  commit. §6 builds on this and must not weaken it.
- Marker instrumentation on all three clients — but every prewarm callsite
  emits a hard-coded `"miss"` and no `"hit"` producer exists, so
  `plurx_playback_marker_prewarm_hit_ratio`
  ([`telemetry.rs:239–248`](../crates/plurxd/src/telemetry.rs)) is pinned at
  zero. [MARKER-PREWARM-HANDOFF.md](MARKER-PREWARM-HANDOFF.md) builds the
  consumer; §8.2 forbids the shortcut.

---

## 3. Contract — subtitle readiness in `DeliveryView` (M1)

The client should be told whether subtitles are ready, warming, or
unavailable, instead of inferring it from an empty segment.

Add to `DeliveryView`:

```rust
/// Readiness of the subtitle track the client's selection names, when this
/// server evaluated it. Absent means "not classified here", never "ready" —
/// an older peer relaying a response has no such field. Values:
/// `ready` (cached text covering the demand window is servable now),
/// `warming` (materialization in flight; empty segments until it lands),
/// `unavailable` (bitmap-only, no such track, or extraction failed and the
/// negative memo is live).
#[serde(default, skip_serializing_if = "Option::is_none")]
pub subtitle_readiness: Option<String>,
```

Rules, each with its reason:

- **Populated only when the request's `selection.subtitle.mode` names a
  native text track.** Burn-in subtitles are video; their "readiness" is the
  producer's, already reported. Reporting on a track nobody selected is
  noise the client would have to ignore.
- **`ready` means the demand window, not the whole track**, once M2 lands.
  Until M2, `ready` ⇔ the whole-track sidecar is cached — state it that way
  in the field's doc so the meaning tightens rather than changes.
- **`unavailable` carries no prose cause on the wire.** The distinctions
  (bitmap vs. missing vs. failed) are already in logs and the 415 message;
  the client's behaviour is the same for all three: stop retrying. Mirroring
  `producer_decision`'s "absent rather than guessed" note on the VOD arm.
- **Three values, spelled as strings, closed set documented in the doc
  comment** — the same shape as `producer_state`. A client that sees an
  unknown value treats it as absent.

Wire compatibility: the field is optional-additive on serialize and
deserialize; `deny_unknown_fields` on the struct means an **older server**
rejecting a **newer relayed response** is impossible only because relays
re-serialize their own view — verify the relay path
(`action_is_believable` and friends) tolerates the field being absent from a
peer, which the `producer_decision` precedent already proves out. Add the
mirror test beside `producer_decision`'s.

Client consumption is deliberately out of this milestone: the field's first
consumer is M2's retry direction, and shipping the field first makes M2's
behaviour observable while it is being built. No client code changes → no
build-number bumps in M1.

---

## 4. Contract — bounded forward materialization (M2)

### 4.1 The design

Layer a **windowed extraction** in front of the whole-track sidecar, driven
by the segment requests the client is already making:

1. A subtitle segment request that misses the whole-track cache computes the
   demand anchor — that segment's window — and ensures a **window sidecar**
   covering `[anchor, anchor + WINDOW_SECONDS)` exists or is in flight, via
   `ffmpeg -ss <anchor - PAD> -to <end>` extraction of that span only. On a
   seekable container this is seconds, not minutes: `-ss` before the input
   is an index seek, and the span demuxed is bounded.
2. The handler then races the **window** sidecar against the same
   publication deadline, slicing with the existing `slice_webvtt`. Hit →
   real cues within AVPlayer's budget after the first couple of segments.
   Miss → the existing empty `WEBVTT\n\n` fallback, unchanged.
3. The whole-track warm (`warm_vtt`) still fires exactly as today. When it
   completes, it wins: window sidecars for that fingerprint are dead weight
   and are pruned. Windows are a bridge over the head of playback, not a
   replacement steady state — the whole-track sidecar stays authoritative
   because `stream::subtitles_vtt` and burn-in both need it anyway.

Bounds, each with its reason:

- `WINDOW_SECONDS`: **default 200 s** (Paul, 2026-09-01), delivered as a
  bounded server setting following the analysis knobs' shape — a
  `bounded_*` clamp over the stored string with the default as a named
  constant (see `bounded_analysis_lease_secs` in
  [`fragment_index_cluster.rs`](../crates/plurx-core/src/store/fragment_index_cluster.rs)),
  surfaced beside `analysis_lease_secs` in the settings API. Clamp to
  [30 s, 900 s]: below 30 s a window dies within one AVPlayer retry cycle;
  above 900 s the extraction stops beating the whole-track warm on NAS
  sources and the bridge loses its point. Playback-lab measurement remains
  the validity check on the default, not the source of it.
- **At most one window flight per session** (reuse the `warmups()` dedup
  shape keyed by `(fingerprint, window_start)`). A seek storm must not fan
  out window extractions — M3's latch gates this too.
- **Forward only, from demand.** No speculative extraction ahead of a
  position the client has not asked about: "bounded forward … from client
  demand" is the specification's own wording, and speculation is how a
  20-seek storm turns into 20 ffmpeg spawns.
- Window sidecars live beside the whole-track cache with the same
  fingerprint discipline (size+mtime) and count toward a small separate cap;
  they are cheap and disposable.

### 4.2 What must not change

- **The empty-segment fallback stays** until the window path demonstrably
  answers within the deadline, and stays even then for the first-touch race.
  Removing it turns healthy HDR and H.264 streams into a black screen
  (§8.2).
- **`slice_webvtt`'s `media_origin_seconds` shift** stays exactly as is —
  the GOP-lead scar at the call site is why.
- **`stream.rs::subtitles_vtt` is untouched.** The `<track>` route is
  whole-track by contract; its consumers download once.
- **No advertised interval starts returning 404.** The playlist keeps
  advertising the full timeline; windowing lives entirely behind the
  segment handler. An advertised-but-unwindowed segment answers empty, as
  today — that is the "avoidable 404" criterion satisfied by construction.

### 4.3 Readiness gets truthful

M1's field now reports `ready` when the *demand window* is servable
(window sidecar or whole-track hit), `warming` while either flight is up.
The client's retry loop becomes directed: re-fetch the empty segment when a
control exchange reports `ready`, instead of on a guessed timer. That client
change is small, per-platform, and rides M2 (build bumps: see §10).

---

## 5. Contract — seek coalescing and cancel-by-sequence (M3)

The specification: a 20-seek storm starts work only for the settled target.
The web player already tokens its client side; the server starts work for
every restart it is asked for.

### 5.1 The latch

Per playback (control-session scoped, in the rolling actor where
`PlaybackDemandSnapshot`s land in sequence order):

- Each accepted snapshot updates the **settled target**: `seek_target_ms`
  when present and `render_state` is `Seeking`, else the anchor is settled.
- Expensive production triggered on behalf of a seek — session restart at an
  offset, M2 window extraction, the prewarm plan's production once it
  lands — is keyed by the control
  **sequence** that requested it. Before the work commits resources (spawns
  ffmpeg, inserts a session row), it checks the latch: a newer sequence with
  a different target means this work is obsolete — skip it; if already
  running and cancellable, cancel it.
- The sequence ordering and stale-rejection already in the actor are the
  authority. Do not add a second clock or a debounce timer as the primary
  mechanism — wall-clock debouncing trades the storm for added latency on
  every single honest seek, and the sequence comparison is free.

### 5.2 Boundaries

- **Cancellation reaches production that has not published.** A producer
  that already published segments for an obsolete target is retired through
  the existing lifecycle, not killed mid-write — atomic publication rules
  are older than this plan and stay.
- **Subtitle window extraction joins the same latch** rather than growing
  its own: one notion of "the target the client actually wants".
- The client-side tokens stay; they solve attach-ordering inside one
  browser, which the server cannot see.
- The disabled VOD seek-storm browser acceptance
  ([VOD-SEEK-STORM-ACCEPTANCE-HANDOFF.md](VOD-SEEK-STORM-ACCEPTANCE-HANDOFF.md))
  is **its own restoration effort with its own handoff — do not adopt it
  here**, but do not break its assertions either: its 20 native seeks with
  no replacement session must remain satisfiable. M3's acceptance test is
  protocol-level (below), cheap, and CI-safe.

---

## 6. Contract — burn changes join the existing successor (M4)

A subtitle burn-in change must ride an existing successor action rather than
opening a competing one. The vehicle is #726's preparation ledger — one
preparation slot per playback is already the invariant — and the join is a
rule about what happens when a burn change arrives while that slot is
occupied:

- **Slot empty:** the burn change prepares a successor whose recipe is the
  current recipe plus the burn delta. Today (item 7 / M6 not yet built) this
  is the only writer; the rule still pays for itself by making the slot the
  single door.
- **Slot occupied:** the burn change must not open a second preparation and
  must not steal the slot by blind abort. Join = abort-and-re-prepare **as
  one guarded transaction**: the re-prepare carries the aborted
  preparation's recipe merged with the burn delta and the same predecessor;
  if the abort loses (the preparation committed first), the burn change
  re-prepares against the *new* current generation. Gate the outcome in the
  SQL, not in a predicate over the return value — that distinction was paid
  for during #726's review.
- **Subtitle work never replaces video independently** (§8.1): the joined
  successor advances the pointer through the ordinary commit path only. A
  burn change alone never commits; it changes what the next commit carries.
  This is the criterion a naive "successor action for a burn change" breaks,
  and it is the reason this milestone is last of the four.

Scope honesty: M6's replacement transaction and Auto policy do not exist at
this baseline, so M4 delivers the store-level join rule, its contract tests
on **both** backends, and the server-side wiring where burn selection
changes arrive. Full client-visible make-before-break burn switching is
M6's; when M6 lands it inherits a slot that already refuses competitors.
If building this reveals the join genuinely requires M6's transaction
first, stop and flag it — do not build a third mechanism.

---

## 7. Prewarm — moved to its own plan

The marker-destination prewarm bullet is separable from everything above and
is now its own document: [MARKER-PREWARM-HANDOFF.md](MARKER-PREWARM-HANDOFF.md)
(split per Paul, 2026-09-01). It carries the design, the honest-`"hit"`
metric contract, and its acceptance. It builds best after §5's latch exists;
nothing here waits on it. §8.2's metric guardrail still binds both plans.

---

## 8. Guardrails

### 8.1 Acceptance, quoted from §13.8 — not negotiable

> a 20-seek storm starts work only for the settled target; subtitle work
> never replaces video independently; no advertised subtitle interval
> returns an avoidable 404; a forced rebuild leaves the current index
> serving until atomic publication; queue counts/stages/progress agree with
> the claimed job; fixture seasons yield frame-refined repeatable markers;
> ambiguous matches are not auto-skipped; clicking/auto-skipping lands at
> the stored exact end time and a seek-back is recorded for detector
> evaluation.

The last five are #700's, already satisfied and to be kept satisfied. The
first three are this plan's bar: the storm is M3's, video-independence is
M4's (and constrains M2 — subtitle window extraction never touches the
video pointer), and the no-avoidable-404 is M2's by construction (§4.2).

### 8.2 Non-goals — do not do these

- **Do not build detectors.** Deciding where an intro *is* stays gated on a
  labelled corpus and a false-positive-weighted evaluation, separately.
- **Do not touch recovery decisions in `playback_control.rs`.** Recovery
  authority is the server's `ControlAction`; a terminal verdict is
  recipe-scoped, not source-scoped (#721→#723). Subtitle work must not
  acquire an opinion about recovery.
- **Do not make subtitle work able to replace video.** §8.1 forbids it; it
  is the failure mode a naive burn-successor produces.
- **Do not redefine an existing metric's meaning** to make it non-zero
  ([MARKER-PREWARM-HANDOFF.md](MARKER-PREWARM-HANDOFF.md) §4 carries the
  full contract).
- **Do not remove the empty-segment fallback** without replacing the
  property it protects: a subtitle route that blocks past AVPlayer's ~2 s
  deadline turns healthy HDR and H.264 streams into a black screen.
- **Do not resurrect the disabled VOD seek-storm browser case** as this
  plan's acceptance — it has its own handoff and its own root cause.
- **No test, lint, or typecheck steps in ansible roles**, and **no deploys
  to the nodes** — deploys are Paul's.

---

## 9. Milestones

Each ends in a runnable command or an observable fact. Command names
re-verified against the tree at build time.

### 9.1 M1 — readiness field

`DeliveryView.subtitle_readiness` per §3, populated on both Live and VOD
arms, with the relay-tolerance test mirroring `producer_decision`'s.

**Acceptance:** protocol tests cover present/absent/unknown-value handling
and the older-peer relay path; a manual session against a large MKV shows
`warming` → `ready` across control exchanges with zero client changes.

```bash
cargo test -p plurxd playback_control   # protocol/model suite, incl. new field
make check                              # the local gate
```

### 9.2 M2 — bounded materialization

Windowed extraction per §4, fallback intact, whole-track warm unchanged,
readiness truthful per demand window; the small per-client directed-retry
change.

**Acceptance:** a fixture whose whole-track extraction is artificially slow
serves real cues within the publication deadline from the second segment
request onward; the first-touch race still answers empty, never blocks, and
never 404s an advertised segment; window flights are deduplicated under
concurrent segment requests.

```bash
cargo test -p plurxd subtitles          # extraction, window, dedup, fallback
node --test tests/playback              # web-side directed retry
```

### 9.3 M3 — seek coalescing

The settled-target latch per §5, gating session restarts, window
extraction, and (later) prewarm.

**Acceptance:** a scripted 20-seek storm at the protocol level results in
production started for exactly one target — the settled one — with obsolete
unpublished work skipped or cancelled by sequence comparison, and equal or
lower sequences rejected as before. The VOD suite's steady and
suspend/resume cases stay green.

```bash
cargo test -p plurxd seek_coalesc       # the storm test this milestone adds
scripts/playback-lab doctor             # rig sanity where hardware allows
```

### 9.4 M4 — burn-join

The occupied-slot join rule per §6 on both backends, wired where burn
selection changes arrive.

**Acceptance:** contract scenarios prove — on SQLite and on three real
voters — that a burn change against an occupied slot yields one surviving
preparation carrying the merged recipe; that a lost abort re-prepares
against the new generation; and that no path commits a pointer advance from
subtitle work alone.

```bash
cargo test -p plurx-core --features hiqlite-contract-tests \
  --test store_contract media_session   # ~3.5 min, three real voters
```

### 9.5 M5 — prewarm

Moved to [MARKER-PREWARM-HANDOFF.md](MARKER-PREWARM-HANDOFF.md); its
acceptance lives there. Not a prerequisite for anything in §9.1–§9.4.

---

## 10. How to work in this repo — the parts that bite here

The full pipeline is [DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md);
these are the rules this plan predictably collides with:

- **`make check`** is the local gate. Do not repeatedly run the broad unit
  suite; the pipeline is built around not doing that.
- **`validation/points.toml`** governs every file — new files here map to
  `playback.pipeline` / `server.api`; a new `scripts/` file with no
  functionality point fails `validation-lint` late.
- **`make history-check`**: a fix-shaped subject demands regression evidence
  in `validation/regressions.d/`, and a client fix needs an anchor row in
  `tests/client-fixes.toml` naming a real corrective commit — land the fix
  first, anchor second.
- **Any change under `clients/android/app/src/main/` or
  `clients/apple/Sources/` forces a build-number bump that must clear
  current `main`**, not the branch point. M1, M3, and M4 need no client
  code; M2's directed retry and M5's client emissions do — batch client
  edits within a milestone to pay the bump once.
- **CI runs only on PRs based on `main`** — a stacked PR's green is a lie.
  **Merge with merge commits, never squashes.**
- Migrations, if M4's join needs one, append to **both** backends' lists —
  #726 landed as SQLite v39→v42 / Hiqlite v20→v23 with every shared census
  summed, and the three-voter contract run is what finds what SQL review
  cannot.

## 11. Settled questions — answered by Paul, 2026-09-01

1. **`WINDOW_SECONDS`** — "that range is fine. 200? … maybe make it
   configurable in settings." → default 200 s, bounded server setting; §4.1
   carries the shape.
2. **Prewarm extraction** — "you can split it into its own doc" → split as
   [MARKER-PREWARM-HANDOFF.md](MARKER-PREWARM-HANDOFF.md); §7 and §9.5 are
   pointers.
3. **Readiness wire format** — no strictly-parsing `DeliveryView` client
   known to him ("but that doesn't mean much") → proceed with the three
   closed-set strings; M1's relay-tolerance test is the guard the answer's
   uncertainty asks for.
