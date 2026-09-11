# Ripwire pilot — measured speed, graph omissions, and opt-in verdict

**Status:** trial complete with measurement limits · **Verdict:** opt-in only ·
**Measured:** 2026-09-10 · **Owner:** validation.framework

Companion to [the usage guide](RIPWIRE.md) and
[implementation status](RIPWIRE-STATUS.md). The adapter is implemented on
`effort/ripwire`. All sixteen navigation sessions and both blind correction exercises have
returned. The results support explicit use with source fallback, without
default worker prompt additions.
Recovered account-limit interruptions remain recorded. The independent
adversarial review has not been requested; navigation is not that review.

## Fixed source and environment

| Field | Measured value |
|---|---|
| Plurx source | `da51ffb7d9fefe518f5746b63e0c5eedf3a7f7ff` (adapter integrated into effort) |
| Original main | `4519f87aa17def278dc4ad6c08a411e7829c2ec4` |
| Ripwire | v0.5.0, upstream source `bacfa3b7b3ad13648ce3892de06af05b6b55a2ac` |
| macOS ARM archive SHA-256 | `f9b4d638f0f0efeac602fa6fc5037eb2871f933deccffc3b932b0c9c5dc1fe88` |
| Executable SHA-256 | `ec57f8c73c78219914c9bdcf75fd755695612f425611989dd65476fc2e0cbab7` |
| Release version output | `ripwire 0.5.0 (Release, AppleClang 15.0.0.15000309, built_from=bacfa3b7b)` |
| Host | macOS 26.6.2, ARM64; Python 3.14.7 |
| Agents | Fresh sessions inheriting this task's model/reasoning settings; exact model identifier/settings were not returned by the collaboration API and are unverified. |
| Compiler/indexer | No Rust compilation or SCIP generation needed for the adapter; no precise index supplied to either arm. |
| Harness M2/M5 | Not implemented in the benchmark tree: hotspot tests remain inline; catalog has no subsystem `guide` field. No residual-cost claim is possible. |
| Profile | `default`; normal Git ignores and upstream denylist; seven exclusions listed in the usage guide |
| Limits | 4 MiB source; 3000 requested tokens; 60 s deadline; 128 KiB stdout; 64 KiB stderr |
| Cache | Clone-local, explicit rich cache for find; lean for other exposed verbs |

All task clones were detached at the fixed commit. No session received
another arm's transcript or expected answers. The coordinator froze expected
contracts before dispatch. Navigation agents were limited to five minutes
and sixteen shell navigation calls, with no source edits, tests, network,
installation, or further agents. They used `rg`, bounded reads, Git and
catalog plans; candidate sessions additionally used the installed wrapper.
Exact model metadata was unavailable and several full-session clocks were
interrupted. These measurements cannot satisfy a strict model/settings-
controlled adoption claim. No unavailable values are inferred.

## Query cost — all 24 completed samples

Each fresh sample began after deleting only the measured clone's cache
folder. The five warm samples immediately followed the third fresh sample.
Commands were `find 'SQL placeholder validation'`,
`callers 'hiqlite.rs:validate_parameter_order'`, and `coverage`.
Wall time includes adapter startup, Git checks, integrity hashing, and the
query. RSS is `/usr/bin/time -l` maximum resident set size, converted from
macOS bytes to MiB; it is not an estimate of total model memory or billing.

| Verb | Cache | Elapsed ms, in order | Median ms | Stdout bytes each | Peak RSS MiB, in order |
|---|---|---|---|---|---|
| find | fresh | 1386, 1287, 1428 | 1386 | 3472 | 644.0, 580.5, 652.1 |
| find | warm | 492, 485, 494, 496, 515 | 494 | 3472 | 301.2, 299.7, 301.2, 301.2, 301.2 |
| callers | fresh | 952, 947, 956 | 952 | 3467 | 454.4, 487.9, 512.0 |
| callers | warm | 324, 324, 328, 325, 328 | 325 | 3467 | 223.7, 224.4, 227.4, 220.0, 219.7 |
| coverage | fresh | 946, 890, 945 | 945 | 32142 | 500.3, 526.2, 457.0 |
| coverage | warm | 318, 317, 312, 327, 319 | 318 | 32142 | 228.2, 231.1, 221.9, 223.6, 226.1 |

