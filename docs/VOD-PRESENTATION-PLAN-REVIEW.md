# VOD presentation plan review — the destination is right, six contracts are not buildable

**Status:** review complete · **Reviews:**
[VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) at `8fe68d56` ·
**Verified against:** `origin/main` @ `9cace96e` · **Written:** 2026-08-23 ·
**Verdict:** request changes · **Outcome:** review only; no product code was
changed

Companion to [VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) (the plan
under review), [SEGMENTER-PLAN.md](SEGMENTER-PLAN.md) (the copy-cut contract),
[PERF2-PLAN-REVIEW.md](PERF2-PLAN-REVIEW.md) (the measured restart-timestamp
seam), and [CLUSTERING-PLAN.md](CLUSTERING-PLAN.md) (the accepted takeover
contract). This document says what must change before an implementing agent
treats the VOD plan as a handoff. It does not replace the plan and it does not
authorize M1.

**How it was reviewed.** The branch was read from an isolated worktree, never
by switching the dirty primary checkout. The normative contract in §2 was
traced through the current copy segmenter, ffmpeg argument builders, cache
lifecycle, HLS handlers, and clustering plan. Commit citations in the plan
were resolved against the repository, and `git diff --check` passed. Findings
below cite the plan by section and current sources by file and line at
`9cace96e`.

**Severity legend:** **BLOCKING** means implementing the plan literally would
produce an invalid media timeline, an unimplementable interface, a false cache
completion, or a conflict with an already accepted contract · **SHOULD-FIX**
means the rollout or documentation contract breaks under ordinary version
skew but does not invalidate the server architecture · **NIT** means editorial
repair.

## 1. Verdict — keep the VOD destination, rewrite the plan inputs and lifecycles

The architectural diagnosis is persuasive. A known-length film should not be
presented as a live broadcast merely because its bytes are produced lazily.
An immutable full-timeline playlist removes the live edge, playlist mutation,
session-relative origin arithmetic, and ordinary prune/reap 404s from the
client contract. Client opt-in plus a server gate is the right migration seam,
and delaying the deletion pass until evidence earns it is the right rollout
discipline.

The plan is not buildable as written. Its copy plan assumes a cheap packet
index contains information the current cut policy obtains only from the
production-shaped fragmented MP4. Its repositioning contract assigns stable
segment names without assigning stable timestamps inside those segments. Its
sparse bitmap is asked to mean both working-set presence and eventual cache
completion, even though eviction prevents both meanings from being true at
once for a large title. The blocking GET has a deadline and no deadline in the
same paragraph. The cluster guardrail points to a document that does not exist
and does not reconcile the accepted generation/discontinuity contract. Finally,
session resurrection has no terminal state, so an explicitly retired handle
can come back through an old autonomous media fetch.

These are corrections to the proposed shape, not reasons to return to live
HLS. Keep the objective, the client inventory, the non-goals, and the staged
rollout. Rewrite §2.2–§2.5 and the M0–M3 acceptance gates before product work.

| Finding | Class | Required before implementation |
|---|---|---|
| B1 · the RAP index cannot replay `CutPolicy` | Blocking | Before revised M0 completes |
| B2 · repositioned producers reset media timestamps | Blocking | Before M1 interface freeze |
| B3 · rolling eviction cannot produce an honest complete bitmap | Blocking | Before M2 |
| B4 · segment wait is both bounded and unbounded | Blocking | M0-P3 must decide it |
| B5 · immutable VOD conflicts with accepted cluster takeover | Blocking | Before M2 |
| B6 · resurrection lacks terminal lifecycle semantics | Blocking | Before M3 |
| S1 · M8 deletes the old-server compatibility path | Should-fix | Before client adoption |

## 2. Blocking findings

### B1 — the proposed RAP index cannot replay the current copy-cut policy (§1, §2.2, M0-P1, M1)

The plan says a persisted index produced by this class of probe is enough to
replay the complete copy segmentation:

```bash
ffprobe -select_streams v:0 \
  -show_entries packet=pts_time,flags,size SOURCE
```

It is not enough for the policy in the tree.

`fmp4::classify` does not accept a generic keyframe flag as proof of a clean
cut. It opens the first video sample, reads the first VCL NAL, distinguishes
HEVC IDR from CRA/BLA, and checks whether the track fragment contains leading
pictures ([`fmp4.rs`](../crates/plurx-core/src/fmp4.rs):1550-1609). A packet
row with `flags=K` does not carry those facts.

