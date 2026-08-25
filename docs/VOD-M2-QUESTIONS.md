# VOD M2 — the questions, and how they were ruled

**Status:** every question ruled 2026-08-24; rulings implemented on
`agent/vod-m2c` · **From:** milestones M1–M2 of
[VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) ·
**Ruled by:** the M1/M2 reviewer · **Written:** 2026-08-24

Companion to [VOD-M0-ISSUES.md](VOD-M0-ISSUES.md) (what measurement found) —
this is *what implementation found, what was decided, and what was built*.

The questions are kept as asked, because the reasoning that produced them is
what a later reader needs in order to judge the answers. Each section now ends
with a **Ruled** block: the decision, and what it cost to implement.

## What was decided, in one place

| § | Question | Ruling |
|---|---|---|
| 1 | D6 and the playlist | One renderer, both arms. D6-B routes the encode through `fmp4::Segmenter`, so it changes the producer and not the playlist. |
| 2 | Which init `§2.2` governs | The **served** init. Promotion inputs are captured canonically at index time, so byte-identity holds by construction. Mid-film variation is an index-time refusal. Rewrite-on-reposition is prohibited. |
| 3.1 | The audio tail | Confirmed. D10 covers it: the plan outranks the segmenter for planned generations, `c58a4307` governs unplanned ones. |
| 3.2 | Stop or terminate | Confirmed as built. |
| 3.3 | Zero budget | Confirmed as built, with a settings-validation obligation for M3. |
| 4.1 | Durability | fsync members then directory **at admission only**; reconcile checks the recorded length. |
| 4.2 | Cleanup | A per-node sweep on the index job's tick. `delete_files` untouched. |
| 4.3 | Reconcile's cost | No change now; carried to the M3 list. |
| 4.4 | A missing init | Regenerate-verify-else-purge, enabled by the two stored digests. |
| 5 | Scope | M2 ends here. Review runs. Attachment opens M3. |

---

## 1. D6 is still open, and the playlist already assumed an answer

**What the plan says.** §9 D6: "Transcode EXTINFs nominal-2.0 (D6-A) unless
M0-P2 objects, then segmenter-exact (D6-B). **STILL OPEN** — the web half says
D6-A (§12.6) but P2 spoke for hls.js only, so this cannot be decided until the
AVPlayer and Media3 halves return. It gates M3's transcode serving, not M1 or
M2."

**What I built.** `SegmentPlan::playlist()` renders `EXTINF` from the plan's own
entry durations — **nominal**, D6-A. That is the only thing it *can* do without
media: the plan exists before any byte is produced, and a segmenter-exact
duration is by definition not knowable until the segment is cut.

**Why this is not simply "M2 chose D6-A".** The copy path's plan durations come
from the fragment index, so they are exact for copy renditions whether or not
D6 is settled. D6 is really a question about the **transcode** path, where the
plan asserts a 2.0 s grid the encoder is asked to hit and may miss. If the
device halves come back demanding D6-B, `playlist()` does not change — what
changes is that a transcode rendition's playlist cannot be rendered from the
plan at all, and must instead be assembled from produced segments, which is a
different artifact with a different lifetime and undoes "render once, store,
serve".

That is the part worth knowing now rather than in M3: **D6-B is not a tweak to
the playlist renderer, it is a different design for transcode renditions.**

**What I did meanwhile.** Nominal, from the plan, for both paths. The copy path
is exact by construction, so the risk is confined to transcode.

**Decision needed.** Either (a) confirm D6-A stands for transcode too and I keep
one renderer, or (b) tell me D6 is genuinely undecided, in which case I would
like to scope M3's transcode serving as *explicitly deferred* rather than build
a renderer that may be thrown away. I do not need the measurement result — I
need to know whether to plan for two artifacts or one.

**Ruled: (a), with a correction to the framing.** One renderer, both arms.

My premise was wrong, and usefully so. Plan §2.2's transcode paragraph already
says what D6-B is: "route the encode through the copy path's fMP4 segmenter,
which cuts the planned boundaries exactly." D6-B is a **production-side**
change — the encoder's output goes through `fmp4::Segmenter` so the emitted
segments land on the plan's grid — after which the plan-rendered nominal
`EXTINF` *is* segmenter-exact, because emitted duration equals planned
duration. "Render once, store, serve" survives both arms untouched.

