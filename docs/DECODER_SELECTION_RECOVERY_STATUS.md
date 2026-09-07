# Decoder selection and recovery — implementation status

**Status:** M3c4 candidate; M0–M3c3, M4 and M5a merged into the effort ·
**Updated:** 2026-09-07 · **Integration branch:**
`effort/decoder-selection-recovery` · **M3c4 task base:** effort head
`b602b9f2add7c14861265f7d384c1d9910b0c742`

The effort was created from Forgejo `main` at
`4a6a0268bd314ad5587cb3037f12ebd992c0074e`. The original M0 research baseline was `main` at
`3d847b58b081dcb15a8d2e566d8d0ac1700882fd`. It remains useful as historical
content evidence, but no command or review performed on that tree is presented
as exact-tree evidence for the current Forgejo base.

This is the live execution ledger for
[DECODER_SELECTION_AND_RECOVERY_PLAN.md](DECODER_SELECTION_AND_RECOVERY_PLAN.md).
It records what is merged, what was actually tested, and what remains unsafe.
An unchecked item is not implied by a nearby passing check.

## Current checkpoint

| Field | Current value |
|---|---|
| Milestone | M3c4 — the receipt contract, enforced |
| Task branch | `codex/decoder-selection-m3c4`, based on effort head `b602b9f2` |
| Task PR | Open against the effort branch. One whole-PR adversarial review has run; its findings are repaired in this head |
| M1 dependency | [Forgejo #63](http://192.168.4.7:3000/noirr/plurx/pulls/63) is merged. Exact head `f7f98b01` completed `make validate-full` with 23 passed, 0 failed, and 2 declared skips (`android-device`, no `adb` on the qualifying host; `live-tv-two-node`, which does not run on Darwin); `target/validation/report.json` records `git_ref f7f98b01`, generated `2026-09-07T01:01:02Z`. It fast-forwarded into the effort. M2 ([Forgejo #73](http://192.168.4.7:3000/noirr/plurx/pulls/73)) then fast-forwarded onto it after its own whole-PR review, its findings repair, and the Forgejo effort gate on exact head `773ad488` — which is the commit this M3a branch is based on |
| Effort PR | Not opened yet |
| Working compiler | `rustc 1.97.1 (8bab26f4f 2026-07-14)` via `rustup run 1.97.1` |
| Focused validation | This head on pinned 1.97.1: daemon `playback_control::tests::` 225/225 including six new health-barrier contracts, `decoder_health` 38/38, `transcode::tests::` 223/223, `live_tv::tests::` 40/40, core `decoder_selection` 44/44, `make effort-rust-check`, `make lint` and `make validation-lint` clean, the rolling-producer ownership ledger unchanged, `git diff --check` clean |
| Exact receipt | M3b1's is `de05be04`, merged. M3b2's is the head this PR carries; the whole-PR adversarial review and the effort gate are what make it a receipt |
| Full PR validation | Deferred to the `Main promotion gate`, per `AGENTS.md`. See *Decisions and deviations* — the plan's per-task full-suite instruction and the repository's own pipeline disagree, and the repository's pipeline wins |
| Blocker | The Forgejo `Effort development gate` on this head. Per `AGENTS.md` a task PR into an effort branch defers the full suite to the `Main promotion gate`; the effort's one `make validate-full` is owed at promotion, not here |

## Milestones

| Milestone | State | Exit evidence |
|---|---|---|
| M0 · baseline and diagnostic qualification | Merged | [Forgejo #62](http://192.168.4.7:3000/noirr/plurx/pulls/62) fast-forwarded qualified receipt head `a8bbe574` into the effort after two final approvals and the Forgejo effort gate |
| M1 · explicit plan and facts | Merged | [Forgejo #63](http://192.168.4.7:3000/noirr/plurx/pulls/63) fast-forwarded qualified head `f7f98b01` into the effort after three whole-PR adversarial reviews at `11f3f096`, their four consolidated findings repaired together in `81d46577`, the Forgejo effort gate, and one exact-head `make validate-full` at 23/0/2 |
| M2 · arguments and identity use one plan | Merged | Movie HLS command construction and recipe v3 consume one `ResolvedTranscode`; the recipe remains in `decoder-plan-v1-unqualified`, so M2 cannot claim health-qualified cache artifacts. Retry, resumable/speculative, live, offline, cache lookup, and direct Live TV builder migrations are present but not yet reviewed or qualified |
| M3 · owned observation and health receipts | M3a–M3c3 merged; M3c4 candidate; M3c5 not started | M3a's grammar ([#79](http://192.168.4.7:3000/noirr/plurx/pulls/79)), M3b1's owned readers ([#83](http://192.168.4.7:3000/noirr/plurx/pulls/83)) and M3b2's health barrier ([#84](http://192.168.4.7:3000/noirr/plurx/pulls/84)) are in. M3c1 ([#85](http://192.168.4.7:3000/noirr/plurx/pulls/85), head `bf75c62c`) makes the generation manifest carry and authenticate the joined producer receipt. M3c2 ([#86](http://192.168.4.7:3000/noirr/plurx/pulls/86), head `ff10c2db`) makes a part carry its own receipt across a resume, without which no long film could ever be certified. M3c3 ([#87](http://192.168.4.7:3000/noirr/plurx/pulls/87), head `b602b9f2`) gives qualified production its own artifact identity. M3c4 enforces the receipt contract under that identity. Nothing selects it in production yet; the operator control and the paths that cannot yet publish a manifest are M3c5 |
| M4 · mixed resource admission | Merged | [Forgejo #81](http://192.168.4.7:3000/noirr/plurx/pulls/81) fast-forwarded `f0f7aec8` into the effort after one whole-PR adversarial review, its three blockers repaired, and the Forgejo effort gate |
| M5 · durable budget and prepublication recovery | M5a merged; M5b/M5c not started | `media_session_producer_recovery` exists on both backends with the reservation contract running against each. No daemon behaviour change |
| M6 · postpublication replacement and client intent | Not started | — |
| M7 · offline, shared cache, and handoff enforcement | Not started | — |
| M8 · fleet qualification and promotion | Not started | — |

## M2 working tree — one plan names and builds the attempt

M2 makes one validated `ResolvedTranscode` the semantic input to both movie
FFmpeg arguments and content identity. The plan digest is version 1 and feeds
length-prefixed field names and values. It binds decoder backend, the actual
software decoder implementation, absolute input video stream, surface
contract, evidence class, policy revision, source-fact digest, encoder,
renderer, media options, and output presentation contract. Selection reason
text is deliberately excluded: two routes that resolve to the same byte
contract may reuse one artifact.

Attempt-local values live in a validated `TranscodeExecution`. Source and
output paths, resume position, start number, pacing, software-thread allowance,
subtitle sidecar path, force-IDR request, and scratch location affect the
command invocation but not `plan_digest`. Environment policy is sampled while
the plan is resolved; command and recipe builders do not reread it, so changing
the environment after preparation changes neither arguments nor identity.

Recipe format 3 hashes the plan digest beneath the explicit
`decoder-plan-v1-unqualified` namespace. The version bump makes affected older
entries and prefixes miss. M3 must add owned diagnostic completion before any
artifact namespace can claim decoder-health qualification; M2 does not turn
automatic recovery on.

The three shipping movie builders now consume the plan directly:

| Path | Working-tree binding |
|---|---|
| Live HLS | Resolves before cache lookup; an admission-driven encoder change resolves a new complete plan before spawn |
| Speculative/resumable production | Resolves bound source facts before recipe hash and cache claim, then carries the same plan through every retained part |
| Offline production | Resolves one plan from retained probe facts before recipe identity and production |
| Prepublication retry | Prepares the alternate semantic request, resolves the complete alternate plan, then freezes arguments and the plan digest in the retry fingerprint |

Live TV cannot know codec/profile facts until tuner bytes arrive. Its separate
`LiveTvTranscodePlan` therefore freezes the pre-body contract explicitly:
software input auto-detection, the `0:v:0` stream specifier, selected encoder, output
height, thread allowance, and force-IDR policy. The command states
`-hwaccel none` and stays outside the movie cache namespace rather than
inventing source facts.

No new compile-time feature gate or hidden runtime enable switch is introduced
by M2. This milestone changes argument and identity ownership; the Developer
settings enablement and its safety prerequisites remain M8 work.

### One name per source, however the source was measured

The plan digest binds a `DecodeCacheIdentity` — the catalog row's file id, size
and mtime, length-prefixed and hashed the one way every producer computes it.
`Recipe::new` takes the pipeline digest and the plan, and nothing else.

It previously took a `MediaFile` beside the plan and hashed that file's id,
size and mtime itself, which meant nothing checked that the plan and the file
described the same title: the constructor took the plan's word for the encode
and the caller's word for the source. There is now no second source that could
disagree with the first.

The deeper fault that change closed was in `DecodeFacts`. Its digest included
the *descriptor fingerprint* — device, inode, ctime — and that digest feeds the
plan digest, which feeds the recipe hash. Two producers reach the same title by
different routes: speculative production holds a descriptor, while live and
offline resolution derive a placeholder identity from the catalog row. The two
routes fingerprint one file differently, so they computed different plan
digests and different artifact names for identical work. The speculative
producer would have filled the cache forever without a single live hit, and the
symptom is indistinguishable from a cache that is merely cold. The facts digest
now describes the measured stream only; which stream was selected still changes
it, and the descriptor does not.

There were two divergent terms, not one. The second was the selection rule
itself: the stored-probe route asked for the first playable video stream while
the descriptor-bound route asked for the stream FFmpeg's `0:v:0` resolves to.
On a file carrying cover art as a video stream those are different streams, so
the two routes planned different work and named different artifacts — and only
one of them matched the command the shipping builder emits. Both routes now go
through `legacy_ordinal_facts`, which resolves the ordinal FFmpeg would resolve
and applies the catalog row only when the selected stream is the film. The
ordinal deliberately counts attached pictures, because FFmpeg counts them;
changing that would be a silent routing change, and it is not M2's to make.

The facts digest also stopped carrying `selection_provenance`, and the plan
digest stopped carrying `decode_evidence`. Both are provenance: they record how
a fact was obtained or how well the node knows its own capability, not what will
be produced. Two nodes running the same decoder produce the same picture, and
giving the better-informed one a private key space would have split the fleet's
cache during a rollout. The qualified-versus-unqualified separation belongs to
`artifact_namespace`, which `Recipe::hash` already feeds on its own.

Descriptor continuity survives as a separate question with a separate answer.
`ResolvedTranscode::observed_source_identity` retains the fingerprint the facts
were read through, and `ResolvedTranscode::source_binding` records
`DescriptorBound` or `CatalogRow`. Neither enters the digest: a title measured
through a held descriptor and the same title measured from its row must name
one artifact, or the producer and the player stop sharing a cache. What the
binding governs is trust, not identity.

**Scope limit, stated plainly.** Only the speculative producer currently holds
a descriptor across measurement and encode; live and offline resolution still
plan from stored FFprobe JSON and therefore carry `CatalogRow`. M2 does not
claim otherwise, and nothing in M2 yet refuses a `CatalogRow` plan — the gate
that must require `DescriptorBound` before a reuse is called health-qualified
arrives with receipts in M3, together with the qualified artifact namespace.
Until then every artifact stays in `decoder-plan-v1-unqualified`.

Two regressions hold the key space closed. `an_alternative_plan_cannot_publish_
or_resume_under_the_failed_plans_key` proves a changed decode route resolves to
its own plan digest and its own recipe hash, that the successor attempt cannot
be admitted under the failed producer's name, and that the retry builder
refuses to carry the plan of the route it is replacing.
`a_pre_plan_staged_prefix_is_quarantined_by_the_v3_recipe_hash` proves a staged
prefix written under a pre-plan recipe hash is quarantined rather than
assembled into the new encode.

### What the whole-PR review found, and what it changed

Two independent adversarial reviews ran against exact head `aec3012a`. They
agreed on six blockers. Four of them were already in the M2 working tree before
this milestone's identity work; two were in the identity work itself. All are
repaired in one batch; the disposition is below rather than in a PR comment,
because four of these were shipping behaviour changes that a reader of this page
needs to know about.

| Finding | Disposition |
|---|---|
| Every GPU pipeline tone-mapped SDR sources. The plan path passed `input_dynamic_range_name()` where the legacy path passed `MediaFile.hdr`; the first answers `Some("sdr")` for an ordinary file and the renderers tone-map on `is_some()`. A 4K SDR file on a QSV node got `vpp_qsv=…:tonemap=1`, and the wrong picture would have been cached permanently under the v3 key | Added `ResolvedTranscode::input_hdr_format`, which is `None` unless the source is HDR, and used it at the one call site |
| A file with `probe_json IS NULL` became unplayable, including one already fully cached: planning is resolved before the cache lookup, and a missing probe was a hard error. That row state is expected — the scan's repair pass and the probe retry job exist for it | `catalog_plan_probe` builds facts from what the scan recorded. It carries no frame rate, because the row has none and inventing one is the silent default the plan forbids |
| Any codec outside the seven-family startup inventory became unplayable on a software-decode node — VC-1, MPEG-1, WMV3, ProRes — and a node whose `ffmpeg -decoders` call failed lost all transcoding. Before planning, the command named no decoder and FFmpeg read the container | Under the legacy policy an uninventoried codec resolves to software decode with no named implementation. Under `Enforce` the inventory is the contract and the refusal stands |
| The startup inventory claimed `implementation == codec`, which the plan then forced as `-c:v`. `-c:v av1` selects the native decoder where FFmpeg would otherwise choose `libdav1d`, and the slower choice would have been baked into artifact identity | `SoftwareDecoder::implementation` is now `Option<String>`. The daemon's startup inventory records availability and names nothing, so the command names nothing, exactly as it did before planning existed. Naming an implementation is M3's job, once one has been measured |
| Cluster cache locality was off. `media_offer_probe` hardcoded `cache_hit = false`, and that field is an *eligibility* input downstream, not a preference — a node holding a complete byte-verified generation was refused whenever its source was momentarily unreadable, which is exactly when serving from cache is the point. The cache-offer verification subsystem went `#[cfg(test)]` with it, so corrupt generations on offered-but-unselected nodes stopped being detected | The offer resolves a plan the same way live and offline do — no descriptor, no `stat`, only the catalog row and stored probe facts — and the verification subsystem is production code again. A plan that cannot be resolved yields no claim |
| Descriptor-bound fact collection could spend a producer's entire deadline and then fail the job, with the 2-second cap and the budget restoration both removed. A title whose source probes slowly would produce nothing, every cycle, forever | `DECODE_PLAN_PROBE_BUDGET` caps it at two seconds, `retain_production_budget_after_planning` gives the time back, and a probe that cannot finish falls back to the stored-probe plan rather than failing production |

The reviews also took apart the milestone's own new tests. Three proved less
than their names claimed and one passed identically on the base commit; the
assertions no mutation could fail were deleted rather than reworded, and the
tests that were testing their own fixtures were rebuilt to go through the
production derivation. Two assertions the recipe suite had dropped during M2
were restored, and one of them is now stronger than it was: a renderer the
source cannot feed is refused outright rather than merely given a distinct
cache entry.
## M3a working tree — deciding whether a decode worked

A producer that emits segments and exits zero is indistinguishable, from
outside, from one that spent the whole film failing to decode and wrote green
frames. Both advance the playlist; both finish. The difference was in stderr,
and stderr was logged and thrown away — which is how a cache can hold a broken
title indefinitely with every counter reading healthy.

`crates/plurxd/src/decoder_health.rs` is the part that reads it. M3a lands the
grammar, the accumulator and the bounded reader as one reviewable piece with
their whole test suite; M3b wires them into the producer. Nothing outside the
tests calls any of it yet, which is the point of the split — the subtle logic
gets reviewed on its own, before it is threaded through a 36,000-line file.

### Two tiers, because §7.2 draws the line and the difference is a session

A line with the FFmpeg 8 *shape* — the `vist#<file>:<stream>` and `[dec:…]`
contexts, the selected stream, the literal message `Error submitting packet to
decoder:` — is a **structural** primary record. It counts toward the window, it
latches a fault, and it refuses the attempt's output a cache receipt. It is not
permission to end a session.

For that the line must additionally match a **versioned diagnostic contract**:
this FFmpeg version, this binary and buildconf, these log flags, this codec and
this decoder, the severity label in its right place, the exact detail text after
the colon, and the context addresses when the contract says those are
load-bearing. §7.2's words are "a tolerant structural match without that
external build receipt is observation only", and the accumulator implements them
literally — `automatic_action_allowed` requires that *every record in the window
that latched the fault* carried the receipt, not that the attempt saw one
somewhere.

The retained #913 capture is why the tier exists rather than being a nicety. Its
records have the shape and name the right stream, but FFmpeg 7.1.4 wrote them
without severity labels and compressed thirty-six of them into one summary line.
Replayed, it latches a fault, refuses the cache, and never acts — which is
exactly the three answers it should give.

Three rules carry the rest, and each exists because of a specific way this could
report a healthy stream as broken:

**Only the selected video stream counts.** An audio decoder failing, an
unselected video stream failing, the encoder failing, a *filter graph* failing
with the contract's own detail text, or a filename containing the word `error`
all appear in the same stream of text, and none of them is a decode fault on the
picture being served. Attribution is the full `vist#<file>:<stream>` pair — both
halves, because a session with an overlay or a concat has a second input and
`0:0` would then attribute another file's failures to this picture — plus the
`[dec:…]` context and the message as a literal prefix rather than a search.

**A repeat summary disqualifies the attempt from automatic action, and keeps
its provenance.** `Last message repeated 8 times` means the log was compressed
and the timestamps the window is evaluated against are gone. Counting the
summary as one record understates it; expanding it to eight invents
observations. Neither is a basis for acting. §7.2 also asks for the summary's
provenance, so the accumulator keeps the three counts the M0 harness keeps —
summaries following the selected stream's own failure, summaries following
something else, and summaries that follow nothing attributable — and the
retained control fixture places one in all five positions so that a single
boolean cannot pass for an answer.

**Progress never clears a fault.** The accumulator has no method that clears a
latch. A decoder that fails and then recovers enough to emit frames is still
producing a film with holes in it, and the frames arriving afterwards are
exactly what made this invisible before.

The deployed fleet's FFmpeg 5.1.9 reports `Error while decoding stream #0:0`
with no decoder context at all, and the retained `unqualified-ffmpeg-5.stderr`
fixture is replayed as a test asserting it produces no attributed fault — and
asserting, in the same test, that the fixture really does carry a decode
failure, so the test cannot be satisfied by a grammar that classifies nothing.
**That is the honest state of automatic recovery on this fleet: it will observe
and it will not act, until either the container FFmpeg is upgraded or a 5.1.9
grammar is qualified against its own retained fixture.**

No contract names a fatal family, and that is deliberate. The one fatal in the
retained evidence is `Decode error rate 1 exceeds maximum 0.666667` — FFmpeg
abandoning a corrupt *input*, not a backend that has become unavailable. A
decoder swap cannot repair a file, so treating it as `DecodeBackendUnavailable`
would have asked for one. The fatal family is contract-driven
(`backend_fault_detail`), absent from every retained contract, and therefore
unreachable on the retained builds; a test asserts that the retained fatal
classifies as nothing at all.

One defect the suite found on its own: the sliding window held every timestamp
inside it, so its size was a function of the child's error *rate*. A flood test
of ten thousand records caught it. Only the newest few can ever matter — if
five records are in the window then the five most recent are, because the
window is a suffix in time — so the deque is capped at the limit and memory is
a function of the parser rather than of the stream.
### What the whole-PR review found, and what it changed

One adversarial review ran against the candidate. It found six blockers, all inthis milestone's own work; all are repaired in this head, together with eight ofits non-blocking findings.

| Finding | Disposition |
|---|---|| `qualifies_reuse` ignored `compressed_log` and `oversized_lines`, so replaying the originating #913 capture certified its output as reusable — the exact artifact this effort exists to stop caching. §7.1 says a truncated or unreadable stream disallows a qualified receipt | Reuse now requires a complete *and* uncompressed observation with no structural record and no fault, and `the_originating_capture_is_diagnosed_and_never_certified_reusable` replays #913 to prove all three answers |
| `oversized_lines` was counted and never consulted. A child emitting three megabytes without a newline had its tail discarded, and the attempt still reported a complete observation | `note_oversized_line` marks the observation incomplete, as does a lossily-decoded non-UTF-8 line — read for the log, never counted as exact |
| The primary-error rule was `contains("[error]") && contains(error_detail)`, far looser than the qualified M0 grammar. `[vist#0:0/rawvideo @ …] [dec:rawvideo @ …] [error] Error initializing filters: Invalid data found when processing input` matched it, and five such lines would have latched a fault on a filter-graph failure | The Rust grammar now parses what the harness's regex parses: the bracketed contexts and their addresses, the stream pair, the severity, the literal message prefix, and the detail compared for equality. That exact line is asserted `Unrelated` |
| `covers_build` compared three of thirteen contract fields, and neither `decoder` nor `stderr_mode` was among them. A contract for `h264` still claimed to cover the build after this effort swapped in `h264_qsv`, whose failures the grammar cannot see — a silent permanent all-clear. `stderr_mode` is the same hazard one level up: without `level` there are no severity labels at all | `covers_build` takes an `ObservedBuild` and compares version, binary, buildconf, log flags, codec and decoder. A table-driven test mutates each field alone and asserts the contract stops covering it |
| `HealthAccumulator` derived `Default`, and the derived value set `observation_complete: false` — so the idiomatic `HealthAccumulator::default()` M3b would write yields an accumulator that can never qualify and never act, silently | `Default` is hand-written as `Self::new()`, with a test |
| Any `[fatal]` on the selected stream latched `DecodeBackendUnavailable`, consulting no contract field, and the only fatal in the evidence is an input-corruption fatal | The fatal family is a contract field, absent everywhere in M0. The retained fatal now classifies as `Unrelated`, and a synthetic contract that does name a family proves the latch still works when one is qualified |

Also repaired from the non-blocking set: the repeat marker is anchored rather
than searched, so `/media/Last message repeated 8 times.mkv` can no longer
disqualify a title forever; a repeat marker without a bounded count is
`Malformed` rather than an ordinary line, because reading it as ordinary reports
a compressed stream as an uncompressed one; the three repeat-provenance counters
§7.2 asks for are kept instead of one boolean; a fatal no longer inflates
`primary_error_records`; the `vist#` *file* index is parsed rather than
hardcoded to `0`; two contracts sharing an id are refused rather than
first-wins; the fixture-comment branch is gone from the production classifier;
and `observe` states its monotonic-observation invariant as a `debug_assert`.
`AUTOMATIC_RECOVERY_LIMIT` is renamed to the plan's
`AUTOMATIC_PRODUCER_RECOVERY_LIMIT` and its doc corrected to the plan's meaning
— one per *recovery epoch*, not one per attempt, which is not a budget.

Focused evidence, pinned `rustc 1.97.1`:
`cargo test -p plurxd --bin plurxd decoder_health` — 22/22, including replays of
`qualified-ffmpeg-8.stderr` (latches on its fifth record, all five
contract-qualified, action allowed, reuse refused), `issue-913-legacy.stderr`
(latches, zero contract-qualified, compressed, no action, reuse refused),
`tolerant-controls.stderr` (five records, never five at once, never latches),
`repeat-attribution-controls.stderr` (one selected, two unrelated and two
ambiguous summaries) and `unqualified-ffmpeg-5.stderr` (no attribution, no
fault). `make effort-rust-check` and `make lint` clean; `git diff --check`
clean.

`Cargo.lock` gains one line: `toml` moves from a workspace dependency
plurx-core already used to one plurxd also declares, for reading the retained
contract table. No new crate enters the graph.

## M5a working tree — the budget has to outlive what it recovers from

An automatic recovery has to survive the thing it is recovering from. The
producer dies; the session ends; the node hands the playback to another node.
An allowance held in a process survives none of those, so a viewer whose file
cannot be decoded would get a fresh attempt after every one of them, forever,
and each attempt would fail the same way.

`media_session_producer_recovery` is that allowance as a durable fact, keyed by
`(user_id, playback_id, recovery_epoch)`. The epoch is the key, and this
milestone stores it — it does not yet *mint* it. Nothing here writes an epoch
onto a playback, an incarnation or a preparation record, and the store treats it
as an opaque caller-supplied string it validates only for length. Presenting an
unused epoch therefore mints a fresh budget, which the ledger's own test asserts
as correct behaviour. Making reopen, seek, track change and handoff inherit one
epoch rather than each minting their own is §8.2's remaining half and is M5b's
work; until it lands, the durable budget bounds one epoch and not one playback.
This is said here because the earlier draft of this section stated the
inheritance as present fact, and it is not.

Four properties are load-bearing, and each has a test on both backends:

**Reservation is one conditional insert.** A read followed by an insert grants
two racing nodes the same budget, and it passes every single-threaded test —
so the contract that proves it spawns two identities at the same epoch and
asserts exactly one is granted. The insert is `INSERT … ON CONFLICT DO NOTHING`
and the answer comes from the read-back, because a Hiqlite affected-row count
cannot distinguish "another identity won" from "this is my own replay" and there
is no rollback available afterwards to undo a wrong guess. Not `INSERT OR
IGNORE`: that form also suppresses `CHECK` and `NOT NULL` violations, so a row
the schema rejected would read back as absent and the method would report a
spent budget for a write that never happened — a playback silently losing its
one recovery to a constraint nobody was told about.

**Only the identity that reserved may settle.** `settle_producer_recovery` takes
the `failed_incarnation_id` and both backends carry it as a predicate. Without
that fence anything holding `(user, playback, epoch)` — a stale node that
handled this playback before a handoff — could exhaust a live reservation, and
the real owner's later settle would find nothing to update, read back a terminal
state it did not write, and leave the ledger permanently recording the wrong
outcome for a recovery that actually installed. Settling the same way twice is
idempotent, because a replaying owner needs its answer back rather than a
silence it cannot tell from a loss; settling the *other* way afterwards finds
nothing to settle.

**The reservation's identity includes its restriction.** The conditional insert
does not update an existing row, so an executor that crashed, reloaded its
policy and recomputed a different restriction under the same failure identity is
not a replay: it is told no, rather than handed a reservation carrying a
restriction the store never accepted and would go on serving to every later
continuation.

**The budget is never refunded.** Not by timeout, cancellation, crash or failed
spawn. A crash between reserving and spawning therefore costs the playback its
one automatic recovery, which is the correct direction to fail: an unbounded
retry loop against a decoder that cannot decode the file is worse for the
viewer than one lost attempt, and every refund path is a way for that loop to
come back. `settle_producer_recovery` moves `reserved → installed | exhausted`
and refuses to move back; settling back into `reserved` is a refund with extra
steps and returns an error.

**A restriction that will not parse is not an absent restriction.** This is the
one that would have been silent. "No restriction" and "unreadable restriction"
look identical to every caller downstream, and treating the second as the first
restores automatic hardware selection for a source that already failed on
hardware — invisibly, at the moment the recovery was supposed to be protecting
it. `ContinuationDecodeRestriction::decode` validates the version, the digest
shape, every field's bounds — including the stream index and the one-based
policy revision, which the first draft left unbounded — and the requirement that
the required backend is not the failed one (a restriction that requires the
decoder that just failed is a loop). It carries `deny_unknown_fields`, so a
later build that adds a field while keeping `version = 1` cannot have it
silently dropped here: a restriction read as a weaker restriction is the same
failure the version check exists to prevent, one level down.

The row read refuses rather than degrading, and both backends refuse
*identically*. That took moving the conversion out of the `rusqlite` row mapper:
a decode failure raised there travels out as `StoreError::Database` while the
replicated store raises `StoreError::Task`, so a caller trying to tell "the row
is unreadable, refuse the recovery" from "the store is unavailable, retry" would
have got a different answer per backend. One shared converter now turns a stored
row into a reservation for both, and the contract writes three kinds of
unreadable value — not JSON, a valid shape with an invalid field, and a later
serialization version — into each store and asserts the same typed refusal.

The contract suite found two defects on its first runs, and both are the kind
that only a contract finds. A settled reservation still answered
`reserve_producer_recovery` with itself, because the failure identity matched —
so an owner that had already installed an alternative, or already exhausted the
attempt, could have installed a second one; a settled row is now a spent budget
in both backends. And the replicated `settle` statement introduced `$4` in its
`SET` clause before `$1` in its `WHERE` clause, which Hiqlite's
`validate_parameter_order` refuses at execution. That is the same class of
defect as the placeholder-order fault that stopped the fragment index building
anything for days: it is invisible on SQLite, which does not care about
first-appearance order, and it fails on every single call once replicated.
Only running the same contract against both backends catches it, which is
exactly what §10.4 asks for. Both statements are now numbered in text order.

`SQLITE_SCHEMA_VERSION` goes to 48 and the Hiqlite cluster schema to 28, with
the DDL spelled identically in both, as the migration conventions require:
SQLite keeps one element per schema version and Hiqlite one per table, and a
divergence between the two spellings makes a replicated import disagree with
the node it imported from. The Hiqlite step is `CREATE TABLE IF NOT EXISTS`, so
unlike the `ADD COLUMN` steps it is safe for both voters to attempt — and the
safety is the statement's own idempotence, not `settle_migration_attempt`, which
inspects the marker only when the transaction failed. `replicated_v28_store_
migrates_the_producer_recovery_ledger_on_daemon_open` constructs an upgrading
v27 tree and runs the migration arm, which nothing else in the suite executes:
the harness bootstraps a *current* schema, so without that test the branch every
existing cluster will take on upgrade is dead code. It asserts the marker moves,
the table and its primary key exist, unrelated rows survive, the migrated store
actually reserves, and — the case that separates this step from the `ADD COLUMN`
ones — that a table already present under a stale marker *succeeds* rather than
being refused, because refusing it would leave a cluster interrupted
mid-migration unable to open.
### What the whole-PR review found, and what it changed

One adversarial review ran against the candidate. It found six blockers, all inthis milestone's own work; all are repaired in this head, together with six ofits non-blocking findings.

| Finding | Disposition |
|---|---|| The v27→v28 migration arm had no test, though every previous schema version has one. The harness bootstraps a current schema, so the branch every upgrading cluster takes was dead code under test | `replicated_v28_store_migrates_the_producer_recovery_ledger_on_daemon_open`, modelled on the v27 test and extended with the idempotence case that distinguishes this step from the `ADD COLUMN` ones |
| The corrupt-restriction refusal — the property this milestone leads with — was untested against either backend, and the two backends mapped it to *different* `StoreError` variants | One shared row converter for both backends, and a contract that writes three kinds of unreadable value into each store and asserts the same typed refusal |
| `INSERT OR IGNORE` suppresses `CHECK` and `NOT NULL` violations, not only the primary-key collision. A rejected row read back as absent, which this method reports as "the budget is already spent" | `ON CONFLICT (user_id, playback_id, recovery_epoch) DO NOTHING` on both backends, so only the collision is silent. A unit test also binds the column's `CHECK` literal to `MAX_DECODE_RESTRICTION_BYTES`, which were two unlinked hand-copied numbers |
| `settle_producer_recovery` had no owner fence: any holder of the three key fields could exhaust a live reservation, and the real owner's settle would then read back a terminal state it did not write | The trait method takes `failed_incarnation_id` and both `UPDATE`s carry it as a predicate, with the read-back checking it too |
| The replay-identity comparison omitted the decode restriction, and the conditional insert never updates an existing row — so a "replay" could be answered with a restriction the store had never accepted | The restriction is part of the comparison, and a test asserts a moved-on request is refused and the stored row is unchanged |
| Hiqlite's reserve was an insert and a read across two Raft operations where SQLite's was one transaction | The remaining interleavings are a competing conditional insert, which cannot alter an existing row, and a settle, which the new fence restricts to this caller. `txn` cannot carry the read — it returns affected rows, not result sets — so the reasoning is written down at the call site rather than left implied |

Also repaired from the non-blocking set: the new table is added to
`validation_reset_contract_state`, so replicated contract cases no longer leak
rows into each other; the migration comment no longer credits
`settle_migration_attempt` with a guarantee it does not provide; the two
hand-copied validation helpers are one shared copy next to the DDL, where the
schema already lives so the backends cannot drift; a negative stored counter is
a typed refusal on the read path rather than a silent clamp to zero, matching
the write path's refusal to wrap; and the pure read no longer opens a write
transaction.

Focused evidence, pinned `rustc 1.97.1`:
`cargo test -p plurx-core --test store_contract` — the reservation contracts
(one budget per epoch, two racing identities, settlement, the owner fence, the
replay identity, the readable epoch, and the identical unreadable-restriction
refusal) and the 295-name `Store` method inventory, run through the SQLite
fixtures. `cargo test -p plurx-core --lib producer_recovery` pins the schema's
size cap to the encoder's constant. `make effort-rust-check`, `make lint` and
`make validation-lint` clean; `git diff --check` clean.

**Owed at promotion:** the replicated half. `--features hiqlite-contract-tests`
runs these same contracts against a three-voter cluster and the v28 migration
test against an upgrading one; that run belongs to the `Main promotion gate`'s
`make validate-full`, and no count for it is claimed here.

## M3b working tree — owning the reading

M3a decided what a line *means*. M3b decides who owns the reading of it, and
what an attempt is allowed to claim afterwards.

Both of a producer's readers were `tokio::spawn`ed and forgotten. That is the
shape §7.1 refuses — "detached best-effort logging is insufficient for cache
qualification" — and the reason is not tidiness: a reader that died looked
exactly like a stream that was clean, and a stream that looks clean is what let
a broken title into the cache and keep it there. Both are now `JoinHandle`s held
by the attempt, in `ObservedDiagnostics`, and the only way to obtain an
attempt's health is to join its reader.

`settle(budget, exit_disposition)` is where an attempt becomes a
`ProducerHealthReceipt`. Three ways of not knowing produce the same answer, and
it is not the answer a clean stream produces:

| What happened | Receipt |
|---|---|
| The reader reached EOF | The accumulator's verdict — `Qualified`, `Unqualified` or `Rejected` |
| The reader did not reach EOF inside the drain budget | `observation_complete: false`, `Unqualified` — a bounded refusal to qualify rather than a hang |
| The reader panicked, was cancelled, or never existed | The same |

Both the drop path and the expiry path *abort* their readers rather than
detaching them, and the difference is not cosmetic: dropping a tokio
`JoinHandle` detaches its task, so a `timeout(budget, handle)` that expires
leaks a running reader — one per expiry, each still holding the pipe and still
writing lines nobody owns. That is precisely the reader-outlives-its-producer
shape this milestone removes, arrived at through the tidy-looking spelling. The
budget is one deadline for the whole settle rather than one per reader, so a
teardown cannot spend twice what its caller was told it could.

### What an unqualified build may claim, which is nothing

`DiagnosticPolicy` is resolved once per process, from the FFmpeg binary this
node is actually going to run: its version banner, the SHA-256 of the executable
file resolved through `PATH` the way the spawn will resolve it, and the SHA-256
of its `-buildconf`. The retained contract table is compiled into the binary
rather than read from disk, because a contract is evidence about one build and
the evidence a node applies has to be the evidence its own build was qualified
with — a file beside the daemon can be edited, can go missing, and can describe
a build nobody ran.

Every failure to measure is silence rather than a refusal to start. A node that
cannot hash its own FFmpeg loses automatic decoder actions and loses nothing
else; turning a diagnostic capability into an availability requirement would
trade a cache problem for an outage.

An attempt whose build no contract covers still has its stderr read, bounded and
logged. It simply has no grammar, so `HealthAccumulator::grammar_available` is
false and `qualifies_reuse` is false with it. `HealthAccumulator::new()` starts
with no grammar and `with_qualified_grammar()` is the only constructor that can
produce a qualifying observation, so the grammar and the permission to certify
are one decision rather than two that can drift. **That is the deployed fleet's
state today**: FFmpeg 5.1.9 reports `Error while decoding stream #0:0` without
naming a stream, no contract covers it, and a clean-looking stream from it
certifies nothing. The alternative — treating "we could not read it" as "it was
fine" — is the exact substitution that made #913 invisible.

A contract is matched only to a plan that *names* its decoder. Guessing the
family name is the same failure wearing a success: a hardware backend
substitutes `<codec>_qsv` and prints that in `[dec:…]`, and a software plan with
no measured implementation emits no `-c:v` at all so FFmpeg picks its own
default — `av1` selects the native decoder where it would otherwise choose
`libdav1d`. A contract found under the guessed name yields a grammar that
matches nothing, and a grammar that matches nothing marks every stream clean.
So the answer is no unless the backend is software *and* the plan named an
implementation. The daemon's startup inventory names none for any codec — that
is M1's deliberate honesty about reading `ffmpeg -decoders` — so no production
attempt reaches a grammar today, and the same qualified inventory that unblocks
it is M3's own remaining work.

### The log flags, and why they are not the default

§7.1 requires `-loglevel repeat+level+error` for a qualified grammar: `level`
supplies the severity labels the contract matches on, and `repeat` stops FFmpeg
compressing repeated messages into a summary whose timestamps the window cannot
honestly evaluate. `TranscodeExecution` carries the choice, because it is
execution context and not plan: the same semantic plan run on a node whose
diagnostics are qualified and on one whose are not is the same work and must
resolve to the same artifact identity. Only the flags differ, and the plan
digest never sees them.

A node asks for the qualified flags only when a retained contract covers the
exact binary it is about to run. So a fleet with no qualified build emits the
arguments it has always emitted, and the frozen M0 argv baselines keep
describing what ships. `AV_LOG_FORCE_NOCOLOR` is set for every FFmpeg child:
colouring is suppressed on a pipe today, but the grammar matches on exact
bracketed contexts and a build or environment that decided otherwise would make
every qualified line unreadable — silently, since an unmatched line is simply
unrelated.

### What this milestone deliberately does not do

Nothing consumes a receipt yet. The offline part producer settles one and logs
it, after its admission permits are back — the child is already reaped so the
drain is normally instant, and holding a hardware slot through a bounded wait
for a reader is the trade that path explicitly refuses. No cache reader, no
publication path and no actor decision reads a receipt, and at this head
`artifact_namespace()` returned `decoder-plan-v1-unqualified` for everything.
That is M3c.

A *rolling* attempt does not settle a receipt at all yet, and the teardown path
deliberately drops its reader instead. Settling there was the first draft and it
was wrong twice over: `terminate_exact_prepublication_child` is the ending for
every rolling attempt including ordinary retirement of a healthy session, so any
disposition it hardcoded would be wrong most of the time — and a receipt that
says `failed_termination` about a clean session is worse than no receipt — while
two of its callers hold `child_transition` across the call, whose own comment
forbids sleeping under it because that blocks the global reaper and every
explicit stop. The rolling receipt belongs at the exit classification, which
already knows which of the three dispositions an ending was and holds no session
lock. That, and the actor's two new `RollingProducerEvent` variants and sticky
health barriers, are M3b2.

Live TV keeps its bounded 2 KiB window. Replacing it with the shared line reader
would be a regression today rather than an improvement: its `decoder_unavailable`
latch is a setup signal that must survive a diagnostic arriving inside an
over-long line, and the retained test writes exactly that. It also has no plan
and therefore no codec to bind a grammar to. It joins M3b2, alongside the actor
barriers, when there is a selected-stream grammar for it to use.

### One further split, and why

The plan splits M3 into three. M3b is the largest of them by surface — it
changes the return type of both FFmpeg spawns and every caller — so it is split
again: **M3b1** (this candidate) owns the reading and produces receipts;
**M3b2** carries a fault to the actor, adds the sticky barriers, settles a
healthy rolling completion, and takes the live-TV and VOD-generation surfaces.
The same argument the plan used for splitting M3 applies: M3b1 is
behaviour-neutral for cache identity, because nothing consumes what it produces.

Focused evidence, pinned `rustc 1.97.1`:
`cargo test -p plurxd --bin plurxd decoder_health` — 38/38, including the
receipt matrix above, the deployed-build case, and the abort-on-expiry
regression. Daemon `transcode::tests::` 223/223 with the three new observation
tests; `live_tv::tests::` 40/40 unchanged; core `decoder_selection` 44/44 with
the log-flag test; core `transcode::` 138/138 unchanged.
`make effort-rust-check`, `make lint` and `make validation-lint` clean;
`git diff --check` clean.

The argv evidence is one explicit test rather than an unchanged count. The M0
baselines run through `hls_args_with_compatibility`, which passes
`DiagnosticLogging::Legacy` as a literal, so they are structurally incapable of
moving whatever the shipped builder emits — 44/44 passing says nothing about
`-loglevel`. `the_diagnostic_log_flags_are_execution_context_and_never_identity`
builds the same plan twice through the shipped `hls_args` and asserts that
exactly one token differs between the two commands, that it is the `-loglevel`
value, and that the mode a contract is matched under is the same string the
command emits.

### What the whole-PR review found, and what it changed

One adversarial review ran against the candidate. It found five blockers, all in
this milestone's own work; all are repaired in this head, together with six of
its non-blocking findings.

| Finding | Disposition |
|---|---|
| `settle` used `timeout(budget, handle)`, and dropping a tokio `JoinHandle` *detaches* its task. Every budget expiry leaked a running reader — the exact shape §7.1 refuses, reached through the code that claimed to remove it, and the comment above it asserted the opposite of what it did | Both handles are awaited by reference and aborted on expiry, under one deadline for the whole settle. `a_reader_that_never_reaches_eof_is_aborted_rather_than_detached` holds a `Drop` guard inside the reader task and fails if the task is still alive afterwards |
| `for_plan` fell back to the codec name when the plan named no decoder, so a contract could be matched under a name the child would never print. Because a contract *was* found, `grammar_available` stayed true and a stream whose every decode failure classified as `Unrelated` settled as `Qualified` | A contract is matched only when the backend is software and the plan names an implementation. `only_a_plan_that_names_its_decoder_gets_a_grammar` covers the three ways that can be false |
| Every rolling attempt settled as `FailedTermination`, including ordinary retirement, because the disposition was a literal in the one teardown path | The rolling settle is removed; see above |
| That settle was awaited while `child_transition` was held, whose own comment forbids sleeping under it — up to two drain budgets per teardown, in a loop | Removed with it |
| A producer SIGKILLed mid-line had its truncated tail returned as a whole line and its observation reported complete, so a decode record straddling the kill was dropped while the receipt said the stream was clean. §7.2: a truncated stream disallows a qualified receipt | `BoundedDiagnosticReader::finish` marks the observation incomplete for any non-empty partial line; a stream that ended on a newline is unaffected, and both cases are asserted |

Also repaired from the non-blocking set: the offline part settles after its
admission permits are released rather than before; `MeasuredBuild` checks the
executable bit the way `execvp` does, refuses a non-zero exit instead of hashing
an error message into `buildconf_sha256`, and streams the binary through the
hasher instead of reading a hundred megabytes into memory; `HealthAccumulator`
defaults to having no grammar; a copy attempt's receipt is named after its
session rather than by an empty digest; and the orphaned doc comment left
sitting above `ffmpeg_version` is back where it belongs.


## M3b2 working tree — the health barrier

M3b1 made the reading owned. It left the fault with nowhere to go: a reader
latched one, the receipt recorded it, and the actor — the thing that decides
what an attempt is allowed to do — never heard about it.

Two new producer events carry it. `DecodeFault` is published the moment the
reader latches, not when the stream ends, and that timing is the whole point: a
producer that stops decoding and keeps running holds its stderr open for the
rest of the film, so a fault delivered at EOF arrives after every success fact
it was supposed to precede. `DiagnosticsComplete` carries the settled receipt,
and §7.4 keeps it separate from process exit because a process can exit long
before its stderr reaches EOF — "the process finished" is not "we saw
everything it said".

### Why the fault needs its own slot

The ingress has a progress slot that *coalesces*. A batch of progress
publications collapses to exactly one observation at drain, chosen by a
precedence chain over the samples; anything riding on an intermediate sample is
dropped silently. A publication for an attempt older than the watermark is
discarded before it reaches any slot at all. §7.4's phrase for this is "cannot
be overwritten by progress compaction", and a field on a progress observation
would be overwritten by exactly that.

So the fault gets a sticky `Option<SequencedProducerBarrier>` slot beside the
existing `exit` one, and the receipt gets a third. Each is first-write-wins for
one attempt and replaced only by a later attempt, written as one rule rather
than three — an attempt has one terminal outcome, one latched fault and one
settled receipt. Sharing a slot would lose whichever arrived first, and the
case that matters is precisely a fault followed by a clean exit.

A barrier absorbs the progress batch that was open when it arrived. That is
what makes it a barrier: everything published before it is applied before it,
and nothing published after can reorder ahead of it. The retained test replays
the shape this milestone exists for — a fault, then twenty progress
publications at twice the speed, then an exit with status zero — and asserts
three things about it: that the twenty publications really do coalesce into one
block, that the fault barrier is applied *before* the exit that would otherwise
be the last word, and that the fault is still the answer afterwards. A companion
test drives an absorbed batch through the actor, because a batch that travels
with a barrier and is never applied would satisfy every ingress-shaped
assertion.

### Attempt scope, and what the actor does with it

A fault belongs to one attempt. A successor gets fresh counters, and a
predecessor's fault arriving late cannot condemn work it never touched, so the
actor drops a fault whose attempt is not current and a `latched_decode_fault()`
accessor is the single place that comparison is made. Re-latching is refused:
a second observation must not relabel a decision already taken from the first.

`commit_producer_decision_at` observes the latched fault before it builds the
decision, which is §7.4's "classifying exit must first observe all preceding
health barriers". **It does not yet change the decision.** This is observe
mode, and the plan's exit criterion for M3 says so: "Observation mode does not
initiate a new decoder retry." The typed reasons that let a fault select a
different alternative — `VideoDecodeFailure`, `DecodeBackendUnavailable`,
`DecoderRecoveryExhausted` — are M5b's, and the veto of a *reusable* artifact is
M3c's. What M3b2 owes is that the fault is present, ordered, and visible when
those arrive.

Visible means the operational snapshot: `decode_fault`, `decode_error_records`
and `diagnostics_qualification`, a bounded vocabulary and never a message, per
§9's rule that raw diagnostics stay out of the snapshot. An operator can now see
that a producer which exited cleanly was failing to decode the whole time,
which before this effort could not be seen at all.

### Where the rolling receipt settles

In the child supervisor, after it publishes the terminal and before it goes
away. That is the one place that knows how the process actually ended — so the
disposition is derived rather than assumed, and only a process that exited zero
on its own is a `CleanEnd` — and the one place holding no session lock, which
is what M3b1's review found wrong with settling in the teardown path.

### Still not here

Nothing consumes a receipt: no cache reader, no publication authorization, no
actor decision, and at this head `artifact_namespace()` returned
`decoder-plan-v1-unqualified` for everything. That is M3c.

Live TV keeps its bounded 2 KiB window, and the reason is unchanged and worth
repeating rather than quietly dropping: its `decoder_unavailable` latch is a
*setup* signal that has to survive a diagnostic arriving inside an over-long
line, and the retained test writes exactly that — a 32 KiB preamble with no
newline followed by the diagnostic. The shared line reader would discard that
tail by design. Live TV also has no resolved plan and therefore no codec to bind
a grammar to. It needs a selected-stream grammar of its own, which is M3c's
qualified-inventory work.

Focused evidence, pinned `rustc 1.97.1`:
`cargo test -p plurxd --bin plurxd playback_control::tests::` — 225/225, with
six new contracts: the fault surviving healthy progress and a clean exit, the
barrier absorbing its open batch and that batch reaching the actor, three
barrier kinds keeping three slots, the once-per-attempt latch together with the
successor's own first fault being accepted, the snapshot's two sources being
distinguishable, and both clauses of both admission guards.
`transcode::tests::` 225/225, with the reporting path driven end to end: a
grammar latches inside a real reader, the sink reports, the handle publishes,
and the actor's snapshot carries it. Without that one, replacing
`DecodeFaultSink::report` with an empty body left the entire workspace green.
`decoder_health` 38/38, `live_tv::tests::` 40/40 and core `decoder_selection`
44/44 unchanged. `make effort-rust-check`, `make lint` and
`make validation-lint` clean; the rolling-producer ownership ledger is unchanged,
because this milestone adds no task, timer or process shape.


### What the whole-PR review found, and what it changed

One adversarial review ran against the candidate. It found two blockers and a
test gap that mattered more than either; all are repaired in this head, together
with four of its non-blocking findings.

| Finding | Disposition |
|---|---|
| The re-latch guard was a bare `is_some()` on a field that is never cleared. A stale fault from a predecessor therefore refused the *successor's first* fault — so the recovery attempt, the one this feature exists to inform, was the one running blind, and the drop was counted as a rejected stale exit | The guard reads through `latched_decode_fault()`, and both health fields are cleared when an attempt begins. The test now asserts the successor's own fault is accepted, which the original did not |
| The teardown path still took and dropped the same diagnostics handle the supervisor now owns, racing it. Whoever won decided whether the attempt got a receipt at all | The teardown leaves it alone; the supervisor is the only owner. `AttemptChild::observing` also takes the reader at construction, closing the window in which a child that died immediately could reach the take before the reader was stored |
| Nothing tested the *reporting* path. Replacing `DecodeFaultSink::report` with an empty body left all six new tests and the whole `decoder_health` suite green — the feature dead in production and the suite silent about it | `a_latched_fault_travels_from_the_reader_to_the_actor` drives a real reader, a real sink and a real actor handle, and asserts the fault reaches the operational snapshot |

Also repaired: health no longer borrows the `exit` metrics label;
`decode_error_records` falls back to the settled receipt; the fault's plan
digest is read rather than carried unused; and a doc comment orphaned onto the
wrong function is back where it belongs. The review's remaining observations —
that a retried attempt's receipt is usually dropped, that a copy session reports
nothing, and that only one of §7.4's four observation points is wired — are
recorded above as limits rather than repaired, because each is M3c's or M5b's to
close.

### A correction to the plan's own command list

Plan §13 lists `cargo clippy -p plurx-core -p plurxd --all-targets -- -D
warnings` and its `--no-default-features` sibling. Both are red on the merged
effort head `f7f98b01` itself, before any M2 change: selecting those two
packages individually drops the feature unification the workspace build
provides, so `hiqlite`-gated fixtures in
`crates/plurx-core/tests/store_contract.rs` and the `live-hls-recovery`
surfaces in `admission.rs` and `playback_control.rs` become dead code. Measured
on the untouched M1 clone at `f7f98b01`: 3 dead-code errors for the
package-selected form and 10 for the no-default-features form.

The repository's own gate is `make lint` — `cargo clippy --workspace
--all-targets -- -D warnings` (Makefile) — and that is what CI runs. It is
green on the M2 working tree, as is `make effort-rust-check`. This effort
therefore records the workspace commands as its lint evidence throughout, and
does not repair unrelated pre-existing dead code to satisfy a package-selected
invocation the repository does not use. §13's list should be corrected to the
workspace form.

## M3c1 working tree — the artifact carries what was observed about it

M3b settled a receipt per producer attempt and threw it away. This milestone
gives it somewhere durable to live: the generation manifest, which is the file
every cache reader already loads and authenticates before serving a byte of a
title.

`ProducerHealthReceipt` moved from `crates/plurxd/src/decoder_health.rs` to
`crates/plurx-core/src/transcode/health.rs`, because the manifest carries it
and the manifest is core's. Only the *shape* moved. Everything that produces a
receipt — the grammar, the accumulator, the bounded reader — stays in the
daemon, and the derivation moved with it rather than staying behind: settling an
observation is now `HealthAccumulator::settle_receipt`, a method on the only
object that saw the stream. `plurx-core` deliberately owns no way to decide that
an attempt was clean.

### The receipt is inside the digest, not beside it

`GenerationManifest` gained `producer_health: Option<ProducerHealthReceipt>`,
and so did the private `ManifestBody` that `body_digest` hashes. That second
half is the whole point. A receipt the manifest digest did not cover would be a
reuse certificate that anyone able to write the generation directory could
edit, which is worse than no certificate at all, because a reader would believe
it. Two tests hold that line from both directions: promoting `"unqualified"` to
`"qualified"` in a published manifest fails validation, and so does deleting the
receipt from one.

The field is `#[serde(default, skip_serializing_if = "Option::is_none")]`, and
that is a compatibility contract rather than a style choice. Every generation
already on every deployed box was published by a build with no such field, and
its stored digest was computed over a body without one. A receipt-less manifest
therefore has to serialize — and so digest — exactly as it always did.
`a_manifest_without_a_receipt_digests_exactly_as_it_did_before_the_field`
asserts that against a locally declared copy of the pre-field body struct, so
the test would fail if a future edit changed the encoding even in a way that
kept the new field absent.

The constraint is one-way, and that is worth stating rather than discovering.
Old manifest into new build authenticates; new manifest into *old* build does
not, because the older `body_digest` cannot see a field its `ManifestBody` does
not have. Rolling this change back on a node that has already published
receipts therefore invalidates the generations produced in the interval — they
are re-encoded, not served wrong — and a rollback plan has to expect that.

### Absent means unqualified, everywhere

There is no third state. A manifest with no receipt is treated exactly as one
with a rejected receipt would be: not reusable. That makes every path this
milestone does not thread — `publish`, the legacy-generation adoption in
`produce_normalized`, the shared cache, offline, the scrub's own fixtures —
correct by default rather than by inspection. `publish` cannot take a receipt at
all, which is honest: it exists for callers assembling bytes they did not watch
being produced.

### Joining the parts, weakest wins

A generation is one artifact assembled from many parts, so
`ProducerHealthReceipt::join` reduces every contributing part's receipt to the
one the generation presents. Qualification takes the weakest; counters sum;
the first terminal fault survives; the exit disposition takes the worst; and
the contract identifier survives only if every part named the same one, because
parts read by different grammars have not been read by one grammar.

What enters the join is *every producer attempt this pass made*, not one
receipt per part — a distinction the first draft of this milestone got wrong,
and the review caught. An attempt whose decode fails writes no segment and
exits zero, which is the exact failure this whole effort is named after; keying
the record on "did it leave bytes behind" discards precisely the receipt worth
keeping. Nothing else would have caught it either: the producer's progress
observer is `FfmpegProgressObserver::offline`, which carries no control handle,
so `DiagnosticObservation::fault_sink` returns `None` and the actor never hears
about an offline part's fault. The receipt was the only record, and it was
being thrown away. `GenerationObservation` now owns the accumulation —
`inheriting`, `record`, `settle` — so "record every attempt" is the type's
shape rather than a line one `if` could re-capture.

A part this pass inherited from an earlier one enters that join as
`unobserved`, which is `Unqualified` by construction, and one of those is
enough to refuse the whole generation. A long film is produced across many
preempted passes, and certifying it from the tail this pass happened to watch
is the same false certificate in a different disguise.

An intentional yield does *not* weaken the join. Almost every part in this
pipeline ends by yielding; if it did, nothing would ever be certified.

### Two places the receipt is deliberately dropped

`publish_from` returns early when an earlier pass already placed the assembled
generation. This pass's receipt describes parts, not those bytes, so it is
dropped and the adopted assembly carries none.
`an_adopted_assembly_never_inherits_this_pass_receipt` pins it, and the
legacy-manifest adoption path in `produce_normalized` passes `None` for the same
reason.

The other is a receipt whose own fields are impossible — a plan digest that is
not a digest, a contract identifier past its bound, more contract-qualified
records than structural ones. Those are programming errors, not inputs: the
counters are assembled in one process from one accumulator, and the identifier
bound is now enforced where the identifier is authored.
`MAX_DIAGNOSTIC_CONTRACT_BYTES` and `safe_diagnostic_contract_id` live beside
the receipt in `plurx-core`, and `DiagnosticContract::load` refuses a table that
would produce an unstorable id. Without that, a contract named with a Debian
epoch — `ffmpeg-7:6.1.1-3ubuntu5-h264-v1` — would load cleanly and then quietly
strip the receipt off every generation the fleet published. Publication treats
one as a `debug_assert` and drops the receipt, leaving the generation serveable
and uncertified. Refusing to publish would trade a whole encoded film for a
wrong integer. On the *read* side the same bounds are a hard rejection, because
there the bytes are an input, and a self-consistently digested manifest from a
directory an attacker can write must not be believed just because it hashes.

### Still not here

Nothing yet *acts* on a receipt. At this head `artifact_namespace()` returned
`decoder-plan-v1-unqualified` for every plan, `manifest_cache::load` still
authorizes reuse on digest equality alone, and the `cachekeep` scrub, the
`resume_parts` validation branch and `assembled_publication` all still accept an
unobserved generation. Splitting the namespace — which must depend on the
*policy* capability at plan time, never on the post-hoc receipt, or the cache key
would depend on its own contents — and gating those four readers is M3c2.

Live TV still has no plan and therefore no grammar; that is unchanged and still
M3c's qualified-inventory work.

### What the whole-PR review found, and what it changed

One adversarial review ran against the candidate. It found one blocker, and the
blocker was the milestone's own central claim being false on the one path that
matters most; all findings are repaired in this head.

| Finding | Disposition |
|---|---|
| A part's receipt was recorded only when the part produced bytes. An attempt whose decode fails writes no segment and exits zero — the exact failure this effort is named after — so its `Rejected` receipt was logged and dropped, and the truncated film published as `qualified`. The comment justifying the drop said the actor already had the fault; it does not, because the offline progress observer carries no control handle and `fault_sink` returns `None` | `GenerationObservation` records every attempt, produced or not, before anything looks at what it wrote. The rule is the type's shape now, and `an_attempt_that_produced_no_bytes_still_refuses_the_generation` pins it |
| The status-doc rewrite broke two assertions in `tests/validation/test_decoder_recovery_status.py`, which the milestone's own claim of a clean `make validation-lint` said nothing about | The contract test moves to the M3c1 strings, and gains `test_m3c1_receipt_is_authenticated_and_unattributed_bytes_carry_none` |
| The contract-identifier bound was an invariant on a data file with nothing enforcing it at authoring time. A Debian-epoch id would `debug_assert` in test builds and silently uncertify every generation in release | `MAX_DIAGNOSTIC_CONTRACT_BYTES` and `safe_diagnostic_contract_id` moved to `plurx-core` beside the receipt, and `DiagnosticContract::load` refuses an unstorable id with `ContractLoadError::UnsafeId` |
| Forward compatibility is one-way — a manifest with a receipt does not authenticate against an older build — and nothing said so | Recorded above, under the compatibility contract |
| `join` weakened `qualification` on a plan-digest mismatch but not `exit_disposition`, and still attributed the foreign part's contract identifier to this generation | A mismatch now clears the identifier and weakens every mergeable field. `a_mismatched_plan_weakens_every_field_it_can` covers it |
| Two new tests were vacuous: an invariant asserted as `u64::MAX <= u64::MAX`, and an emptiness that held by construction | Replaced by `each_counter_sums_its_own_field_and_saturates_there`, which fails if the join reads the wrong field, and by a settle test that also asserts the positive case |

Focused evidence on this head, pinned `rustc 1.97.1`: core
`cargo test -p plurx-core --lib -- transcode::` 157/157, which is 13 new
`transcode::health::` join contracts and 6 new `transcode::manifest::` receipt
contracts on top of the previous 138. Daemon `transcode::tests::` 230/230, with
five new contracts — the inherited part refusing the generation it was carried
into, a pass that attempted nothing settling to no receipt and a clean pass
certifying its own, an attempt that produced no bytes still refusing the
generation, a fresh assembly carrying the receipt its parts earned, and an
adopted assembly carrying none. `decoder_health` 38/38 and
`playback_control::tests::` 225/225 unchanged. `make lint` and
`make validation-lint` clean;
`python3 -m unittest tests.validation.test_decoder_recovery_status` 11/11 with
one new contract; the rolling-producer ownership ledger is unchanged, because
this milestone adds no task, timer or process shape.

## M3c2 working tree — a part carries its own observation across a resume

M3c1 left a defect that only becomes visible when you try to *use* a receipt.
A generation is encoded across many preempted passes, and `GenerationObservation`
had to call every inherited part `unobserved` because nothing survived the gap
between passes. Weakest wins, so any film long enough to need a second pass was
permanently `Unqualified` — meaning the qualified artifact namespace M3c3 is
supposed to introduce could never hold a long title at all, and a reader gating
on the receipt would re-encode such a film from scratch on every pretranscode
pass forever.

That is not the conservative answer. It is the useless one, and it is worth
naming the difference: refusing to certify what you did not observe is correct;
refusing to *read back* what you did observe, and then calling the resulting
ignorance safety, is not.

### The record, and what makes it evidence

Each produced part now carries `.part-health.json` beside its segments: a
`RetainedPartReceipt` holding the settled `ProducerHealthReceipt`, the shape it
was settled over, and a digest of the two.

`part_shape_digest` is what makes it evidence rather than an assertion. It
covers the plan digest and, for every listed segment, its name, its byte count
and its `EXTINF` — values `read_validated_part` already measures on the way
past, so measuring the shape costs nothing beyond what validating the part
already cost, and reading a record adds one bounded 4 KiB open-and-read per
resumed part.

It deliberately does not cover segment *content*, and that is a cost decision
rather than a claim that content could not matter. A seal-time content digest
is the only thing that would catch an in-place, same-size mutation of a
resumed part's bytes between one pass and the next; the shape by construction
cannot, and the generation manifest cannot either, because it hashes the
objects at publication of *this* generation — after the resume. Hashing every
resumed segment on every pass is a full read of the film, which is the exact
cost resuming exists to avoid, and the staging tree is node-local. So an
in-place same-size mutation between passes is out of scope, stated plainly
rather than argued away.

What the shape does catch is the case that actually occurs: a record left
behind by a part that was truncated, re-encoded to a different length, or
renumbered. A deterministic re-encode under the same plan and the same build
would produce the same sizes, so the shape is not a proof of identity — the
record is refused whenever the bytes visibly moved, not certified whenever they
did not.

The shape is digested under *this pass's* plan, not the one the record names, so
a record can only be opened by the plan it was sealed for. A receipt from
another plan is evidence about other bytes even when the sizes happen to line
up.

`record_digest` covers the rest, so a torn or edited record is refused rather
than read — including the obvious edit, rewriting `part_shape` to whatever is
actually on disk, which changes the body and therefore the digest.

### Everything that is not exactly that reads as unobserved

No record, an unreadable one, a version this build does not know, one over its
4 KiB bound, one whose digest does not check out, one sealed over a different
shape, one sealed for another plan. Every one of those yields
`ProducerHealthReceipt::unobserved`, which is what the code did before this
record existed.

The digests are unkeyed SHA-256 over public inputs and `seal` is public, so the
records are corruption-evident, not tamper-evident: they prove nothing against
anyone who can write into the staging tree. That is the right bound for where
they live — a node-local staging directory no peer and no shared cache writes
into — and it is the generation manifest, which is what other nodes actually
read, that authenticates its own copy of the conclusion.

Writing is best effort for the same reason. The bytes are already on disk and
already listed in a playlist; a staging directory that will not take a 4 KiB
record is not a reason to throw away an encoded part. The record is written
after the segments, so a crash between the two leaves a part with no record —
unobserved, which is safe.

### The attempt that leaves no part

A part record can only describe a part that exists, and the attempt this whole
effort is named after leaves none: FFmpeg drops every frame, exits zero, writes
no segment, and the retry reuses the same directory. `GenerationObservation`
records that receipt within a pass — M3c1's review is what put it there — but
until this milestone it was forgotten at a pass boundary, so whether a film
certified depended on where preemption happened to fall. A failed attempt
followed by a clean retry inside one pass refused the generation; the same two
attempts either side of a preemption did not.

`.generation-health.json` at the staging root carries it. It is a monotone
weakening accumulator: each pass joins its own non-producing attempts into
whatever it read and writes the result back, so the value can only become more
restrictive. It is written the moment such an attempt is recorded rather than
at the end of the pass, because a pass about to be preempted is precisely the
one whose observation would otherwise be lost.

Its absence and its unreadability say different things, and the asymmetry with
a part record is deliberate. A part's bytes exist whether or not a record
describes them, so a missing part record is `unobserved`. The ledger describes
no bytes at all, so its absence is "no pass has claimed an unproductive
attempt" — nothing to carry. A ledger that is *present* and will not open is a
statement that something was recorded and cannot be read, and that is
`unobserved`.

### Parts read by different grammars

Now that a part survives a pass, an FFmpeg upgrade between two passes of one
film gives two parts two different diagnostic contracts under one plan digest,
which does not name the build. `join` already erased the contract name in that
case; it now weakens the qualification too. `Qualified` beside
`diagnostic_contract: None` is indistinguishable, to a reader, from a receipt
no grammar ever covered — and M3c3 should not have to guess which it is
looking at.

### Where it does not go

The record is staging-local evidence about how a part was made, not one of the
objects the manifest inventories, so it never reaches the assembled generation.
`a_record_is_never_placed_into_the_assembled_generation` pins that: `assemble`
places only what the part playlists list, and a dotted name could not be a
segment name in any case.

### Still not here

Nothing yet acts on a receipt. At this head `artifact_namespace()` returned
`decoder-plan-v1-unqualified` for every plan, and no cache reader consults one.
That is M3c3 and M3c4, and they now have something to enforce: with this
milestone a long film resumed across passes can reach `Qualified`, which before
it could not.

The identity's shape is also now settled, and the reason is worth recording rather
than discovering during it. The namespace must not split on per-node contract
coverage: `contract_for` matches `binary_sha256` and `buildconf_sha256`, so two
nodes running two distribution builds of the same FFmpeg version would compute
different cache keys for the same source and the same encode, fragmenting the
replicated store and `cache_consumer_pins` mid-rollout. Plan §"Use a new recipe
namespace/version" says the split happens when *qualification is turned on* —
an operator decision, applied fleet-wide, with the cache rotation deliberately
accepted — and that "observation-only rollout uses explicit unqualified/legacy
output identity until qualification is enabled". So the namespace follows the
effective qualification mode, and the four readers are gated only at retain
boundaries, never at a serve boundary: unqualified bytes may still be served to
the viewer waiting for them.

### What the whole-PR review found, and what it changed

One adversarial review ran against the candidate. It found no blocker and one
major defect, together with five findings about tests and documented claims;
all are repaired in this head.

| Finding | Disposition |
|---|---|
| `retain_part_health` was gated on `if produced`, so persistence was keyed on exactly the thing `GenerationObservation` exists to refuse to key on. Within a pass a non-producing failed attempt refused the generation; across a pass boundary it was forgotten, and whether a film certified depended on where preemption fell | The staging-root ledger described above, written the moment such an attempt is recorded. `an_attempt_that_left_no_part_is_carried_across_a_pass_boundary` runs the two-pass sequence and asserts both passes reach `Rejected` |
| `join` erased the contract name when parts disagreed without weakening, and a build upgrade between passes is now an ordinary way to reach that. The result was `Qualified` beside `diagnostic_contract: None` — what a reader also sees when no grammar ever covered the bytes | Disagreement now weakens to `Unqualified` as well as clearing the name |
| `RetainedPartReceipt::body_digest` fed the version *constant* rather than the record's own field, so `record_version` was outside its own digest — safe only while exactly one version is accepted | The version is digested from the record |
| `a_record_is_never_placed_into_the_assembled_generation` could not fail: no production line can put a dotted name into an assembly | It asserts the assembled directory's whole listing instead of probing one name |
| `an_inherited_part_refuses_the_generation_it_was_carried_into` asserted, after the refactor, on receipts it had constructed itself | Removed; `a_part_with_no_record_resumes_unobserved` covers the real path |
| Two python contract assertions could not fail, and `M3C_TASK_BASE` was left unreferenced by a test whose name is `…_cannot_be_confused_with_history` | The `unobserved` assertion is scoped to the `resume_parts` slice, the unfalsifiable one is replaced, and M3c1's merged head is pinned |
| Doc claims: "would buy nothing" about content hashing, "no extra `stat`", "cannot manufacture one", and "a re-encoded part cannot keep every segment's byte count and duration" — none of them true as written | All four restated above as what they actually are: a cost decision, a bounded extra read, corruption-evidence rather than tamper-evidence, and a refusal that triggers when bytes visibly move rather than a proof of identity |
| The record-over-bound branch had no test and the "torn record" test edits rather than tears | `a_record_too_large_for_its_bound_is_not_written` covers the branch, and the test is named for what it does |

Focused evidence on this head, pinned `rustc 1.97.1`: core
`cargo test -p plurx-core --lib -- transcode::` 162/162, five new
`transcode::health::` record contracts. Daemon `transcode::tests::` 239/239,
with the milestone's own contracts: a resumed film with clean records
certifying, a part with no record resuming unobserved, a record no longer bound
to the bytes beside it, a record sealed for another plan, an edited record, a
record over its bound, the record never reaching the assembled generation, an
unproductive attempt carried across a pass boundary, and a ledger that is
present but will not open. `decoder_health` 38/38 and
`playback_control::tests::` 225/225 unchanged. `make lint`,
`make validation-lint` and `make history-check` clean;
`python3 -m unittest tests.validation.test_decoder_recovery_status` green with
one new contract; the rolling-producer ownership ledger is unchanged, because
this milestone adds no task, timer or process shape.

## M3c3 working tree — the qualified artifact identity

`artifact_namespace()` returned one constant, so every plan named the same key
space and nothing could ever be produced under a contract. This milestone gives
qualified production a name of its own:
`ArtifactQualification::{Unqualified, HealthQualified}`, carried on
`ResolvedTranscode`, set from the policy snapshot at resolution, and fed to
both `plan_digest()` and `Recipe::hash`.

### Why it is a plan-time decision, and why it is fleet-wide

It cannot come from the receipt. The receipt describes bytes the plan has not
produced yet, so a key derived from it would depend on its own contents.

It also must not come from whether the running FFmpeg build happens to have a
diagnostic contract, and this is the sharper trap because it looks reasonable.
`DiagnosticContract::covers_build` matches `binary_sha256` and
`buildconf_sha256`. Two nodes running two distribution builds of the same
FFmpeg version would compute two different cache keys for the same source and
the same encode — fragmenting the replicated store, and `cache_consumer_pins`
with it, for the whole duration of a rolling upgrade. The plan digest already
refuses this one level down for `DecodeEvidence`, with the comment that giving
a qualified node a private key space "would split the fleet's cache
mid-rollout"; the namespace has to refuse it for the same reason.

So the identity follows the effective qualification mode: an operator's
persisted request, bounded by what the node can do. That is a fleet-wide
decision, taken once, and the plan document accepts its cost explicitly — "the
broader project deliberately accepts a new cache namespace when the new
qualification contract turns on".

### Shipping it costs nothing until it is used

`ArtifactQualification::Unqualified` is the default and its namespace is the
existing constant, so a fleet that has not turned qualification on computes
byte-for-byte the artifact names it computed before this type existed.
`the_qualified_identity_is_a_separate_key_space_and_costs_nothing_until_it_is_used`
asserts both halves: identical digest and recipe hash when the mode is
unstated or stated as unqualified, and a different digest *and* a different
recipe hash under the qualified one — with the decode decision and the encoder
unchanged, because this is an identity, not a different encode.

Separate namespaces are also how "no migration mode may label old artifacts as
health-qualified without reprocessing them" is enforced rather than promised.
Nothing under the qualified name can be an artifact that predates the contract,
because no plan that predates the contract ever computed that name.

### What is deliberately not here

No production site selects `HealthQualified` yet, and that is the sequencing
rather than an oversight. Turning the identity on rotates a fleet's whole key
space; an operator control that did that while nothing yet enforced a receipt
would be a cache flush in exchange for nothing. The operator setting, the
per-node effective-mode intersection, the `Settings > Developer` enable section
with its prerequisites, and the enforcement itself land together in M3c4, so
the control never exists without the behaviour behind it. This milestone is the
identity and its digest behaviour, exercised end to end through
`resolve_transcode`.

M3c4's shape is settled too, and the reasons are recorded here because they are
what makes the enforcement safe rather than merely present:

- **Enforce at retain, never at serve.** Unqualified bytes may still be served
  to the viewer waiting for them. `serve_cached`, `prepare_shared_cached_read`
  and the offline package download are serve paths and stay ungated — and so
  does `verified_cache_hit`, which reads like a reuse decision and is not: its
  only caller feeds cluster offer *eligibility*, so refusing there does not
  decline to keep a generation, it moves the viewer to a node that has to
  encode the film again. The retain decisions are `produce_normalized`'s reuse
  branch and the publication and claim writes at the end of that function.
- **Never through `invalidate_cache_location`.** It removes the location row
  and, via `invalidate_cache_entry`, deletes dependent `cache_consumer_pins` and
  flips dependent `ready` `offline_packages` to `failed`; the bytes themselves
  are left for orphan reclamation, and the whole call is a no-op for the
  `shared` storage class. Failing a package a user already downloaded is reason
  enough never to route a health refusal here.
- **Not in the `cachekeep` scrub.** Every generation published before receipts
  existed carries none, so a scrub that treated an absent receipt as corruption
  would delete an entire fleet's cache in one rotation. In the qualified
  namespace the question does not arise: nothing under that name predates the
  contract.
- **`complete = 0` is not "serve but do not reuse".** Such a row is invisible to
  `cache_bytes` and to `cache_by_age`, so its bytes are unbudgeted and
  unevictable, and `stale_cache_claims` reaps the directory on an age bound. A
  refusal has to be a state of its own, not a reuse of the claim state.
- **The queue needs a terminal refusal.** `enqueue_pretranscode_job` has two
  independent suppression clauses. One is freshness: a `ready` job with a live
  `complete = 1` location, both inside their own recency bounds. The other is
  terminal: any same-`dedupe_key` row in `queued`, `running`, `failed` or
  `cancelled`, *unless* its `last_error_code` says the refusal was provisional
  — `policy_changed`, or `eligibility_expired` past its retry window. A health
  refusal that left only bytes behind would re-enqueue the same dedupe key on
  every discovery pass and re-encode forever; it has to land in the second
  clause with an error code that is not re-enqueueable.
- **Ride `expected_policy_generation`.** The house pattern for settings is a
  per-request read, so an operator flipping the mode mid-production would let a
  claim be taken under one policy and settled under another. That class of
  change already has a mechanism in this function and the health mode should use
  it rather than invent a second one.

### What the whole-PR review found, and what it changed

One adversarial review ran against the candidate. No blocker; one major error,
and it was in this document rather than in the code — which matters, because
the bullets above are the constraints a later milestone will implement
literally.

| Finding | Disposition |
|---|---|
| `verified_cache_hit` was listed as a retain decision. Its only caller feeds cluster offer eligibility, so gating it would not decline to keep a generation — it would move a viewer to a node that has to encode the film again, which is the serve refusal the same bullet forbids | It is named as a serve path, with the reason it reads like a reuse decision. The stale in-code comment pointing the same wrong way is corrected too |
| The queue-dedupe bullet said suppression happens "only" via a `ready` job with a live `complete = 1` location. There are two independent clauses, and the terminal one — a `failed` or `cancelled` row with a non-provisional `last_error_code` — is the one a health refusal actually needs | Both clauses described, with which is freshness and which is terminal |
| `invalidate_cache_location` was described as deleting the row and the bytes. It removes the row and dependent pins, fails dependent `ready` packages, leaves the bytes for orphan reclamation, and is a no-op for the shared storage class | Restated; the conclusion is unchanged and now rests on the true reason |
| The recipe-hash half of the new test is entailed by the plan-digest half, because `plan_digest` already feeds the namespace — so removing `Recipe::hash`'s own `plan_namespace` field leaves the test green | The claim of symmetric coverage is withdrawn, and `Recipe::hash` now says why the redundancy is deliberate: `plan_digest` is a revisable field list, and this field is what stops a revision from silently merging the two key spaces |
| Three earlier sections asserted in the present tense that `artifact_namespace()` returns one constant, and the M3c2 section still pointed reader enforcement at M3c3 after this PR renumbered it | Past-tensed against the heads they describe, and renumbered |
| `enforces_receipt` used `matches!`, so a third identity would default to no enforcement | Exhaustive `match` |
| `ArtifactQualification` derived `Serialize` with a wire spelling nothing serializes or tests | Derive dropped until something needs it |

Focused evidence on this head, pinned `rustc 1.97.1`: core
`cargo test -p plurx-core --test decoder_selection` 46/46, two new — the
separate key space that costs nothing until used, and the refusal to let a
node's capability inventory move the artifact identity. `make lint`,
`make validation-lint` and `make history-check` clean; the rolling-producer
ownership ledger is unchanged.

## M3c4 working tree — the receipt contract, enforced

M3c3 named the qualified identity; this is what the name promises. One rule,
`generation_permits_reuse`, decides in both directions — whether an existing
generation may be reused, and whether one just produced may be kept — and it
reads the *manifest*, never any in-memory value.

That last point is the design. A receipt that exists only inside the process
that settled it cannot certify anything to whatever reads the bytes tomorrow,
so under the qualified identity a generation with no manifest is refused
exactly as one whose receipt refuses itself is. Both directions read the same
artifact, so a generation that was kept can be reused, and one that could not
be kept can never be found.

"Can be reused" holds across builds only because the qualified namespace names
the receipt schema version — `decoder-plan-v1-health-qualified-r1`, with a
compile-time assertion tying the two together. Without that, a build that
bumped the receipt version would compute the same artifact keys as one that
did not, find rows it could not read, refuse every request for them, and have
no way to replace them; naming the version makes version skew a key-space
change instead, which is the one shape this effort already knows how to
survive.

The unqualified identity keeps its present behaviour exactly. It never asked
for a receipt and it still does not, and the rule's first line says so.

### Refusing to keep is not refusing to serve

The check runs after the generation is assembled and renamed into place, and
before any write that would let a *future* request find it. What it declines is
the row.

That distinction is a rule about where the check belongs, not a claim about
who is watching: neither caller of `produce_normalized` has a live session
attached, so today a refused generation is served to nobody. The rule matters
because the same predicate is what a serving path would have to consult, and it
is written so that consulting it never deletes or hides bytes that exist.

It is also not an invalidation. `invalidate_cache_location` removes the
location row, deletes dependent consumer pins, and flips dependent `ready`
offline packages to `failed`; routing a health refusal through it would fail a
download a user already has over a decode fault in some other production. The
refusal quarantines the staging tree it just made and writes nothing.

### The refusal is terminal, because retrying is an encode loop

`OfflineProduceOutcome::HealthRefused` is a distinct outcome rather than a
yield, and each caller settles it as terminal:

- A queue job is **cancelled** with `health_refused` — deliberately not one of
  the re-enqueueable codes. `enqueue_pretranscode_job` suppresses a duplicate
  for any `queued`/`running`/`failed`/`cancelled` row whose `last_error_code`
  is not provisional, so this stops the dedupe key from being re-enqueued on
  every discovery pass. Yielding instead would re-encode the title forever: the
  same plan on the same source reaches the same decoder and settles the same
  refused receipt.

  The terminality is bounded rather than absolute, and it is worth stating
  where: terminal rows are pruned beyond `MAX_TERMINAL_TOMBSTONES`, newest
  first, so on a library that churns terminal rows a `health_refused` tombstone
  is eventually collected and the title is re-encoded, refused, and
  re-tombstoned once. That is a bounded periodic cost, not a loop, and it is
  the same bound every other terminal code lives under.
- An offline package **fails** with `decode_unhealthy`, a message that says
  what the user can act on — the server could not produce a verified copy —
  and its own label in `plurx_offline_failures_total`. Bucketing it with
  `encoder_failed`, or with `other`, would hide the one thing an operator
  needs to see: the encoder did not fail.

- The **cached-reuse** refusal is the same terminal outcome rather than an
  `Err`. `Err` is retryable in both callers, so a condition stored on disk
  would be retried on a backoff forever and reported to a user as an encoder
  fault.
- Speculative warming has nothing to settle: no queue row, no package. The
  absence of a warmed entry is the whole report.

Every path that took an unfenced claim releases it, exactly as the failure arm
does. Leaving one behind is not litter: the next request for that recipe cannot
claim it, finds no staging tree to resume, and stands down — while the reaper
refuses to collect a claim whose package is still queued. The package would
hold the claim and the claim would hold the package.

### What this cannot yet keep, and why that is stated rather than patched

Only the pretranscode-queue path publishes a manifest. Speculative warming and
offline preparation settle a receipt but write it nowhere, so under the
qualified identity they retain nothing — an offline package would fail with
`decode_unhealthy` even when its own production was clean.

That is a real gap and it is deliberately not papered over here. The fix is to
give those paths a manifest, and doing it carelessly is expensive and
far-reaching: `publish_controlled_directory` is a second full read of every
byte of the generation, on the interactive warm path whose whole purpose is to
be ready before playback needs it; a manifest on those rows would also newly
make them eligible for shared-cache fanout and for cluster placement offers,
enrol every cached generation in the integrity scrub, and flip offline segment
serving from lenient to fatal on pre-existing bit rot. Each of those is a
decision, not a detail. M3c5 makes them.

### Nothing turns this on

`TranscodeManager::test_publish_artifact_qualification` is the only writer of
the effective identity, and it is `#[cfg(test)]`. The operator setting, the
node effective-mode intersection and the `Settings > Developer` enable section
land with M3c5's manifest work, so a control that rotates a fleet's key space
never exists before everything behind it works. What is here is the behaviour
that control will select, complete and exercised.

### What the whole-PR review found, and what it changed

One adversarial review ran against the candidate. It found two blockers and two
major defects; all are repaired in this head.

| Finding | Disposition |
|---|---|
| `assembled_publication` returned `health: None`, discarding a receipt `produce_into` had already loaded eight lines earlier. Assembling a long film and then hashing it for its manifest is preemptible, so a pass leaves a finished assembly behind and yields; the next pass adopted it receipt-less and, under the qualified identity, refused the film **permanently** — the exact outcome the per-part records exist to prevent, reached one step later | The adoption carries the settled receipt when the on-disk playlist is byte-equal to the one `assemble` produces from these parts, which is an exact tie because `publish_from` writes those bytes. Two tests: the parts' own assembly keeps the receipt, an assembly they did not produce carries none |
| The refusal quarantined the bytes and left the `complete = 0` claim row behind. The next request for that recipe cannot claim it, finds no staging tree to resume, and stands down — while `stale_cache_claims` refuses to collect a claim whose package is still queued. The package holds the claim and the claim holds the package: a two-second retry spin with no exit | The refusal releases the claim exactly as the failure arm does, through one shared helper so the two cannot drift. `a_refused_generation_is_neither_kept_nor_left_claimed` drives a real production and asserts the claim can be taken again |
| The qualified namespace did not name the receipt schema version, but `permits_reuse` refuses an unknown one. A version bump followed by a rollback, or one older node in a cluster, would compute the same artifact keys, find rows it could not read, refuse every request for them, and have no way to replace them | The namespace is `…-r1` with a compile-time assertion tying it to `PRODUCER_HEALTH_RECEIPT_VERSION`. Version skew is a key-space change, not an unreadable row |
| The cached-reuse refusal was an `Err`, which is retryable in both callers — a stored condition retried on a backoff forever and reported to a user as an encoder fault | It is the same terminal outcome as the retention refusal |
| No test reached either call site: deleting the whole retention check failed no Rust test | `a_refused_generation_is_neither_kept_nor_left_claimed` runs a real ffmpeg production under each identity — refused and unclaimed under one, kept under the other, so the assertion is about the receipt contract rather than a fixture that failed to encode |
| A python contract assertion compared two string literals and could never fail; `decode_unhealthy` fell through to the `other` bucket in the offline failure metric; and three sentences in this section were not true of the code | The assertion reads the suppression clause it claims to guard; `decode_unhealthy` has its own label; the three sentences are corrected above |

Focused evidence on this head, pinned `rustc 1.97.1`: daemon
`transcode::tests::` 245/245, with the milestone's own contracts: the published
identity reaching the plan and renaming the artifact, the unqualified identity
asking a generation for nothing, the qualified identity keeping only what a
written receipt permits (no manifest, no receipt, `Rejected`, `Unqualified`,
`Qualified`), a receipt from an unknown version not counting as permission,
adoption keeping this pass's parts' receipt and refusing another assembly's,
and a real ffmpeg production refused-and-unclaimed under one identity and kept
under the other. `make lint`, `make validation-lint` and `make history-check`
clean; the rolling-producer ownership ledger is unchanged.

## M4 working tree — a hardware encoder is not evidence of an idle CPU

Admission decided what a session would cost from the encoder's name. A
software decode feeding a hardware encoder therefore reserved a hardware slot
and nothing else, while spending most of a box's cores on the decode — so
several of them could land on one node with every counter reading healthy, each
running under realtime. Nothing had reserved the CPU they were using, so
nothing could refuse the next one.

`TranscodeResourceEstimate::of(plan, work)` reads the cost off the resolved
plan: a hardware slot when the encode needs one, and `Workload::software_threads`
as the whole pipeline's CPU reservation whenever any stage of that pipeline runs
on the CPU — the decode, the encode, or the filter chain. It is deliberately the
same number an all-software pipeline already reserved — adding a second full
encode estimate on top of it would halve the node's apparent capacity for work
that has not changed.

The filter chain belongs in that list and was missing from the first draft of
this milestone. `Pipeline::keeps_frames_off_the_cpu()` is true for exactly two
graphs, `VppQsv` and `TonemapVaapi`; every other renderer downloads frames and
spends real cores on them. Two hardware-decode, hardware-encode sessions running
the CPU float tone-map are the same failure this milestone exists to stop — the
measurement that made that chain's own header a warning was 0.71x realtime — and
a subtitle burn is libass on the CPU whatever the rest of the graph does, and on
the two vendor graphs forces a download of every frame besides. Both now count.

`Admissions::try_admit_bundle` takes both halves or neither. That is not
defensive coding, it is the whole safety argument: taking the slot and then
waiting for CPU holds the node's scarcest resource while blocked on its most
contended one, and two starts doing that in opposite orders is a deadlock.
There is no lock order to document because there is no second lock — hardware
and software ownership already live under one `PermitState` mutex, so the
bundle is one decision. The empty-pool exception is preserved exactly: a box
whose every session is over budget would otherwise have its budget turned into
a ban.

The recovery transition is now two transitions, because it always was two:

| Retry shape | What it owes |
|---|---|
| GPU pipeline, encoder retained, chain moves to the CPU | The difference between what its plan costs and what the session already holds. The hardware slot stays: releasing it would leave a live hardware encoder running against nothing reserved, and the next hardware start would be admitted onto the same video block — one slot authorizing two encoders |
| CPU pipeline, encoder replaced by software | The existing demotion. The slot goes back at the transition, and the forced take stands: the viewer is already watching and this is the documented mid-session fallback |

The two are mutually exclusive by construction and a test asserts it, because
setting both would reserve the pipeline twice. `take_forced` is untouched and
is not extended to the new path; a delta that does not fit is a bounded
capacity answer the caller reports.

"Difference" is load-bearing, and the first draft of this milestone got it
wrong: it reserved the retry's whole estimate on top of what admission had
already taken, so a session that was to end up owning one pipeline's worth of
CPU paid for two — and on a box where two did not fit, the retry failed over
capacity the session itself was holding, by which point the predecessor was
already terminated and its scratch already cleared. The frozen recipe therefore
carries `cpu_total`, the whole cost read off the retry's own resolved plan, and
the executor subtracts `Session::software_threads_held()` at the moment of the
transition; a difference of zero reserves nothing at all. The new permit lands
in its own slot (`sw_delta_permit`) rather than overwriting `sw_permit`, because
overwriting drops the original — the session would have paid for both and owned
only the second. A recipe cannot carry the difference itself: it is frozen
before the transition, and what the session holds is only knowable at it.

The wait for that difference is bounded, not instantaneous. The usual reason
the pool refuses is a background producer holding permits, and a background
producer yields — but only once a live waiter is registered, and only at its
next checkpoint. So the executor registers the wait (`SwPool::wait_for_capacity`,
the same `live_waiting` guard the hardware queue uses, on the same shared state)
and polls to `MIXED_RECOVERY_CAPACITY_WAIT`, five seconds. A single
non-blocking try turned "wait two seconds" into "destroy the session"; an
unbounded wait would leave a viewer watching a stall with no deadline, which is
what §5's existing startup budget exists to prevent.

A mixed pipeline also gets its own speed bucket (`<class>+swdecode`) and its
own startup kind (`MixedSoftwareDecode`, on the 30-second software budget
rather than the 12-second hardware one, because the slow part of such a start
is the decode). Filing its measurements under either existing bucket would
poison that bucket's record for the work actually in it — and the measurement
is what admits the next session.

Both apply to a mixed *start*, not only to a mixed recovery. A hardware encoder
fed by a software decode is that shape from its first frame, so
`InitialProducerPolicy::hardware_with_startup` is told which of the two it is
and the plan's own decode backend decides: calling such a start `Hardware` gives
it twelve seconds to do thirty seconds of work and then kills it for being slow
at something it was never going to finish. The class is likewise written once,
by the one writer that knows which of the three shapes the attempt is —
software, mixed, or all-hardware — instead of being written twice from the
encoder's name and then corrected.

Not in M4, and named here so it is not mistaken for done: decoder thread caps.
`TranscodeResourceEstimate::decoder_threads` exists and is always `None`,
because the startup inventory names no decoder implementation and a cap for a
decoder nobody measured is a guess. It gets a value in M3, alongside the
qualified inventory.

### What the whole-PR review found, and what it changed

One adversarial review ran against exact head `d767a0c9`, the whole candidate.
It found three blockers, all of them in this milestone's own work rather than
inherited, and all three are repaired in one batch on top of that head.

| Finding | Disposition |
|---|---|
| `cpu_delta` was not a delta. The frozen recipe carried the retry's whole estimate and the executor reserved all of it on top of what admission had already taken, so a mixed recovery paid for two pipelines to own one. On a box where two did not fit, the retry failed — over capacity the session itself was holding — and it failed after the predecessor was terminated and its scratch cleared, so the answer to a transient shortage was a destroyed session | The recipe carries `cpu_total`, read off the retry's own resolved plan; the executor subtracts `Session::software_threads_held()` at the transition and reserves nothing when the difference is zero. The new permit lands in `sw_delta_permit` rather than overwriting `sw_permit`, which would have dropped the original |
| The estimate read only the decode and the encode, so a hardware-decode, hardware-encode session running the CPU float tone-map — measured at 0.71x realtime, and the reason that chain's header carries a warning — reserved no CPU at all. A subtitle burn is libass on the CPU on every graph and was likewise free | `Pipeline::keeps_frames_off_the_cpu()` is true for `VppQsv` and `TonemapVaapi` only; any other renderer, or any subtitle burn, now costs the pipeline's software threads |
| A single non-blocking `try_take_delta` with no live waiter registered. The pool's usual refusal is a background producer holding permits, and a background producer yields only once a live waiter is registered and only at its next checkpoint — so the one case the wait exists for was the one case it could not survive | The executor registers `SwPool::wait_for_capacity()` — the same `live_waiting` guard the hardware queue uses, on the shared `PermitState` — and polls to `MIXED_RECOVERY_CAPACITY_WAIT`, five seconds, before answering with a capacity refusal |

The review also found the milestone's own tests weaker than their names: the
bundle suite ran entirely on pools with no background owner, so deleting the
priority block left it green, and no test pinned the exactly-fits boundary.
`a_bundle_obeys_the_same_priority_rules_as_the_two_pools_it_replaces`,
`a_bundle_that_exactly_fills_the_budget_is_admitted`, and
`the_estimate_reads_the_cost_off_the_plan_and_not_off_the_encoders_name` were
added, and `a_retry_that_keeps_its_encoder_keeps_its_slot_and_pays_for_its_
decode` was rewritten: it had asserted the pre-repair accounting, and passed.

**Qualification on the repaired head:** `admission::` 24 passed,
`transcode::tests::` 220 passed, `playback_control::tests::` 219 passed,
`live_tv::tests::` 40 passed, `plurx-core --test decoder_selection` 43 passed,
`make effort-rust-check` green, `make lint` green — all under pinned 1.97.1.
The Forgejo `Effort development gate` on the pushed head is the remaining
receipt.

## M1 exact qualification record

| Field | Evidence |
|---|---|
| Exact receipt | `81d46577`: exact mapped history audit 1,387, catalog 24 points / 30 checks / 1,444 audited files, validation 154/154, operations 211/211, formatting, and the pinned all-target workspace compile pass. Runtime repair receipt `bc3c3bee` remains the exact 1,378-history mutation-proof receipt |
| Rejected full run | M1 head `22ab9c8d` completed 22 checks, failed only `cluster-auth`, and declared 2 skips; its 1,800-second aggregate ceiling expired after replicated Store and live topology passed and while activation was 3/7 |
| Timeout containment | `81d46577` retains the exact 3,600-second outer ceiling. The runner proves recorded live identities disappeared and reaps only its owned shell. Validation checks may not daemonize, double-fork, or deliberately orphan child sessions; arbitrary detached-session containment is not claimed |

## M1 explicit plan and bound-fact extraction

M1 adds pure `DecodeFacts`, `DecodeCapabilities`, `DecodePolicySnapshot`,
`AttemptRestrictions`, `ResolvedDecode`, and `ResolvedTranscode` contracts. A
single selector validates complete decoder, renderer, encoder, surface, source,
and presentation semantics before returning a plan. It retains the current
legacy preference order without making advertised decoder names qualified
evidence. M1 models a not-yet-consumed MPEG-4/VideoToolbox compatibility
exclusion independent of container and profile. The M2 working tree makes
command arguments and cache identity consume that result; qualification is
still pending.

The daemon collector discovers and fingerprints a configured parser artifact.
The enforced production boundary requires a self-contained Linux ELF with no
interpreter or dynamic dependency closure and copies it to a sealed anonymous
descriptor. The child executes that exact descriptor with `execveat`; a
deny-all Landlock execution domain blocks path-backed replacements, while a
seccomp user-notification owner continues exactly the trusted pre-exec call
and denies every later `execveat` from the installed image or its descendants.
The BPF layer also prevents the parser from changing FD 4 or leaving its
killable session/process group. Unsupported kernels,
architectures, production Unix targets, scripts, and structurally dynamic ELF
images fail closed. The boundary binds the immutable primary image and later
exec behavior; it does not prove arbitrary static parser code trustworthy. M1
treats the unavailable observation as neutral and
continues the unchanged legacy route. Test-only scripted fixtures are selected
through an explicit fixture constructor and do not replace the production-mode
tests. Every probe owns a process session so timeout or cancellation signals
the complete process group. On Linux a pidfd observes leader exit without
reaping; descendants are killed while the zombie still anchors the numeric
PID/PGID, then the leader is reaped. The caller-facing deadline may detach the
owned cleanup task, but that task retains the child, source descriptor, offset
restoration, and admission permits until reap completes. Caller and task share
one take-once process-group terminator. Lease admission is charged to the same
end-to-end deadline.
Legacy video ordinals are resolved to
absolute input indices, and file-level catalog metadata is merged only when the
selection is the catalog's first non-attached playable stream. The fact digest
binds source and selected-stream facts; the bounded cache key additionally
binds the FFprobe build. Source, held build, private snapshot, and configured
build path are revalidated immediately before cache publication.

The global hash worker and probe singleflight retain their owned admission when
a waiter times out or cancels, preventing detached work from fanning out.
Deadlines, oversize output, changed source identity, and replaced probe binaries
fail closed. Because M1 only observes facts, its two-second subdeadline logs and
continues the unchanged legacy production route, and the time spent observing
is added back to the producer deadline so it cannot consume the legacy FFmpeg
startup budget. Already assembled output is checked before probing. Command
construction is now present in the unqualified M2 working tree described
above.

Historical first-review focused evidence on M1 code head `cc464663`:

- `cargo test -p plurx-core --test decoder_selection`: 34 passed.
- macOS `cargo test -p plurxd decode_facts::tests:: -- --nocapture`: 14 passed.
- pinned Linux 1.97.1 container `cargo test --locked -p plurxd decode_facts::tests:: -- --nocapture`: 15 passed, including descriptor-exec and crossed-fd assignment.
- `cargo test -p plurxd transcode::tests::neutral_decoder_fact_deadline_retains_the_legacy_route -- --nocapture`: 1 passed.
- `cargo clippy -p plurxd --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check` and the working-tree whitespace diff pass.

Post-repair validation receipt `006d832d` retains production and test behavior
from `cc464663` and adds:

- `make validation-lint`: 24 points, 30 checks, and 1,434 audited files pass.
- `make history-check`: 1,356 corrective commits pass.
- `make operations-check`: 211 passed.
- `cargo check --workspace --locked --all-targets` on Rust 1.97.1: passed.

The corrected head closes both independent reviews: selected-stream catalog
binding, first-playable catalog provenance, typed Dolby compatibility,
PQ/Dolby refinement, Dolby-compatible legacy routing, RPU-aware profile-5
refusal, negative-capability precedence, one-pixel geometry, neutral probe
timeout behavior and budget retention, descriptor-bound or immutable probe
execution, collision-safe child-fd assignment, process-group signalling,
end-to-end probe deadlines, final source fencing,
cancellation-safe hash admission, and source-offset lifecycle ownership now
have regressions. The no-default-feature `-D warnings` Clippy profile still
reports the same 116 pre-existing dead-code diagnostics on the exact M0 base;
M1's no-default `cargo check` passes, while default-feature Clippy is clean.

Fresh independent reviews cover the pure planner and the bound probe/cache
owner separately. Their earlier requests exposed incomplete Profile 5
compatibility validation, neutral observation budget erosion, Linux snapshot
writer ownership, and crossed fixed descriptors. Exact code head `cc464663`
closes those gaps plus the diagnostic full-suite inventory and lint findings;
both reviewers approve it with no actionable findings. One concurrent reviewer
run briefly exceeded the five-second version deadline while two Cargo workloads
contended in the same checkout; the standalone test and three subsequent full
group runs passed, so final qualification is intentionally isolated.

The first final review approved receipt `d3747931`. The independent subprocess
review requested three additional repairs: refuse indirect wrappers whose
downstream parser can change without changing wrapper bytes; return a bounded
caller result while a stuck reap retains ownership in a detached task; and kill
the whole process group on every post-spawn error. Those repairs are present on
commit `0dcbcdf2`. Sixteen macOS collector tests pass in three consecutive runs,
and 17 tests pass in the pinned Linux 1.97.1 container, including direct-native
identity, descriptor execution, detached ownership, and cancellation/reap
assertions. Test-only scripted identity discovery is serialized so parallel
fixtures cannot consume one another's production-scale version deadline. The
complete Rust unit profile also passes after the repairs: core 955/955, store
87/87, daemon 1,776/1,776, 3 declared ignores, and no failures.

Exact-head re-review of `0dcbcdf2` found that the native-magic check still
allowed a compiled launcher or replaceable dynamic parser closure, Linux
snapshot bytes remained owner-writable, explicit and drop cleanup could signal
a reused numeric PGID, two ownership counters were stale, and the corrective
commit lacked a history mapping. Commit `608dd04d` requires a self-contained
Linux ELF, seals its anonymous snapshot, rejects other production Unix targets,
shares take-once termination ownership, updates the inventory, and maps
`0dcbcdf2` to its Rust and catalog contracts. These changes make unsupported
prerequisites a typed refusal rather than an unbound best effort.

The exact second-review repair `608dd04d` passes 17/17 macOS collector tests,
20/20 pinned Linux 1.97.1 collector tests, and all-target denied-warning Clippy
on both platforms. The complete Rust unit profile passes with core 955/955,
store 87/87, daemon 1,777/1,777, 3 declared ignores, and no failures. Twelve
combined ownership/status contracts, catalog lint, formatting, and the
working-tree whitespace check also passed. The mapping lived only in successor
`b0b206df`, whose exact post-map history audit passes with 1,359 corrective
commits; `608dd04d` itself does not pass that exact audit.

Two independent reviews of exact `b0b206df` found three remaining P1 defects.
The production Landlock rule attempted to allow an anonymous memfd, which the
pinned Linux kernel rejects with `EBADFD`; even if accepted, Landlock cannot
mediate a second anonymous memfd. ELF classification preceded the sealed copy,
leaving a mutable-file TOCTOU, and normal completion reaped the leader before
its sole process-group kill. The current repair classifies the sealed bytes,
uses FD-based first execution under a deny-all filesystem-exec domain, restricts
later execution and FD mutation with architecture-checked seccomp, refuses an
executable source FD, and uses pidfd readiness to kill the group before reap.
The mandatory pinned-Linux production API tests exercise static discovery,
sealed FD 4, source FD 3, version output, JSON facts, and cleanup; companion
tests prove path-backed, direct-memfd, absolute `/proc/self/fd` memfd, FD-reuse,
and process-session escape attempts are denied without treating unsupported
isolation as a pass. Injected pidfd-open and readiness failures prove the
caller deadline can detach cleanup without releasing version or source
ownership before explicit reap.

Three independent reviews of exact `7b11a8e6` reopened M1. They found that the
initial seccomp policy still admitted descriptor export, the control-socket
receiver could be interrupted or outlive its owned descriptor, setup work and
source identity were outside the absolute deadline, Linux architecture support
was overclaimed, fixture-only supervisor tests did not compose the production
path, odd geometry was rounded in the wrong order, and the neutral timeout
regression stopped short of launching a real legacy producer. They also found
that the status page blurred code-head evidence with its successor receipt and
described the trusted parser boundary too broadly.

Commits `b5ad30ae` through `19190ee8` close those findings. The child now
confirms that `fork` has captured the intended descriptor table before the
supervisor may inject failure or release ownership. Listener transfer is
followed by a stacked filter that denies `sendmsg` and `sendmmsg`; production
native tests attempt to export source FD 3 and prove that no rights arrive.
The entire version launch, blocking spawn, source metadata/hash, and final
revalidation retain their admission and descriptor owners behind one absolute
deadline. Native modes cover successful production execution, descendant
session/group escape, pre- and post-transfer supervisor failure, and pidfd read
failure. All control sends are nonblocking, ancillary truncation is rejected,
and the raw receiver number is never used after its owning `OwnedFd` can be
dropped. Presentation width is derived from the capped raw height before both
axes are rounded. A real `produce_into` regression proves a neutral two-second
observation timeout restores the full legacy producer budget and publishes
segments.

Review of exact receipt `0eac4425` found three more boundary defects: child-side
error construction could allocate after `fork`, repeated `EINTR` could evade
the launch deadline, and a stalled final source observation retained the shared
offset lane needed by the neutral legacy fallback. The current repair uses only
raw OS errors in the audited pre-exec call graph, rechecks one absolute deadline
on every bootstrap retry, and restores the offset plus releases its permit as
soon as the probe tree is reaped. A stack-local injection covers ready send,
acknowledgement, first notification receive, and first response retries; a real
producer regression stalls only final metadata observation and proves legacy
FFmpeg can still publish segments. The same review corrected task-base,
MPEG-4-scope, and trusted-parser wording without expanding M1 behavior.

Adversarial review of exact PR head `b38523be` then found that
`SECCOMP_IOCTL_NOTIF_RECV` still retried directly after `EINTR`. A target can
invalidate its pending notification while interrupting that ioctl, so the
next receive can block even though the preceding poll was ready. Commits
`bfd623e5` through `4c1abfaf` make every receive ioctl a single attempt and
return `EINTR`, `ENOENT`, or `EAGAIN` to a fresh deadline- or stop-aware poll.
The Linux regression now drives persistent interruption through expiry at
ready send, acknowledgement, first notification receive, and first response;
it separately models invalidation after the first receive interruption and
waits for supervisor ownership to return after teardown. A static source audit
enumerates the complete transitive pre-exec helper set and rejects common
allocation and panic forms. This is source-level enforcement of the enumerated
forms, not a general proof that arbitrary future Rust code cannot allocate.
Review of the published `c7a339fb` receipt found the analogous steady-state
response loop could still retry forever on repeated `EINTR`. Commits
`b8646183` through `22d89d27` bind that denial response to the same absolute
launch deadline and supervisor stop signal. A production-composed second-exec
regression forces persistent response interruption, observes caller timeout
while ownership remains held through a deliberately slow reap, and then proves
the supervisor join and version permit return. Its ownership counter is
isolated from parallel bootstrap regressions. A pre-existing 400 ms
kill-domain assertion passed alone but failed once under the expanded parallel
Linux suite; its outer observation window is now 1.5 seconds, while the escaped
fixture still writes invalidating output after 500 ms, so the safety mutation
continues to fail deterministically.
Three reviews of `b8f6312b` then found that slow reap could satisfy the timeout
and ownership assertions even if the response injection were removed. Commit
`ac520a0b` through `07c8f905` add mode-isolated interruption counters and
require repeated hits plus a response-loop deadline receipt before accepting
the deadline result. A separate production-composed launch uses a two-second
deadline, raises the supervisor stop signal after an observed response
interruption, requires a response-loop stop receipt, and must complete within
500 ms. Exact Linux mutation runs remove injection, deadline enforcement, and
the stop check individually; all three regressions fail at the intended
assertion.

The security contract is deliberately narrower than a general parser sandbox.
Production binds the immutable primary self-contained ELF, prevents a
path-backed or later descriptor execution, prevents descriptor export through
the inherited control channel, and retains a killable process tree. It does not
make arbitrary malicious parser code safe and does not prevent an already
trusted parser from interpreting or mapping bytes it can read as code. Safe
enablement therefore requires an operator-selected, qualified FFprobe artifact
in addition to the named kernel primitives. Unsupported Linux audit
architectures have an explicit fail-closed filter builder. A RISC-V workspace
compile was attempted, but native dependency compilation stopped before Plurx
because `riscv64-linux-gnu-gcc` was unavailable; no full unsupported-architecture
compile pass is claimed.

## M0 frozen source inventory

The machine-checked inventory is
[`decoder-selection-m0-inventory.toml`](../tests/playback/decoder-selection-m0-inventory.toml).
It stores a stable identifier, source file, exact anchor, classification, and
migration obligation for every row summarized below. Its four HLS-builder rows
also carry an exact M2 plan anchor and `migrated` state. The focused inventory
test fails when either generation of anchor moves, a fourth movie `hls_args`
call appears, or a direct shipping Live TV FFmpeg command is added or moved
without an inventory update.

The shipping HLS surface has three movie `hls_args` callers and one direct Live
TV builder. Core calls below `mod tests` are argument regressions, not shipping
construction paths.

| Caller | Owner | M2 binding | Remaining obligation |
|---|---|---|---|
| `PrepublicationTranscodeRetry::build` | `crates/plurxd/src/transcode.rs` | Accepts the resolved alternate plan and binds its digest into the frozen retry fingerprint | M5 must bind this alternative to the shared recovery budget |
| `ProducerRunner::produce_into` | `crates/plurxd/src/transcode.rs` | Carries one `ResolvedTranscode` through the recipe and every resumable part command | M3 must attach joined health evidence to part assembly and publication |
| `Manager::start_with_audio_offset` | `crates/plurxd/src/transcode.rs` | Resolves before lookup and resolves again after an admission-driven encoder change | M3 must gate reusable publication on owned health completion |
| `live_ffmpeg_command` | `crates/plurxd/src/live_tv.rs` | Consumes a frozen `LiveTvTranscodePlan` with explicit software input decode and absolute video mapping | M3 must replace the current bounded substring latch with selected-stream health evidence |

The media subprocess ownership surface is broader than the three builders.
The distinction between owner, consumer, and support process is deliberate:

| Inventory ID | Current state | Required owner/evidence |
|---|---|---|
| `process.observed_ffmpeg` | `transcode::spawn_ffmpeg` owns live, retry, and offline HLS children; progress and stderr readers are detached | Return one owned observed-child handle and join diagnostics before qualification |
| `process.fragmented_ffmpeg` | `transcode::spawn_ffmpeg_pipe` owns fragmented-copy children | Preserve its pipe split while giving the attempt a complete observer result |
| `process.live_tv_producer` | `live_tv::spawn_live_ffmpeg` owns the direct tuner HLS child | Use the same plan identity, observation result, and recovery budget as every other decoded producer |
| `process.live_tv_stderr` | `live_tv::capture_live_stderr` currently owns a bounded substring latch | Replace it with selected-stream, build-bound diagnostics and join it before health classification |
| `process.live_tv_graph_probe` | `live_tv::run_graph_probe` builds a synthetic startup/capability graph | Bind its result to the exact build, device, encoder, renderer, and surface; never qualify arbitrary media |
| `process.vod_generation` | `vodserve::spawn_generation` owns the FFmpeg copy producer and hands stdout onward | Classify copied output separately and stop discarding stderr |
| `process.vod_head_regeneration` | `vodserve::regenerate_init_head` owns a bounded FFmpeg head probe | Keep it probe/head-only; it cannot attest a complete producer |
| `process.vod_pipe_consumer` | `vodgen::run` consumes the pipe; it does not spawn production FFmpeg | Do not create a second child or health owner here |
| `process.pgs_demux` | `pgs_overlay::prepare_stage` copies one subtitle stream to SUP | Keep this support process out of video health evidence |
| `process.fragment_index`, `process.progressive_remux` | `fragindex::build_with_args` and `http::stream::remux` own copy/remux FFmpeg children | Never claim decoded-video qualification; classify audio-only transcode separately |
| `process.subtitle_extract`, `process.subtitle_window` | Whole-track and bounded-window subtitle FFmpeg extraction | Keep support output outside video qualification |
| `process.settings_ffmpeg_version`, `process.pipe_probe` | Settings version display and `pipeprobe::Spawn` capability runs | A successful version/probe query is not producer completion |
| `process.ffmpeg_*`, `process.dovi_*`, `process.hdr10_*`, `process.pacing_probe` | Exact build, graph, pixel, and pacing probes in `ffmpeg.rs` | Retain node/per-file capability evidence without promoting arbitrary media |
| `process.media_origin_probe`, `process.chapter_probe` | Bounded FFprobe timeline/metadata helpers | Keep facts distinct from decoded-video health |
| `process.dv_disk_capability`, `process.dv_disk_media_probe` | Version and descriptor-bound FFprobe children | Keep capability evidence distinct and retain the media source fence |
| `process.dv_disk_unbound_conversion`, `process.dv_disk_bound_conversion` | Unbound and descriptor-bound offline Dolby conversion children | Retain independent timeout, source, and verification receipts |

Renderer correctness is distributed across the candidate order and eleven
existing methods. Candidate validation must consume all twelve contracts
rather than duplicating their decisions:

| Inventory ID | Frozen constraint |
|---|---|
| `renderer.candidates` | `CANDIDATES` defines the deterministic graph order |
| `renderer.live_tv_filter` | `live_video_filter` independently owns deinterlace, scale, pixel-format, and upload composition for tuner HLS |
| `renderer.pairing` | `Pipeline::pairs_with` restricts vendor graphs and measured HDR10 encoders |
| `renderer.residency` | `Pipeline::on_gpu` distinguishes GPU-resident and system-memory frames |
| `renderer.dynamic_range` | `Pipeline::handles` restricts HDR, HLG, and Dolby Vision inputs |
| `renderer.decode_arguments` | `Pipeline::decode_args` claims QSV or VA-API input surfaces for vendor graphs |
| `renderer.device_initialization` | `Pipeline::init_args` supplies Vulkan/OpenCL device setup |
| `renderer.filters` | `Pipeline::filters` owns exact graph, pixel format, and transfer composition |
| `renderer.software_requirement` | `Pipeline::requires_software_decode` preserves Dolby Vision frame side data |
| `renderer.session_selection` | `Pipeline::for_session` applies source, encoder, and workload constraints |
| `renderer.output_grade` | `Pipeline::output_grade` binds SDR/HDR10 filter and encoder arguments |
| `renderer.fallback` | `Pipeline::fallback` refuses grade- or Dolby-incompatible fallback |
| `renderer.declined` | `Pipeline::declined` records bounded graph-refusal reasons |

Cache identity and delivery cross distinct write, lookup, and serve paths:

| Path class | Frozen owners | M7 obligation |
|---|---|---|
| Node-local write | `cache.local_publish`, `cache.manifest_publish`, `cache.manifest_capability_publish` | Publish decoder plan and completed health receipt with the generation |
| Shared root and write | `cache.shared_mount_io`, `cache.shared_root_identity`, `cache.shared_root_admission`, `cache.shared_publish` | Enter the verified root, copy, and reverify the same receipt; never mint qualification during replication |
| Local/shared lookup | `cache.location_read`, `cache.local_location_read`, `cache.speculative_hit`, `cache.offer_verification`, `cache.shared_read_prepare`, `cache.decoded_manifest` | Require compatible plan and complete receipt before calling bytes reusable |
| Live cache serving | `cache.session_serve`, `cache.playlist_read`, `cache.object_read` | Install and serve the exact authenticated generation under its read fence |
| Offline ownership | `offline.prepare`, `offline.publish_ready` | Keep one job-scoped recovery budget and refuse `ready` without producer evidence |
| Offline lookup | `offline.generation_cache`, `offline.location_validation` | Cache decoded manifests without bypassing local/shared receipt checks |
| Offline serving | `offline.playlist`, `offline.segment` | Serve authenticated snapshots/handles from the qualified generation |

Planning currently also crosses `transcode::decode_setup`, `PipelineDigest`,
`Recipe`, `ValidatedRetryRecipe`, `PreparedSuccessorAction`, and
`replace_failed_producer`. None currently provides a complete decoder plan,
producer-health receipt, or durable one-shot decoder-recovery budget.

## M0 argument and media controls

The test-only snapshot
[`decoder-selection-m0-args.json`](../tests/playback/decoder-selection-m0-args.json)
freezes normalized token arrays from the current `hls_args` builder. It
normalizes source/output paths and the configured VA-API device path; option
order and every other token remain exact. The test injects the legacy
`PLURX_HWDECODE` compatibility choice, so it is hermetic while freezing both
default and forced-software behavior.

Live TV bypasses `hls_args`, so its complete software-producer argument vector
is frozen independently by
`live_tv_software_hls_argument_baseline_is_stable`. The adjacent probe-budget
and encoder-filter matrix tests freeze its selection-sensitive initialization,
deinterlace, scale, pixel format, and upload behavior. The M0 vector contains no
independent `-hwaccel` choice; M1–M2 must introduce any decoder choice through
the shared bound plan rather than a second Live TV policy.

| Cases | Decoder/renderer/encoder baseline frozen |
|---|---|
| `software-sdr-h264` | Software input, CPU renderer, x264 |
| `qsv-light-h264`, `vaapi-light-h264` | Software input, CPU renderer/upload, hardware encoder; no light-source hardware decode |
| `qsv-heavy-hevc-hdr`, `vaapi-heavy-hevc-hdr` | Heavy hardware input, CPU HDR renderer/download-upload, matching encoder |
| `qsv-vendor-renderer`, `vaapi-vendor-renderer` | Vendor input surface and renderer, matching encoder |
| `nvenc-libplacebo-renderer`, `vaapi-opencl-renderer` | Vulkan/OpenCL device initialization and neutral GPU graphs |
| `nvenc-light-h264`, `videotoolbox-sdr-h264` | Normal CUDA and VideoToolbox input with CPU renderer |
| `videotoolbox-avi-mpeg4` | Incident-sensitive legacy VideoToolbox input behavior |
| `qsv-heavy-hevc-hdr-forced-software` | Legacy operator software-decode override while retaining QSV encode |
| `dovi-tonemapx-videotoolbox` | Required software decode, Dolby reshape, SDR VideoToolbox output |
| `dovi-passthrough-qsv`, `hdr10-passthrough-qsv` | Grade-preserving Main10/PQ output and their distinct decode rules |

The reproducible
[`decoder-media-baseline`](../scripts/decoder-media-baseline) ran three actual
encoded sources through the baseline software HLS command on `nynuc` FFmpeg
8.0.1. Source hashes, probe facts, playlist/segment hashes, decoded-frame
digests, output facts, generator hash, and binary identity are retained in
[`decoder-media-baseline-2026-09-05.toml`](../tests/playback/decoder-media-baseline-2026-09-05.toml).
Generated controls cover unaffected behavior without pretending to replace the
unavailable incident media or hardware qualification. The generator's
`--verify` mode regenerates every source/output on the recorded FFmpeg build
and compares build identity, generator hash, source/playlist/segment/frame
hashes, and probe facts with the TOML.

The exact verifier was copied with only its evidence TOML to a task-scoped
directory on `nynuc`. Its first run rejected the evidence: repeated identical
probe rows exposed ambiguous extraction, Matroska/x265 output was not byte
reproducible, and encoder scheduling changed the MPEG-4-derived HLS segment.
The repaired generator collapses only identical repeated probe rows, rejects
conflicting rows, writes HEVC to MP4, and fixes x264/x265 to deterministic
single-threaded settings. Two independent fresh generations then matched
byte-for-byte. The final exact run returned `"verified": true` for every
retained build, source, output, frame, and probe fact.

| Control | Available evidence | M0 result / prerequisite |
|---|---|---|
| #913 MPEG-4 Part 2 ASP/XVID AVI, MP3 audio, 624×352 at 25 fps | Sanitized FFmpeg 7.1.4-Jellyfin diagnostic capture | Five explicit selected-video primaries in 361 ms; original media is not present, so VideoToolbox/software output comparison remains required |
| Generated H.264 SDR, MPEG-4 Simple Profile AVI, and HEVC Main10 HDR10 | Actual encoded source → HLS runs with retained probe and pixel digests | All three baseline software runs completed; MPEG-4 is not the #913 ASP/XVID file |
| Malformed rawvideo on host FFmpeg 8.0.1 | Exact `repeat+level+error` grammar plus version/binary/build hash | Only the addressed `rawvideo` contract is action-qualified; replayed #913 offsets are a separate timing control |
| Synthetic malformed rawvideo in deployed FFmpeg 5.1.9 container | Exact retained replay fixture | Top-level stream error lacks selected-stream attribution; observation-only, not automatic action |
| Hardware decode, Dolby Vision, subtitles, and multi-stream source controls | No M0 output evidence | Preserve source facts and add backend/pixel/metadata/startup controls in M8 |

## M0 diagnostic contract v1

The qualification harness is
`scripts/decoder-diagnostic-qualification`. Its fixture format is one monotonic
observation-time millisecond value, a tab, then one sanitized FFmpeg stderr
line. This is development evidence, not production parsing.

| Constant | Frozen M0 value | Result |
|---|---:|---|
| Video error window | 2,000 ms | Count selected primary records in `(now - 2,000 ms, now]` |
| Video error limit | 5 | The fifth retained record latches the fault |
| Automatic recovery limit | 1 | One budget across retry/replacement for a logical playback epoch |
| Diagnostic drain budget | 2,000 ms | Later implementation must join stderr completion inside this bound |
| Maximum retained line | 16 KiB | Oversize input makes qualification incomplete while draining continues |
| Maximum retained counter | `u64::MAX` | Every published count saturates instead of growing without bound |

The harness streams bounded binary records instead of loading a capture into
memory. An oversize, malformed, invalid-UTF-8, or non-monotonic record marks
the observation incomplete but is drained so later diagnostics remain
visible. A repeat summary belongs only to the immediately preceding classified
record; any repeat summary, including a severity-prefixed or unrelated one,
proves the stream is compressed and blocks automatic action. Detection latency
begins at the oldest record in the window that actually triggered the fifth
error, not at a stale earlier error.

The retained #913 capture supplies five explicit primary MPEG-4 video decoder
records in 361 ms, plus 49 messages hidden behind two legacy repeat summaries.
The explicit records meet the proposed threshold, but the capture has neither
severity markers nor uncompressed repeat timing and is deliberately not
eligible for automatic action. The FFmpeg 8 rawvideo companion replays the
observed offsets `0, 0, 294, 299, 361` in that build's verified rawvideo
grammar and latches at 361 ms. This composes a real grammar capture with a
deterministic timing control; it does not claim the rawvideo errors happened at
those times. The
tolerant control contains audio, unselected-video, encoder, filename, and
subordinate messages; its five selected primaries never place five timestamps
in the open-lower-bound window.

Tolerant observation matching recognizes the structured stream/decoder shape.
Automatic action additionally requires an explicit versioned contract from
[`diagnostic-contracts.toml`](../tests/playback/decoder-health/diagnostic-contracts.toml)
that binds FFmpeg version, binary/build hashes, codec, decoder, context
addresses, severity, exact error detail, and retained fixture hash. The sole
M0 harness parses those fields into a closed typed schema, cross-checks the
host version and hashes against the fleet evidence, then hashes and classifies
one open fixture descriptor. Its sole action contract is host
FFmpeg 8.0.1 `rawvideo`; it is not deployed-producer or MPEG-4 qualification.
`No frame decoded?` is supporting evidence. The #913 FFmpeg 7.1.4 MPEG-4
capture, deployed FFmpeg 5.1 shape, fatal backend initialization grammars, and
every additional build/backend grammar remain observation-only until retained
fixtures prove their exact attribution.

## FFmpeg and client qualification gaps

Read-only inventory captured 2026-09-05. The running container is the producer
that matters; a host binary is diagnostic context only.

Exact image IDs and hashes of the container binary, build configuration, and
complete decoder-list output are retained per node in
[`fleet-ffmpeg-2026-09-05.toml`](../tests/playback/decoder-health/fleet-ffmpeg-2026-09-05.toml).

| Node | Host FFmpeg | Running `plurxd` FFmpeg | Advertised container acceleration | Qualification |
|---|---|---|---|---|
| `nynuc` | 8.0.1 Ubuntu | 5.1.9 Debian | VDPAU, CUDA, VA-API, QSV, DRM, OpenCL, Vulkan | Advertised only |
| `m6` | Not on host `PATH` | 5.1.9 Debian | VDPAU, CUDA, VA-API, QSV, DRM, OpenCL, Vulkan | Advertised only |
| `nuc4` | 8.0.1 Ubuntu | 5.1.9 Debian | VDPAU, CUDA, VA-API, QSV, DRM, OpenCL, Vulkan | Advertised only |
| `nuc3` | 8.0.1 Ubuntu | 5.1.9 Debian | VDPAU, CUDA, VA-API, QSV, DRM, OpenCL, Vulkan | Advertised only |
| Local Apple build host | Full Homebrew FFmpeg 8.1.2 with `zscale`, run through a task-scoped x265 ABI 216 wrapper | No deployed daemon inspected | VideoToolbox | Qualified only as the local validation toolchain; not fleet evidence |

All four containers advertise software MPEG-4 plus QSV/CUVID families. None of
those names proves a usable device, correct surface transfer, metadata
preservation, or decoded pixels. Each node row retains the exact `sha256:` image
ID, full decoder-list hash, hardware-acceleration-list hash and decoded list;
no fleet backend class is qualified in M0.

Postpublication recovery is likewise unqualified. The ordinary movie clients
advertise only `hold`, `retry_resource`, and `terminal`; their existing parsers
intentionally reject undeclared `prepare_replacement`. Server preparation/commit
machinery does not make a client perform a two-player handoff. Live TV Web is a
separate lifecycle: it never enters `playback-control`, has no action
negotiation, and stops/releases the tuner session on failure. Live TV is limited
to prepublication recovery until that distinct lifecycle implements and
physically qualifies a two-player successor.

| Client | Declared actions | Parses `prepare_replacement` | Two players and readiness | Commit/retirement evidence | Current effective recovery |
|---|---|---|---|---|---|
| Web | `hold`, `retry_resource`, `terminal` | No | No | No | At most `prepublication` after server qualification |
| Apple | `hold`, `retry_resource`, `terminal` | No; unit test expects terminal protocol error | No | No physical run | At most `prepublication` after server qualification |
| Android | `hold`, `retry_resource`, `terminal` | No; unit test expects terminal protocol error | No | No physical run | At most `prepublication` after server qualification |
| Live TV Web | None; outside `playback-control` | No protocol participant | No; one owned media element | No physical run | Prepublication only after server qualification |

The visible Settings → Developer values are operator-requested upper bounds,
not a claim that every node or session can execute them. Each node computes an
effective plan policy from its current local capability/grammar receipt. Each
playback computes an effective recovery mode from that node result, owned
observation/resource/store readiness, and the actual client's advertised and
qualified behavior. The card must show requested value, per-node readiness,
session-effective value, and every bounded refusal. Unsupported sessions end
explicitly; they do not silently run an unqualified automatic replacement.

## Adversarial review ledger

Two independent agents reviewed the original pre-rebase M0 candidate before the full suite.
Their initial verdict was request changes; those repairs were committed before
both agents performed a second pass. Both second passes also requested changes;
their repairs were committed at pre-rebase `f9d68467`. Both third passes requested
the additional repairs committed at pre-rebase `195b7574`; both fourth passes
approved pre-rebase `6009367b` with no actionable findings. The fifth pass found
missing direct probe-normalization test coverage. That repair is committed at
pre-rebase `a16db3da`; both reviewers approved pre-rebase `7649ccdd` with no
remaining actionable findings. Both independent reviewers then approved
pre-rebase receipt head `2c266dce` after matching the retained full-suite JSON
and JUnit evidence to the historical ledger.

Those approvals are historical content evidence only. After rebasing onto
Forgejo `main` at `4a6a0268`, both fresh reviews of `3fbeedb2` requested changes:
the newly inherited direct Live TV producer/probe/filter/diagnostic path was
missing from the inventory; historical receipts had been relabeled with rebased
hashes; the baseline/current checkpoint was stale; and diagnostic counters did
not saturate. Those findings were repaired. Both independent reviewers approved
exact code head `59d0a4d1` with no actionable findings after separately checking
the formatting-only final delta, focused timing regressions, history audit, and
full-base diff. The exact head then passed the full PR suite recorded below.

| Finding | Resolution on working tree |
|---|---|
| Severity token was matched in the wrong position and addresses were omitted | Grammar and fixtures now preserve FFmpeg 8 context/address/severity order; FFmpeg 5 is explicitly unqualified |
| Fixture reader allocated the whole input | Bounded streaming reader drains oversize tails and records coverage loss |
| Latency began at the first historical error | Latency begins at the oldest record in the triggering window |
| Normalized baseline argv and media controls were absent | Sixteen exact argv snapshots plus explicit real-media/prerequisite matrix added |
| Client replacement support was overstated | Web/Apple/Android capability matrix records parse-only and physical-evidence gaps |
| Activation semantics depended on “the node/current client” | Persisted values are requested upper bounds; effective mode is node/session-local and visible |
| Legacy repeats were counted without provenance | Only an immediately preceding classified record owns a repeat summary; ambiguous summaries refuse action |
| Producer/cache inventory was inaccurate and not auditable | Machine-checked owner/consumer/renderer/local/shared/offline inventory added; `vodgen` corrected to consumer |

| Second-pass finding | Resolution on working tree |
|---|---|
| An unrelated repeat summary still allowed automatic action | Every repeat provenance now blocks action; severity-prefixed summaries and threshold controls are covered |
| Tolerant grammar was mistaken for build/codec qualification | Automatic action requires an explicit versioned contract; M0 qualifies only the captured FFmpeg 8 rawvideo family |
| Rawvideo timing replay changed the second #913 offset from 0 to 1 ms | Fixture now preserves `0, 0, 294, 299, 361` and labels grammar versus synthetic timing provenance |
| Argument baseline inherited `PLURX_HWDECODE` | Compatibility input is injected into the internal builder; default and forced-software cases are both frozen and pass under hostile process environment |
| VA-API and materially distinct renderers/output grades were absent | Baseline expanded to 16 cases covering every renderer family, normal VideoToolbox, heavy/light VA-API, and forced software decode |
| Process/renderer/manifest-cache inventory remained incomplete | Pre-rebase inventory expanded to 67 exact IDs with equality, count, and unique-anchor assertions plus the missing subprocess/method/cache owners |
| No actual media baseline existed | Reproducible H.264, MPEG-4 AVI, and HEVC HDR10 source-to-HLS evidence captured on a build-bound FFmpeg 8 host |
| Fleet hashes were global and lacked canonical commands | Binary/build/decoder/image hashes are now stored per node with exact capture and byte-canonicalization commands |

| Third-pass finding | Resolution on working tree |
|---|---|
| Contract metadata was declared but not enforced | A closed typed schema now validates version, hashes, stderr mode, and host against retained fleet evidence |
| Address/codec rejection tests were pre-rejected by fixture identity | Structural mismatches are tested directly with a fully validated contract |
| Fixture hash and parse used different opens | One descriptor now supplies both the streaming digest and classified records before action is decided |
| Light VA-API and legacy override spellings were not frozen | A sixteenth argv case proves light VA-API stays on software decode; a pure table covers `off`, `0`, `false`, and `no` |
| `dv_disk` subprocess inventory was incomplete and ambiguous | Capability, bound probe, unbound conversion, and Unix bound conversion have distinct rows; every retained anchor must occur exactly once |
| Media evidence was not rerunnable from the ledger | The exact `nynuc` verifier rejected three nondeterminism defects; after repair, two fresh generations matched byte-for-byte and the exact verifier returned `"verified": true` |
| Image IDs and hardware acceleration lacked exact per-node capture | Exact prefixed image IDs and per-node `-hwaccels` output/hash evidence are retained for all four nodes |

| Fifth-pass finding | Resolution on working tree |
|---|---|
| Probe normalization had no direct unit coverage and the ledger overstated the suite | A pure helper now has single, duplicate-identical, empty, and conflicting-row tests; the ledger separates those tests from the remote generation evidence |

| Post-rebase finding | Resolution on working tree |
|---|---|
| Current `main` added direct Live TV FFmpeg construction outside the frozen movie builders | Five producer-side Live TV rows plus its distinct Web client boundary bring the inventory to 73; static discovery and a complete software argv baseline prevent silent omission |
| Live TV Web bypasses the ordinary playback-control replacement protocol | A separate client capability row and gap matrix limit it to prepublication recovery until two-player handoff is implemented and physically qualified |
| Historical receipts were relabeled with rebased hashes | Original hashes are retained as pre-rebase evidence; only commands actually run on the current tree may be recorded as current qualification |
| Baseline and milestone state remained stale after rebase | The authoritative Forgejo base is explicit; exact head `59d0a4d1` is now qualified and linked to Forgejo PR #62 |
| Diagnostic counters were unbounded | Every published diagnostic counter saturates at `u64::MAX`; exact maximum and maximum-plus-one repeat summaries are covered |

| Final-review finding | Resolution on working tree |
|---|---|
| The historical mapping named `catalog-contract` without a test that read the mapped status page | The page is now catalog-governed and a validation contract cross-checks its frozen artifacts, safety boundaries, review repairs, and invalid-run labels |

## Decisions and deviations

| Date | Decision | Reason |
|---|---|---|
| 2026-09-05 | Use one effort branch with M0–M8 task PRs | Required large-effort lane; keeps incomplete intermediate states out of `main` |
| 2026-09-05 | Decoder activation belongs in Settings → Developer | Operator request and existing UI convention; no compile feature or hidden environment gate |
| 2026-09-05 | Persist requested upper bounds; compute effective modes per node/session | One settings writer cannot prove every node or client; qualified peers should not be disabled by an unqualified peer |
| 2026-09-05 | Unsafe requested modes are explicit refusals | Safety prerequisites stay visible without silently accepting a nonfunctional setting |
| 2026-09-05 | Treat container FFmpeg 5.1.9 as deployed evidence | It runs the producer; host 8.0.1 does not define container diagnostics |
| 2026-09-05 | Keep the proposed 2 s / 5-record threshold | #913 reaches five explicit records in 361 ms; tolerant controls do not trigger |
| 2026-09-05 | Do not expand legacy repeat summaries into timestamps | Their timing is unknowable, so expansion would manufacture recovery evidence |
| 2026-09-05 | Use generated encoded controls for M0 while preserving the original-media gap | This creates reproducible source-to-HLS evidence without pretending the controls are the #913 ASP/XVID asset |
| 2026-09-05 | Bind automatic diagnostic action to one exact build/codec grammar | Structural matching remains useful for observation, but cannot safely authorize recovery across unqualified FFmpeg builds |
| 2026-09-05 | Use MP4 and explicit single-thread encoder controls for generated media evidence | The exact remote verifier proved Matroska/x265 and unconstrained encoder scheduling were not byte reproducible |
| 2026-09-05 | Collapse only identical repeated probe rows | FFprobe may emit the same selected-stream fact more than once; conflicting rows remain an evidence failure |
| 2026-09-05 | Map review-only documentation commits to a status contract inside `catalog-contract` instead of ignoring them | The governed test reads the page and verifies its frozen artifacts, safety boundaries, review repairs, and invalid-run labels |
| 2026-09-05 | Use full Homebrew FFmpeg 8.1.2 plus its retained x265 ABI 216 library for local qualification | The default FFmpeg 8.1.2 lacks `zscale`; FFmpeg 9.0.1 has `zscale` but changes muxer output identities expected by the repository's FFmpeg 8 contracts |
| 2026-09-06 | Run qualification through task-scoped `ffmpeg-full` 8.1.2_2 wrappers | The installed full build provides `zscale`; the wrappers add only the retained x265 ABI 216 library path and leave the host installation unchanged |
| 2026-09-06 | Accept two declared full-suite skips on this Darwin builder | Android device validation requires unavailable `adb`; the two-node Live TV drill is Linux-only. Both remain explicit M8 fleet/client prerequisites rather than passing claims |
| 2026-09-06 | Reuse warmed cluster targets for the final exact-head suite | The first diagnostic run spent its 1,800-second bound compiling vendor targets. Warm targets change no source or test semantics and let the complete three-node workload run inside the same fixed bound |
| 2026-09-06 | Require a sealed, self-contained Linux FFprobe artifact plus kernel isolation for enforced fact collection | Hashing a script, dynamic launcher, or mutable tempfile does not bind the parser that actually runs. The supported path requires memfd seals, `execveat`, Landlock, seccomp-BPF with user notification, `close_range`, pidfds, and a supervised native thread on x86-64 or arm64 Linux; failure is a neutral M1 observation and keeps legacy routing. Other production Unix targets refuse until they have an equivalent dependency-closure guarantee. The later Developer settings UI must name these prerequisites rather than hide them behind a code feature gate || 2026-09-07 | Qualify task PRs with the `Effort development gate` and defer the full suite to the `Main promotion gate` | The plan's per-milestone workflow asks for one `make validate-full` on every task candidate; `AGENTS.md` and `effort-ci.yml` say a task PR into an effort branch runs the development gate and the effort's single full qualification happens at promotion. The repository's own pipeline is what the branch protection enforces, and one full suite per task is fifty minutes of three-node cluster work per milestone with nothing between them to invalidate. Recorded here rather than silently: the effort still owes exactly one `make validate-full` on the exact promotion head |
| 2026-09-07 | No M0 contract names a fatal decode family | The only fatal in the retained evidence is `Decode error rate 1 exceeds maximum`, which is FFmpeg abandoning a corrupt input rather than a backend becoming unavailable. Labelling it `DecodeBackendUnavailable` would ask M3b to swap decoders for a fault a decoder swap cannot repair, so the family is contract-driven and absent until one is qualified against its own fixture |

## Validation ledger

No passing run is recorded until its command finishes on the named tree. Each
task PR gets adversarial agent review, findings are fixed, then one full suite
runs on the corrected head before merge. During development, the effort fast
lane and focused tests provide earlier feedback.

| Commit/tree | Command | Result |
|---|---|---|
| Pre-rebase `a9cb879b` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 6 tests |
| Pre-rebase `a9cb879b` | `make operations-check` | Pass · 192 tests; rerun outside restricted socket sandbox |
| Pre-rebase `a9cb879b` | `make validation-lint` | Pass · 23 points, 28 checks, 1,376 files |
| Pre-rebase `a9cb879b` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| Pre-rebase `a9cb879b` | `git diff --check` | Pass |
| Pre-rebase `2d4900e1` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 14 tests |
| Pre-rebase `2d4900e1` | `rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · 1 test |
| Pre-rebase `2d4900e1` | `make operations-check` | Pass · 200 tests; run outside restricted socket sandbox |
| Pre-rebase `2d4900e1` | `make validation-lint` | Pass · 23 points, 28 checks, 1,381 files |
| Pre-rebase `2d4900e1` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| Pre-rebase `2d4900e1` | `git diff --check` | Pass |
| Pre-rebase `6c23d17a` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 15 tests, including exact 16 KiB and bounded repeat-count edges |
| Pre-rebase `6c23d17a` | `git diff --check` | Pass |
| Pre-rebase `f9d68467` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 21 tests |
| Pre-rebase `f9d68467` | `rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · 1 test |
| Pre-rebase `f9d68467` | `PLURX_HWDECODE=off PLURX_VAAPI_DEVICE=/unexpected/device rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · 1 test; hostile environment cannot alter fixture output |
| Pre-rebase `f9d68467` | `make operations-check` | Pass · 207 tests; run outside restricted socket sandbox |
| Pre-rebase `f9d68467` | `make validation-lint` | Pass · 23 points, 28 checks, 1,384 files after mapping the media baseline |
| Pre-rebase `f9d68467` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| Pre-rebase `f9d68467` | `sh -n scripts/decoder-media-baseline` | Pass |
| Pre-rebase `f9d68467` | `git diff --check` | Pass |
| Pre-rebase `195b7574` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 22 tests, including contract provenance and media-verifier mutation controls |
| Pre-rebase `195b7574` | `rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · 16 argument cases |
| Pre-rebase `195b7574` | `rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_compatibility_override_values_are_stable -- --exact` | Pass · all four legacy false spellings and non-matches |
| Pre-rebase `195b7574` | `PLURX_HWDECODE=off PLURX_VAAPI_DEVICE=/unexpected/device rustup run 1.97.1 cargo test -p plurx-core transcode::tests::decoder_selection_m0_argument_baseline_is_stable -- --exact` | Pass · hostile environment cannot alter fixture output |
| Pre-rebase `195b7574` | `make operations-check` | Pass · 208 tests; run outside restricted socket sandbox |
| Pre-rebase `195b7574` | `make validation-lint` | Pass · 23 points, 28 checks, 1,384 files |
| Pre-rebase `195b7574` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| Pre-rebase `195b7574` | `python3 -m py_compile scripts/decoder-media-baseline scripts/decoder-diagnostic-qualification` | Pass |
| Pre-rebase `195b7574` | `git diff --check` | Pass |
| Pre-rebase `a16db3da` | `python3 -m unittest tests/operations/test_decoder_diagnostic_qualification.py` | Pass · 23 tests, including direct single/duplicate/empty/conflicting probe-row coverage |
| Pre-rebase `a16db3da` | `python3 -m py_compile scripts/decoder-media-baseline scripts/decoder-diagnostic-qualification` | Pass |
| Pre-rebase `a16db3da` | `/tmp/codex-decoder-m0-review5/decoder-media-baseline --verify /tmp/codex-decoder-m0-review5/decoder-media-baseline-2026-09-05.toml /tmp/plurx-decoder-m0-media-review8` on `nynuc` | Pass · `"verified": true` |
| Pre-rebase `7649ccdd` | Fifth diagnostic and scope adversarial re-reviews | Pass · both approve; no actionable findings |
| Pre-rebase `7649ccdd` | `make operations-check` | Pass · 209 tests; run outside restricted socket sandbox |
| Pre-rebase `7649ccdd` | `make validation-lint` | Pass · 23 points, 28 checks, 1,384 files |
| Pre-rebase `7649ccdd` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| Pre-rebase `4ec13f3b` | `make operations-check` | Pass · 208 tests; run outside restricted socket sandbox |
| Pre-rebase `4ec13f3b` | `make validation-lint` | Pass · 23 points, 28 checks, 1,384 files |
| Pre-rebase `4ec13f3b` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check` | Pass · format and all locked workspace targets compiled |
| Pre-rebase `4ec13f3b` | `python3 -m py_compile scripts/decoder-media-baseline scripts/decoder-diagnostic-qualification` | Pass |
| Pre-rebase `4ec13f3b` | two fresh `scripts/decoder-media-baseline` generations on `nynuc` | Pass · every retained artifact digest matches byte-for-byte |
| Pre-rebase `4ec13f3b` | `/tmp/codex-decoder-m0-review4/decoder-media-baseline --verify /tmp/codex-decoder-m0-review4/decoder-media-baseline-2026-09-05.toml /tmp/plurx-decoder-m0-media-review7` on `nynuc` | Pass · `"verified": true` |
| Pre-rebase `4ec13f3b` | `git diff --check` | Pass |
| Pre-rebase `3fe6a6ca` | `CARGO='rustup run 1.97.1 cargo' make unit` | Invalid environment run · 143 FFmpeg-backed tests failed because Homebrew FFmpeg could not load retained `libx265.216.dylib`; no non-loader failure observed |
| Pre-rebase `3fe6a6ca` | `PLURX_FFMPEG=/private/tmp/codex-ffmpeg-abi216 PLURX_FFPROBE=/private/tmp/codex-ffprobe-abi216 CARGO='rustup run 1.97.1 cargo' make unit` | Pass · 2,785 tests; 3 declared ignores; task-scoped wrappers use the retained installed x265 ABI 216 library without modifying the host |
| Pre-rebase `ac456b44` | `make validate-full` with the initial task-scoped FFmpeg wrapper | Diagnostic pass · 18 passed, 3 failed, 3 skipped; history lacked five review-doc mappings, playback selected an FFmpeg without `zscale`, cold cluster work exceeded 1,800 s, Playwright was not on `PATH`, and `adb` was unavailable |
| Pre-rebase working tree before `01368ce1` | `make history-check` | Pass · 1,322 corrective commits had current evidence; review-only M0 documentation mapped to `catalog-contract` and was not ignored |
| Pre-rebase working tree before `01368ce1` | `make ui-check` through the existing `plurx-ui` Playwright 1.62.0 environment | Pass · 60 captures and 5,464 structural facts matched the golden; no console or page errors |
| Pre-rebase working tree before `01368ce1` | `scripts/reader-browser` through the existing `plurx-ui` environment | Pass · online/native/offline handoff, profile isolation, force-relaunch restore, style, TOC, search, finish, stale revision, and hostile-content checks |
| Pre-rebase working tree before `01368ce1` | `make cluster-check` with the warmed Rust 1.97.1 targets | Pass · 133 three-voter store contracts, topology/growth/failure drills, 7 activation tests, and 2 activity tests |
| Pre-rebase working tree before `01368ce1` | `make unit` with full FFmpeg 9.0.1 | Invalid tool-version run · 2 FFmpeg 8 muxer-identity tests failed; all other tests passed |
| Pre-rebase working tree before `01368ce1` | the two failed FFmpeg-sensitive tests with full FFmpeg 8.1.2 and retained x265 ABI 216 | Pass · copy-segment decode equivalence and mid-film generation identity |
| Pre-rebase working tree before `01368ce1` | `make unit` with full FFmpeg 8.1.2 and retained x265 ABI 216 | Pass · 2,785 tests; 3 declared ignores |
| Pre-rebase working tree before `01368ce1` | `make playback-smoke` with full FFmpeg 8.1.2 and Playwright 1.62.0 | Pass · 11/11 Chrome cases, including HDR tone-map, copy-HLS, no-MSE, seek, audio switch, and subtitle toggle |
| Pre-rebase `01368ce1` | `make validate-full` with full FFmpeg 8.1.2, retained x265 ABI 216, Playwright 1.62.0, and anonymous pinned Android container preflight | Pass · 23 runnable checks; Android-device was the sole declared skip because `adb` was unavailable; cluster-auth passed in 1,638.3 s |
| Pre-rebase `2c266dce` | Independent diagnostic-safety and milestone-scope adversarial reviews | Pass · both reviewers approved with no actionable findings after checking the retained full-suite JSON and JUnit evidence |
| `59d0a4d1` | `python3 -m unittest tests.operations.test_decoder_diagnostic_qualification tests.validation.test_decoder_recovery_status` and focused Rust Live TV/timing regressions | Pass · 29 Python tests plus current-base diagnostic saturation, inventory, exact Live TV arguments/grammar, and wedge-boundary timing controls |
| `59d0a4d1` | `CARGO='rustup run 1.97.1 cargo' make unit` | Pass · complete Rust unit profile; daemon 1,759 passed with 3 declared ignores, core 955 passed, store contracts 87 passed, and no failures |
| `59d0a4d1` | `CARGO='rustup run 1.97.1 cargo' make effort-rust-check`; `make history-check`; `make validation-lint`; `git diff --check effort/decoder-selection-recovery...HEAD` | Pass · pinned format/compile evidence, 1,344 corrective commits mapped, 24 catalog points / 30 checks / 1,425 files, and clean diff |
| `59d0a4d1` | Independent milestone-scope and diagnostic-safety adversarial reviews | Pass · both reviewers approved the exact current-base code head with no actionable findings |
| `59d0a4d1` | First current-base `make validate-full` diagnostic run | Diagnostic pass · 20 passed, 3 failed, 2 skipped; failures were the non-full FFmpeg missing `zscale`/x265 ABI and cold cluster compilation exceeding 1,800 seconds, not product assertions |
| `59d0a4d1` | `make playback-smoke`; `scripts/reader-browser`; warmed `make cluster-check` plus exact `cluster_activity` rerun, all through the corrected FFmpeg wrapper where media was involved | Pass · browser playback 11/11, reader online/offline lifecycle, 135 cluster store contracts with 2 helpers ignored, topology/growth/failure drills, 7 activation tests, and 2 activity tests |
| `59d0a4d1` | `PATH=/private/tmp/plurx-toolbin:/private/tmp/plurx-full-venv/bin:... PLURX_FFMPEG=/private/tmp/plurx-toolbin/ffmpeg PLURX_FFPROBE=/private/tmp/plurx-toolbin/ffprobe CARGO='rustup run 1.97.1 cargo' make validate-full` | Pass · 23 passed, 0 failed, 2 declared skips; generated `2026-09-06T06:46:16Z`, aggregate 2,794.94 check-seconds; Rust gate 130.9 s, UI 72 captures / 6,648 facts, playback 11/11, Apple tvOS 357/357, cluster 1,651.4 s; skips were missing `adb` and Linux-only two-node Live TV |
| `bb25576c` | Same full-suite command and environment as the qualified M0 run | Rejected diagnostic · 20 passed, 3 failed, 2 declared skips; ownership inventory had eight stale M1 counts, Rust gate found two test-only Clippy findings, and cluster compilation exhausted disk after its preceding tests passed |
| `cc464663` | M1 focused tests, workspace all-target Clippy, ownership/status contracts, formatting, and diff check on Rust 1.97.1 | Pass · planner 34/34, macOS collector 14/14, Linux collector 15/15, neutral policy 1/1, ownership 7/7, status 5/5; both adversarial reviewers approve exact code head with no actionable findings |
| `da1b704e` | Effort commit hook | Pass · history 1,354, catalog 24/30/1,431, operations 211/211, formatting, and pinned all-target workspace compile |
| `d6ecbfb7` | Exact repair receipt effort hook | Pass · catalog 24/30/1,432, operations 211/211, formatting, status contract, and pinned all-target workspace compile; the parent-only history audit rejected its then-unmapped documentation-only subject, which the exact successor maps |
| `a129560e` | Exact receipt effort hook and adversarial evidence-map review | Hook pass · history 1,356, catalog 24/30/1,433, operations 211/211, formatting, status contract, and pinned all-target workspace compile; one reviewer approved and one found that `cc464663` needed separate Rust plus catalog ownership evidence |
| `a129560e` | Isolated `make validate-full` attempt | Superseded and intentionally interrupted during the cold Rust build after catalog, history, operations, benchmark, version, and input-fence checks passed; no qualification claimed |
| `006d832d` | Adversarial evidence-map finding plus effort hook | Finding applied · `cc464663` now names both playback/Rust and validation/catalog coverage; history 1,356, catalog 24/30/1,434, operations 211/211, formatting, status contract, and pinned all-target workspace compile pass |
| `2e8c7d48` | Full-suite command and environment used by the qualified M0 run | Rejected diagnostic · 21 passed, 2 failed, 2 declared skips; all product cluster workloads preceding cold `cluster_activation` compilation passed, then the 1,800-second aggregate cluster bound expired; Rust completed 1,772 daemon tests but two transient-path-swap tests exceeded their two-second helper readiness bound under full parallel load, and both passed immediately in isolation |
| Working tree after `2e8c7d48` | `cargo test -p plurxd uses_manifest_bound_source_during_transient_path_swap -- --nocapture` repeated five times | Pass · both descriptor-bound FFmpeg and mkvmerge transient-path-swap tests passed in every run after replacing the scheduler-sensitive iteration count with a 15-second wall-clock deadline |
| Working tree after `2e8c7d48` | `CARGO='rustup run 1.97.1 cargo' make unit` with the qualified FFmpeg wrappers and loopback fixture permission | Pass · core 955/955, store contracts 87/87, daemon 1,774/1,774, and all remaining workspace and documentation tests passed; 3 declared ignores and no failures. A preceding restricted-sandbox diagnostic was invalid because 65 loopback fixtures were denied socket binds |
| Working tree after `2e8c7d48` | Warmed `CARGO='rustup run 1.97.1 cargo' make cluster-check` | Pass · 133/133 runnable three-voter store contracts with 2 helper-process ignores, topology/growth/failure drills, 7/7 activation tests, and 2/2 activity tests; the formerly cold activation target compiled in 10.06 seconds |
| `d3747931` | Effort commit hook plus independent plan/scope and subprocess adversarial reviews | Hook pass · history 1,356, catalog 24/30/10,402, operations 211/211, formatting, status contract, and pinned all-target workspace compile; plan/scope review approved, while subprocess review correctly blocked qualification on indirect-wrapper identity, unbounded caller reap, and incomplete post-spawn group cleanup |
| `0dcbcdf2` before exact-head re-review | `cargo test -p plurxd decode_facts::tests:: -- --nocapture` repeated three times after first review repair | Pass · 16/16 in every run, including direct-native wrapper refusal, detached cleanup ownership, cancellation/reap, executable/source fences, cache singleflight, and selected-stream binding |
| `0dcbcdf2` before exact-head re-review | pinned Linux 1.97.1 container `cargo test --locked -p plurxd decode_facts::tests:: -- --nocapture` | Pass · 17/17, including direct-native identity, descriptor execution, crossed-fd assignment, bounded caller cleanup ownership, and process-group reap |
| `0dcbcdf2` before exact-head re-review | `CARGO='rustup run 1.97.1 cargo' make unit` with qualified FFmpeg 8.1.2 and loopback fixture permission | Pass · core 955/955, store contracts 87/87, daemon 1,776/1,776, all remaining workspace and documentation tests, 3 declared ignores, and no failures |
| `0dcbcdf2` | Independent plan/scope and subprocess adversarial re-reviews | Changes requested · both identified stale exact-head evidence; plan review found a reusable-PGID double-signal, and subprocess review additionally showed that native launchers, dynamic dependencies, mutable Linux snapshot bytes, and unsupported Unix execution remained outside the claimed build boundary |
| Working tree based on `608dd04d` before exact-head receipt | macOS and pinned Linux 1.97.1 `cargo test --locked -p plurxd decode_facts::tests:: -- --nocapture` | Pass · macOS 17/17 and Linux 20/20; Linux added structural static classification, sealed in-place mutation refusal, path-backed launcher denial, descriptor execution/crossing, and take-once group termination. Later review proved this was not a usable production isolation path |
| `608dd04d` before exact-head receipt | macOS plus ephemeral pinned Linux 1.97.1 `cargo clippy --locked -p plurxd --all-targets -- -D warnings` | Pass · no warnings on either platform; the Linux run compiled the ELF, memfd, Landlock, and Linux-only regressions |
| `608dd04d` before exact-head receipt | `CARGO='rustup run 1.97.1 cargo' make unit` with qualified FFmpeg 8.1.2 and loopback fixture permission | Pass · core 955/955, store contracts 87/87, daemon 1,777/1,777, all remaining workspace and documentation tests, 3 declared ignores, and no failures |
| Working tree based on `608dd04d` before exact-head receipt | ownership/status contracts, `make validation-lint`, precommit `make history-check`, formatting, and diff check | Pass · 12/12 contracts, catalog 24/30/1,435, and clean formatting/whitespace. This was source-tree hook evidence with the successor mapping staged, not an exact audit of commit `608dd04d` |
| `608dd04d` precommit source tree | Effort commit hook | Pass · catalog 24/30/1,435, operations 211/211, formatting, status contract, pinned all-target workspace compile, and staged-map history check; no exact postcommit history claim |
| `b0b206df` | Exact post-map receipt and independent plan/probe adversarial reviews | Receipt pass · history 1,359, catalog 24/30/1,436, operations 211/211, formatting, status contract, and pinned all-target workspace compile. Reviews requested changes for unusable anonymous-memfd Landlock allowance, second-memfd escape, classification TOCTOU, reap-before-group-kill, and missing production-path composition evidence |
| `29dd8e88` source tree | macOS and pinned Linux 1.97.1 `cargo test --locked -p plurxd decode_facts::tests:: -- --nocapture` | Pass · macOS 17/17 and Linux 24/24. Linux runs the normal production API through sealed FD 4 and bound source FD 3, rejects path and second-memfd exec, classifies captured bytes, refuses executable media, and kills descendants before leader reap |
| `29dd8e88` source tree | `CARGO='rustup run 1.97.1 cargo' make unit` with qualified FFmpeg 8.1.2 and loopback fixture permission | Pass · core 955/955, decoder integration 34/34, store 87/87, PGS 20/20, daemon 1,777/1,777, 3 declared ignores, and no failures |
| `29dd8e88` source tree | macOS plus pinned Linux 1.97.1 `cargo clippy --locked -p plurxd --all-targets -- -D warnings` | Pass · no warnings on either platform |
| `adce1125` | Exact planner and subprocess adversarial reviews | Changes requested · both reviewers demonstrated the stateless FD-4 `execveat` rule could execute an absolute `/proc/self/fd` memfd; the subprocess review additionally found session/group escape and pidfd-open/readiness ownership gaps |
| Working tree after `adce1125` | pinned Linux 1.97.1 `cargo test --locked -p plurxd decode_facts::tests:: -- --nocapture` | Pass · Linux 1.97.1 collector 27/27, including one-shot exec supervision, absolute proc-memfd and FD-reuse attempts, session-escape denial, and injected pidfd-open/readiness cleanup ownership |
| Working tree after `adce1125` | macOS focused tests, cross-platform all-target Clippy, repository contracts, and policy audits | Pass · macOS collector 17/17, neutral legacy routing 1/1, no Clippy warnings on macOS or pinned Linux, status/ownership 13/13, catalog 24/30/1,437, history 1,361, operations 211/211, formatting, and diff check |
| `31dc5d26` source tree | Effort hook and focused validation | Pass · the commit hook reran catalog 24/30/1,437, history 1,361, operations 211/211, formatting, status contract, and pinned all-target workspace compile after the focused macOS 17/17, Linux 27/27, neutral-route 1/1, and cross-platform denied-warning Clippy passes |
| Working tree after `31dc5d26` | Corrective-history and evidence-map audits | Pass · catalog 24/30/1,438 and history 1,362 with the supervised-probe mapping present; status/ownership contracts remain 13/13 and formatting/diff checks are clean |
| `2baf0861` | Exact post-map receipt | Pass · history 1,362, catalog 24/30/1,438, operations 211/211, formatting, status/ownership contracts 13/13, and pinned all-target workspace compile |
| `7b11a8e6` | Three independent exact-head adversarial reviews | Changes requested · one reviewer approved; probe and milestone-scope reviewers found descriptor export, interrupted/stale receiver ownership, launch and source-identity deadline gaps, fixture-only production claims, unsupported-architecture compilation risk, odd-height geometry drift, a helper-only legacy fallback test, stale receipt attribution, and an overbroad parser-sandbox claim |
| `b5ad30ae` source tree | Rust 1.97.1 focused macOS planner, collector, and real legacy-producer tests | Pass · planner 35/35, collector 18/18, and neutral-timeout producer 1/1; a first Linux delayed-failure run then exposed a real pre-transfer fork/receiver deadlock, so this tree was not advanced |
| `73180743` | Production post-fork readiness/acknowledgement repair | Finding applied · failure injection waits until the child descriptor table exists; pre-fork failure shuts down but retains the owned receiver, and the child cannot block forever waiting to transfer the listener |
| `b13d7f7b` | Exact pinned Linux 1.97.1 collector plus Linux all-target Clippy diagnostic | Collector pass · 31/31 production/native tests. Clippy correctly rejected unused non-test launch state, an oversized collection signature, and two lint-only ownership helpers; `d827a2f0` repairs those findings |
| `d827a2f0` | Exact effort fast-lane hook after Linux Clippy repair | Pass · history 1,364, catalog 24/30/1,440, operations 211/211, formatting, and pinned all-target workspace compile |
| `19190ee8` | Exact Rust 1.97.1 macOS and Linux focused qualification | Pass · planner 35/35, macOS collector 18/18, pinned Linux collector 31/31, real neutral-timeout producer 1/1, formatting, all-target compile, and all-target Clippy with warnings denied on both platforms. The focused producer test selected the working installed FFmpeg 9.0.1 after the default Homebrew link failed to load its removed x265 ABI; no FFmpeg-8 muxer-identity qualification is inferred |
| `19190ee8` | Unsupported RISC-V compile probe | Incomplete environment evidence · the target was installed, but a transitive native build required unavailable `riscv64-linux-gnu-gcc` and stopped before compiling Plurx. The unsupported audit-architecture fallback remains source-reviewed and fail-closed, not cross-compiled evidence |
| `0eac4425` | Exact post-map receipt and ownership reconciliation | Pass · history 1,365, catalog 24/30/1,441, status/ownership contracts 13/13, formatting, and branch diff check. Its tracked effort hook additionally passed operations 211/211 and the pinned all-target workspace compile |
| `c2248aa7` | Exact sixth-review repair and focused qualification | Pass · planner 35/35, macOS collector 20/20, pinned Linux collector 33/33, both real neutral-timeout producer regressions, status/ownership contracts 13/13, formatting, and denied-warning all-target Clippy on macOS and Linux. Its tracked effort hook passed history 1,365, catalog 24/30/1,441, operations 211/211, formatting, and the pinned workspace compile |
| `fa9e27d7` | Exact post-map sixth-review receipt | Pass · history 1,366, catalog 24/30/1,442, status/ownership contracts 13/13, operations 211/211, formatting, branch diff check, and the pinned all-target workspace compile |
| `b38523be` | Three independent exact-head adversarial reviews on Forgejo PR #63 | Changes requested · one reviewer approved; two found the direct post-`EINTR` notification receive could block after invalidation, the single-interruption regression did not prove expiry or teardown, and the post-fork source audit omitted transitive helpers and common allocation forms |
| `4c1abfaf` | Latest notification-receive repair and exact focused qualification | Pass · receive is single-attempt after fresh poll; persistent interruption covers all four bootstrap phases and invalidated-notification teardown. Planner 35/35, macOS collector 20/20, pinned Linux 1.97.1 collector 34/34, both real neutral-timeout producer regressions, and macOS/Linux all-target Clippy with warnings denied pass. Its tracked precommit source hook passed history 1,369, catalog 24/30/1,443, operations 211/211, formatting, and pinned workspace compile; exact post-map policy evidence is recorded separately at `123b522a`. The first local final-producer invocation selected a broken Homebrew FFmpeg link; the exact rerun used the installed `ffmpeg-full` binary and passed |
| `123b522a` | Exact post-map interrupted-receive receipt | Pass · history 1,370, catalog 24/30/1,443, status/ownership contracts 13/13, operations 211/211, formatting, diff check, and the pinned all-target workspace compile |
| `c7a339fb` | Three independent exact-head adversarial reviews | Changes requested · all found the already-published PR's stale “pending push” wording; probe review additionally found unbounded repeated `EINTR` in the steady-state notification-response loop and missing production-composed teardown coverage |
| `22d89d27` | Steady-response repair and exact focused Linux qualification | Pass · steady denial retries observe the shared launch deadline and supervisor stop; the production-composed second-notification test proves detached ownership returns after slow reap and bounded join. Pinned Linux 1.97.1 all-target Clippy passes and the collector passes 35/35 in parallel; the scheduler-sensitive kill-domain test also passes alone on the preceding exact tree and in the final parallel suite after its observation-window correction |
| `628277d1` | Exact post-map steady-response receipt | Pass · history 1,374, catalog 24/30/1,443, status/ownership contracts 13/13, operations 211/211, formatting, diff check, and the pinned all-target workspace compile |
| `b8f6312b` | Three independent exact-head adversarial reviews | Changes requested · all found that the steady-response regression's one-second slow reap could satisfy every timeout and ownership assertion without proving the second notification or interruption injection was reached; two also required distinct long-deadline stop-path evidence |
| `ac520a0b`–`07c8f905` | Mutation-sensitive steady-response proof and exact Linux qualification | Pass · mode-isolated counters prove repeated response interruptions precede the deadline result, branch-specific receipts distinguish deadline from stop, and a separate two-second launch completes through the stop path in under 500 ms. Pinned Linux Rust 1.97.1 all-target Clippy passes and the complete collector suite passes 36/36 in parallel. Exact removal of injection, deadline enforcement, and the stop check produces 0/1, 0/1, and 0/1 as required |
| `bc3c3bee` | Exact post-map mutation-proof receipt | Pass · history 1,378, catalog 24/30/1,443, status/ownership contracts 13/13, operations 211/211, formatting, diff check, and the pinned all-target workspace compile |
| `077c2bd0` | Three independent exact-head adversarial reviews | One approve, two changes requested · runtime, security, scope, dormancy, ownership, and mutation-sensitive evidence are clean; two reviewers found the focused-validation row incorrectly attributed unchanged-path macOS/planner/fallback results to exact Linux code head `07c8f905` |
| `8702b05d` | Three independent exact-head adversarial re-reviews | Changes requested · all confirmed the evidence split and runtime proof, then found its implementation-scope sentence incorrectly called production-compiled, inert Linux hook plumbing entirely `cfg(test)`-confined |
| `87f869e3` | Three independent exact-head adversarial re-reviews | Two approve, one changes requested · wording is accurate and runtime approval holds; the remaining reviewer showed the status contract did not pin the test-only observer-activation clause |
| `22ab9c8d` | Three independent exact-head adversarial re-reviews | Pass · all three reviewers approved the complete runtime, tests, ownership, evidence wording, and observer-activation contract with no actionable findings |
| `22ab9c8d` | Full-suite command and environment used by the qualified M0 run | Rejected qualification · 22 passed, 1 failed, 2 declared skips. The sole failure was the 1,800-second aggregate `cluster-auth` ceiling: replicated Store, vendor, growth, failure, and topology checks passed, then the ceiling interrupted `cluster_activation` after 3/7 visible passes. Android device remained the declared missing-`adb` skip and two-node Live TV remained the declared Darwin skip |
| `22ab9c8d` | `cargo test --locked -p plurxd --features cluster-integration-tests --test cluster_activation -- --nocapture --test-threads=1` | Pass · 7/7 in 73.83 seconds with warm exact-candidate artifacts; the full-suite cutoff was aggregate budget exhaustion, not a hung activation case |
| `f9e02558` | Validation-budget repair, complete validation package, and effort hook | Changes requested · `cluster-auth` alone receives a 3,600-second cold-host ceiling while the production three-second Store deadline is unchanged; validation tests pass 123/123; history 1,379, operations 211/211, formatting, and pinned all-target workspace compile pass. A clean exact-tree rerun reports catalog 24/30/1,443. Reviewers found stale PR/status provenance, a lower-bound-only timeout assertion, and missing descendant cleanup after an outer timeout; all are repaired in the successor under review |
| `f80100bd` | First adversarial timeout-tree repair and exact receipt | Partial repair · validation tests passed 124/124 and the budget was pinned exactly to 3,600 seconds; exact history 1,380 and catalog 24/30/1,443 passed, and its effort hook passed operations 211/211, formatting, and pinned all-target workspace compile. Exact-head review then proved that outer-PGID signalling missed a cluster child that creates another process group, the test did not model that escape, and `.gitignore` could change the catalog census without selecting `validation.framework` |
| `9a491db1` | Three independent exact-head adversarial re-reviews | Changes requested · all three reproduced or confirmed the nested process-group escape and inaccurate descendant-reap claim; one additionally required `.gitignore` to be governed by the validation catalog. The recursive descendant/session regression and governance repairs are in the successor under focused validation |
| `33c60bd9` / `a04dc484` | Recursive PID cleanup and three exact-head adversarial reviews | Changes requested · all three reviewers proved a daemonized child could escape the PPID walk; TERM/CONT opened a fork and stale-PID window; census failure leaked the tree; and Windows could accept a nonzero partial tree kill after the shell exited. The status/validation claims also overpromised arbitrary descendant containment |
| `f33b0dcb` | Frozen process-group cleanup and exact receipt | Findings applied · POSIX preflights a strict census, immediately stops the root group, includes same-launch-session groups and retained-parent child sessions, confirms a complete stopped closure unchanged in two subsequent censuses, then force-kills each owned group deepest-first without TERM or CONT. One cleanup deadline covers census, kill, owned-shell reap, and output EOF; every incomplete path aborts without exit 124. Windows success/nonzero/timeout paths are explicit. The narrowed trusted-check contract forbids daemonization and claims only the topology actually governed. Validation passes 136/136, exact history 1,383 and catalog 24/30/1,444 pass, and the effort hook passed operations 211/211, formatting, and pinned all-target workspace compile. This following status-contract update changes no runtime or validation behavior |
| `41c16db1` | Forgejo effort gate and three adversarial reviews | Changes requested · Forgejo Linux exposed a valid kernel inventory row with PGID/SID zero; the runner rejected it before any check launched. Reviewers also found the missing post-kill live-identity proof, a shell-leader-exit census gap, cross-call PID metadata races, and uncapped census/group work under the cleanup deadline |
| `d82726ec` | Linux CI and final timeout-review repair | Findings applied · Linux consumes one atomic bounded parent/group/session/state/start inventory and safely excludes non-killable zero-ID kernel rows. Darwin requires identical complete rows plus stable independent group/session observations across two inventories. Cleanup remains root-session anchored after shell-leader exit, bounds rows, bytes, groups, parsing, and signalling, and accepts exit 124 only after every recorded non-zombie identity disappears. Focused runner 55/55, validation 143/143, exact history 1,384, catalog 24/30/1,444, operations 211/211, formatting, and pinned all-target workspace compile pass |
| `7d8bb857` | Three independent exact-head adversarial reviews | One approve, two changes requested · reviewers found that Darwin treated ordinary parent/state transitions and unresolved owned group/session observations as disappearance, numeric PGID reuse could target an unrelated replacement, second-resolution Darwin identity was overclaimed, and a Windows `taskkill` launch error skipped direct shell cleanup |
| `38e52b44` | Identity-bound timeout repair | Findings applied · Darwin preserves owned identity across parent/state transitions, retries relevant ambiguity to the deadline, requires a live stopped owner before committing a child group, and refuses final unowned occupants. Linux identity uses `/proc` start ticks; discovered-child stop and final kill use exact pidfd signals, eliminating final numeric PGID reuse, while the initial anchored root-group stop remains numeric. Post-stop and final-reuse regressions fail closed. Windows launch errors directly kill and reap the owned shell before reporting infrastructure failure. Focused runner 62/62, validation 150/150, exact history 1,385, catalog 24/30/1,444, operations 211/211, formatting, and pinned all-target workspace compile pass |
| `11f3f096` | Three independent whole-PR adversarial reviews | Changes requested · the complete reviews found that a child stopped before admission could be omitted from fallback cleanup, late Linux kill-stage census errors could bypass direct shell cleanup and reap, Darwin collected only the first reappeared ambiguity, and broad pidfd wording incorrectly included the numeric anchored root-group stop |
| `81d46577` | Consolidated whole-PR findings repair | Findings applied in one batch · attempted child groups remain in the final cleanup ledger, half the cleanup budget is reserved for kill/reap/proof, every kill-stage exception becomes an infrastructure error after direct shell kill and reap, all Darwin reappeared ambiguities are evaluated, and pidfd claims now distinguish the numeric anchored root stop from exact child admission/final kill. Focused runner 66/66, validation 154/154, exact history 1,387, catalog 24/30/1,444, operations 211/211, formatting, and pinned all-target workspace compile pass |

## Remaining evidence before release

- Original #913 media on an Apple VideoToolbox node, compared with software
  decode while retaining the hardware encoder.
- Qualified FFmpeg diagnostic output from each supported build/backend class.
- Pixel, metadata, startup, concurrency, and recovery-latency evidence for the
  fleet workload matrix.
- Web, Apple, and Android prepare/readiness/commit/retirement runs with one
  replacement, preserved position/pause/tracks/grade, and no reopen loop.
- Exact-tree Main promotion qualification after current `main` is merged into
  the frozen effort branch.