The byte input is different too. `CutPolicy::cut_before` compares
`pending_bytes` with the 48 MB ceiling
([`fmp4.rs`](../crates/plurx-core/src/fmp4.rs):1762-1786), and `Segmenter`
increments that counter with `fragment.len()` after ffmpeg has remuxed the
video and copied or encoded the selected audio (`:2322-2339`). Source packet
sizes do not equal those recipe-specific output fragment sizes. Track choice,
AAC conversion, A/V correction, and mux overhead can change the boundary.

This is why `scripts/gop-census` launches the production-shaped ffmpeg copy
pipe and parses its `moov`, `moof`, and `mdat`; its ffprobe call discovers only
the stream codec. The script's census input is the remuxed fragment stream,
not the proposed header index
([`gop-census`](../scripts/gop-census):264-312, `:352-374`). The plan's claim
that the script already walks the proposed structure is false.

`rap_byte_offset` also names no restart mechanism in the current producer.
The copy path seeks by time with `-ss`; no code consumes a packet byte offset
as a container-safe demux entry point
([`mod.rs`](../crates/plurx-core/src/transcode/mod.rs):1164-1173,
`:1393-1416`). A packet position is not automatically a Matroska cluster or
MP4 sample-table entry that ffmpeg can open as a new input.

**Required correction.** Choose one implementable plan input:

1. Persist a **recipe-specific fragment index** produced by the exact copy
   pipe, carrying classification, duration, output byte count, and a proven
   demux restart coordinate; or
2. Replace `CutPolicy` with a policy whose complete inputs really are
   available from a file-only packet index, then re-prove the fidelity and
   byte ceilings the segmenter plan protects.

Move replay equivalence from M1 into M0. Timing an index that cannot determine
the answer is not a useful feasibility result. Revised M0 acceptance must
prove, over the fixture corpus and representative real files, that
`plan(source, recipe)` produces the same boundaries and EXTINFs as the current
copy pipe before it measures how long the index takes.

### B2 — a stable filename does not make a repositioned segment film-time-stable (§2.1, §2.2, §2.4, M2)

The plan requires timeline zero to be file time zero and says a producer may
be killed and restarted at any demanded plan entry. It defines stable names
for the restarted output but never defines the timestamps inside the media.

Today a copy restart applies `-avoid_negative_ts make_zero`
([`mod.rs`](../crates/plurx-core/src/transcode/mod.rs):1403-1416). The fMP4
merger then preserves the first fragment's `tfdt` as the published segment's
base decode time ([`fmp4.rs`](../crates/plurx-core/src/fmp4.rs):1893-1944).
A new producer started around film minute 30 therefore emits a fragment whose
decode timeline begins near zero. Renaming it `seg00900.m4s` does not move its
`tfdt` to minute 30.

The transcode path has the same class of problem: a new ffmpeg process seeks
the input with `-ss` and starts a new HLS muxer
([`mod.rs`](../crates/plurx-core/src/transcode/mod.rs):905-909). The live probe
recorded in [PERF2-PLAN-REVIEW.md](PERF2-PLAN-REVIEW.md) §2 R1 already proved
that `-start_number` changes filenames and media sequence, not PTS/DTS. That
review required a discontinuity for a restart. This plan cannot add a new
discontinuity after publication because its VOD playlist is immutable.

The initialization section has the same ownership question. A repositioned
copy process can emit a new `init.mp4`; serving it at the old immutable URI is
safe only if its bytes and track configuration are proven identical to the
one every cached segment expects.

**Required correction.** Specify a media-time contract, not only a naming
contract:

- every materialized segment's video and audio PTS/DTS or fMP4 `tfdt` must be
  rebased to the planned film-time start;
- every process generation must use an initialization section compatible with
  every segment already stored under the recipe;
- regeneration of an evicted URI must either be byte-identical or be served
  under a new URI that the immutable playlist already names; and
- sequence numbers inside fMP4 fragments must be deterministic or explicitly
  declared irrelevant and proven so on every target player.

M1/M2 acceptance needs a noncontiguous test: materialize the first segment,
kill the producer, materialize a far segment first, fill both neighbors from
separate process generations, and prove continuous video/audio timestamps and
playback on hls.js, AVPlayer, and Media3. A filename assertion is insufficient.

### B3 — the bitmap cannot mean both current presence and eventual completion (§2.4, M2)

The title store permits holes, evicts individual segments outside reader
windows, and declares the generation complete when its bitmap is complete.
Those rules cannot all hold for a title larger than the working-set budget:

- If eviction clears the bit, a forward watch of a large title discards early
  segments before late ones arrive, so the bitmap never becomes complete.
- If eviction leaves the bit set, the bitmap means *produced once*, not
  *present now*, and `complete_cache_entry` publishes a directory with holes.

