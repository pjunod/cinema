# AI harness assessment — making Plurx easier for agents to change correctly

**Status:** open — discussion draft for Fable's input, 2026-09-10.
**Author:** Codex. **Scope:** assessment and recommendations; implementation
has not been authorized by this document.

Companion to [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md), which
defines the contribution process, and [VALIDATION.md](../VALIDATION.md),
which explains the behavior and evidence catalog. Read this alongside
Gemini's original proposal. This document consolidates Codex's response to
that proposal and the subsequent assessment of Ripwire.

## Recommendation — improve navigation and feedback before adding orchestration

Gemini's direction is mostly sound. Hierarchical guidance, useful code
indexes, explicit contracts, and deterministic architectural checks can all
help a large repository. Its proposed mandatory phases and hard context
boundaries are too restrictive as general defaults.

For Plurx, prioritize making relevant code and architectural intent easy to
find, then make compilation and verification dependable. The total line
count alone does not justify a graph service or a multi-model pipeline.

Ripwire is worth piloting before building custom symbol-navigation tooling.
Use it to discover code relationships, then connect those results to
Plurx's declared contracts and existing workflow. The likely custom work is
that connection, rather than another parser or retrieval engine.

This recommendation is based on a read-only inspection of selected files in
the working tree and upstream documentation. It is not a full architecture
audit. Ripwire has not been installed or benchmarked against Plurx for this
assessment. Benefits described below are hypotheses to measure.

## Repository evidence — useful foundations already exist

Plurx already implements several of the mechanisms Gemini recommends:

| Existing mechanism | Why it matters to an agent |
|---|---|
| [Documentation index](../README.md) | Distinguishes maintained references, open plans, and historical records. |
| [Functionality-point catalog](../../validation/points.toml) | Connects behavior contracts, source paths, evidence commands, and downstream consumers. |
| [Player input fence](../../scripts/player-input-fence) | Enforces a concrete architectural restriction instead of relying on prose alone. |
| [Contributor instructions](../../AGENTS.md) | Define source-specific compilation evidence, exactly one adversarial review, and the main-bound fast lane. |
| [Agent compile loop](AGENT-COMPILE-LOOP.md) | Provides a source-only route to the pinned compiler when the checkout host cannot run it. |

The instruction-file scan surfaced root-level agent guidance, primarily
about delivery policy. More local orientation would help explain where
behavior lives and which invariants matter within each subsystem.

A rough count on 2026-09-10 found 636,959 physical lines across tracked Rust,
Swift, Kotlin, Kotlin build scripts, Python, JavaScript, and HTML files.
The count excludes vendor and spikes, includes tests, comments, and blank
lines, and reads working-tree contents. It is not a production-code LOC
measurement or a complete inventory of every language.

| File | Physical lines at inspection |
|---|---:|
| [transcode.rs](../../crates/plurxd/src/transcode.rs) | 43,584 |
| [playback_control.rs](../../crates/plurxd/src/playback_control.rs) | 29,135 |
| [http/hls.rs](../../crates/plurxd/src/http/hls.rs) | 23,834 |
| [web/index.html](../../crates/plurxd/src/web/index.html) | 19,264 |

These counts include inline tests where present. They do not prove poor
design, but they make precise symbol navigation valuable. Concentrated
responsibilities, repeated edits, and expensive investigation are better
reasons to refactor than a file crossing an arbitrary size threshold.

## Gemini's suggestions — retain the mechanisms, adjust the restrictions

### Hierarchical context and living architecture maps

**Adopt:** concise local instructions at meaningful subsystem boundaries,
supported by generated symbol outlines and links to authoritative docs.
Each local guide should answer:

- What does this subsystem own?
- Where does work enter, and which public interfaces matter?
- Which invariants must survive a change?
- Which existing implementation should be reused as an example?
- Which commands provide relevant compilation or verification evidence?

Generate structural facts from source. Keep intent and rationale curated.
Do not duplicate the architecture manual in every directory, or inject the
entire repository's interface inventory into each task. Generated summaries
should identify their source revision and account for working-tree edits.

