# Ripwire — bounded navigation in one agent checkout

**Status:** built on effort; promotion deferred · **Owner:** validation.framework

Companion to [the measured pilot](RIPWIRE-PILOT.md),
[the implementation status](RIPWIRE-STATUS.md), and
[the development pipeline](../DEVELOPMENT_PIPELINE.md). Ripwire locates
candidate symbols and heuristic callers. Read source and the validation
catalog before deciding what a change affects. Empty results cannot prove
that a consumer is absent.

## Install explicitly, then navigate offline

Python 3 and Git are required. Setup supports Linux and macOS on ARM64 and
x86-64. Each clone owns its executable and disposable cache. No server,
client, image, normal build target, or worker configuration depends on it.

```bash
scripts/ripwire setup                 # fetch and verify the pinned release
scripts/ripwire doctor                # availability, pin, limits, exclusions
scripts/ripwire find 'playback handoff' # twelve candidate signatures
scripts/ripwire outline 'transcode.rs:prepare_decode_restricted'
scripts/ripwire body 'transcode.rs:prepare_decode_restricted'
scripts/ripwire callers 'hiqlite.rs:validate_parameter_order'
scripts/ripwire impact 'hiqlite.rs:validate_parameter_order'
scripts/ripwire map                   # compact repository orientation
scripts/ripwire coverage              # omissions and degraded parses
scripts/ripwire pr-context --base origin/main # use the actual PR base
scripts/ripwire-smoke                 # opt-in fixture and freshness checks
```

Invoke the script by path from any directory; its location selects the
checkout. Setup does not register a skill, modify PATH, configure an MCP
server, or run upstream installation scripts. Queries never download.
To stop queries locally, set `PLURX_RIPWIRE_DISABLED=1`. Setup can still
prepare the executable; doctor reports the disabled state. This is an
agent development tool, with no product Settings integration.

The reviewed [lock](../../scripts/ripwire.lock.json) pins v0.5.0, its source
commit, four release assets, archive digests, and exact executable members.
The installer verifies downloaded bytes, the executable version, and all
required CLI flags. It checks the locally recorded executable hash before
every query. Updates require an explicit lock change and repeated
compatibility, fixture, coverage, and task-search probes.

An unsupported platform is unavailable. An operator may build the pinned
upstream source manually following its build instructions for separate
experimentation, but the wrapper has no PATH or source-build fallback.
The wrapper accepts only its verified release installation.

## Interpret output and refusals

Successful upstream stdout passes through unchanged. The first stderr line
is a JSON provenance record: checkout root and HEAD, dirty state, release
and binary hashes, profile, cache family and configuration fingerprint,
requested budget, enforcement, elapsed milliseconds, output bytes, upstream
exit, outcome, and coverage limitations. Remaining stderr is bounded
upstream diagnostics. Cache recovery diagnostics are preserved verbatim.
Absent values on preflight failures are `null`, not invented measurements.

| Exit | Meaning | Next action |
|---|---|---|
| 0 | Completed, possibly with no useful matches | Inspect source and limitations. |
| 1 | Upstream query refused or failed | Narrow the selector or use source search. |
| 2 | Invalid arguments, pin, platform, base, or installation | Correct the input; setup remains explicit. |
| 3 | Upstream budget refusal or output exceeded a byte limit | Entire stdout is withheld; narrow the request. |
| 69 | Missing or locally disabled | Use `rg` and bounded source reads. |
| 75 | Another setup/query holds this clone's lock | Fall back or retry once. |
| 124 | Deadline exceeded; owned process group terminated | Record the timeout and use source search. |

Query options are `--profile default|history`, `--cold`, `--budget 500–8000`
(default 3000), and `--timeout 1–120` seconds (default 60). Setup and doctor
accept none of these options. Map and PR context shape their output using
the upstream budget. Other verbs report `budget_enforced=false`; their
budget is best effort. PR context can exceed its shaping request at the
upstream structural floor, disclosed in its output. Every query has independent limits of 128 KiB stdout
and 64 KiB stderr. The adapter buffers both pipes concurrently and withholds
all stdout on failure, timeout, or overflow. Setup's help check allows
256 KiB because the pinned release's help is 180,172 bytes.

