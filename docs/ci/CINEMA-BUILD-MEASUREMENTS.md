# Cinema build measurements — reproducible developer comparisons

**Status:** built — SDK-free build comparisons and focused migration/authority checks pass; exact-candidate main promotion is tracked in the canonical plan.
**Written:** 2026-11-04 · **Owner:** Codex.

The [modular-build plan](CINEMA-MODULAR-BUILD-PLAN.md) owns the phases and
acceptance gates. This ledger owns commands, results and their limits. It does
not qualify a release or establish the final Hiqlite-free dependency graph.

## 1. Measurement runner — owned source, preserved receipts

Use [scripts/build-measure](../../scripts/build-measure) on a source-only
archive extraction, outside any Git checkout. The source, target directory and
caller-supplied slot each have an exclusive lease. The outer staging workflow
must hold its existing compiler-slot lease through transfer, execution and
receipt collection as well. Never stage over an active measurement.

The initial private compiler wrapper predates these remote leases. These
measurements therefore use a controlled single-writer slot: no other wrapper
runs while the measurement owns it. They do not establish protection against
an unrelated wrapper after host-orchestrator death. P2a must make every staging
entry point honor the same remote source/target leases before claiming that
stronger concurrency guarantee.

Source and target directories must be disjoint. Receipts also live outside
both. A target inside the source tree could otherwise hide real inputs from
the inventory, so it is refused before any mutation.

Initialize ownership once, then measure under the same declared commit/tree:

```bash
scripts/build-measure --source "$source_dir" --target "$target_dir" \
  --output "$receipt_dir" --recipe "$recipe_file" --lease "$slot_lease" \
  --revision "$commit" --tree "$tree" --label system --initialize
scripts/build-measure --source "$source_dir" --target "$target_dir" \
  --output "$receipt_dir" --recipe "$recipe_file" --lease "$slot_lease" \
  --revision "$commit" --tree "$tree" --label system \
  --cases noop,asset,leaf,api --samples 3
```

The recipe is JSON, with these fields:

| Field | Meaning |
|---|---|
| `command` | Argument array, such as Cargo with a pinned toolchain, `build --locked --offline -p plurxd --message-format=json-render-diagnostics --timings -j4`. No shell evaluation. |
| `environment` | Allowed build flags, jobs, wrappers and fixed build identity inputs. Record the same identity within a comparison. |
| `capabilities` | Explicit runtime storage, cluster, sharing and inference capabilities. Equal-capability comparisons use the same manifest. |
| `cache_state` | Separate `target` and `compiler_result_cache` descriptions. An empty target is not evidence of a cold compiler-result cache. |
| `expected_artifacts` | Per-case exact Cargo `package_id`, `target_name` and `target_kind` identities. `warmup` identifies the daemon; `api` includes core and its dependent daemon. Obtain IDs from the actual extraction's Cargo metadata. |
| `linker_probe` | Optional argument array for recording the selected linker's version. |

Cargo must emit artifact JSON. Each changed-source sample must rebuild its
intended unit. No-op samples must report the relevant fresh artifact and may
not compile other units. Successful restoration builds are separate from
sample medians. The probes change actual bytes: an embedded CSS comment, a
used daemon function body, and an additive public core constant. The API probe
measures this small metadata change, not every possible shared API refactor.

The runner prints a short summary and receipt path. The receipt retains raw
output, Cargo artifact events, elapsed wall time, host and recipe context,
allocated target size when `du` is available, and Linux GNU time peak child RSS. Cargo timing reports
remain in the owned target directory and must be copied into the durable
receipt bundle. Receipts live outside source and compiler caches. Record
source staging time and target size separately.

Probe journals preserve original bytes and generated hashes. Cancellation
drains the compiler process group before restoration; uncertain child ownership
retains the probe. Numeric group signals require a live, unreaped direct child,
default SIGCHLD handling and a sole reaper. A reaped leader cannot authorize
signalling its old numeric group ID: remaining descendants require checked
outer cleanup, while the probe is retained. `--recover` uses the same arguments and refuses live or
uncertain compiler ownership and unexpected source edits. Inspect retained
receipts before manual recovery; restored source alone does not qualify its
compiled artifacts. Never run a benchmark artifact as a release candidate.

## 2. Baseline — integrated runtime source

