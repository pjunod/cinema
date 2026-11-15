# Shared subtitles and Start503 — ownership, cause and delivery

**Status:** done · **Owner:** Claude (from Codex/root) · **Updated:** 2026-11-11

Companion to [the shared playback root causes](SHARED-LIBRARY-PLAYBACK-RCA.md),
which defines S5, and [the prior delivery ledger](SHARED-LIBRARY-PLAYBACK-STATUS.md),
which records merged PR #1472. This follow-up owns S5 and the encoded/native
fixture Start503. A1, native client releases and physical acceptance remain separate.

## Current state

Merged to `main` in PR #1490 as `df710af12` (head `32e9eec5d`, 2026-11-11),
with all fourteen `Regression-Test:` trailers in the landing commit. Focused
qualification passed 42 daemon and two core cases on pinned Rust 1.97.1. The
fast lane on the merged head was green; its first run failed only the
rolling-producer ownership inventory, which `32e9eec5d` recorded. One final
adversarial review preceded qualification and all five findings are addressed.
No Windows runtime run, fleet deployment or physical playback acceptance is
claimed.

## The Start503 defect

The prior failed run recorded one actual Source HTTP Start returning 503
before the receiver obtained a Source binding or publication. Six later
runs passed. Its log did not retain the Source error body or stage warning;
those later passes do not establish the original cause.

Source initialization contains a concrete concurrency defect consistent
with that observation. The owner exposes its assigned dispatch, then reads
ingress custody and asserts the entire captured accounting revision. The
accepted HTTP registrar may legitimately register or acknowledge a driver
between that read and assertion. The stale assertion deliberately fails
its SQL transaction; Start maps that Store error to `Unresolved` and 503.
The claim creates its initial ledger atomically, so an absent-ledger race
is not the explanation. Pinned Tokio notification generation semantics
also rule out the suggested unpolled-`Notified` lost-wakeup explanation.

Initialization is a read-only authority check, not an accounting mutation.
It should use the existing current-admission predicate: exact Source
assignment, owner, incarnation, routing boot/credential identity and an
unsealed ledger. Register and ACK may change accounting while those facts
remain valid. Actual accounting mutations retain their snapshot CAS.
Genuine SQL failures and changed authority remain failures.

A request-scoped fixture pause orders a real initializer snapshot, real
HTTP registration, and the initializer assertion without timing sleeps.
The planned negative restores only the old revision assertion; it must
produce an actual Source503. The repaired ordering must serve real media
and captions, then confirm ordinary End and durable release. Core
interleavings separately cover benign accounting and changed authority.
This establishes the defect causally; exact attribution of the historical
event remains limited by the missing original diagnostic.

## S5: requested windows belong to the Source

The current native stage extracts complete tracks before either copy or
encoded recipe preparation. Its stored-probe requirement can also add a
caption-only startup refusal. Publish supported track facts immediately
and remove this physical startup stage. Encoded burn-sidecar preparation
remains under its existing encoded operation custody.

Demanded caption windows use the existing detached resource factory. Its
counted response guard retains the actual Source file fence; caller timeout
does not drop that owner, and Source SQL release waits for counted work.
Window admission is bounded. A Source copy producer owns a real CPU permit
even though its recipe has no encoder. It must participate in the existing
stopped-producer poll, contention decision and reaper-owned permit yield,
so a paused producer cannot retain the pool indefinitely ahead of a caption
waiter. This adds no separate timer or admission bypass. End or revocation
cancels the actual operation,
which retains admission and custody until the child is reaped, readers are
joined and file work closes. No generic whole-track or window warmer is
started as a substitute owner.

Post-publication children require refreshed ingress and publication
authority. The pre-start preparation predicate intentionally requires an
unresolved start and no media-session row; it must not be weakened or
reused after publication. Share the physical child executor through typed
preparation/resource authorization. Cue-index and demux mechanics remain
shared with local captions, with Source-owned reads of the held object.

Cached reads and readiness observations retain counted Source custody.
Cache identity alone grants no permission. Source-derived window names bind
a SHA-256 of the exact held object version; generic local sidecars cannot
supply Source cues. These artifacts use the existing managed subtitle cache,
byte reservations, pins and eviction rather than a new cache owner. Versioned
whole/window revisions distinguish complete, partial and absent data; old or stale
revisions remain parseable but cannot supply another object's cues. Keep
UTF-8, 2 MiB, cue-count, timestamp and timestamp-map bounds per served window.

Caption work preserves typed capacity, deadlines and unsupported data. The
Source and receiver retain bounded retry delays for busy caption resources;
a caption-only unsupported response does not retire the movie. Authentication,
physical-fence and unresolved Source failures still fail closed.

## Evidence required before merge

- Deterministic accounting interleavings and actual HTTP503 counterfactual.
- Copied and encoded native Start publish while caption work is parked.
- Requested windows deliver actual cues, including cue-index failure fallback.
- Missing stored native probe adds no caption-only Start requirement.
- Cancellation, End and revocation retain real child/read/file settlement.
- Expired authority, changed grants/route/file and exhausted admission refuse work.
- Revision identity, stale-cache absence and cached filesystem custody hold.
- Existing encoded burn custody stays intact.
- A Source copy yields its real permit through the existing reaper before
  a caption waiter acquires it; an ordinary local copy retains its behavior.

A before `/metrics` snapshot was captured from an existing server and is
retained privately with the source-bound receipts. Its
`plurx_subtitle_source_foreground_self_claims_total` is 0;
`plurx_subtitle_source_requests_total{trigger="foreground"}` is 1 and
`plurx_subtitle_source_lookups_total{outcome="miss"}` is 70. The expected
after observation for a shared caption window is no generic foreground
self-claim caused by Source delivery. This baseline is not performance or
physical-playback acceptance. Final fixture receipts must additionally
show caption work outside Start and counted settlement before release.

## Final review dispositions

Astra requested five corrections; `21493f2fe` and the shared control-body
bridge address them. Managed-cache pressure retains typed capacity/deadline
results. Sparse intermediates reserve bytes through converter reap and
actual handle closure, using the established Windows named-file pattern.
Controls share counted custody with bounded observation of caption work,
report permanent unsupported data accurately and retain genuine failures.
Raw UTF-8 and timestamp maps are checked before presentation. Optional
timeline evidence no longer prevents an independently authorized demux fallback.

The initializer counterfactual uses a request-scoped fixture-only release
mode to execute the former accounting predicate against real Store SQL and
real HTTP registration. It has no wire or product configuration entry;
unarmed, cancelled and nonfixture execution retain the current authority
predicate. Both the actual503 control and repaired200 flow can be proved in
one compiled binary. Runtime and gate receipts remain pending.