A task may contain at most 4096 UTF-8 bytes. NUL is refused. Use `--` before
a selector starting with a dash, for example `scripts/ripwire body -- -name`.
Values are passed as single arguments; shell-like text is never executed.
The selector itself retains upstream's grammar, including ambiguity and
comma-separated selectors. Prefer path-qualified names.

PR context requires an explicit existing commit/ref with a merge base with
HEAD. Provenance records the requested ref, resolved commit, and merge base.
It refuses staged, unstaged, and untracked changes because the report covers
committed changes only. Ordinary navigation permits a dirty checkout.

## Coverage and cache boundaries

Both profiles retain normal Git ignore handling, upstream's denylist, and a
4 MiB source ceiling. Neither excludes tests. The default excludes scratch
subtrees `./tmp`, `./Claude outputs`, `./target`, and `./.swarm`, plus
`./docs/archive`, `./docs/evidence`, and `./docs/apple-builds`. History removes
only those three documentation exclusions. These are substring filters,
not glob patterns or security boundaries. Explicit subtree exclusions are
reported as counts upstream; their contents are unknown. The wrapper
also records the configured exclusion names. Upstream separately caps JSON
at 256 KiB and YAML at 512 KiB.

| Surface | Required fallback |
|---|---|
| Rust direct, trait, macro, and closure calls | `rg`, source wiring, pinned compiler diagnostics; reference tools where available. |
| Swift callbacks/delegates | Source/Xcode navigation and callback wiring. |
| Kotlin | Source/IDE navigation; v0.5.0 reports the fixture as unsupported. |
| HTML-embedded JavaScript | Search and read the original HTML; HTML recognition is not a JavaScript graph. |
| Shell/config/runtime routes | Inspect invocation and serialization mappings. |
| Documentation | Read status headers and the docs index; archived prose is not current authority. |

No SCIP option is exposed in v1. It needs a separate index manifest and
freshness contract. Heuristic call counts are floors, not complete totals.

All generated state is under this clone's `target/ripwire/`. Installations
use `tools/<version>/<platform>/` as an atomic symlink to an adjacent private
generation containing the executable and metadata. Failed setup preserves
the old generation. Successful replacement removes its predecessor.
Cache paths are `cache/<version>/<profile>/{lean,rich}.ripwirecache`.
Find uses rich; other exposed verbs use lean. Metadata binds caches to the
canonical root, architecture, adapter bytes, version, profile, and exclusions.
A changed configuration discards old caches; `--cold` passes `--no-cache`.
The release rebuilds corrupt caches from source and reports recovery on
stderr. Setup and queries use one kernel advisory lock with a two-second
contention limit. Regular queries retain no prompts or source transcripts.

The smoke command explicitly saves captures under `evidence/smoke-*/` and
removes its temporary fixture repository. Local navigation is offline after
setup; source read by an agent still enters that agent's existing model
context. Delete only this clone's `target/ripwire/` after its queries finish
to remove all installed and generated state.

## Navigation is advisory

Use `rg` for a known literal. Try one broad task query, inspect selected
source, then verify consumers the graph cannot see. Separately consult
`scripts/validate plan --profile commit --paths …`; Ripwire's suggested
coverage does not choose tests or approve a change. The normal merge rules
remain authoritative. Default worker prompts stay unchanged until measured
adoption criteria are met.

Ripwire's [license](https://github.com/redhat-et/ripwire/blob/v0.5.0/LICENSE)
and [third-party inventory](https://github.com/redhat-et/ripwire/blob/v0.5.0/THIRD_PARTY.md)
remain upstream. No executable, symbol map, cache, source excerpt, or
SCIP index is committed to Git or included in release images.