All 24 wrapper and instrument exits were zero; warm medians meet the two
second target and all samples meet the configured deadline. This is one
host and corpus, not a cross-platform performance claim.

**Retained measurement failure:** the first 24 instrumentation attempts
successfully ran the queries, but sandboxed `/usr/bin/time -l` could not read
`kern.clockrate` and exited 1 without RSS. These are retained separately as
instrument failures. The table above is the subsequent run with resource
counter access, not a silent replacement of failed queries. An exploratory
query also returned 75 when started during the fixture smoke's lock; it was
retried once after smoke completed.

The cost sample's short wording did not locate the SQL validator: it ranked
an Apple `placeholder` variable, a `SQL` constant, and validation prose.
The longer navigation task used by candidate agents did locate the validator.
Latency is not answer quality. Large upstream explanatory comments account
for much of a caller result's output size; stdout remains intact.

## Real-source coverage — source is the reference

The frozen default coverage run reported 1401 indexed files, zero oversize
files, 202 unsupported-extension files, four explicitly excluded subtrees,
four upstream-pruned subtrees, 21 degraded parses, three minification
suspects, and ten indexed-but-unmeasured files. There were no source-ceiling
omissions to name. A subtree count does not enumerate its missing contents.
Language file counts are symbol-bearing floors, not complete file totals.

| Surface | Source-backed probe and observation | Required fallback |
|---|---|---|
| Rust direct | `hiqlite.rs:3992` validator has one known direct caller, `validate_sql` at 3981; graph returned 1/1. | Read validation and binding semantics. |
| Rust cross-module | `hiqlite.rs:validate_sql` returned 121 caller symbols, paged to 40. The page includes `hiqlite_catalog.rs:178` `install_schema` as well as local TimedClient accessors. Complete denominator was not established. | Inspect the Store slices and catalog; do not equate a page to all callers. |
| Rust decode | `transcode.rs:2391` definition returned four caller symbols, including production `start_with_audio_offset` at 18747 and three tests. Literal search finds seven call sites; symbols and sites are different denominators. | Inspect calls around 19057 and tests around 35273–35671. |
| Rust traits | `playback_control.rs:stage_preparation_for_owner` resolves four definitions and returns six caller symbols, including `activate_reserved` at 5406. The trait call at 5412 is visible, but dispatch identity remains ambiguous. | Read trait, implementations and `.control` wiring. |
| Rust macros | `hiqlite.rs:4487` `dump_row` returned zero callers despite twelve top-level invocations beginning at 4500. These are macro invocation sites, not twelve ordinary function callers. | Inspect macro expansion and invocation sites. |
| Rust closures | Fixture closure consumer is returned; real SQL sessions located a renewal closure in `state.rs` through source reads. A complete real closure-edge set was not measured. | Direct reads/reference tooling. |
| Swift direct | Snapshot mapper `demand` at 138 returned its `snapshot` consumer at 93 (1/1 selected consumer). | Read mapper and transport. |
| Swift callbacks | Reporter closure field `capture` at 726, invoked at 1188: selector resolves three definitions and returns zero callers. | Read closure injection and `currentCapture` wiring. |
| Standalone JS | `playback-control.js:208` `makeRequest` returned `drain` at 356 (1/1 selected consumer). | Inspect transport and serialization. |
| HTML JS | `index.html:7144` `playbackControlSnapshot` exists and is called in the player; selector was refused (exit 1, no stdout). | `rg -n` and reads of the original HTML. |
| Kotlin | Android snapshot mapper `demand` at 86 exists and maps pause to `HOLD`; selector was refused (exit 1). | Kotlin/source navigation. |
| Python / shell / config | `validation/runner.py:133` `load_catalog` returned 20 callers including runner main at 1639. A broad prove-fix query missed `scripts/prove-fix` and ranked filesystem symbols. | Read extensionless scripts and actual catalog command mappings. |
| Documentation | Both profiles locate `docs/performance/PERF-REVIEW-ASSESSMENT.md`. A query naming its archived duplicate returns no archive row under default and ranks `docs/archive/PERF-REVIEW-RESPONSE-ASSESSMENT.md` first under history. | Read status headers; directory placement is not proof that prose remains authoritative. |