The initial diagnostic run against `f30dfbbc9` is retained privately but is
excluded from speedup claims. The comparison baseline incorporates current
main: runtime candidate `cf095b203`, tree
`e1ce9af4ce7247c6e9743f9952692758259282bc`. If integration moves before the
comparison, recapture the relevant baseline and identify the new source.

The compiler slot is Linux ARM64, pinned Rust 1.97.1, four Cargo jobs, a fetched
registry and an existing warm target. Its daemon capabilities include both
native SQLite and cluster support, remote-user sharing and inference. This
baseline still contains Hiqlite. No measurement here establishes physical Mac,
Windows, FFmpeg 6, production-release or final standalone-package acceptance.

The source is a benchmark snapshot. Later promotion-cancellation corrections
have separate [runtime qualification evidence](../cluster/STANDALONE-SQLITE-MODE-PROPOSAL.md#11-implementation-ledger--source-evidence-not-release-qualification).

## 3. Diagnostic baseline — system linker, full capabilities

Linux ARM64, 12 virtual CPUs, 24 GiB VM memory, four Cargo jobs, Rust 1.97.1
and GNU ld 2.46. Compiler-result wrappers and Rust flag overrides were absent.
The initial check inspected the compiler guest but missed a legacy daemon on
its physical host. These runs predate correcting that host-isolation mistake;
they are diagnostic evidence, not acceptance of a safe developer recipe.
Mac-hosted compilation now uses the repository's voter-build limits (8 GiB,
no additional swap, four CPUs). A separately verified physical non-voter
compiler has also passed the pinned compiler and native SDK checks in a
24 GiB/no-swap/four-CPU scope. Both timing variants must be rerun on the same
verified host with identical actual resource limits and the normal development
profile before recipe acceptance. The original results remain retained.
Source archive creation/transfer/staging took 7.14 seconds, outside Cargo time.
The warm target's initial 243.26-second rebuild is excluded from these medians;
it is not a cold compiler measurement.

| Case | Samples, seconds | Median, seconds | Range, seconds |
|---|---|---|---|
| No-op | 0.388, 0.174, 0.175 | 0.175 | 0.174–0.388 |
| Embedded CSS edit | 54.548, 54.002, 56.527 | 54.548 | 54.002–56.527 |
| Used daemon function edit | 68.622, 54.050, 51.722 | 54.050 | 51.722–68.622 |
| Small public core API edit | 70.761, 85.682, 74.709 | 74.709 | 70.761–85.682 |

Raw output, Cargo timings, each actual probe hash and original bytes, machine
context and all artifact events are retained in the owned evidence bundle
outside compiler caches. Every restoration rebuilt its intended owner; API
restoration rebuilt the dependent daemon too. The restored inventory matches
the original. No-op rows report fresh relevant artifacts with zero compilation.

The frozen comparison runner's SHA-256 is
`e7b9d0369a18a76c764d1ecee7451c0e09ac7d0edce19685d3f846e66214fdcf`;
the baseline report's SHA-256 is
`410755c0ecb5baf5b0656db1f36fdc2c1251e3af2291aa63ac4e6891e2d4d310`.
Subsequent runner hardening makes restoration rebuild validation automatic,
records inherited compiler/target selection, refuses nested targets and keeps
CLI output short. Its initial 21 focused tests passed. These later checks do not alter
the frozen timing loop; actual baseline restoration events were independently
postvalidated. The developer-recipe task records its own after measurements
and acceptance, including every outlier. No disk-size or cold-build gain is
claimed by this baseline.

Before P1 merge, Astra confirmed a cancellation defect: signalling a numeric
process group after reaping its leader could affect an unrelated reused group.
The public runner now verifies sole-reaper ownership before each TERM/KILL and
retains changed source when descendant drainage is uncertain. Its SHA-256 is
`24e05df06ded864b5b401e87b74449d0bc7520f162d837e6c428ff87a7bce470`.
All 27 focused tests pass on macOS and Linux. This corrects cleanup safety;
it does not qualify new performance measurements. A fresh exact-head effort
gate remains required, and the final dependency-omission milestone stays open.

## 3. Primary capped linker comparison — 2026-11-04

