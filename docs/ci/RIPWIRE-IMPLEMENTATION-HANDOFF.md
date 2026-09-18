# Ripwire implementation — a bounded navigation tool for Plurx agents

**Status:** ready to build; no integration or benchmark performed ·
**Written:** 2026-09-10 · **Executor:** GPT Sol ·
**Baseline:** `4519f87aa17def278dc4ad6c08a411e7829c2ec4` plus the inspected,
dirty working tree · **Owner:** `validation.framework`.

Read [AGENTS.md](../../AGENTS.md), then this handoff. Use
[AI-HARNESS-ASSESSMENT.md](AI-HARNESS-ASSESSMENT.md) for the earlier rationale,
[AI-HARNESS-IMPLEMENTATION-PLAN.md](AI-HARNESS-IMPLEMENTATION-PLAN.md) for the
broader harness work, and [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md)
for promotion. This document specifies the Ripwire integration you should
build, milestone by milestone. It does not implement the rest of the harness.

Paul's request to implement Ripwire is the authority for preparing and
building this scoped integration now. For this effort, it supersedes the
older harness plan's M9 scheduling prerequisite that M2 test extraction and
M5 subsystem guides must already have merged. Record whether those changes
exist in the benchmark tree; do not claim to have measured their residual
cost when they do not. The older proposal to start with MCP is also replaced
here by a CLI-first integration. All repository merge rules remain in force.

## 1. Outcome — Sol can locate code without assuming the graph is complete

Give agents a repository-owned command that installs a pinned Ripwire binary
on demand and supports compact task searches, symbol reads, caller searches,
impact estimates, and optional change summaries. Keep normal builds and
checks independent of Ripwire. Agents must remain able to work with `rg`,
bounded source reads, compiler diagnostics, and the validation catalog.

The completed integration has these observable properties:

1. A fresh clone can run explicit setup, then navigate offline without
   changing global agent configuration or adding a runtime dependency to
   Plurx. A clone without setup can still build and validate normally.
2. Every query identifies its source checkout, tool version, crawl profile,
   and coverage limitations. Empty results never mean a consumer is absent.
3. The initial agent recipe is short enough to use during ordinary work.
   Large bodies and change reports require deliberate follow-up commands.
4. A reproducible Plurx trial records useful hits, omissions, elapsed time,
   and output cost. Default agent adoption follows that result (§8).
5. The implementation leaves the main promotion gate, test sweep, mobile
   versions, product code, and release images unchanged.

## 2. The current tree — what the integration must fit

Re-verify these contracts at implementation time. The baseline is an
orientation point, not permission to overwrite subsequent work.

### 2.1 Existing interfaces and ownership

| Existing surface | Observed contract | Integration consequence |
|---|---|---|
| [AGENTS.md](../../AGENTS.md) | Main-bound draft, exactly one adversarial review, findings addressed, ready, `fast-lane`, green current-head `Main promotion gate`. | Ripwire output is advisory evidence; it cannot approve or merge work. |
| [CLAUDE.md](../../CLAUDE.md) | Delegates repository policy to AGENTS. | Put the shared navigation pointer in AGENTS; avoid duplicated policy. |
| [swarm/config.json](../../swarm/config.json) | Schema 3; Codex runs through `exec -C {clone}`; required executables currently contain `gh`; builder workers include `gpt-5.6-sol`. | A repository-relative CLI needs no new provider or model routing. |
| [swarm/builder.txt](../../swarm/builder.txt), [swarm/core.txt](../../swarm/core.txt) | Existing task reading and implementation guidance. | Add a compact optional navigation recipe after the measured trial. |
| [swarm/project.txt](../../swarm/project.txt) | Contains stale references to re-review and branch protection alongside the queue protocol. | AGENTS is authoritative. Do not copy those stale rules into this integration or rewrite queue behavior. |
| [Makefile](../../Makefile) | `validation-lint`, `operations-check`, `history-check`, `fmt-check`, `effort-rust-check`, `lint` already exist. | Add explicit Ripwire convenience targets only; no prerequisite edges into existing targets. |
| [validation/runner.py](../../validation/runner.py) | `plan --profile P --paths …`, `--changed-from REF`, and `--point ID` select catalog work without running it. | Consult the catalog independently of Ripwire's suggested tests. |
| [validation/points.toml](../../validation/points.toml) | `validation.framework` owns harness/catalog tooling; `ci.operations` owns operations tests. | Assign every new governed file in its introducing commit. |
| [.gitignore](../../.gitignore) | Root `target/`, `.swarm/`, generated Apple outputs, and common build artifacts are ignored. | Put installations, caches, captures, and benchmark scratch under `target/ripwire/`. |
| [test_docs_index.py](../../tests/operations/test_docs_index.py) | Every tracked document is indexed; local document links must resolve. | Add a row whenever a companion document is created. |