Global graph counters in the fixed source were 8250 ambiguous and 18525
unresolved edges. These gauges are upstream heuristics, not a complete error
rate or a correctness verdict. The precision/recall denominators above are
explicit selected consumers only. No repository-wide precision or recall
is claimed. SCIP remains deferred.

Map, body, outline, impact, and PR context also completed on the frozen
source. An explicit PR context against the original main used 7571 stdout
bytes and reported an upstream estimated 3028 tokens for a requested 3000,
with `budget-floor-exceeded`. Its structural floor can exceed the requested
shaping budget; the adapter's independent byte cap remains hard.

## Fixtures and freshness — parser checks are distinct from Plurx evidence

[The explicit smoke helper](../../scripts/ripwire-smoke) copies
[the fixture corpus](../../tests/fixtures/ripwire/) into a disposable Git
repository under ignored state. It checks every exposed verb and saves its
captures. The release rejected Kotlin and HTML-JS selectors, found standalone
JS and Python direct callers, found a Swift direct caller but omitted the
callback, and omitted the Rust qualified cross-module caller. A duplicate
Rust name resolves two definitions; qualify the path and still inspect source.

The original two smoke runs passed twelve comparisons but the single
adversarial review found that map extraction retained only file paths and
omitted nested symbol names. Their 9/8-row lean results establish file-list
agreement only. The corrected run `smoke-20260910-204536` associates each
symbol with its parent path and asserts the actual edit, rename, deletion,
and restoration. It passes all twelve comparisons below; 32 rows represent
9 files plus 23 symbols, and deletion leaves 8 files plus 16 symbols.

| Mutation | Rich find warm versus cold | Lean map warm versus cold |
|---|---|---|
| Initial source | equal, 12 rows | equal, 32 file/symbol rows |
| Definition edit | equal, 12 rows | equal, 32 file/symbol rows |
| Rename | equal, 12 rows | equal, 32 file/symbol rows |
| Deletion | equal, 12 rows | equal, 24 file/symbol rows |
| Branch switch | equal, 12 rows | equal, 32 file/symbol rows |
| Corrupt cache | equal, 12 rows | equal, 32 file/symbol rows |

Corrupt caches were overwritten with `corrupt-cache`; upstream reported
`truncated` and source rebuild. A warm/cold agreement does not prove parsing
correctness; the separately observed graph omissions remain.

Twenty-one focused adapter tests pass without network or a real executable.
They cover argv data, nested invocation, profiles, cache families, missing
and disabled tools, corrupt installation, invalid refs/unrelated histories,
dirty PR context, preserved exits, bounded output, both pipes, process-group
timeout, lock contention, archive members/checksums/size, version/flags,
failed-setup preservation, atomic publication, idempotence, and reporting
a cleanup failure separately after the new generation is already active.
The two review regressions prove SIGTERM/SIGINT cancellation terminates
detached children before lock release and nested map-symbol changes are
visible to the freshness comparison. The corrected smoke capture records
the dirty source state and exact adapter/smoke hashes alongside its base
commit, rather than attributing an uncommitted fix to the older snapshot.

## Paired navigation — completion and source-backed limitations

Freeze these tasks and expected contracts before either arm runs:

