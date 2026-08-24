# VOD M2 — every question I could not answer myself

**Status:** M2 implemented on `agent/vod-m2c`, gates green, nothing on a
request path · **From:** milestones M1–M2 of
[VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) ·
**Written:** 2026-08-24 · **§2 partially measured** 2026-08-24

Companion to [VOD-M0-ISSUES.md](VOD-M0-ISSUES.md) (what measurement found) —
this is *what implementation found, and what I could not decide on my own*.

Read §1 and §2 first: both block M3, and §2 is the one I would attack — now
partly measured, and the measurement narrowed it rather than closing it. §3 is
three decisions I made in order to keep moving, each of which reverses cheaply
now and expensively later. §4 is four defects I found and deliberately did not
fix. §5 is a scope question.

Nothing here is blocking M2 from being reviewed — the branch is complete and
green as it stands. Every question is about what happens when this meets a
request path.

**How to answer:** each section ends with a **Decision needed** line naming the
smallest thing that unblocks me. Anything you don't rule on, I will keep doing
what §"What I did meanwhile" says, and that choice is now load-bearing.

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

### 3.3 An unset working-set budget means "not configured"

`WorkingSet { budget_bytes: 0 }` never holds anything. The other reading — zero
bytes allowed — stops every producer on the node the moment the config is
missing or misparsed. Stated because it is the kind of default that is obvious
until it is wrong.

**Decision needed.** None expected. Flagged so it is not a surprise.

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

### 4.3 `reconcile` costs one syscall per planned segment

A `metadata()` per entry — about 4,000 blocking-pool round trips on a two-hour
film. Correct, and irrelevant today because it runs only on adoption and has no
callers. A single `read_dir` would do it in one pass.

**Decision needed.** None now. Worth remembering if adoption ever lands on a
startup path that walks many renditions at once, which is exactly what a node
restart is.

### 4.4 A missing init is reported, not acted on

`reconcile` now reports `init_present: false`, because a rendition without an
init is unplayable however many segments survived. It does not purge or
otherwise act.

**Decision needed.** Whether M3 treats a missing init as "purge and re-produce"
or as "produce the init alone and keep the segments". The second is cheaper and
only valid if the init is reproducible independently of the segments — which is
§2 all over again.

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