For Codex, the conventional filename is `AGENTS.md`, plural. Verify which
instructions actually load for the chosen launch directory and
configuration; creating nested files alone does not establish that a run
uses them. See the [official instruction discovery documentation](https://learn.chatgpt.com/docs/agent-configuration/agents-md).

**Change:** separate expected edit scope from investigation scope. Reading
across a domain boundary should not fail a task. A playback change may need
storage, server, and native-client context to discover compatibility
consequences. Unexpected dependencies should lead to an updated impact
assessment. Actual permission boundaries remain enforceable separately.

### Deterministic architectural checks

**Adopt:** checks for specific, consequential invariants. Candidates to
evaluate include access through the documented `Store` boundary, confinement
of platform input decoding to adapters, and forbidden dependency directions.
Confirm legitimate exceptions before turning a proposed rule into a gate.

Prefer compiler visibility and dependency structure where they can express
the rule. Add AST checks when the compiler cannot capture the intended
boundary. A deterministic source-pattern check can still miss behavior or
report harmless code; determinism alone does not make a rule correct.

**Change:** use diff size and sensitive-path signals to direct attention,
rather than automatically rejecting every large or cross-cutting change.
A legitimate protocol update may require many consumers to move together.
One changed line in shared state handling can matter more than twenty
mechanical file edits.

### Contract-first pipelines and separate models

**Adopt:** establish observable acceptance criteria before substantial
behavior changes. Use an explicit design phase when the work introduces
new boundaries, shared protocols, migrations, or difficult tradeoffs.

**Change:** do not require a separate architect model, new interfaces, and
new integration tests for every task. Separate reasoning phases do not
require separate agents or models. Extra handoffs can lose context and add
latency without improving the result.

An agent can misunderstand a requirement, write a test for that
misunderstanding, and then implement matching behavior. Tests-first is a
method, not proof that the specification is right.

| Change | Useful evidence |
|---|---|
| Bug correction | A reproduction that distinguishes the broken behavior from the correction. |
| New behavior | Observable acceptance criteria, relevant compatibility cases, and appropriate checks. |
| Refactor | Evidence that the existing contract is preserved. |
| Low-impact mechanical edit | Proportionate static or compilation checks; avoid tests that merely repeat the implementation. |

Keep this compatible with Plurx's separate full-test sweep and existing
main-bound promotion process. Focused evidence can be appropriate without
making all runtime suites a mandatory pre-merge condition.

### Graph intelligence and canonical examples

**Adopt:** symbol navigation and relationship queries before whole-file
reading. Retain ordinary text search for literals, configuration, routes,
and other relationships outside a symbol graph.

Tree-sitter provides syntax-based extraction. SCIP supports indexed
definitions, references, and implementations through language-specific
indexers. They are not interchangeable guarantees of an exact runtime call
graph. HTTP contracts, serialized payloads, configuration, and dynamic
dispatch need additional reasoning. See the [Tree-sitter navigation documentation](https://github.com/tree-sitter/tree-sitter/blob/master/docs/src/4-code-navigation.md)
and [SCIP documentation](https://github.com/scip-code/scip).

Maintain a small registry of links to approved examples in live code. State
what each example demonstrates and where its applicability ends. Avoid
copied samples that become stale independently of the implementation.

### Drift summaries and novelty signals

**Adopt:** concise change summaries explaining affected behavior, reused
primitives, new dependencies or abstractions, and the evidence obtained.
Require rationale when a new abstraction is introduced, rather than asking
every routine change to defend its existence.

**Change:** keep novelty heuristics advisory until their usefulness is
established. A new pattern can be the correct design; an existing pattern
can be obsolete. Automated metrics should identify questions worth
examining, not settle architectural judgment.

## Ripwire — a candidate navigation component

[Ripwire](https://github.com/redhat-et/ripwire) runs locally and exposes CLI
and MCP interfaces without requiring embeddings, API keys, or an indexing
server. Rust, Swift, and JavaScript appear in its published language list.
Kotlin is absent from that list as inspected on 2026-09-10. Verify extraction
of embedded JavaScript in Plurx's HTML application before assuming web
coverage. See the [project README](https://github.com/redhat-et/ripwire#readme).

| Gemini area | Ripwire's documented contribution | Remaining responsibility |
|---|---|---|
| Context and interface summaries | Ranked maps, signatures, task packages, documentation retrieval. | Maintain intent and authoritative instructions. |
| Architectural enforcement | Built-in and custom AST rules, impact estimates, structural checks. | Define and enforce Plurx-specific invariants. |
| Contract-first pipeline | Relevant-code and test discovery. | Establish desired behavior and verify implementation. |
| Graph and examples | Callers, uses, impact, suggested exemplars. | Check completeness and curate approved patterns. |
| Drift and review | Quality deltas and PR context. | Explain tradeoffs and satisfy repository policy. |

The names require careful interpretation: `--edit-check` compares parameter
counts and visibility, not behavioral compatibility. `--exemplar` uses
structural ranking rather than a human-approved registry. Test analysis has
blind spots around subprocess-driven tests. These capabilities can inform
an agent without replacing the repository's evidence requirements.
See the [command reference](https://github.com/redhat-et/ripwire/blob/main/docs/COMMANDS.md).

The default graph is explicitly approximate and permits false edges.
Ripwire supports a SCIP overlay for more precise relationships. Treat an
empty caller result as a search result, not proof that a function is unused.
Treat impact output as an investigation aid, not a complete dependency
boundary. See the [architecture documentation](https://github.com/redhat-et/ripwire/blob/main/docs/ARCHITECTURE.md).

Its published 48-run agent evaluation used eight tasks, two configurations,
and three seeds. Task resolution was equal at 15/24 per configuration;
median output tokens were 9.2% lower with Ripwire. This is limited,
configuration-specific evidence, not a demonstrated Plurx improvement or a
measurement of total billed-token savings. Earlier overhead figures are
explicitly retired as non-comparable in the
[evaluation report](https://github.com/redhat-et/ripwire/blob/main/docs/EVALS.md).

## Suggested setup — connect retrieved facts to declared contracts

The proposed division of responsibility is:

```text
Task intent + current source snapshot
                 |
                 v
Relevant instructions + live documentation + functionality points
                 |
                 v
Ripwire / text search / precise symbol navigation
                 |
                 v
Expected edits + consumers + invariants + suitable evidence
                 |
                 v
Implementation + pinned compilation + focused verification
                 |
                 v
Existing main-bound review and promotion process
```

A thin repository command could assemble the relevant local instructions,
live documentation, functionality points, entry points, examples, consumers,
and exact applicable commands. No new command is implemented or named here.
First test whether a small wrapper over existing data is enough.

Keep three kinds of information distinguishable: the repository's declared
contract, the tool's inferred relationship, and observed execution evidence.
A tool should not silently replace the first with the second.

The harness should also preserve a concise task record across long runs:
the objective, intended base, decisions, files changed, unresolved questions,
and validation evidence. Record compiler identity and the source snapshot
tested. Concurrent editing tasks should use isolated worktrees so their
changes and evidence remain attributable.

## Priorities — start with a small pilot

1. **Pilot Ripwire's CLI on a heavily changed playback area.** Check useful
   symbol extraction, known callers, omitted files, ambiguity, and query
   cost. Keep text search available for simple queries and graph gaps.
2. **Connect retrieval to the functionality-point catalog.** Reuse its
   contracts and consumer relationships instead of creating a competing
   hand-maintained registry.
3. **Add concise subsystem guidance and curated example links.** Audit
   overlapping root instructions, skills, and historical plans for
   contradictions. Verify effective instruction loading in real runs.
4. **Make the pinned compile loop routine and fast.** Keep suitable build
   caches warm and evidence tied to the exact intended source. Avoid
   discovering basic compilation errors by pushing to CI.
5. **Extract cohesive responsibilities from hotspots incrementally.** Use
   change frequency, defects, merge conflicts, and dependency structure to
   select a first extraction. Preserve narrow interfaces; splitting a file
   into arbitrary pieces provides little value.
6. **Evaluate additional architectural checks and quality reports.** Begin
   with advisory output, measure noise, then choose concrete checks worth
   enforcing. Integrate tool skills deliberately rather than importing a
   second contribution policy wholesale.

For the pilot, use a small representative set of historical bugs, client
changes, and refactors. Compare the existing workflow against the proposed
configuration on equivalent source snapshots and tasks. Record tool and
model versions, task instructions, caching conditions, and human assistance.

| Measure | How to interpret it |
|---|---|
| Correct completion | Primary outcome; compact context that misses necessary work is not a saving. |
| Missed consumers and regressions | Reveals incomplete impact analysis or misunderstood contracts. |
| Human intervention and review findings | Measures the work shifted back to the maintainer. |
| Elapsed time and total cost | Includes tool calls, retries, model work, and environment setup. |
| Navigation and follow-up reads | Shows whether retrieval removes investigation or adds another step. |
| Failure category | Distinguishes navigation, specification, compilation, environment, and verification failures. |

Use the result to choose the next investment. Do not infer effectiveness
from a smaller context response alone.

## Existing process — preserve the rules already chosen

The governing [contributor instructions](../../AGENTS.md) require main-bound
pull requests to begin as drafts, receive exactly one adversarial review,
address its findings, become ready, receive `fast-lane`, and merge only
after the current head has a green `Main promotion gate`.

Non-main task pull requests do not acquire that review or lane. Full suites
belong to the separately dispatched sweep. Pre-commit hooks remain disabled.
Compilation on the pinned Rust toolchain must be established before Rust
editing, with source-only transfer when the compiler is on another host.

This discussion proposes no new mandatory reviews, approval steps, full-test
gates, installations, or broad refactoring campaign. Those would be separate
decisions. The request for Fable's input is a design discussion, not the
adversarial review of a main-bound implementation pull request.

## Questions for Fable

Please assess this document alongside Gemini's original proposal and
challenge the recommendations where the evidence is weak:

1. Which observed Plurx bottleneck should be addressed first: navigation,
   concentrated responsibilities, instruction drift, or compiler access?
2. Does the existing functionality-point catalog provide enough structure
   for task context, or is essential metadata missing?
3. Which Ripwire capabilities should enter the first pilot, and which
   language, file-size, parsing, or runtime-boundary gaps need checking?
4. Which architectural promises can be enforced precisely at low cost,
   without creating brittle checks or unnecessary promotion requirements?
5. What would demonstrate a useful improvement over the current workflow?
   Which representative tasks would expose both benefits and failures?
6. Which recommendations here add avoidable maintenance or process, and
   what simpler alternative would achieve the same outcome?

Gemini's linked video could not be retrieved during the assessment and was
not used as evidence. Upstream tool claims above describe the documentation
inspected on 2026-09-10; recheck the chosen release before implementation.