| Task wording | Expected source/consumer set |
|---|---|
| Trace SQL placeholder validation: locate validate_parameter_order, its execution wrapper, regression coverage, and separate SQLite/Hiqlite responsibilities. | Hiqlite validator and TimedClient, placeholder census and regressions, SQLite binding difference, Store boundary, ownership/proposed proof. |
| Trace a viewer pause through playback-control hold demand across server, web, Apple, Android, and transport/serialization consumers; distinguish demand from buffering. | Server hold and serialized snapshots; web mapper plus transport; Apple mapper/reporter/session; Android equivalents; generation/sequence fencing and demand versus observed buffering. |
| Investigate decode fallback: locate prepare_decode_restricted, its callers/tests, and documented recovery constraints. | Restricted prepare definition, production caller, retained tests, same-encoder/software-decode and prepublication/budget rules, current decoder guides and ownership. |
| Plan the historical correction for prove-fix Cargo-target contamination, including implementation/test/ownership paths and focused proof. | Historical `ea899568`, target-directory selection and child environment, retained Python regression, validation.framework, pre-fix proof. |

Two paired repetitions per task are required (sixteen fresh sessions).
Dispatch order for SQL was baseline-1, candidate-1, candidate-2, baseline-2.
Every other task used the same baseline-1, candidate-1, candidate-2,
baseline-2 dispatch order. Trials were concurrent on separate
clones; do not use their elapsed time as an isolated latency benchmark.

| Session | Outcome | Required groups located | Read operations | Shell calls | Recorded elapsed s | Wrapper queries / refusals | Fallback |
|---|---|---|---|---|---|---|---|
| SQL baseline 1 | returned | 5/5 | 32 | 12 | 105.045, partial | 0 / 0 | ordinary source tools |
| SQL candidate 1 | returned | 5/5 | 24 | 11 | 114, after instructions | 2 / 0 | source reads and catalog |
| SQL candidate 2 | returned | 5/5 | 20 | 9 | 67.453, partial | 2 / 0 | source reads and catalog |
| SQL baseline 2 | returned after interruption | 4/5; owner not explicit | 29 | 11 | 66.907, partial | 0 / 0 | ordinary source tools |
| Playback baseline 1 | returned after interruption | 5/5 | 24, excluding instructions | 10 | 817 including interruption | 0 / 0 | ordinary source tools |
| Playback candidate 1 | returned after interruption | 5/5 | 19 | 9 | 799 including interruption | 1 / 0 | source reads and catalog |
| Playback candidate 2 | returned after interruption | 5/5 | 24 | 9 | 1155.717 including interruption | 1 / 0 | source reads and catalog |
| Playback baseline 2 | returned | 5/5 | 27 | 9 | 147 | 0 / 0 | ordinary source tools |
| Decode baseline 1 | returned after interruption | 4/5; owner not explicit | 20 | 7 | 82, resumed segment | 0 / 0 | ordinary source tools |
| Decode candidate 1 | returned after interruption | 5/5 | 14 bounded reads; 20 with searches | 8 | unavailable | 4 / 0 | source reads and catalog |
| Decode candidate 2 | returned | 5/5 | 22 | 9 | 151 | 3 / 0 | source reads and catalog |
| Decode baseline 2 | returned | 4/5; owner not explicit | 29 | 9 | 187 | 0 / 0 | ordinary source tools |
| Correction baseline 1 | returned | 5/5 | 21 including search/diff reads | 6 | 106.59 | 0 / 0 | ordinary source tools |
| Correction candidate 1 | returned | 5/5 | 12 including Git diff reads | 6 | unavailable | 1 / 0 | source reads and catalog |
| Correction candidate 2 | returned | 5/5 | 14 including Git diff reads | 6 | 51.374, partial | 1 / 0 | source reads and catalog |
| Correction baseline 2 | findings returned; artifact refused | 5/5 | 13 including Git diff reads | 7 | unavailable | 0 / 0 | ordinary source tools |