This is not an edge-sized title. Current defaults are 2 GB per live session
and 8 GB across session scratch, while the pretranscode cache is a separate
50 GB budget ([PERF-PLAN.md](PERF-PLAN.md):1675-1678). The plan itself uses a
69 Mb/s 4K remux as its reference case; two hours at that rate is about 62 GB.
Neither the scratch budget nor the default completed-cache budget can hold
that title in full.

The claim that bitmap completion gives the copy path a cache for the first
time therefore needs an admission qualifier. Some remuxes can complete; some
cannot. The current prose promises the second watch is pure VOD for all copy
titles.

**Required correction.** Separate three facts in the store schema and
lifecycle:

1. **Planned** — the segment belongs to this immutable rendition.
2. **Materialized now** — the bytes exist and may be served.
3. **Durably admitted** — the complete rendition fits the cache budget and
   has been atomically published as a cache hit.

Name the working-set and completed-cache budgets separately, define whether
promotion reserves space before backfill, and state the honest behavior when
the full title cannot fit. M2 acceptance must include a synthetic title larger
than both scratch budgets and assert that eviction never produces a false
cache hit, unbounded backfill loop, or missing segment under a completed row.

### B4 — the blocking segment GET has a deadline and no deadline (§2.3, M0-P3)

Outcome 2 says the GET is bounded by `playback.vod_block_secs`, default 15 s.
The next sentence says that when the budget expires while the producer is
healthy, the server waits until it can return 200. That request is unbounded,
and the client cannot perform the promised retry while the original response
remains open.

The distinction matters operationally. A VOD playlist exposes every segment
immediately, so a player or retrying intermediary can hold several future
requests. Without a hard deadline, per-session and global caps, disconnect
cancellation, and demand coalescing, a seek storm becomes an unbounded set of
tasks waiting on one producer.

**Required correction.** M0-P3 must choose one exact wire contract:

- a hard server deadline followed by a typed retryable response and named
  client retry behavior; or
- an intentionally open request whose only deadline is the measured client
  timeout, with server-side concurrency caps and cancellation on disconnect.

Do not specify `playback.vod_block_secs` until P3 determines which stack owns
the shorter timer. Whichever option wins, add tests for client disconnect,
producer death during the wait, ten concurrent requests for one segment, and
a 20-seek storm across distant segments.

### B5 — immutable VOD does not compose with the accepted cluster takeover contract (§7, M2, M7)

The stop-and-flag guardrail points to `CLUSTER-MEDIA-POOL-PLAN.md`, which does
not exist in this tree. The applicable document is
[CLUSTERING-PLAN.md](CLUSTERING-PLAN.md), and its accepted contract conflicts
with this one in two places.

First, a survivor rebuilds the request with its locally valid encoder,
pipeline, and ffmpeg digest; mixed nodes may produce protocol-compatible but
non-byte-identical output ([CLUSTERING-PLAN.md](CLUSTERING-PLAN.md):305-327,
§7 non-goal 4). Second, takeover advances media/discontinuity sequence and
uses a generation-specific fMP4 map (`§3.5`, `§6.9`). That is how the current
live playlist avoids reusing a URI for different bytes.

An immutable VOD playlist cannot discover a new generation-specific init URI
after node failure. Reusing the old URI violates the cluster plan's rule that
no identical segment URI may name different bytes. The VOD plan's persisted
session recipe and title store also do not name an owner, fence, proxy path,
or takeover rule, even though M7 flips the setting across the fleet.

**Required correction.** Decide the cross-plan contract before M2:

- keep each VOD rendition bound to a media owner and proxy all requests to that
  owner, with an explicit behavior when it dies;
- place rendition bytes in storage whose generation survives the node;
- require byte-identical production for every node eligible to own one
  immutable recipe; or
- declare VOD presentation unavailable in clustered mode until a revised
  takeover protocol is accepted.

Do not leave this to an implementation-time stop-and-flag. M2 defines the
store identity, which is the decision cluster takeover depends on. Re-review
acceptance needs a power-pull while a VOD copy and VOD transcode are active,
proving that no immutable URI changes bytes and no playlist mutation is
required.

### B6 — resurrection needs terminal states, not only a persisted recipe (§2.5, M3)

The plan says a playlist or segment GET naming a reaped session resurrects the
handle from its persisted recipe. It does not distinguish an idle reap from an
explicitly terminal session.

That distinction is load-bearing because HLS child requests are autonomous
capability fetches. An in-flight request from a predecessor can arrive after a
quality change, typed stall reopen, client DELETE, admin stop, authorization
revocation, or file removal. If every missing in-memory session with a recipe
is resurrectable, that late request reattaches demand to a handle the server
deliberately retired and can restart production after resource release.