So `SegmentPlan::playlist()` is the only playlist artifact for both paths,
whatever the device halves say. If they demand D6-B the M3 work item is "wire
the transcode encode through the segmenter" — not a second renderer, and not a
deferral.

**Cost: nothing.** No code changed.

---

## 2. "The generation's init" names three byte strings, and the identity rule
may be unsatisfiable

This is the one I would attack first.

**What the plan says.** §2.2: "The rendition's `init.mp4` is written by its
first process generation and stored as **the** init. Every later generation's
init must be byte-identical or the generation is refused with a typed
`producer_failed`."

**What is actually true.** Three distinct byte strings answer to "the
generation's init", and the rule does not say which one it governs:

1. **The muxer's init** — `ftyp` + `moov` as ffmpeg emits it at the head of the
   byte feed, surfaced as `fmp4::Unit::Init`.
2. **The promoted init** — (1) after `copyseg` mutates it, and this is what is
   actually written to `init.mp4`.
3. **The index pipe's init** — the video-only probe's, which carries no audio
   track at all. Different by construction, and the one `SourceIdentity`
   fingerprints.

**Why (2) is the problem.** `crates/plurxd/src/copyseg.rs:339` and `:351`:

```rust
match fmp4::promote_hevc_parameter_sets(&mut init, &fragment) { … }
match fmp4::promote_hdr10_static_metadata(&mut init, &fragment) { … }
```

Both take **the first fragment of that generation** and copy authored NAL units
out of its first video sample into `hvcC`. A repositioned generation's first
fragment is a *different fragment of the film*.

So the concrete failure: a rendition's first generation starts at segment 0 and
promotes parameter sets from the film's opening IDR. A viewer seeks; the
producer repositions to segment 300; that generation promotes from segment 300's
IDR. If the two IDRs carry different parameter sets — legal, and routine in
encodes that resend SPS/PPS per IDR with varying VUI — the two inits are not
byte-identical, and §2.2 refuses the generation with `producer_failed`.

The same applies to HDR10 static metadata, which is promoted from mastering
display and content light level SEI that need not appear on every GOP.

**The shape of it:** the byte-identity rule and the promotion path want opposite
things. Promotion exists to make the init describe *the media that follows it*.
Byte-identity requires the init to be *the same regardless of what follows*. On
exactly the files promotion exists for — HEVC, Dolby Vision, HDR10 — a
repositioned generation is the case where those two collide.

**What clause (d) actually measured.** `scripts/vod-plan-probe` hashes
`Unit::Init` and says so in as many words — "the same bytes `FragmentReader`
publishes as `Unit::Init` ... not 'everything before the first moof'". That is
byte string (1). The probe never calls either `promote_*` function. So M0
proved **(1)** is stable across generations including a seeked one, 9/9, and
measured **(2)** — the one written to `init.mp4` and served to a viewer — not
at all.

**What I measured, 2026-08-24.** A new probe,
`crates/plurx-core/examples/init-promotion-probe.rs`, runs the video copy pipe,
then promotes the init from *every clean fragment in the file* and compares.
Two useful results and one dead end:

| Fixture | Clean starts | In-band parameter sets | Promoted init |
|---|---|---|---|
| `closed-gop-2397` | 23 | byte-identical at all 23 | identical from all 23 |
| `clean-cra-2397` | 23 | byte-identical at all 23 | identical from all 23 |
| `open-gop-2397` | 1 | — | not measurable, one start |

The dead end: **promotion never fires on a synthetic fixture.** It is guarded on
`hvcC` carrying zero NAL arrays, and `fmp4.rs:718` names the real population —
"a few WEB-DL Matroska sources carry the 23-byte minimum hvcC record and put all
three parameter sets in their first sample". A libx265 encode always writes a
rich `hvcC`, so the corpus cannot exercise the branch directly, and neither
could M0's.

