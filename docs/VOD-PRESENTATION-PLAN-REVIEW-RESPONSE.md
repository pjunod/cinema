# VOD plan review response — all six blockers accepted, one premise refuted

**Status:** assessment complete · **Assesses:**
[VOD-PRESENTATION-PLAN-REVIEW.md](VOD-PRESENTATION-PLAN-REVIEW.md) (review of
the plan at `8fe68d56`) · **Resolves into:** plan v2, revised in the same
commit as this response · **Verified against:** `origin/main` @ `9cace96e` ·
**Written:** 2026-08-23

Every finding was re-verified against the source before acceptance; the
verdicts below say what was checked, not what was believed. The disposition:
**B1–B6 accepted in substance and resolved in plan v2; B5's factual premise
is refuted while its conflict is accepted; S1 accepted with the
version-floor option; both documentation corrections accepted.** The review's
§5 re-review gate is reproduced at the end with the v2 section that answers
each statement.

## Finding-by-finding assessment

### B1 — accepted, and it invalidates more than the review claims

Verified: `fmp4::classify` (`fmp4.rs:1556-1610`) reads the first VCL NAL and
answers IDR (19|20) vs CRA/BLA-with-leading-pictures — facts an ffprobe
`flags=K` row cannot carry; `Segmenter::push` accumulates `pending_bytes +=
fragment.len()` (`fmp4.rs:2338`), i.e. **output** fragment bytes after track
selection and audio handling; `gop-census` parses the production copy pipe's
`moof`/`mdat` stream and uses ffprobe only to discover the codec
(`gop-census:264-275, 352+`). The plan's sentence "gop-census already walks
exactly this structure" was wrong about *what the input is* — the script
walks production-shaped fragments, which is precisely the review's point.
`rap_byte_offset` likewise named no mechanism: the copy path restarts by
`-noaccurate_seek -ss` (`mod.rs:1164-1173`), never by byte offset. One
misquote flagged in passing: the byte ceiling is **64 MB**
(`COPY_SEGMENT_MAX_BYTES`, `mod.rs:114`), not 48 MB — immaterial to the
finding.

v2 goes further than the required correction: it drops the pretense that the
plan *predicts* today's cuts and makes the plan **normative** — the index is
produced by the production-shaped copy pipe itself (video-only variant, so
one plan serves every audio selection), plan entries are keyed by film-time
DTS, and the materializing segmenter cuts *at* planned boundaries rather
than re-deciding them. Replay equivalence becomes M0-P0 (determinism +
clean-boundary + ceiling-with-real-audio proofs) and runs before any cost
measurement, exactly as the review requires. Unindexed files simply keep the
legacy presentation for their first watch. Plan v2 §2.2, M0.

### B2 — accepted as written

Verified: `-avoid_negative_ts make_zero` on the copy pipe (`mod.rs:1403-
1416`), first-fragment `tfdt` preserved as the published base
(`fmp4.rs:1896-1944`), transcode restart = fresh `-ss` + fresh muxer
(`mod.rs:905-909`), and PERF2-PLAN-REVIEW §2 R1's probe already proved
`-start_number` renames files without moving PTS. A repositioned producer
would emit near-zero timestamps under a minute-30 name. v2 adds the
media-time contract the review demands (§2.2): the copy segmenter rebases
`tfdt` to the plan's film-time start and stamps `mfhd` = plan index (the
merger already owns both structures); the transcode path uses
`-output_ts_offset`, the exact mechanism R1's probe validated; the rendition
init is written once and every later generation must be byte-identical or
the generation is refused; and M2's acceptance includes the review's
noncontiguous two-generation materialization test on all three stacks.

### B3 — accepted; the arithmetic is right

Verified: scratch budgets 2 GB/8 GB (`PERF-PLAN.md:1677-1678`), completed
cache `cache.max_gb = 50` (`:1681`); the plan's own 69 Mb/s reference title
is ~62 GB — bigger than every budget, so "bitmap complete ⇒ cache hit" was
unsatisfiable for exactly the titles the plan cares most about. v2 §2.4
separates **planned / materialized-now / durably-admitted**, names the
working-set and completed-cache budgets separately, adds an admission
threshold with the honest over-budget outcome (working-set-only rendition:
still VOD-presented, never cache-completed, second watch re-materializes),
and qualifies the "copy path gets a cache" claim to admissible titles. M2
acceptance gains the synthetic over-budget title.

### B4 — accepted; the contradiction was real and self-inflicted

