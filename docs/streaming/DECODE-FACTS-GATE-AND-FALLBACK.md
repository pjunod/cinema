# Decode-fact gate and fallback — measure the lane, then classify what falls out of it

**Status:** implementation blocked on fleet evidence · **Executes:** C13 (§3.3.2) from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md),
with the assessment's correction 14 (F-stream-9 / F-stream-16: the
complete probe identity stays in the key; the final source check is not
sampled) as its guardrail · **Written:** 2026-09-20 against `main` @
`88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [VOD-ENCODING.md](VOD-ENCODING.md) (immutable VOD preparation
is the main caller), [DECODER-EFFORT-HANDOFF.md](DECODER-EFFORT-HANDOFF.md)
(where the descriptor-bound fact model came from) and
[PLAYBACK.md](../PLAYBACK.md). Read §2 first — it is the lookup path as it
runs today, step by step, with what each step costs and what it proves —
then §3.2, which is the only part of this plan that changes what a caller
gets back. Work §5 in order: §5.1 adds counters and nothing else, and §5.4
and §5.5 are conditional on what §5.1 shows. If a step appears to require
dropping one of the four cache-key dimensions, hashing a source instead of
observing its identity, or skipping the *final* source observation after
collection, stop and flag it — those are the three things correction 14
forbids.

**Correction to the review:** none on the mechanism. Two additions from
the tree that sharpen it:

- `resolve_held_movie_plan`'s fallback plan carries
  `PlanSourceBinding::CatalogRow`
  ([`decode.rs:835`](../../crates/plurx-core/src/transcode/decode.rs)),
  and the type's doc says "callers that need proof must refuse them" —
  but no production caller reads `source_binding()` today (the only
  reference outside tests is a comment at `transcode.rs:14998`). The
  label is informational; nothing refuses on it. The immutable-VOD
  caller relies instead on its own `source.unchanged()` fence check
  *after* planning (`transcode.rs:18359`). So "contention changes fact
  provenance" is exactly right, and today the change is invisible.
- The executed parser image is a **sealed memfd**
  (`memfd_create` + `F_SEAL_SEAL|SHRINK|GROW|WRITE`,
  [`decode_facts.rs:2373-2388`](../../crates/plurxd/src/decode_facts.rs)),
  and `build_digest` is computed from it at startup. A warm lookup's
  three full-content hashes therefore do not protect fact correctness
  (the image cannot change); they enforce a *policy* — refuse to keep
  using an ffprobe the operator has replaced on disk. That distinction
  is what lets §3.3 amortise one of the three hashes without weakening
  anything, and why the other two are amortised differently.

## 1. Objective

Make the cost and the provenance of every decode-fact lookup visible
(gate wait, identity validation, source observation, cache hit,
collection — as five separately named series), classify the reasons a
held-source plan falls back to catalogue facts so an identity change is
never logged and counted like a timeout, and only then — with numbers —
remove the per-lookup work that proves nothing (re-hashing a sealed
image) while keeping every check that does. The p50/p95 gate wait and
first-frame time before and after, measured with the production static
binary on a node, are the acceptance; the review's "latency unmeasured"
is the reason the first milestone is measurement only.

## 2. Contract today

Re-verify at build time; lines from `88a3957a`.

### 2.1 Bounds and gates

[`crates/plurxd/src/decode_facts.rs:24-43`](../../crates/plurxd/src/decode_facts.rs):

```rust
const MAX_PROBE_STDOUT_BYTES: usize = 256 * 1024;
const MAX_PROBE_STDERR_BYTES: usize = 16 * 1024;
const MAX_PROBE_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_VERSION_BYTES: usize = 64 * 1024;
const IDENTITY_DEADLINE: Duration = Duration::from_secs(10);
const VERSION_DEADLINE: Duration = Duration::from_secs(5);
const PROBE_DEADLINE: Duration = Duration::from_secs(10);
const MAX_CACHE_ENTRIES: usize = 256;