So the probe measures the thing that *decides* the answer instead: the
VPS/SPS/PPS NAL units in each candidate starting fragment's first sample, which
are exactly the bytes promotion copies. Identical NALs at every start means an
identical promoted init whether or not the branch fires.

**What that settles, and what it does not.** On a single-pass libx265 encode
with `repeat-headers=1`, every IDR carries the same parameter sets and §2.2 is
safe. That is a real result and it removes the most alarming reading — this is
not broken for everything.

It does not settle the population promotion exists for. Every fixture here was
produced by one encoder in one pass with fixed settings, which is the case
*least* likely to vary. The sources that reach the promotion branch at all are
WEB-DL remuxes, and the ones this most likely bites — a title assembled from
more than one encode, or one with per-scene parameter changes — are neither
synthetic nor in the corpus. M0 recorded the same gap for Dolby Vision: "no DV
source was reachable".

**What I did meanwhile.** Nothing. I reverted my attempt rather than pick one of
the three, because every choice writes a different rule into the store:

- governing (1) makes the check meaningful across generations but lets the
  written `init.mp4` differ from what was compared;
- governing (2) is what the viewer actually receives, and is the one that can
  refuse a legitimate reposition;
- governing (3) is stable and says nothing about what plays.

This blocks wiring init identity in M3.

**Decision needed.** Which byte string §2.2 governs. And, if it is (2), whether
a differing promoted init on a reposition is a `producer_failed` (current
reading) or a reason to *re-promote and rewrite* the stored init — which is
safe only if every already-materialized segment still decodes against the new
one, and I do not think that is knowable without measuring.

**Cheapest thing that would finish settling it:** run the probe against a real
HEVC WEB-DL from the library — one whose `ffprobe` shows an `hev1` sample entry
rather than `hvc1`, which is the shape that reaches the branch:

```bash
cargo run -p plurx-core --example init-promotion-probe -- /path/to/title.mkv
```

It prints one line per file and needs no fixtures. If in-band parameter sets are
byte-identical at every clean start there too, §2.2 is safe as written and I
will implement (2) and stop asking.

**Ruled: §2.2 governs (2), the served `init.mp4`** — it is the byte string a
viewer holds, and "byte-compatible media or fails loudly" is a promise about
what is served. (3) is already governed by its own rule; (1) is a mechanism
below, not the governed artifact.

**But not by comparing each generation's freshly promoted init to the stored
one.** That is the collision, and it is real in shape. The ruling dissolves it
instead: make promotion deterministic by capturing its inputs once,
canonically, rather than from whichever fragment a generation landed on.

The ruling also named a second failure mode I had noticed but under-weighted:
variation is not the only way this breaks. Mastering-display SEI need not
appear on every GOP, so a repositioned generation's first fragment may simply
*lack* it — promotion no-ops, the init comes out missing boxes the stored one
has, and a naive byte-compare refuses a generation whose media the stored init
describes perfectly. That case is likelier than variation.

And it refused the real-title run as a gate, correctly: under canonical
promotion, per-title variation cannot corrupt playback — it can only flag a
title out of the VOD presentation — so the probe measures prevalence, not
correctness. It belongs on the operator checklist beside the already-owed
ten-real-titles P0 protocol, not in the merge path.

**Built:**

1. `fmp4::PromotionInputs` — what the two promotions copy, captured away from
   the generation that uses it. Both `promote_*` functions split into
   extraction and surgery; today's signatures stay as wrappers, so the live
   path (a session with no plan, which has nothing else to go on) is unchanged.
2. The indexer captures the inputs from the film's opening clean fragment and
   stores them beside the index. The index pipe and the production pipe build
   their argv from the same `copy_video_args`, so the index pipe carries the
   same authored NALs — including in the DV Profile 5 case, where that argv
   deliberately omits the filter that would strip them.
3. The indexer compares every later clean fragment against the canonical
   inputs. A film whose starts disagree is `parameter_sets_constant: false`,
   not VOD-presentable, and keeps the legacy presentation. That is the ruling's
   step 3, and it turns a `producer_failed` mid-playback into a scan-time
   verdict nobody watches happen. The probe's comparison is now the indexer's;
   the example binary stays as the operator's one-shot tool.