Plan v1 §2.3 outcome 2 gave a 15 s budget and then promised to "answer 200
as soon as it can rather than erroring" past it — an unbounded response the
client cannot retry. v2 picks one wire contract: **the deadline is always
hard**; expiry answers a typed, retryable `segment_pending` 503 with
`Retry-After`; the client's create body declares its block budget so the
server never outlives the stack's own timer; disconnect cancels the wait;
blocked GETs are capped per session and globally and coalesced per segment.
M0-P3 now measures each stack's retry behavior to set the numbers, not to
choose the shape. The review's four required tests (disconnect, producer
death mid-wait, ten waiters one segment, 20-seek storm) are in M3's
acceptance verbatim.

### B5 — conflict accepted and resolved; factual premise refuted

Refuted: the review states `CLUSTER-MEDIA-POOL-PLAN.md` "does not exist in
this tree." It exists at the reviewed ref — `git cat-file -e
9cace96e:docs/CLUSTER-MEDIA-POOL-PLAN.md` succeeds — with status "P0–P3
delivered, written 2026-08-21," and it, not only CLUSTERING-PLAN.md, governs
media-byte routing and takeover mechanics. The guardrail's reference was
correct; the review's re-review checklist should not carry that claim
forward. (How the reviewing worktree missed a file present at the ref it
names is worth the reviewer checking.)

Accepted, fully: the substantive conflict is real and the review located it
precisely. CLUSTERING-PLAN.md's takeover contract has a survivor rebuild the
recipe with its *locally valid* encoder and digest — protocol-compatible,
not byte-identical output (`CLUSTERING-PLAN.md:322-327`) — and advance
discontinuity/media sequence on takeover, which an immutable playlist cannot
do. v2 §7 resolves it by decision rather than deferral (new ledger entry
D11): **a VOD rendition is bound to its owner node for its lifetime; no
takeover ever reuses its URIs.** Owner death mid-play is a typed refusal the
client's existing reopen path converts into a fresh rendition on a survivor
— a bounded interruption, which is the media-pool plan's own promise, and no
immutable URI ever names two different byte streams. Completed renditions
are shared across nodes only where local digests match, CLUSTERING-PLAN's
existing rule. M2's review gains a cluster-aware pass and M7 gains the
power-pull acceptance the review specified.

### B6 — accepted; the state machine is adopted as drawn

The v1 text let any persisted recipe resurrect, which would indeed let a
predecessor's autonomous segment fetch re-animate a handle the server
deliberately retired — quality changes, typed stall reopens, DELETE, admin
stop, revocation, and file replacement all produce exactly such late
fetches. v2 §2.5 adopts the review's lifecycle verbatim: only an idle reap
produces a *dormant* handle; explicit DELETE, supersession, admin stop,
revoked access, and file-identity invalidation produce *terminal*
tombstones that answer 410 and never resurrect; the dormant TTL is
**sliding**, refreshed by authorized media GETs, with the supported pause
named as a setting (`playback.vod_dormant_ttl_secs`, default 6 h). M3
acceptance proves each terminal cause stays terminal.

### S1 — accepted with the version-floor option

The v1 rollout story did break at M8: a client updated past the deletion
pass, pointed at a pre-VOD server, would receive a shape it no longer
handles. v2 §8 M8 adopts the review's second option: clients learn a
minimum server protocol; after deletion, `vod: false` from an older server
renders a typed "this server needs updating" refusal with the server build
named, never an undefined-behavior playback attempt; and the deletion pass
itself is gated on the operator's fleet floor being proven at or above the
VOD build (all nodes, checked via `/api/v1/server`, recorded in
STATUS.html). For this deployment — one operator, three nodes — the floor is
verifiable directly; if the clients ever ship to stores, the floor becomes a
release-notes contract. The compact-legacy-arm alternative was considered
and rejected: keeping a "small" live arm alive indefinitely is how the
current 35-mechanism inventory started.

### Documentation corrections — both accepted

The orientation now says three decisions (D1, D4, D6). The cluster reference
now names both cluster documents (with the correction above). The
cross-link from the plan's status line to the review was already applied by
the review commit and is kept.

## The review's re-review gate, answered

| Review §5 statement | Where v2 answers it |
|---|---|
| M0 proves `plan == copy output` before measuring cost | §8 M0-P0, run first, gates P1 |
| Segment N materialized first carries plan film-time timestamps | §2.2 media-time contract; M2 noncontiguous test |
| Eviction cannot fake completion; over-budget titles bounded and honest | §2.4 three-state store; M2 over-budget title |
| Every blocked GET ends through one named path | §2.3 hard deadline + typed retryable + cancellation |
| Node failure cannot change an immutable URI's bytes | §7 / D11 owner-bound renditions; M7 power-pull |
| Only idle-dormant resurrects; terminal stays terminal | §2.5 lifecycle; M3 acceptance |
| A `vod: false` client outcome is deliberate | §8 M8 version floor + typed refusal |

One process note for the re-review: verify the `CLUSTER-MEDIA-POOL-PLAN.md`
premise against the ref rather than the worktree, and re-quote the byte
ceiling from `mod.rs:114`.