fn identity_gate() -> Arc<tokio::sync::Semaphore> {   // permits = 1
fn version_gate() -> Arc<tokio::sync::Semaphore> {    // permits = 1
```

`DecodeFactCache` (`:2881-2900`): `entries: Mutex<BTreeMap<CacheKey,
DecodeFacts>>`, FIFO `order`, and `probe_gate: Arc<Semaphore>` with one
permit — "one bounded global lane serializes executable revalidation and
cache misses. Fact preparation cannot fan out unbounded hashing or child
processes when several sessions start together." The caller's budget is
`DECODE_PLAN_PROBE_BUDGET = 2 s`
([`transcode.rs:69`](../../crates/plurxd/src/transcode.rs)), applied at
`:14096` and `:18356`.

### 2.2 The cache key (correction 14 — all four stay)

`decode_facts.rs:52-57`:

```rust
struct CacheKey {
    source: DecodeSourceIdentity,
    ffprobe_build_digest: String,
    catalog_digest: Option<String>,
    selected_stream: ProbeStreamSelection,
}
```

`source` is `sha256({bytes, modified_seconds, modified_nanoseconds,
changed_seconds, changed_nanoseconds, device, inode})` of the **held
descriptor** (`source_observation`, `:3128-3157`) — an `fstat`, not a
content hash, and taken under `identity_gate` (`:3159-3200`).
`ffprobe_build_digest` is `sha256({canonical_path, content_digest,
version})` from startup (`:441-446`). `catalog_digest` is
`DecodeCatalogMetadata::digest()` (the scan facts the on-demand probe
cannot be trusted to reproduce). `selected_stream` is the ordinal.

### 2.3 What a lookup does, in order (`get_or_probe`, `:2905-3024`)

```text
 1  acquire probe_gate            (1 permit, process-wide)   ← gate wait
 2  probe.validate_current        three full-content SHA-256s ← identity
      held fd  (on-disk ffprobe, opened at startup)
      snapshot (the sealed memfd, up to 512 MiB)
      path     (re-open by name; catches replacement)
    all three must equal the startup identities, else ProbeChanged
 3  source_observation            fstat under identity_gate  ← source obs.
 4  entries.get(&key)             HIT → return                ← hit
    ── miss ──
 5  acquire source.offset_gate    (per-source)
 6  spawn collect()               ffprobe on the sealed image ← collection
 7  probe.validate_current        the same three hashes again
 8  source_observation            must equal step 3, else SourceChanged
 9  insert (FIFO evict at 256)
```

Every step shares the caller's budget (`budget.min(PROBE_DEADLINE)`) and
returns `Deadline` when it runs out; every step is cancellable and hands
its permit to the blocking worker so a cancelled waiter cannot let a
second hash or child start (`run_bounded_identity_task_on`, `:2520-2562`).
A **warm hit** therefore pays: the gate wait behind any in-flight miss's
steps 2–8, then three hashes of executable-sized inputs (the shipped
static ffprobe is tens of MB), then one fstat — before touching the map.

`validate_current` (`:464-495`):

```rust
    async fn validate_current(
        &self,
        budget: Duration,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<(), DecodeFactError> {
        let started = std::time::Instant::now();
        let held =
            held_probe_file_identity_within(Arc::clone(&self.executable_file), budget, cancelled)
                .await?;
        let remaining = budget.saturating_sub(started.elapsed());
        let snapshot = held_probe_file_identity_within(
            Arc::new(
                self.executable_snapshot
                    .as_file()
                    .try_clone()
                    .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?,
            ),
            remaining,
            cancelled,
        )
        .await?;
        let remaining = budget.saturating_sub(started.elapsed());
        let current =
            path_probe_file_identity_within(self.executable.clone(), remaining, cancelled).await?;
        if held == self.file && current == self.file && snapshot == self.snapshot_file {
            Ok(())
        } else {
            Err(DecodeFactError::ProbeChanged)
        }
    }
```

`ProbeFileIdentity` (`:318-327`) is `{bytes, modified_seconds,
modified_nanoseconds, changed_seconds, changed_nanoseconds, device, inode,
content_digest}`; `probe_file_identity_from_file` (`:2462-2500`) always
reads the whole file in 64 KiB chunks to fill `content_digest`. All three
hashes run on `spawn_blocking` under `identity_gate` (one at a time,
process-wide).

### 2.4 The fallback (`resolve_held_movie_plan`, `transcode.rs:14070-14122`)

```rust
        match facts {
            Ok(facts) => self.resolve_movie_plan_with_facts(
                file,
                options,
                encoder,
                &facts,
                &AttemptRestrictions::none(),
            ),
            // A probe that could not finish inside its budget, or a node with
            // no probe artifact at all, must not make the title unproducible.
            // Fall back to the same stored-probe plan live and offline already
            // use; the plan records `CatalogRow` so nothing downstream can
            // mistake it for a descriptor-bound measurement.
            Err(error) => {
                tracing::debug!(
                    file_id = file.id,
                    %error,
                    "bound decoder planning fell back to stored probe facts"
                );
                self.resolve_movie_plan(file, options, encoder).await
            }
        }
```

`DecodeFactError` (`:3058-3075`) has fourteen variants; the arm treats all
of them alike at `debug`. Callers: `resolve_bound_movie_plan` (offline /
pretranscode artifacts, `:14043`) and immutable VOD preparation
(`:18348`, which maps a plan *error* to `vod_decoder_plan_refused` and,
separately, refuses `vod_source_rescan_required` when its own
`SourceFence` reports the source changed). Rolling first play does not
come through here (review §3.3.2: "rolling start uses catalogue facts, so
this is not every play").

### 2.5 The existing tests that must stay green

`decode_facts.rs` tests (all by name, `cargo test -p plurxd decode_facts::tests::<name>`):
`a_replaced_probe_cannot_reuse_cached_facts` (`:4935`),
`transient_path_swap_cannot_change_the_executable_object` (`:4979`),
`snapshot_path_swap_cannot_change_the_executable_object` (`:5035`),
`sealed_snapshot_refuses_in_place_mutation` (`:4211`),
`cancelled_identity_hash_keeps_the_single_worker_admission` (`:5147`),
`caller_deadline_detaches_cleanup_without_releasing_its_ownership` (`:5197`),
`concurrent_same_key_misses_spawn_one_probe` (`:5228`),
`cancellation_while_waiting_for_probe_admission_is_bounded` (`:5279`),
`running_cancellation_reaps_before_restoring_and_releasing_the_source` (`:5348`),
`source_lease_wait_is_charged_to_the_probe_deadline` (`:5455`),
`blocked_source_identity_returns_deadline_without_releasing_probe_ownership` (`:4705`).
These are the "binary replacement, same-size mutation, cancellation and
source-change tests" §3.3.2's acceptance names.

## 3. Change

### 3.1 Instrument the five phases separately (no behaviour change)

Two series, rendered by the existing hand-written `/metrics` writer
(`http/system.rs:4991`, same shape as `plurx_store_operation_seconds` in
[`hiqlite.rs:676-720`](../../crates/plurx-core/src/store/hiqlite.rs) —
fixed bucket array, atomics, no allocation on the hot path):

```text
plurx_decode_facts_phase_seconds{phase, outcome}   histogram
  phase   ∈ {gate_wait, identity_validation, source_observation,
             collection, final_validation}
  outcome ∈ {ok, deadline, cancelled, changed, error}
  buckets: 0.001 0.005 0.01 0.025 0.05 0.1 0.25 0.5 1 2 5 10 +Inf

plurx_decode_facts_lookups_total{result}           counter
  result  ∈ {hit, miss_collected, refused}
```

`gate_wait` is step 1's duration; `identity_validation` is step 2 (and
step 7 as `final_validation`); `source_observation` covers steps 3 and 8;
`collection` is step 6 wall time; a `hit` is counted after step 4
returns. Labels are closed sets defined as enums with `ALL` arrays, as
`StoreOperationClass` does; no file id, path or digest ever enters a
label. The `DecodeFactCache` gains a `metrics: Arc<DecodeFactMetrics>`
field constructed in `TranscodeManager` next to the cache
(`transcode.rs:13010`) and passed to `/metrics` through `AppState` the way
the store histogram is.

### 3.2 Classify the fallback (the one caller-visible change)

Replace the single `Err(error)` arm with a match on the variant, producing
a bounded reason and a disposition:

| `DecodeFactError` | reason label | disposition | log level |
|---|---|---|---|
| `Deadline`, `Cancelled` | `deadline`, `cancelled` | fall back to catalogue facts (unchanged) | `debug` |
| `ProbeChanged` | `probe_changed` | fall back (unchanged), **but** log once per process at `warn` with the text "the configured ffprobe changed on disk after startup; bound facts are refused until plurxd restarts" — that is what the check means, and today it is invisible | `warn` (rate-limited), then `debug` |
| `SourceChanged` | `source_changed` | **refuse**: return `Err("the held source changed during decoder probing; rescan before playback")`. Catalogue facts describe the object the scanner saw; the held descriptor now describes something else, and a plan from the catalogue would be a plan for the wrong bytes. The VOD caller already has this refusal one line later (`vod_source_rescan_required`); this moves it to where the evidence is. | `warn` |
| `Spawn`, `MissingPipe`, `Read`, `OversizedOutput`, `Failed`, `InvalidJson`, `InvalidFacts` | `probe_failed` | fall back (unchanged) | `warn` |
| `ProbeIdentity(_)`, `SourceMetadata(_)` | `identity_io` | fall back (unchanged) | `warn` |
| `CacheInvariant`, `UnsupportedPlatform` | `invariant` | fall back (unchanged) | `error` |

Counter: `plurx_decode_plan_fallbacks_total{reason}` with `reason` the
closed set above plus `refused_source_changed` for the new refusal. The
plan the fallback returns keeps `PlanSourceBinding::CatalogRow`; the
refusal is the only disposition change, and it is the one the review
asked for: an identity change is not a timeout.

Why not refuse on `ProbeChanged` too: the executed image is sealed, so
the facts the daemon *would* have used are still the ones its
`build_digest` names; refusing bound facts and planning from the
catalogue is safe. Refusing the title would make an operator's ffprobe
upgrade an outage until restart. The `warn` is what was missing.

### 3.3 Amortise validation of the sealed image; keep tamper checks on the mutable objects

Three objects are hashed per lookup; they are not alike.

- **The sealed snapshot** cannot change: `F_SEAL_WRITE|SHRINK|GROW|SEAL`
  are kernel-enforced and `sealed_snapshot_refuses_in_place_mutation`
  proves the refusal. Re-hashing it proves only that the seals held,
  which `fcntl(F_GET_SEALS)` answers in one syscall. Change: replace the
  snapshot's per-lookup content hash with `F_GET_SEALS == expected &&
  fstat(dev, inode, size) == captured`, and hash the content only at
  startup (already done, `:409-426`) and once per **validation
  generation** (below) as a belt. This is a stronger-or-equal check at
  O(1) instead of O(image).
- **The held on-disk descriptor and the path object** are mutable: an
  in-place write changes content (and `ctime`), a replace changes
  `inode`. Their check is the policy in §3.2 (`probe_changed`). Change:
  introduce a *validation generation* — a counter on `DecodeProbeIdentity`
  advanced when `PROBE_REVALIDATION_INTERVAL` (proposal: 30 s; a
  constant, not a setting) has elapsed since the last full check. Within
  a generation a lookup compares the cheap identity tuple `(device,
  inode, bytes, modified_*, changed_*)` of the held fd (`fstat`) and of
  the path (`open` + `fstat`) against the captured tuple; **any tuple
  difference forces a full content hash before the verdict**, and the
  generation boundary forces one regardless. So the check is never
  mtime-only: it is the seven-field tuple every time, the content digest
  on every tuple change, and the content digest at least every 30 s. A
  same-size in-place mutation that also restores `mtime` still moves
  `ctime` (an unprivileged writer cannot set it), and is caught at the
  next lookup; a root-level actor who can also rewind `ctime` is outside
  this trust model (the doc comment at `:329-336`: "an operator-trusted
  artifact", "not a sandbox for malicious parser behavior").
- What this does to fact correctness: nothing — facts come from the
  sealed image and the key names its digest. What it changes: the
  *latency* of the policy refusal after an operator replaces ffprobe is
  at most one interval instead of one lookup. That is the trade the plan
  proposes, and it is stated in the constant's doc comment.

Contrast with §2.7 of the review (font attestation), where a TTL was
withdrawn: there, a missed re-enumeration changes the bytes a recipe
emits. Here it cannot.

### 3.4 Separate warm-hit admission from cold collection — only after §3.3

Today a hit waits on `probe_gate` behind a miss's whole collection (up
to the `DECODE_PLAN_PROBE_BUDGET`, which is then spent; that budget is
10 s on main since S-08 bound its descriptor-based `idet` verification to
it, so the wait this section removes is five times the one it was written
against). After §3.3 a hit does one
`F_GET_SEALS`, two `fstat`s (or a tuple compare), one source `fstat` and a
map read — no child, no hash — so the guarantee `probe_gate` exists for
("cannot fan out unbounded hashing or child processes") no longer needs
the hit to hold it. Change: steps 2–4 run under `identity_gate` only
(which already serialises the `fstat`s); `probe_gate` is acquired on the
miss path before step 5. Guarantees that must be shown unchanged, by the
tests in §2.5 plus two new ones:

- one child per key under concurrent misses
  (`concurrent_same_key_misses_spawn_one_probe`);
- a hit observed *during* a miss's collection for the same key returns
  the entry only after step 9's insert, or misses and joins the
  singleflight — never a half-built entry (new test:
  `hit_during_collection_waits_or_joins`);
- a hit's source observation is the same `fstat` under the same
  `identity_gate`, so `blocked_source_identity_returns_deadline_without_releasing_probe_ownership`
  still holds with `probe_gate` no longer held (the test's assertion
  moves to `identity_gate`; new twin
  `blocked_source_identity_on_hit_does_not_block_a_miss`).

If §5.1's numbers show `gate_wait` is not where warm time goes, §3.4 is
not built.

## 4. Guardrails (non-goals)

1. **All four `CacheKey` dimensions stay.** Correction 14. No inode/size/
   mtime key, no persisted cache in this plan (F-stream-9's persisted
   attestation is a separate design).
2. **The final source observation (step 8) is never sampled or skipped.**
   Correction 14 again ("do not rate-limit the final source check").
   §3.3 amortises *executable* validation, never source observation.
3. **Never mtime-only.** The tuple is seven fields, any change forces a
   content hash, and the content hash recurs per generation (§3.3).
4. **The sealed snapshot stays the executed image**, and `build_digest`
   stays a content digest of it computed at startup. Nothing here
   changes what runs or how the key names it.
5. **No new global lock.** The generation counter is an atomic; the
   tuple compare runs under the existing `identity_gate` as today.
6. **Bounded labels only.** No file id, path, digest or session in any
   metric label (§4.9 of the review: "bounded labels and no content
   identifiers").
7. **Fallback dispositions other than `SourceChanged` do not change.**
   A title that plays today from catalogue facts after a timeout still
   does.
8. **Measurement first.** §5.4 and §5.5 do not open until §5.1's fleet
   reading is in a PR body; the review's own acceptance is p50/p95 gate
   wait and first-frame time with the production static binary, and a
   number nobody measured cannot improve.
9. **Do not touch `DECODE_PLAN_PROBE_BUDGET`** (2 s) in this plan; if the
   numbers say the budget is wrong, that is its own decision with its own
   evidence.

## 5. Milestones

One whole-plan draft PR into `main` under the fast lane. Milestones are
logical commits in that PR, following the work board's canonical rule. M3 and
M4 remain conditional on the fleet measurements named below; a source-only
session must not infer those results from unit timings.

### 5.1 M0 — instrumentation

§3.1: the two series, the metrics struct, the `/metrics` rows, and a unit
test that drives one hit and one miss through a fixture cache and asserts
the five `phase` counts and the `result` counts. Then a fleet reading:
deploy, play three encoded-VOD titles concurrently on media1 twice (cold
then warm), and read the histograms.

Acceptance: `cargo test -p plurxd decode_facts::tests::metrics_count_each_phase_once`
green; `make unit` green; `curl -s http://media1:32400/metrics | grep
plurx_decode_facts_` shows all five phases with non-zero counts after the
run, and the PR body carries p50/p95 for `gate_wait` and
`identity_validation` on hits, with the ffprobe size from `ls -l
$(readlink -f $(which ffprobe))`.

### 5.2 M1 — classify the fallback (except the refusal)

§3.2 minus the `SourceChanged` row: the match, the reason counter, the
rate-limited `ProbeChanged` warning. No disposition changes. Test: one
table test feeding each `DecodeFactError` variant through a fixture
`resolve_held_movie_plan` and asserting the reason label and that the
result is a `CatalogRow` plan.

Acceptance: `cargo test -p plurxd transcode::tests::held_plan_fallback_reasons`
green; `plurx_decode_plan_fallbacks_total` present on `/metrics`; a
deliberate `chmod`/`touch` of the configured ffprobe on a lab node
(`lab2`) produces exactly one `warn` line in `journalctl -u plurxd` and
`reason="probe_changed"` increments.

### 5.3 M2 — refuse on `SourceChanged`

The one behaviour change. Tests: (a) the fixture path where the source is
replaced between steps 3 and 8 now returns `Err` from
`resolve_held_movie_plan` and `vod_decoder_plan_refused` from the VOD
caller, instead of a `CatalogRow` plan; (b) the pretranscode caller
(`resolve_bound_movie_plan`) surfaces it as a retryable job failure with
the reason text, not a silent catalogue plan; (c) existing
`running_cancellation_reaps_before_restoring_and_releasing_the_source`
unchanged.

Acceptance: `cargo test -p plurxd transcode::tests::source_change_refuses_bound_plan`
green; `make unit` green; `reason="refused_source_changed"` exists on
`/metrics`.

### 5.4 M3 — generation-scoped validation (conditional on M0)

Opens only if M0 shows `identity_validation` p50 on hits above 10 % of
the warm first-frame time on media1 (a threshold Paul may move; the point
is that it is a number). §3.3: `F_GET_SEALS` + `fstat` for the snapshot,
tuple-compare-then-hash-on-change for the held fd and path,
`PROBE_REVALIDATION_INTERVAL = 30 s`. Tests: the four replacement /
mutation tests from §2.5 unchanged; new
`same_size_in_place_mutation_is_caught_within_one_lookup` (writes one
byte at the same length through a separate descriptor, asserts the next
lookup returns `ProbeChanged` because `ctime` moved and the forced hash
disagreed); new `generation_boundary_forces_a_content_hash` (advances a
test clock past the interval with an unchanged tuple and asserts the hash
ran — counted through the `identity_validation` histogram's bucket
movement, which is why M0 comes first).

Acceptance: those tests green; `make unit` green; the M0 fleet
measurement repeated shows `identity_validation` p95 on hits below 5 ms
and `plurx_decode_facts_lookups_total{result="hit"}` unchanged in ratio.

### 5.5 M4 — warm-hit admission (conditional on M0 and M3)

Opens only if, after M3, `gate_wait` p95 on hits still exceeds the M3
target because hits queue behind misses. §3.4 with its three named tests.

Acceptance: `cargo test -p plurxd decode_facts::tests::` green including
the two new tests; the concurrent warm/cold run from M0 repeated shows
hit `gate_wait` p95 independent of a concurrent cold start (numbers in
the PR body).

### 5.6 Acceptance with the production static binary

Every fleet measurement above runs against the shipped jellyfin-ffmpeg 8
static `ffprobe` on media1 (the production path refuses anything that is
not a self-contained ELF, `require_self_contained_linux_elf`), never
against the fixture scripts the unit tests use. The PR body for each
milestone records `ffprobe -version | head -1`, the file size, and the
node.

## 6. Verification and rollout

- Fast lane: `make unit`; focused `cargo test -p plurxd decode_facts::`
  and `cargo test -p plurxd transcode::tests::held_plan` per milestone.
- Fleet: the reading in each milestone's acceptance; deploy through the
  existing Ansible path; no setting, no migration.
- Rollback: revert the milestone's PR. M2's refusal reverts to the
  catalogue fallback; nothing durable changes in either direction.
- GPT prompt for the M0/M3/M4 concurrent measurement:

```text
On media1, with the current main deployed: start three encoded-VOD plays
of different titles within five seconds of each other on three clients
(any three of web, Apple TV, Android TV), let them run 30 s, stop all
three, then start the same three again within five seconds. Before the
first start and after the second stop, save
`curl -s http://10.42.0.10:32400/metrics | grep -E
'plurx_decode_facts_(phase_seconds|lookups_total)|plurx_decode_plan_fallbacks_total'`
to two files and send both. Also send `journalctl -u plurxd --since
-10min | grep -E 'decoder planning|decode_facts'`. Do not restart plurxd
between the two rounds.
```

## 7. Open questions

1. The 30 s `PROBE_REVALIDATION_INTERVAL` and the 10 % threshold in §5.4
   are proposals; Paul sets both once M0's numbers exist.
2. Whether the pretranscode caller should retry `refused_source_changed`
   automatically after a rescan or leave the job failed for the operator
   — M2 implements "failed with reason" and asks.
3. Whether `ProbeStreamSelection::FirstPlayable` (marked
   `#[allow(dead_code)]` for "M2's selected-stream adapter") will land
   before this plan's M3, which would add a key dimension the metrics
   should not label by (it stays unlabelled either way).
4. Whether any production caller should start *reading*
   `source_binding()` and refusing `CatalogRow` — the type's doc says
   some must; none do. Not this plan's change, but the fact is recorded
   here for the owner of the immutable-VOD contract.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | claim | [#424](http://192.168.4.7:3000/noirr/plurx/pulls/424) | Claimed `plan/S-13` for one whole-plan draft PR. Rust 1.97.1 compiler loop established; M3/M4 remain closed until M0 fleet evidence opens them. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M0 | [#424](http://192.168.4.7:3000/noirr/plurx/pulls/424) / `a4682e5d` | Added fixed-cardinality phase histograms and lookup counters to `/metrics`; the real miss-then-hit fixture asserts every phase and result. Focused test, Rust 1.97.1 check, format and Clippy are green. Needs: deploy this exact branch artifact on media1 and run the §6 production-static-binary cold/warm measurement; no p50/p95 or binary size was inferred locally. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M1 | [#424](http://192.168.4.7:3000/noirr/plurx/pulls/424) / `8432e9bc` | Every `DecodeFactError` maps exhaustively to a bounded reason, severity and counter. `ProbeChanged` warns once per process, while existing catalogue fallback disposition remains unchanged. Focused classification test is green. Needs: the lab2 deliberate ffprobe-change log/counter observation. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M2 | [#424](http://192.168.4.7:3000/noirr/plurx/pulls/424) / `8432e9bc` | `SourceChanged` alone now refuses the held plan with the rescan instruction and increments `refused_source_changed`; a deterministic real held-source mutation regression is green, together with the existing post-probe source fence and cancellation cleanup regressions. The VOD caller preserves `vod_decoder_plan_refused`; the pretranscode path propagates the actionable failure into its existing retry lifecycle. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M3-M4 | [#424](http://192.168.4.7:3000/noirr/plurx/pulls/424) | Not opened. M3 requires M0's media1 result to exceed the 10% threshold; M4 additionally requires M3 and remaining hit gate wait above target. Source-only unit timings are not a substitute. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | review disposition | [#424 comment 3333](http://192.168.4.7:3000/noirr/plurx/pulls/424#issuecomment-3333) / `0abf5615` | Added a bounded hit-only phase histogram so fleet p50/p95 can be joined to cache hits without miss contamination; collection latency is now recorded inside the owned task after kill/reap and source restoration. The fallback table now drives the real disposition and proves every non-identity failure returns a `CatalogRow` plan, while the production caller seam proves VOD keeps `vod_decoder_plan_refused` and pretranscode keeps the actionable retry text. Rust 1.97.1 check, format, scoped Clippy, and the four named regressions are green. Fleet and lab evidence remain pending exactly as above. |