P1 landed through PR #1384 (`6545218ef`). A paired experiment on exact
commit `cbc9cc75885f6cc94307d37394b0493e0d24f33b`, tree
`523511a2a395f3829ae2479af1c3c099005fddf4`, compared the unchanged default Cargo recipe
and an explicit final-daemon-only Clang/LLD recipe. Both used Rust 1.97.1, the same owned warm target,
four CPUs, 24 GiB memory, zero swap, and the unchanged default dev profile
(full debug information). Each recipe warmed up before three samples per case.

| Rebuild case | Default median, range (seconds) | Explicit LLD median, range (seconds) |
|---|---:|---:|
| No change | 0.216, 0.215–0.266 | 0.266, 0.266–0.316 |
| Embedded asset | 17.454, 17.444–17.856 | 17.730, 17.641–17.790 |
| Daemon leaf | 17.726, 17.712–17.837 | 17.713, 17.699–17.843 |
| Core API | 27.668, 27.425–27.968 | 27.652, 27.553–28.121 |

All measured samples, in execution order (seconds):

| Case | Default samples | Explicit LLD samples |
|---|---|---|
| noop | 0.266, 0.216, 0.215 | 0.316, 0.266, 0.266 |
| asset | 17.856, 17.444, 17.454 | 17.641, 17.790, 17.730 |
| leaf | 17.726, 17.712, 17.837 | 17.713, 17.843, 17.699 |
| api | 27.968, 27.668, 27.425 | 28.121, 27.652, 27.553 |

There is no demonstrated useful linker gain in this experiment. Do not promote
the experimental linker recipe as an improvement. Keep the normal recipe and
measure the actual standalone dependency cut against this baseline. This is an
incremental comparison; it supplies no clean-build or SDK-removal result.

Both runs completed and restored the identical source inventory
`0b3100cc1856ac92035fbe10ff3392111dca25cc241aac1fa6b579e325273e35`.
The retained, collected evidence bundle has SHA256
`afdac15fec74d351313015f9f677eef9d478bad8e955aafedd82f43c4f1d0f09`;
run ID `40f7dcfcc0f3405db8322d5509c7f554`. The scope recorded no OOM or
OOM kill. The physical compiler was a non-voter with no external daemon load.

## 4. Actual standalone SDK removal — 2026-11-07

The before revision is current main `8c654d37b5ee3f1418fa3393662c671a6555455e`
(tree `1a8ddcb00c2aaff287dd6eed45b6cc2153421ae2`); after is the integrated
standalone candidate `02ea8ed11d61f737f660a1f60d2f20223d8487c0`
(tree `5aad4a8a6010c4a6818d5bc0af0fdbdc32172a29`), containing that main.
The unchanged product runner and existing exclusive compiler slot measured both
with Rust/Cargo 1.97.1, four CPUs, 24 GiB, no swap, default dev debug level 2,
no compiler-result cache and the same requalified extraction-only SDK.
The two installed PCRE security patch updates were revalidated before either
run; all other package identities and the SDK preparer remained unchanged.

Both revisions ran exactly:

```text
cargo +1.97.1 build --locked --offline -p plurxd --message-format=json-render-diagnostics --timings -j4
```

The private orchestration reused the existing guard, leases, runner and sealed
receipts. Its only recipe changes from the earlier linker experiment were
direct Cargo and one system-linker variant, applied equally to both revisions.
Changed-source artifact freshness and complete source restoration were checked.

| Probe | Before samples (seconds) | After samples (seconds) | Before median | After median | Reduction |
|---|---|---|---:|---:|---:|
| noop | 0.215781, 0.216170, 0.215704 | 0.165457, 0.164635, 0.164240 | 0.215781 | 0.164635 | 23.7% |
| asset | 17.783790, 17.622364, 17.652122 | 15.448159, 15.361606, 15.343836 | 17.652122 | 15.361606 | 13.0% |
| leaf | 18.031873, 18.144088, 17.743023 | 15.375393, 15.554111, 15.566167 | 18.031873 | 15.554111 | 13.7% |
| api | 27.979877, 26.948786, 26.839063 | 23.281637, 23.703074, 23.414733 | 26.948786 | 23.414733 | 13.1% |

Actual Cargo artifact inventories contain **448 before / 380 after** units.
Before includes Hiqlite, Hiqlite WAL/derive and OpenRaft/macros; after includes
none. This proves omission in the measured daemon build. The separate shipping
graph check additionally covers controller and tray across normal/build edges.