4. `renditiondir::InitIdentity` holds both digests. Muxer-init mismatch is
   `producer_failed` — real pipeline drift, and the byte string clause (d)
   actually proved stable. Served-init mismatch is an assertion that cannot
   fire spuriously; if it ever does, promotion stopped being a pure function of
   stored facts. Re-promoting and rewriting is not offered.
5. `SEGPLAN_VERSION` 2 → 3, SQLite v29, sidecar v6. A v2 index cannot say what
   promotion must copy, and defaulting that to "constant" would present a title
   nothing has checked, so v2 rows are rebuilt rather than read.

**The test that matters** builds the minimal-`hvcC` shape promotion actually
fires on and asserts *the collision is real* — two landings, two different
served inits — before asserting the mechanism removes it. Without that first
assertion the second passes on any source promotion no-ops on, which is exactly
what my first fixture run did.

---

## 3. Three decisions I made to keep moving

Each of these is currently implemented and tested. Each reverses cheaply now.

### 3.1 S8 — the audio tail splits at the plan, not at the ceiling

**The collision.** `segplan::append_audio_tail` models a trailing audio-only
stretch as beginning where video ends and splitting at the policy ceiling.
`fmp4::end_of_stream_chunks` let the last *video* chunk absorb audio out to the
whole ceiling (`first_limit = ceiling.max(video_limit)`). For any tail between
50 ms and `ceiling − last_video_segment` — up to nine seconds on a typical
six-second final segment — the plan named a tail entry the producer never
emitted.

With the playlist now published as `VOD` and closed with `EXT-X-ENDLIST`, that
is a reader blocked on a segment nothing will ever produce, burning its
first-byte deadline at the end of every affected film, with `next_gap` never
closing.

**What I did.** Ruled it by D10 — the plan is normative, so the segmenter obeys
it. A generation with boundaries uses `video_limit` as the first chunk's
ceiling, so the last video segment stops where video stops and the tail is its
own entries. A generation *without* boundaries keeps the `c58a4307` rule
unchanged: fewer boundaries, and nobody is holding a playlist that says
otherwise.

I proved the test fails against the old rule first — one segment where the plan
named two.

**Why it needs your eyes anyway.** It changes what `finish()` emits for planned
generations, and `c58a4307` is a scar with a measurement behind it. I believe
D10 already decides this and no new ruling is required. I would rather that were
confirmed than assumed.

**Decision needed.** Confirm D10 covers it, or tell me the tail is the one place
the segmenter outranks the plan.

**Ruled: confirmed, D10 covers it.** The plan is normative and the segmenter
obeys the boundaries it is handed; `c58a4307` keeps governing exactly the
generations that have no plan, which is the population it was measured on.

One residual carried into M3: a planned tail entry's `EXTINF` and the emitted
segment differ by at most one audio frame (~21–32 ms), well inside RFC
rounding. Recorded in `SegmentPlan::playlist`'s doc so it is known rather than
rediscovered.

### 3.2 An indefinite hold gives the process back; an ahead hold keeps it

**The constraint, from your own code.** `transcode.rs` yields to a live viewer
by terminating rather than stopping, because "a stopped ffmpeg still holds the
hardware codec session, so the viewer this is yielding to would be blocked by a
process that is doing nothing."

**What I did.** `prodexec` answers the two hold classes differently:

| Hold | What clears it | Answer |
|---|---|---|
| `Ahead` | a reader advances — by itself, and soon | `SIGSTOP`, keep the codec session |
| `WorkingSetFull` | another rendition releasing bytes | terminate |
| `NoRoom` | another rendition releasing bytes | terminate |

A producer stopped for `Ahead` terminates the moment its reason changes to one
of the other two.

**The trade accepted.** Terminating costs a reposition when the hold does clear.
Keeping it costs a scarce hardware session held by a process that is doing
nothing and may never resume. I took the second as worse, on your comment's
authority.

**Decision needed.** Confirm, or tell me the codec session is cheap enough on
the target hardware that holding it beats a reposition.

