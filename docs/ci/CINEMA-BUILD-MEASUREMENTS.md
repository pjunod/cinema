# Cinema build measurements — reproducible developer comparisons

**Status:** open — diagnostic baseline collected; guarded runner reviewed and tested; capped recipe measurements pending.
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
retains the probe. `--recover` uses the same arguments and refuses live or
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
Further compilation uses the repository's voter-build limits (8 GiB, no
additional swap, four CPUs). Both timing variants must be rerun under those
same limits before recipe acceptance. The original results remain retained.
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
CLI output short. Its 21 focused tests pass. These later checks do not alter
the frozen timing loop; actual baseline restoration events were independently
postvalidated. The developer-recipe task records its own after measurements
and acceptance, including every outlier. No disk-size or cold-build gain is
claimed by this baseline.