These are warm incremental rebuild medians, not clean-build measurements.
Warmups took 208.096292 s and 165.615445 s in the reused slot; its prior cache
contents differ, so those values do not establish a cold-build improvement.
Allocated mixed target/cache inventory grew from 70,630,109,184 to
83,552,571,392 bytes while retaining both configurations, not a binary-size
or standalone disk-footprint comparison. Release Thin LTO remains unchanged.

Durable run IDs and sealed bundle identities:

- Before `5872a61e8c6a4412b06144ff1147210e`, bundle SHA-256
  `fb604e87e11f870c1d4eff2e8facd2f09bd1020a16c45024dec485ec236265e4`.
  SSH stalled after successful remote completion. Recovery independently checked
  the sealed manifest, exact source/tools, caps and successful terminal launcher
  receipt; the failed transport log is preserved.
- After `6767885a6929451d817fed7060ae6285`, bundle SHA-256
  `de3e5e9964ef011f9b9fee108a16b3aee649ab7fabb159a6c3354f96263e0f0e`.

Both retain full samples, Cargo events/timings, exact archives, tool identities,
resource receipts and restored source inventories outside disposable caches.
All nine focused runtime/graph qualification steps passed on the measured
after tree, including the separate AUTH81/AUTH82 CLI and real FFmpeg HTTP.
The main promotion receipt remains separate release acceptance evidence.

## 5. Post-decoder standalone build — 2026-11-07

Intermediate source `45e4402c276b53892368c02bade9a96837092249`
(tree `fad0b461e25846c0f82dd1f62a70f520094f5e10`) includes main `ae779d141f`
and decoder corrections, before the retained-HLS repair correction.
This retains a second actual SDK-free compile measurement after §4. The same before source, unchanged runner, SDK,
Cargo command, debug level, four CPUs, 24 GiB and no-swap cap apply.
All probes use three samples; complete source restoration and changed-artifact
freshness were verified again.

| Probe | Before median (s) | Post-decoder samples (s) | Post-decoder median (s) | Reduction |
|---|---:|---|---:|---:|
| No-op | 0.215781 | 0.215759, 0.215361, 0.215624 | 0.215624 | unchanged |
| Embedded asset | 17.652122 | 15.822141, 15.696399, 15.851703 | 15.822141 | 10.4% |
| Daemon code | 18.031873 | 16.149660, 15.669881, 15.449537 | 15.669881 | 13.1% |
| Shared API | 26.948786 | 24.073589, 23.841190, 23.658487 | 23.841190 | 11.5% |

This actual daemon build contains **380 units and zero Hiqlite, Hiqlite WAL
or OpenRaft units**, versus 448 units before. No-op time is effectively unchanged;
changed-source medians improve 10–13%. These remain warm rebuild measurements.
The 107.422269-second warmup uses an already populated shared slot and establishes
no cold-build gain. The mixed target cache is not a standalone size measurement.

Run `5467c91e828947749766617ae51b7091`, sealed bundle SHA-256
`5da0cdd71f0f1a1678723d42ae96935ebd7a070fd85f2e22cffccf5ad24a8e5c`, retains
Cargo events/timings, all samples, exact source/tool identities, resource receipts
and restored inventories. The intermediate `ae41a3edb` measurement remains
retained as run `d8f1e9d2108e4a6fa5a0b46eed8e50f8`, bundle
`94f099f9a28e2be030cd5b76a50c4828214054dd85c97f5751c5bda2ec646dd9`.
No failed measurements or earlier reports were replaced.

## 6. Integrated standalone build — 2026-11-08

Measured source `83da88f89e4e51a461cf6eb22d20f3f0a7c78547` (tree `d437661e4cbe74cccf57c616f62a974b47b4010d`) includes
main `78544ee0aef7f8e40d998d1a7164e162f8e7e055`, native SQLite composition and
the current Cinema corrections. The final cluster-only test locator change
alters no production code from the eight passing native runtime cases.
The same pinned runner, SDK, Rust 1.97.1, four CPUs, 24 GiB, no swap and
full dev debug level 2 used in §4 apply. These are actual daemon builds
including linking:

```bash
cargo +1.97.1 build --locked --offline -p plurxd --message-format=json-render-diagnostics --timings -j4
```