Read operations are agent-reported explicit cat/sed file inspections;
search scans are excluded. Instruction inclusion and timing start points
were not fully consistent, so these are descriptive counts rather than a
valid median comparison. The first three SQL shell-output character counts were
135533, 154575, and 112470 respectively, with one/two/two truncated tool
outputs. Those are not UTF-8 byte or model-token measurements. Both SQL
candidate sessions measured 9029 total Ripwire stdout bytes from two queries.
SQL baseline 2 measured 85,976 output bytes for calls 6–11 only. Actual
model tokens and billing are unavailable. One coordinator resume request
recovered SQL baseline 2 after its account-limit failure; both SQL candidates used
source fallback and reported graph limitations. The first three returned answers covered the five required SQL groups.
Baseline 2 returned implementation, regression, backend and proof details
but did not explicitly name the functionality-point owner. No required
SQL execution consumer was omitted from these bounded answers.

The recovered playback pair traced server, web, Apple, Android and relay
serialization. Both found the bounded startup grant that temporarily permits
production under hold, superseding older demand-lease prose. Candidate
source fallback also found concrete native HTTP serializers and VOD marker
prewarm consumption. Baseline explicitly left concrete native encoders and
Android button notification incompletely traced. Neither answer claimed
complete actor/replay/device verification. Candidate used one query (4160
stdout bytes) then source fallback. Their 817/799-second wall intervals
include unknown interruptions and are invalid as speed comparisons.

All four decode answers locate the production caller, the four direct
helper tests, the shared offline validator, and the prepublication recovery
constraints. Both candidate answers recover the graph's omitted no-op test
through literal source search. Both baseline answers omit explicit catalog
ownership but propose relevant validation. This is bookkeeping incompleteness,
not a missing execution consumer. Current implementation and regression
permit structurally safe unqualified recovery; older qualification comments
are stale. Complete postpublication client wiring is outside this bounded
helper trace and was explicitly left unverified by both arms.

Decode candidate 1 lost a tool response to an instrumentation error and
repeated the read; that query is counted. Its 14 bounded file reads plus six
search inspections must not be presented as 20 reads under the other
sessions' cat/sed convention. Decode candidate 2 includes one Ripwire body
in its 22 reads. Correction baseline 1 includes visible search and Git diff
reads. These inconsistent definitions prevent a strict cross-task adoption
comparison. No candidate answer is treated as complete solely because the
graph returned success.

All correction answers identify the script's child environment, retained
regressions, historical pre-fix proof and `validation.framework` ownership.
Both candidate broad queries require source fallback to find the actual
extensionless implementation; candidate 2 ranks a relevant test tenth.
Candidate output was 3269/3760 stdout bytes, with no wrapper refusals.
One baseline agent's artifact write was rejected by automatic approval
review; its findings were returned directly instead. This is an output
artifact limitation, not a failed navigation result.