The inspected working tree contains unrelated edits and literal conflict
markers in the docs index and validation catalog. Do not carry those edits
into the implementation branch or resolve their substantive choices as a
Ripwire task. Work from a clean intended base in your own checkout and port
only this handoff and its bookkeeping if they have not landed. If the
intended base itself is conflicted, record that as an environment blocker.

### 2.2 Plurx is larger than the examples in a quickstart

Measured from the working files on 2026-09-10:

| Source | Bytes | Lines | Why include it in the trial |
|---|---:|---:|---|
| [transcode.rs](../../crates/plurxd/src/transcode.rs) | 1,829,466 | 43,951 | Large Rust implementation with inline tests. |
| [playback_control.rs](../../crates/plurxd/src/playback_control.rs) | 1,188,882 | 29,155 | Stateful control flow and caller ambiguity. |
| [http/hls.rs](../../crates/plurxd/src/http/hls.rs) | 976,065 | 24,037 | HTTP consumers and playback boundaries. |
| [http/mod.rs](../../crates/plurxd/src/http/mod.rs) | 522,447 | 13,747 | Route wiring and tests. |
| [web/index.html](../../crates/plurxd/src/web/index.html) | 1,460,359 | 19,931 | JavaScript embedded in HTML; document extraction is insufficient. |

Use a 4 MiB source ceiling initially: these measured hotspots fit. Do not
raise it speculatively or exclude tests to manufacture a smaller corpus.
Report future oversize files by name. Android Kotlin, Rust macros and trait
dispatch, Swift dynamic behavior, and JavaScript inside HTML need explicit
coverage checks before trusting a cross-client result.

## 3. Dependency — pin the release, then verify the actual binary

### 3.1 Start with Ripwire v0.5.0