The proposed TTL has a second contradiction: `title duration + slack` from
creation does not guarantee the promised pause-for-an-hour resume for a short
episode. The plan does not say whether authorized media access refreshes the
TTL or what exact pause duration is supported.

**Required correction.** Persist a lifecycle, not only inputs:

```text
active ── idle reap ──▶ dormant ── authorized media GET ──▶ active
  │
  ├── explicit DELETE / supersession / admin stop ──▶ terminal
  ├── access revoked ────────────────────────────────▶ terminal
  └── file identity invalidated ─────────────────────▶ terminal

terminal ── any old capability GET ──▶ 404/410, never resurrection
```

Define which transition deletes media demand, whether dormant TTL is sliding,
and the exact maximum supported pause. M3 acceptance must prove idle reap
resumes, while explicit DELETE, supersession, admin stop, revoked access, and
source replacement never resurrect.

## 3. Rollout finding

### S1 — M8 deletes the compatibility behavior §2.7 promises

Section 2.7 says a new client on an old server receives the legacy shape and
keeps its recovery machinery. M8 then deletes that machinery from all three
clients. After that release, a client updated before its server can receive
`vod: false` without the code required to handle the live presentation.

A fleet flip proves the operator's current nodes are ready; it does not make
every older server a future mobile client may contact disappear. If plurx
intentionally supports only lockstep server/client upgrades, the plan must say
so and the client must show a typed minimum-server-version error.

**Required correction.** Pick one:

- retain a compact legacy playback arm while the server returns `vod: false`;
- negotiate a minimum server protocol and refuse playback with explicit
  upgrade guidance; or
- defer deletion until the supported server-version floor guarantees VOD.

The opt-in field is a migration seam only while both response shapes remain
actionable.

## 4. What survives review

Keep these parts unless evidence from the revised M0 contradicts them:

1. **VOD is the target presentation for known-duration titles.** The live
   contract is a defect generator, and the cache-hit counterfactual is strong
   evidence for changing it.
2. **NULL-duration files remain legacy live.** A complete plan requires a
   duration; the non-goal is honest.
3. **Client opt-in plus server gating remains the rollout seam.** It composes
   old and new deployments until S1's deletion boundary.
4. **Encoders, ladder numbers, DV policy, admission, and offline packaging stay
   out of scope.** Changing them would confound the presentation result.
5. **Client adoption precedes deletion.** Recovery code is removed only after
   the replacement contract survives the named devices and the M7 evidence
   window.
6. **The stopgaps remain independent work.** S1–S4 in the plan are current
   defects and should not wait for the architecture rewrite.

## 5. Required rewrite and re-review gate

Revise the plan in this order, because each step constrains the next:

1. **Replace M0-P1 with a plan-equivalence feasibility probe.** Decide what
   index can actually reproduce the copy segmenter, then measure its cost.
2. **Write the media-time contract.** Specify PTS/DTS/tfdt, init generation,
   and regeneration identity before defining storage or scheduler interfaces.
3. **Split store presence from cache completion.** Define budgets, admission,
   bitmap meanings, and over-budget behavior.
4. **Resolve cluster ownership.** Name the actual cluster plan and choose how
   immutable bytes survive or fail over.
5. **Finish the request and handle lifecycles.** Decide blocked-fetch timeout,
   cancellation, dormant resurrection, terminal tombstones, and TTL refresh.
6. **Repair rollout compatibility.** State the server-version floor or retain
   the legacy client arm.

The revised plan is ready for re-review when all of these statements are true:

- M0 can prove `plan == current copy output` before measuring plan cost.
- A producer can materialize segment N first and its internal media timestamp
  equals the plan's film-time start.
- Eviction cannot make a cache row complete while any referenced object is
  absent, and an over-budget title has a bounded honest outcome.
- Every blocked GET ends through one named deadline/cancellation path.
- A node failure cannot make an immutable URI expose different bytes.
- Only an idle-dormant handle can resurrect; terminal handles stay terminal.
- A client receiving `vod: false` either retains the legacy path or produces a
  deliberate version error.

## 6. Documentation corrections

- The orientation says M0 can change two decisions, D1 and D6. M0-P1 also
  decides D4; the count is three.
- Replace the nonexistent `CLUSTER-MEDIA-POOL-PLAN.md` reference with the
  applicable [CLUSTERING-PLAN.md](CLUSTERING-PLAN.md) section after B5 is
  resolved.
- Cross-link this review from the plan status line while the verdict remains
  request changes, so an implementing agent cannot land on the plan without
  seeing its blockers.

**Disposition:** architectural direction approved · current contract rejected
for implementation · revise B1–B6 before M1 · resolve S1 before client
deletion.