The descriptive median read counts are SQL 30.5 baseline versus 22 candidate
(27.9% fewer), playback 25.5 versus 21.5 (15.7% fewer), and decode 24.5 versus
18 (26.5% fewer using candidate 1's 14 bounded reads). These are not valid
controlled improvement estimates because read definitions, instruction
inclusion, and interruptions differ. Correction is 17 versus 13 (23.5% fewer), also with differing search-read
counting. Playback does not reach 20% even on
these descriptive counts. Missing model metadata further prevents a strict
default-adoption conclusion. Tokens and billing remain unavailable.

## Historical correction — blind replay and retained proof

Selected correction: `ea89956823bf61f68091ee025b9f6a3fa1992316`, which isolates
prove-fix's `CARGO_TARGET_DIR` from the normal gate's target. Pre-fix source
is `aae5ba9a230e54e3973dfdb6a390f29c4bdd7621`. Scratch repositories were
initialized from that source archive with no later Git history; both received
the same adapter source overlay and only the candidate has it installed.
Both fresh implementation sessions were dispatched after navigation finished.
Their task names the observed contamination and required target policy but
withholds the later correction diff and retained tests. They may edit only
the Python proof script; no product patch is promoted from the exercise.

The coordinator loaded the retained four Python tests from the correction
commit, pointed them at the historical pre-fix script, and observed the two
isolation tests fail while the two existing proof-protocol tests passed.
The same tests all pass against the known corrected script (1.819 s).
Its Git blob is `29838b3a9f93736fe61926fa7cceadab4abfb9a6`, identical to the
original correction's script. An initial harness attempt used a relative
executable path and produced setup errors; it was corrected before the
reported red/green runs. No Rust compilation or product edit was involved.
Both blind patches changed only the child target environment. Each used
`Path.resolve()` before appending `-prove-fix`. Retained tests in the default
macOS temporary directory passed 3/4 for both arms: the inherited-target
assertion compared `/var/...` with canonical `/private/var/...` and failed.
This is a real path-spelling mismatch against the retained contract, not an
all-green implementation result. Neither arm overwrote the ordinary target.
A diagnostic rerun with `TMPDIR=/private/tmp` passed 4/4 for both unchanged
patches (2.350/1.281 s), confirming the symlink spelling distinction. The
original failures and patches remain retained; this rerun does not replace
the blind score. No candidate correctness regression versus baseline was
observed, and neither patch is promoted.

| Arm | Navigation calls | Explicit reads | Elapsed s | Ripwire bytes / refusals | Blind retained result |
|---|---|---|---|---|---|
| Baseline | 7 | 3 source/doc reads plus final diff | 91.175, after initial discovery | 0 / 0 | 3/4; inherited path spelling mismatch |
| Candidate | 9 | 4 source/doc reads plus final diff | 47, through patch inspection | 3915 / 0 | 3/4; same mismatch |

The candidate read the wrapper and ran one broad query before source fallback;
its extra reads did not improve this small implementation task. Timing start
and end boundaries differ, so 91/47 is not a controlled speed comparison.
The historical archive predates AGENTS and the docs index; both agents
reported their absence and read the relevant validation guide instead.

## Reproduce the evidence and interpret the verdict

Use a new clone at the frozen source, explicit setup, and the settings above.
For every command, capture stdout and stderr separately and retain failures.
Run `scripts/ripwire-smoke` explicitly. For timing, clear only that clone's
`target/ripwire/cache/` before each of three fresh samples, then take five
warm samples without clearing it. Use `/usr/bin/time -l` on macOS and record
its exit separately from the wrapper provenance. Use equivalent documented
resource instrumentation on other hosts.

Disposable trial clones were removed after all sessions finished.
Their reconstruction helpers are retained under `pilot/harness/` alongside the explicit local evidence.
The current local evidence root is `target/ripwire/evidence/` in the own clone
`/private/tmp/plurx-ripwire`. It contains `smoke-20260910-193051/`, the first
smoke capture, and `pilot/` with `latency.json`, the separately retained
instrument failures, per-command captures, `coverage.json`, extra coverage
and documentation-profile captures, frozen expected contracts, session
summaries, and retained exercise proof. These are ignored local records;
this report contains durable summaries, not copied source or transcripts.

Default adoption requires no worse correctness on all four tasks, at least
three tasks improving median reads or measured context by 20%, and no worse
correction exercise. The complete answers show no additional required
consumer omission from the candidate arms, and the blind patches have the
same retained failure. However, inconsistent read definitions, interruptions
and unavailable exact model metadata prevent establishing the improvement
threshold. No token, cost or statistically significant speed claim is made.

Verdict: **opt-in only**. Ship the explicit installed commands and usage guide.
Warm medians are below two seconds, source fallback recovers measured graph
omissions, and the candidate trial did not worsen the selected correctness
contracts. Add no default worker guidance: its prompt cost is **0 bytes**.
SCIP and unexecuted release platforms remain documented follow-ups. A future
default-adoption study needs consistent instrumented reads and exact model
settings; it must preserve these original observations. No feature gate or
product enablement UI is introduced by this development-tool integration.