The [v0.5.0 release](https://github.com/redhat-et/ripwire/releases/tag/v0.5.0)
was published on 2026-09-08. Its source tag resolved during research to
`bacfa3b7b3ad13648ce3892de06af05b6b55a2ac`.
Upstream main resolved to `c1d90d3eca94d6509adac49faa3da3b53bc8c50a`, which
is a different source snapshot. Use release behavior, not main's examples.

Create `scripts/ripwire.lock.json` with `schema_version: 1`,
`upstream: "redhat-et/ripwire"`, `version: "0.5.0"`,
`tag: "v0.5.0"`, `source_commit`, and a `platforms` object. Each platform
entry contains `asset`, `archive_sha256`, and `binary_member`. The initial
assets and archive hashes below came from GitHub release metadata; verify
them against the downloaded bytes during setup.

| Platform key | Asset | Archive SHA-256 |
|---|---|---|
| `linux-arm64` | `ripwire-0.5.0-linux-arm64.tar.gz` | `efe049b1645045e96751a51b1bc321b2d8653aa1f6ab5d7c2390eb3d49716bf0` |
| `linux-x64` | `ripwire-0.5.0-linux-x64.tar.gz` | `f06e9d7e55032e8e5c397405ed72a19d6e70767d28c8c617a925de4401779f50` |
| `macos-arm64` | `ripwire-0.5.0-macos-arm64.tar.gz` | `f9b4d638f0f0efeac602fa6fc5037eb2871f933deccffc3b932b0c9c5dc1fe88` |
| `macos-x64` | `ripwire-0.5.0-macos-x64.tar.gz` | `0bc6b8b101cdd56c509825a8690c363cfb35a28b669076df0b32e1558a3c68cd` |

Construct the download URL from that fixed repository, tag, and asset under
`https://github.com/redhat-et/ripwire/releases/download/`. Do not resolve
`latest` during installation. The expected binary member is the asset name
without `.tar.gz`, followed by `/ripwire`; verify the real archive layout
before locking it. Record the extracted binary hash in local install
metadata and compare it before execution.

**Upgrade contract:** changing the lock is a deliberate dependency change.
Repeat setup integrity checks, CLI compatibility checks, fixture coverage,
and the four task-search probes. Never update the tool during a query.
An unsupported host gets a clear unavailable result; building Ripwire from
source is a separately documented fallback, not a silent installer action.

### 3.2 Install only the executable into this clone

Implement setup with Python standard library networking, hashing, and
archive reading. Stream the download with a 120-second overall deadline and
a 256 MiB compressed-size limit; refuse unexpected HTTP failures or hashes.
Select the exact regular-file binary member without extracting the whole
archive. Reject duplicate binary members, links, non-regular members at that
path, and a binary larger than 256 MiB. Do not execute bundled scripts.

Stage the binary beside its final path, set executable permissions, verify
`--version` and required flags from `--help`, then replace atomically. Use
the same short lock for installation and queries so a query cannot race a
partial update. A failed setup preserves the previous working installation.
If the release cannot satisfy the documented CLI surface, report the mismatch
and revise the pin explicitly; do not compensate with guessed flags.

The upstream installer also deals with agent skills. This integration needs
only its executable: no changes to shell startup files, user skill folders,
agent config, hooks, system packages, or worker machines as a side effect.
The source and third-party licenses remain upstream; link their
[license](https://github.com/redhat-et/ripwire/blob/v0.5.0/LICENSE) and
[dependency inventory](https://github.com/redhat-et/ripwire/blob/v0.5.0/THIRD_PARTY.md)
from the resulting usage guide. Do not vendor the binary into Git or images.

## 4. Repository CLI — one small adapter with an explicit surface

### 4.1 Files to create and files to extend

All paths in this table are proposed unless linked to an existing file.

| Path | Responsibility |
|---|---|
| `scripts/ripwire` | Executable Python 3 entry point: setup, doctor, argument mapping, bounded subprocess execution, cache policy. Use JSON so no TOML parser dependency is needed. |
| `scripts/ripwire.lock.json` | Reviewed release pin and supported-platform digests. |
| `tests/operations/test_ripwire.py` | Adapter behavior with fake executables and local fixtures; no download, compiler, or real Ripwire requirement. |
| `tests/fixtures/ripwire/` | Small source corpus covering callers, ignored files, duplicate names, Kotlin and embedded JS boundaries. Do not use product changes as fixtures. |
| `scripts/ripwire-smoke` | Explicit opt-in checks against the installed real binary and the fixture corpus. Never called by normal CI or commit hooks. |
| `RIPWIRE.md`, beside this handoff | New live usage guide, created during implementation and indexed then. |
| `RIPWIRE-PILOT.md`, beside this handoff | New measured result, created during implementation and indexed then. |
| [Makefile](../../Makefile) | Add `ripwire-setup`, `ripwire-doctor`, and `ripwire-smoke`; each delegates to its script. |
| [validation/points.toml](../../validation/points.toml) | Add scripts, lock, fixture tree, and new docs to `validation.framework`; operations tests already match `ci.operations`. |
| [AGENTS.md](../../AGENTS.md), [swarm/builder.txt](../../swarm/builder.txt), [swarm/core.txt](../../swarm/core.txt) | After §8's result, a short navigation pointer/recipe that preserves existing authority. |
| [docs/README.md](../README.md) | Rows for the usage guide and measured report when those files exist. |

Do not build a generic plugin platform or a second indexer. Keep the adapter
small; extract a helper module only when the implementation requires it.
Assign ownership to any extracted helper in the same commit.

### 4.2 Public command contract

The commands below are **new Plurx commands to implement**. They are not
available merely because this handoff exists.

```bash
scripts/ripwire setup                          # explicit download/install
scripts/ripwire doctor                         # local availability and policy
scripts/ripwire map                            # small orientation map
scripts/ripwire find 'prepared playback handoff' # compact candidate signatures
scripts/ripwire outline 'transcode.rs:prepare_decode_restricted'
scripts/ripwire body 'transcode.rs:prepare_decode_restricted'
scripts/ripwire callers 'hiqlite.rs:validate_parameter_order'
scripts/ripwire impact 'hiqlite.rs:validate_parameter_order'
scripts/ripwire pr-context --base origin/main   # explicit comparison base
scripts/ripwire coverage                       # crawl/parse limitations
scripts/ripwire-smoke                          # opt-in real-binary verification
```

Resolve the repository root from the script location, validate it with Git,
and run Ripwire with that root as `cwd` and `.` as the crawl root. Calling
the script from a nested directory must give the same paths and answer.
The smoke helper may import the adapter's runner with a temporary fixture
root; do not expose arbitrary extra roots or raw flag forwarding in the
normal CLI.

Common query options: `--profile default|history` (default `default`),
`--cold` (disable parse caches), `--budget N` (default 3000, allowed 500–8000),
and `--timeout N` (default 60 seconds, allowed 1–120). Reject unknown options
and extra positional arguments. Setup and doctor do not accept query flags.
Reject task text exceeding 4096 UTF-8 bytes or containing NUL. Reject symbol
selectors containing NUL; pass all values as single argv elements, never
through a shell. A leading dash inside an accepted selector is data passed
after the upstream flag's `=`, not another CLI switch.

Initial mapping, to verify against the installed v0.5.0 binary:

| Plurx command | Upstream arguments in addition to common crawl/cache flags |
|---|---|
| `map` | `--max-tokens=N --top-k=40` |
| `find TASK` | `--for=TASK --format=candidates --top-k=12` |
| `outline SELECTOR` | `--outline=SELECTOR --top-k=0` |
| `body SELECTOR` | `--expand=SELECTOR --top-k=0` |
| `callers SELECTOR` | `--callers=SELECTOR` |
| `impact SELECTOR` | `--impact=SELECTOR` |
| `pr-context --base REF` | `--pr-context=RESOLVED_COMMIT --token-budget=N` |
| `coverage` | `--skipped` |

The release's [CLI source](https://github.com/redhat-et/ripwire/blob/v0.5.0/src/cli.h)
and [command reference](https://github.com/redhat-et/ripwire/blob/v0.5.0/docs/COMMANDS.md)
are the starting evidence for this mapping. In particular, `--top-k` does not
uniformly limit every verb. Candidate format is chosen to make `find`
compact. Do not append every budget flag to every verb and assume they work.

**Budget semantics:** `--budget` shapes map and PR output where upstream
supports it. Elsewhere it is a best-effort request and the adapter reports
`budget_enforced=false`. Independently cap stdout at 128 KiB and stderr at
64 KiB for every query. Buffer privately before emission; if either limit
is exceeded, terminate the process group and return an explicit refusal,
with no partial artifact presented as a complete result. Read both pipes
concurrently to avoid deadlock. Do not use unbounded `communicate()` and
check lengths only after memory is already consumed.

Validate `REF` with `git rev-parse --verify --end-of-options REF^{commit}`,
require a merge base with `HEAD`, and pass the resolved hash upstream. Print
both requested ref and resolved merge base. The first version refuses
`pr-context` on a dirty tree; it must not pretend the committed diff includes
unstaged, staged, or untracked edits. Navigation commands do allow dirty work.
There is no `--base main` default because task PRs can target an effort.

### 4.3 Result and failure meanings

Keep upstream stdout intact on successful calls. Put one machine-readable
provenance record on stderr before forwarding bounded upstream diagnostics:
`schema_version`, `tool_version`, `binary_sha256`, `head`, `dirty`,
`profile`, `mode`, `cache_mode`, `budget`, `budget_enforced`, `elapsed_ms`,
`stdout_bytes`, `upstream_exit`, and `outcome`. The record's schema is owned
by Plurx; do not parse arbitrary XML comments to invent a stable upstream API.

| Exit | Meaning | Agent action |
|---|---|---|
| `0` | Command completed, including possibly empty search results. | Read diagnostics and inspect relevant source before drawing conclusions. |
| `1` | Upstream query failed/refused. | Report stderr; try a more specific selector or use source search. |
| `2` | Invalid arguments, lock data, checksum, platform, or incompatible setup. | Correct setup/input; no auto-download or guessed replacement. |
| `3` | Output withheld because of a budget/size refusal. | Narrow the query; never call the omitted artifact complete. |
| `69` | Tool absent or locally disabled. | Continue with normal navigation; setup remains explicit. |
| `75` | Another process holds this clone's tool lock. | Fall back or retry once; do not spin. |
| `124` | Deadline exceeded; owned process group terminated and reaped. | Record timeout, use fallback, investigate only if repeated. |

Map upstream exit 3 to adapter exit 3; other upstream nonzero codes map to
1 while preserving `upstream_exit`. Doctor returns 69 for missing setup and
2 for corrupt/incompatible setup. It prints the resolved binary, lock pin,
cache root, configured limits, and activation state without invoking a
repository crawl or modifying user configuration. Setup must be idempotent.

Provide `PLURX_RIPWIRE_DISABLED=1`: query commands exit 69 immediately, doctor
reports disabled, and setup may still prepare files without activating use.
Do not provide an undocumented PATH fallback to an arbitrary Ripwire binary.

## 5. Crawl and cache — make omissions visible

### 5.1 Define two small profiles

`default` crawls the repository with its normal ignore handling and the
upstream denylist. Add explicit exclusions for checkout-local scratch and
records: `./tmp`, `./Claude outputs`, `./target`, `./.swarm`,
`./docs/archive`, `./docs/evidence`, and `./docs/apple-builds`.
These are **substring arguments**, not globs or security boundaries; verify
the spelling matches v0.5.0's emitted `./…` paths and directory pruning.
Keep current guides, source, tests, validation records, and owned clients.

`history` removes only the three documentation exclusions. This allows a
deliberate search of old decisions without ranking archived findings as
current instructions during ordinary work. Both profiles retain scratch
exclusions and the same 4 MiB source ceiling. Do not use `--no-ignore` or
`--ignore-tests` by default. Include exclusions in the provenance/config
fingerprint, and list them in doctor output.

Upstream applies its own crawl policy, documented in its
[crawl implementation](https://github.com/redhat-et/ripwire/blob/v0.5.0/src/ingest_crawl.h).
A tracked path is not a promise that Ripwire will index it. In particular,
vendored code may require direct source reads. Directory exclusions count
as unknown subtree contents, not zero missing files. An HTML document being
recognized is not evidence of JavaScript call edges.

### 5.2 Use per-clone disposable state

Keep all generated state below `target/ripwire/`:

```text
target/ripwire/
  tools/<version>/<platform>/ripwire
  tools/<version>/<platform>/install.json
  cache/<version>/<profile>/<family>.ripwirecache
  run.lock
  evidence/<run-id>/
```

Use separate cache filenames for lean navigation and rich analyses, after
checking each verb's actual cache family in the release. Cache metadata
includes the canonical repository root and adapter/profile fingerprint.
Do not share cache files between worker clones or architectures. A regular
query does not store its prompt, source output, or a transcript; saved
evidence is an explicit smoke/pilot action.

Serialize setup/query access with an OS advisory lock (`fcntl` on the four
supported platforms), waiting at most 2 seconds. The kernel releases the
lock if a process dies. Test concurrent use rather than assuming cache writes
are atomic. `--cold` uses `--no-cache`; normal navigation uses explicit cache
paths. Changing the version/profile starts fresh cache state. A corrupt
cache can be discarded and rebuilt once, with the recovery recorded.

Freshness must be demonstrated for edits, deletion, rename, and branch
switches. Compare the identities/locations returned by warm and cold runs
after each operation in a scratch repository. The commit hash alone is
insufficient because agents navigate uncommitted edits. Local indexes stay
local; any snippets an agent reads still enter that agent's existing model
context. Do not describe the whole agent workflow as offline inference.

## 6. Precision — ship the honest fallback before optional SCIP

### 6.1 Language and consumer coverage are part of the result

The pilot report must include this matrix with observed results:

| Surface | Probe | Fallback that remains required |
|---|---|---|
| Rust direct calls | A known definition, local caller, and cross-module caller. | `rg`, direct source inspection, compiler loop. |
| Rust traits/macros/closures | Known call sites involving each mechanism, with source locations recorded. | rust-analyzer references where available; inspect macro/trait wiring. |
| Swift | Definition and consumer from Apple source, including a callback/delegate case. | Xcode/source navigation; inspect routing and closures. |
| Standalone JS | A known exported/declared function and real consumer. | `rg` plus the relevant JS source. |
| HTML-embedded JS | A function and actual caller inside Plurx's web application. | `rg -n` over the original HTML and bounded reads. |
| Kotlin | A definition and consumer from the Android application. | Kotlin/IDE navigation and `rg`; treat Ripwire coverage as unsupported unless proved. |
| Python/shell/config | A validation entry point, script invocation, and catalog command. | Read the actual command mapping and invocation. |
| Documentation | Current guide versus archived plan for the same topic. | Status header, docs index, and source contract. |

A fixture pass cannot establish Plurx coverage on its own. Record both
fixture behavior and real source probes. Do not preprocess HTML into fake
`.js` files for v1: invented paths and shifted lines would make callers look
more reliable than they are. Do not implement a Kotlin parser here.

### 6.2 SCIP is a measured follow-up, not an installation dependency

Start with the default graph. The existing harness M9 comparison remains
useful, but a SCIP index is an additional source of evidence, not universal
ground truth. It can omit configurations, targets, expansion details, or
languages. Compare caller edges with caller edges, and reference locations
with reference locations; a symbol reference is not automatically a call.

If rust-analyzer is available, inspect `rust-analyzer scip --help` for the
installed version before generating an index. Record the Rust toolchain,
indexer version, workspace root, targets/features, exact source commit,
Cargo.lock hash, and index hash. Use the pinned compiler process in
[AGENT-COMPILE-LOOP.md](AGENT-COMPILE-LOOP.md) if compilation is needed; no
credential or `.git` transfer to the source-only compiler container.

Upstream v0.5.0's
[SCIP overlay](https://github.com/redhat-et/ripwire/blob/v0.5.0/src/scipoverlay.h)
has diagnostic staleness signals; they are not a complete freshness check.
Before optionally exposing a `--scip` wrapper option, require matching
manifest fields and a clean checkout at the indexed commit. Refuse missing,
corrupt, mismatched, or dirty-tree use as precision evidence. Record actual
applied edges and degradation diagnostics; exit 0 alone is insufficient.
Regenerate after source changes. Do not label heuristic edges precise.

SCIP support is not required for v1 completion. Report it as unavailable or
deferred if its environment is absent; the real-source probes and manual
consumer sets in §8 remain required.

## 7. Agent adoption — a short recipe that preserves the existing workflow

After the measured trial supports default guidance, add a short AGENTS
section linking the new usage guide. Update builder/core prompts with a
pointer to the same section. Do not paste upstream's full skills catalog
into worker prompts. The recipe should convey these steps:

1. Read AGENTS, the task contract, and the appropriate current docs first.
2. Use `rg` for a known literal/path. For a broad navigation question, call
   `scripts/ripwire find 'task in repository terms'` once.
3. Select a path-qualified symbol; read its outline or body, then use callers
   or impact when changing an interface. Verify source and non-indexed
   consumers, especially Android, embedded JS, runtime routes, and Store
   backends. Do not repeatedly ask a broad query that already missed.
4. Consult `scripts/validate plan --profile commit --paths …` separately.
   It is a catalog plan, not an instruction to run every historical suite
   before merging. Follow current AGENTS for compilation and evidence.
5. Optionally attach a bounded PR context report against the real PR base.
   A report is navigation evidence for the one review, not another review.
6. On absence, timeout, ambiguity, or weak coverage, continue with ordinary
   navigation and say which evidence is missing. No automatic installation.

Keep the root addition to roughly 15 lines and role additions to roughly
5 lines each. Measure the full prompt addition's bytes in the pilot, because
per-session guidance has a cost even when no query is made.

No change is needed to `swarm/config.json` for this CLI integration. Do not
make Ripwire a required executable or replace `project.quality_command`.
Do not repair the broader prompt/queue drift in the same change; preserve
AGENTS precedence explicitly and record any remaining conflict in handoff
notes for the existing harness M6 work.

**Optional later MCP:** the CLI integration is complete without MCP. If a
specific agent workflow later needs it, validate that launcher's actual
configuration schema and project scope first. Use stdio, the pinned binary,
and the correct clone root. Do not expose a listening socket or register
edit tools as a side effect. Raw upstream MCP exposes a broader surface than
this wrapper; do not call it read-only unless the client actually restricts
tools. Benchmark MCP's schema/context cost separately from the CLI.

## 8. Evidence — measure Plurx before changing the default agent recipe

### 8.1 Use a fixed corpus and keep the comparison fair

Run at a clean, pinned Plurx commit with the release pin recorded. The
implementation can live on an effort branch; do not benchmark a moving
working tree. Use the same model/version, reasoning setting, task wording,
source commit, and compiler environment for both arms. Start each arm in a
fresh session without the other arm's transcript. Alternate which arm runs
first. The baseline uses `rg`, source reads, and existing tooling; the
candidate adds the wrapper. Give both arms the same precise-navigation
tools if available. Do not run tools on another worker's active checkout.

Use four bounded tasks. Define the expected files and known consumers
before running either arm, and keep those answers out of their prompts:

| Task | Required evidence |
|---|---|
| Trace SQL placeholder validation | Locate `validate_parameter_order`, its execution wrapper, regression coverage, and the separate SQLite/Hiqlite responsibilities. |
| Trace a playback control action across clients | Start from one real action and find server, web, Apple, Android, and transport/serialization consumers. |
| Investigate decode fallback | Locate `prepare_decode_restricted`, its callers and tests, and the documented constraints on recovery. |
| Plan one recorded corrective change | Select a real historical bug, locate implementation/test/ownership paths, and propose focused proof including `prove-fix` where applicable. |

For each, first run a navigation-only task: return source locations, relevant
contracts, consumer omissions, and proposed validation. Add at least one
implementation exercise replaying a known small historical correction in
scratch clones at its pre-fix base, with the later correction hidden from
both sessions. Verify the behavior using its retained regression evidence.
No product change from these scratch exercises is promoted. Distinguish
navigation accuracy from actual implementation correctness in the report.

Run two paired repetitions per navigation task: 16 sessions in total.
This is a bounded trial, not a claim of statistical significance. Count
incorrect/missing consumers, not merely whether the answer sounds plausible.
Record actual input/output token usage if available; otherwise retain
stdout bytes and mark tokenizer/cost values unavailable. Never substitute
Ripwire's estimated tokens for the model's measured bill.

### 8.2 Metrics and adoption decision

The result document must carry:

- Exact Plurx commit, Ripwire tag/hash, host OS/architecture, model/settings,
  whether harness M2/M5 exist, and all effective profile/limit settings.
- Three fresh-cache and five warm-cache samples for each of `find`,
  `callers`, and `coverage`; wall time and output bytes per invocation.
  Report peak RSS where the host can measure it and name the instrument.
- The §6 coverage matrix, missing/ambiguous probes, and source-backed
  expected consumer sets. For precision/recall, report numerator and
  denominator and distinguish unavailable denominators from zero.
- Per-task correct completion, missed consumers, manual intervention,
  navigation commands/file reads, elapsed time, tokens/cost when available,
  wrapper refusals, and fallback behavior. Keep failed trials in the table.
- Warm/cold freshness results for edit, rename, deletion, branch change,
  and a corrupt cache; fixture and real-source findings distinguished.
- One verdict: `adopt`, `opt-in only`, or `defer`, with concrete reasons.

Adopt the compact default agent recipe only if correctness is no worse on
all four tasks, at least three tasks improve median navigation reads or
measured context usage by at least 20%, and the correction exercise is not
worse. The 20% threshold is a proposed Plurx adoption rule, not an upstream
benchmark claim. Missing billing data does not block a read-count comparison.
An omitted consumer in the candidate arm is a correctness failure even if
token use improved.

For usability, target warm-query median at most 2 seconds and every measured
query under the configured 60-second deadline. Report misses rather than
raising timeouts to hide them. A slow but useful result can remain opt-in.
If correctness worsens, fix the bounded adapter/guidance defect and repeat
the affected trials, or choose `defer`. Do not keep rerunning unchanged
trials until a favorable result appears. Keep the original observations.

`opt-in only` means the installed commands and usage guide ship, with no
default worker prompt addition. `defer` means keep the experiment/report on
the effort and do not promote activation; record the blocker. Either outcome
is a valid measured result, but neither may be reported as default adoption.

## 9. Build milestones — ordered tasks with explicit acceptance

Use one temporary `effort/ripwire` integration branch for this work. Create
reviewable task branches from its current head into that effort. They need
no adversarial review and no fast lane. A single executor can do these
sequentially. Freeze task merges before the final main-bound PR.

### 9.1 M0 — confirm base, release, and scope

**Build:** identify the clean intended base, inspect current AGENTS and
catalog ownership, inspect the pinned release archive, and record required
flags/version. Create the lock and file plan. Preserve all unrelated work.

**Acceptance:** archive digest, member path, binary version, and supported
flags agree. The implementation branch has no conflict markers or unrelated
product changes. New paths have an identified catalog owner. No Rust edit
is needed; if one becomes necessary, establish the pinned compiler loop
before editing and explain the scope change.

### 9.2 M1 — deliver setup, doctor, and safe process execution

**Build:** installer, root resolution, lock handling, platform mapping,
doctor, disabled mode, provenance, deadlines, and bounded output. Add tests
using fake tools/archives before depending on real repository parsing.

**Acceptance:** focused adapter tests pass without network or Ripwire.
Explicit setup and doctor work on the implementation host. Failed checksum,
unexpected archive member, unsupported platform, timeout, and concurrent
setup all leave source and any previous installation intact.

### 9.3 M2 — deliver navigation commands and coverage smoke

**Build:** §4 mappings, profiles, lean/rich cache isolation, real-binary
fixtures, and coverage probes. Add explicit Make targets. Keep all caches
and captures ignored. Keep query behavior independent of PATH.

**Acceptance:** all documented commands execute with the pinned release;
ambiguous names require investigation and path-qualified reads find the
intended fixture. Warm and cold source identities agree after mutation.
Kotlin and embedded JS limitations appear in the report even when unsupported.
`git status --short` shows no generated artifact outside ignored scratch.

### 9.4 M3 — run the fixed trial and write the usage guide

**Build:** execute §8, create/index the two companion documents, and record
the verdict. Write the guide from commands that actually ran, with output
interpretation and fallbacks. Add the small agent pointers only for `adopt`.

**Acceptance:** the report has raw evidence paths, paired tables, coverage,
freshness, and an honest verdict; the guide accurately describes the shipped
CLI. The correction exercise has actual evidence, or adoption stays pending.
No full test suite or default Ripwire dependency has been added to CI.

### 9.5 M4 — integrate, verify bookkeeping, then promote

**Build:** merge current main into the effort after task merges freeze.
Verify the resulting exact source snapshot. Update affected docs/ownership
with the change, including status here; do not create later paperwork-only
repair commits. Navigation scripts alone require no mobile version changes.
Corrective commits need regression evidence under the existing rule unless
their changed tests directly provide it.

**Acceptance:** local applicable static contracts pass. Open the main-bound
PR as a draft, request exactly one adversarial agent review, address every
finding and verify it yourself, then mark ready and apply `fast-lane`.
Merge only after the current head's `Main promotion gate` is green. No
second review or follow-up approval is part of this workflow.

## 10. Verification — prove the adapter, then observe the real tool

### 10.1 Focused automated contracts

Use temporary repositories, local archive fixtures, and fake executables.
The operation tests must not contact GitHub, install tools, index Plurx,
require CMake/Cargo, or invoke an agent. Cover externally observable failure
and correctness properties rather than mirroring helper implementation:

| Case | Required assertion |
|---|---|
| Query text with quotes, spaces, `$()`, and dashes | Reaches one argv element; no shell command executes. |
| Missing/disabled binary | Exit 69 and actionable fallback; no network attempt. |
| Checksum/member/version mismatch | Refusal; previous executable remains usable. |
| Nested invocation | Same canonical root, flags, paths, and cache scope. |
| Different profiles/clones | Correct exclusions and isolated cache metadata. |
| Timeout or oversized stdout/stderr | Entire owned process group reaped; no partial successful artifact. |
| Concurrent invocation | Bounded lock contention; cache/install files remain valid. |
| Invalid base or unrelated history | Explicit refusal; no fabricated PR report. |
| Dirty PR context | Refuses staged, unstaged, and untracked changes. |
| Upstream failures/empty success | Distinct preserved meanings; no success fabricated from stderr. |

Run the real-binary smoke explicitly for the release contract, ignore
behavior, known source identities, and freshness. Keep tests tolerant of
irrelevant ranking differences; assert the expected consumer can actually
be found and record any unsupported probe, rather than snapshotting a whole
heuristic map.

### 10.2 Commands at implementation completion

```bash
python3 -m unittest discover -s tests/operations -p 'test_ripwire.py'
# Adapter contracts; must work without the binary or network.

make ripwire-doctor
make ripwire-smoke
# Explicit availability and real-release integration evidence.

python3 -m unittest discover -s tests/operations -p 'test_docs_index.py'
make validation-lint
make operations-check
make history-check
# Existing catalog, documentation, operations, and correction bookkeeping.

git diff --check
git status --short
# No whitespace errors, conflict residue, or generated artifacts staged.
```

These are focused implementation checks and existing static contracts.
Do not add them as commit hooks. The full Rust/device/playback suite stays
in the separate sweep. If a check is blocked by an existing unrelated base
defect, record the exact command and failure and reconcile it through the
owning work; do not silently claim a pass or weaken the check.

Verify installation and smoke on macOS arm64 and Linux x64 when available;
test platform-selection logic for all four assets without downloading them
in unit tests. Mark unexercised release platforms clearly in the guide.
Never claim a working remote worker installation from a local unit test.

## 11. Non-goals and rollback — keep the integration easy to remove

- No Rust library dependency, database schema, media pipeline, client UI,
  runtime server, release image, or mobile build change; this is development
  navigation tooling.
- No new mandatory gate, test selection authority, correctness verdict,
  dead-code deletion, architectural fence, or automated refactoring from
  Ripwire's graph; those require stronger evidence and separate scope.
- No global skill installation, MCP registration, listening socket,
  background watcher, telemetry service, or automatic dependency updates.
- No committed symbol maps, caches, source excerpts, private transcripts,
  or generated SCIP indexes. The report contains reproducible summaries,
  not a second copy of the repository.
- No harness-wide test extraction, subsystem refactor, queue rewrite, or
  worker model changes; the existing harness plan owns that work.

To stop using it immediately, set `PLURX_RIPWIRE_DISABLED=1` and use normal
source navigation. To remove the integration, revert the adapter, lock,
explicit Make targets, tests, and small agent pointers in a normal change;
remove their catalog entries in that change and update the report status.
Local state can be discarded by deleting only this clone's
`target/ripwire/` after its processes stop. No deployment rollback,
migration, or client release is involved.

## 12. Prompt to hand to GPT Sol

```text
Implement docs/ci/RIPWIRE-IMPLEMENTATION-HANDOFF.md in Plurx, milestone by
milestone. Read AGENTS.md first. This is the repository's development-tool
integration, not a product dependency or a rewrite of the AI harness.

Use a clean intended base and your own checkout. Preserve unrelated work.
Start with the pinned release and verify its actual binary/CLI contract.
Build the explicit installer, bounded CLI wrapper, coverage/freshness
checks, focused tests, usage guide, and measured Plurx trial. Do not
invent benchmark results, claim unsupported consumers are absent, or
silently fall back from failed precision evidence to a correctness claim.

Use effort/ripwire for integration and task PRs back into that effort.
Only add default agent guidance if the documented trial supports adoption.
Keep Ripwire optional, source edits outside this feature unchanged, and
all normal build/validation paths independent of its installation.

Complete docs/index/catalog bookkeeping in each introducing commit.
For final main promotion, merge current main, open a draft, request exactly
one adversarial agent review, address and verify findings yourself, mark
ready, apply fast-lane, and merge only with the current head's green Main
promotion gate. Follow AGENTS over stale swarm prompt instructions.

Finish with the implemented paths, exact checks and trial results, rollout
verdict, platform/coverage gaps, PR/head state, and any genuine blockers.
```