| Probe | Before median (s) | Integrated samples (s) | Integrated median (s) | Reduction |
|---|---:|---|---:|---:|
| No-op | 0.215781 | 0.416522, 0.164371, 0.164290 | 0.164371 | unchanged/no gain claim |
| Embedded asset | 17.652122 | 15.957632, 15.758912, 16.042622 | 15.957632 | 9.6% |
| Daemon code | 18.031873 | 15.514839, 15.461090, 15.456232 | 15.461090 | 14.3% |
| Shared API | 26.948786 | 24.507512, 23.404642, 23.170679 | 23.404642 | 13.2% |

The actual selected daemon build contains **380 units and zero Hiqlite,
Hiqlite WAL or OpenRaft units**, versus 448 units before. The
shipping-component normal/build graph is checked separately; Cargo.lock and
the mixed shared target cache do not describe distribution closure or size.
All three samples per probe, source restoration and changed-artifact events
were verified. These are observed version comparisons, including intervening
correctness changes; they do not isolate the SDK removal from every other edit.
The already-warm initial build (149.535914s) is not a cold-build result.
No cold, release, macOS timing or binary-size improvement is established.

Before run `5872a61e8c6a4412b06144ff1147210e`, bundle SHA-256
`fb604e87e11f870c1d4eff2e8facd2f09bd1020a16c45024dec485ec236265e4`; integrated run
`a96fbc17b84e4fe19c69d87e1346bf7f`, bundle SHA-256
`2ef1c65ecdeb41299b561e0d2418fb3b7871072ccb7ebf21375ee422502cc78f`. Earlier reports and failures remain retained.
The source-specific runtime/migration qualification and exact final-tree
promotion receipt belong to [PR #1397](http://forge.lan:3000/noirr/plurx/pulls/1397);
this measurement alone does not qualify a release.

## 7. Final standalone build — 2026-11-08

Measured commit `96685a8069609267189f0413a92f32075e8eb0ae`, tree
`c2b86b14c6c5b74dea35170fb02be268e8f8882e`, includes actual main
`99d08f9b5ed0ecfd8992e82ac5d2ff93f59a2b5c` and its qualified reader/FEL
recovery changes. The frozen runner, seven tool hashes, SDK, Rust 1.97.1,
full dev debug level 2, four CPUs, 24 GiB and no-swap controls match the before
run in §6. This is another actual daemon compile and link, not `cargo check`:

```bash
cargo +1.97.1 build --locked --offline -p plurxd --message-format=json-render-diagnostics --timings -j4
```

| Probe | Before median (s) | Final samples (s) | Final median (s) | Reduction |
|---|---:|---|---:|---:|
| No-op | 0.215781 | 0.215427, 0.214653, 0.165313 | 0.214653 | effectively unchanged |
| Embedded asset | 17.652122 | 15.766353, 15.635129, 15.851877 | 15.766353 | 10.68% |
| Daemon code | 18.031873 | 16.168842, 15.643953, 15.955543 | 15.955543 | 11.51% |
| Shared API | 26.948786 | 24.531714, 23.746568, 23.403503 | 23.746568 | 11.88% |

The selected daemon again compiles **380 units, zero Hiqlite, Hiqlite WAL or
OpenRaft units**, versus 448 before. All no-op units were fresh; each changed
probe rebuilt its expected artifacts. Complete source restoration matches the
original inventory. The separately checked daemon/controller/tray normal/build
graph also contains no cluster SDKs.

Run `d7cb4a7b7bd64653a486f666db82bd4f`, sealed bundle SHA-256
`ab0220f88d6ea94673f40e471da8854fa1d6a409632f7b9416e4d7c10783b60e`, retains
the exact samples, Cargo events/timings, source/SDK/tool identities and successful
terminal-scope/resource proofs. Before run and bundle remain those in §6;
earlier measurements and failed runs remain retained.

These are three-run warm medians on the same Linux compiler host and observed
version comparisons, including the correctness changes. They do not isolate a
causal SDK-only gain. The 133.883112-second warmup used the existing warm target;
it establishes no cold, release, macOS or size improvement. The mixed target
cache is not a distribution-size measurement. Final source qualification and
the provider receipt remain owned by [PR #1397](http://forge.lan:3000/noirr/plurx/pulls/1397).