**Ruled: confirmed as built.** SIGSTOP on `Ahead` is the plan's own instruction
(§2.4 reuses the `apply_ahead_window` machinery with request-driven demand);
terminate on capacity follows the yield-to-viewer precedent, and its cost is
exactly one reposition — which A1 made cheap and typed. On the fleet's hardware
the codec-session argument is real: one iGPU video block.

One asymmetry now recorded in `prodexec`'s module doc so it is not "simplified"
later: a **copy** producer holds no codec session, so for it a SIGSTOP costs
only memory and an idle read. The table is still right for both, but the
argument that *forces* it is the transcode path's.

### 3.3 An unset working-set budget means "not configured"

`WorkingSet { budget_bytes: 0 }` never holds anything. The other reading — zero
bytes allowed — stops every producer on the node the moment the config is
missing or misparsed. Stated because it is the kind of default that is obvious
until it is wrong.

**Decision needed.** None expected. Flagged so it is not a surprise.

**Ruled: confirmed.** `budget_bytes: 0` = not configured = never holds. It is
this repo's idiom (`playback.vod_index_mins` 0 = off).

Two obligations ride along. The doc comment is written on the field. The second
is M3's and is recorded there: when this is exposed through `/api/v1/settings`,
validation must **reject** a parsed zero from an operator who meant "no working
set" and offer a small floor instead — the two zeroes mean opposite things and
only the caller knows which it got.

---

## 4. Four things I found and did not fix

None of these is reachable today, because nothing calls this code from a request
path. All four become reachable in M3.

### 4.1 Nothing is fsynced, so power loss and process crash are different

`RenditionDir::publish_file` writes to a temp name and renames. That is
sufficient for the process crash the module's ordering rules are about, and
insufficient for power loss: the rename can be journalled while the data blocks
are not, on ext4 for a fresh destination as well as on XFS and btrfs.

`reconcile` now refuses to adopt a zero-length file, which catches the coarsest
form. It does not catch a *partially* written segment whose length is plausible.

**Not fixed because** the cost is real — an `fsync` per segment on a
materializing producer at several times realtime is a throughput question I have
no measurement for, and picking a durability level for someone else's hardware
is exactly the kind of guess this plan has been good about refusing.

**Decision needed.** Whether M3 wants `fsync` per segment, `fsync` on the
directory at admission only, or nothing plus the reconcile check. I would guess
the middle one, but it is a guess.

**Ruled: the middle one, plus an addition I had missed.** No fsync per segment
on the live path — a power loss there re-materializes honestly. But *directory
fsync alone is not enough*: it makes the renames durable while the data blocks
can still be unwritten, which is precisely the plausible-length half-segment,
served as a cache hit weeks later with nothing left to notice.

**Built:** `RenditionDir::make_durable` fsyncs each member, then the init, then
the directory — files before the directory, because a durable name pointing at
unwritten blocks is the failure being prevented. Once per rendition, off the hot
path, bounded by the member count. And `reconcile`'s adoption check went from
"refuse zero-length" to "refuse length ≠ the manifest record's byte count",
which catches most torn writes for free since the record is already in hand.

### 4.2 A plan outlives the file it describes

`forget_rendition_plans` and `forget_fragment_index` have no callers outside
the store and its contract test. `delete_files` does not touch either table, so
a rescan that removes a file leaves its plans and indexes behind — permanently,
and in the hiqlite case in a sidecar that shares no transaction, no foreign key
and no lifecycle with the `files` row.

Bounded and node-local, so it is a leak rather than a correctness bug. It was
inherited from the M1 index rather than introduced by M2.

**Decision needed.** Whether file deletion should clear both, and whether that
belongs in `delete_files` or in a sweep. I did not wire it because
`delete_files` is on a scan path I have not otherwise touched.

**Ruled: a per-node sweep, and `delete_files` stays untouched.** The deciding
argument is the cluster, and it is better than my reason for hesitating:
`delete_files` executes as a *replicated* write, while the plans and indexes
live in each node's sidecar and share no transaction with it. A hook inside
`delete_files` could only ever clean the node that ran it, and never a node
that was down at the time.

**Built:** `vod_row_file_ids` (node-local, bounded, ordered so consecutive
ticks make progress) and `surviving_file_ids` (the replicated side), swept on
the indexing job's tick. Both backends are held to one behaviour by the
contract test — which is where SQLite reading one database and hiqlite crossing
its sidecar and the replicated table have to agree.

### 4.3 `reconcile` costs one syscall per planned segment

A `metadata()` per entry — about 4,000 blocking-pool round trips on a two-hour
film. Correct, and irrelevant today because it runs only on adoption and has no
callers. A single `read_dir` would do it in one pass.

**Decision needed.** None now. Worth remembering if adoption ever lands on a
startup path that walks many renditions at once, which is exactly what a node
restart is.

**Ruled: agreed, no change now.** Carried onto the M3 list: one `read_dir` pass
instead of per-entry `metadata()`, before adoption lands on the restart path,
so a node restart over a large store does not become 4,000 × N blocking-pool
round trips.

### 4.4 A missing init is reported, not acted on

`reconcile` now reports `init_present: false`, because a rendition without an
init is unplayable however many segments survived. It does not purge or
otherwise act.

**Decision needed.** Whether M3 treats a missing init as "purge and re-produce"
or as "produce the init alone and keep the segments". The second is cheaper and
only valid if the init is reproducible independently of the segments — which is
§2 all over again.

**Ruled: regenerate and verify, else purge** — and §2's ruling is what makes
that valid. With promotion canonical, an init *is* reproducible independently
of the segments: run a generation head, check its muxer init against the stored
digest, apply the stored promotion, check the served digest. Match keeps every
surviving segment; mismatch purges to planned-only and re-produces.

**Built:** the two digests `InitIdentity` needs for that, added now while the
record format is still cheap to change. The verify-else-purge flow itself is
M3's, because it needs the generation head this milestone does not spawn.

---

## 5. Where does attaching this to a process belong?

M2 as I have built it is entirely pure or node-local: the plan, the index, the
title store, the scheduler, the rendition directory, the playlist, and now the
executor's decision table. Nothing is on a request path and nothing spawns
anything.

The remaining piece is thin — turn a `prodexec::Step` into an actual `kill(2)`
against a real child, inside `transcode.rs`. The handoff puts session attachment
in M3, and I read that as putting this there too.

I have not started it, for one reason: **it is the first thing that touches the
live playback path**, and everything before it could be wrong without anyone
noticing. Two adversarial reviews of my own work found thirteen defects,
including two that would have shipped a viewer blocked forever on a segment
nothing would produce, and one that would have started and killed an encoder in
a tight loop. Both were in code that had already passed a review.

**Decision needed.** Whether I should build the attachment now, or whether M2
ends here and it opens M3 with a review in between. My preference is the second
— not out of caution about the code, but because the two questions in §1 and §2
both land on the same file, and doing them in one pass is cheaper than doing the
attachment twice.

**Ruled: stop here** — for that reason plus one more: the attachment is the
first change on the live playback path, and this project's discipline is that a
reviewed base precedes new risk. `agent/vod-m2c` goes for review with the §2
mechanism and 3.1–3.3 as built; the M3 handoff opens with the attachment
against a reviewed tree.

---

## What is not a question

Stated so the list above is not read as more uncertain than it is.

- **The three facts** (planned · materialized · admitted) hold up. Every rule
  B3 asked for is implemented and tested, including the one the repair path
  nearly broke — a rendition that loses a member on disk is now demoted rather
  than left published as a cache hit.
- **The ordering rules** hold up. Bytes before record when materializing,
  record before unlink when evicting; whichever half a crash lands in, the
  residue is an unclaimed byte rather than a claim on bytes that are gone.
- **Plan persistence** is settled. Re-deriving was not a cheaper route to the
  same answer, because `CutPolicy` is built from tuning constants any release
  may move and neither `SEGPLAN_VERSION` nor `SourceIdentity` covers that.
- **The landing matcher** (A1) needs nothing further from me. It is
  implemented as ruled, 3-long byte sequence with typed failure on no match and
  on a double match.
