# Engineering knowledge — trace code to governing intent

**Status:** open · **Updated:** 2026-10-21 · **Owner:** Codex integrator

Companion to [VALIDATION](../VALIDATION.md) for behavior/check ownership and
[the pilot status](../features/ENGINEERING-KNOWLEDGE-GRAPH-STATUS.md) for actual
review, qualification and evaluation evidence. The commands below are the
implemented interface; final qualification is pending. No token savings are
established.

## Start from code or a proposed change

```sh
scripts/knowledge context --path crates/plurx-core/src/playback/caps.rs
scripts/knowledge context --path crates/plurx-core/src/playback/caps.rs --symbol applies_at
scripts/knowledge obligations --base origin/main --worktree
scripts/knowledge obligations --staged
scripts/knowledge why plurx:route:server.learned-decode-limit-expiry --max-bytes 32768
scripts/knowledge evidence plurx:check:routing.server.learned-decode-limit-expiry --max-depth 0 --max-bytes 32768
scripts/knowledge conflicts --scope routing
```

The pilot covers six selected routing rules and the Python receipt boundary,
not every function in their files. Follow returned relationship paths to
contracts, accepted decisions, plans, obligations and check definitions.
Separate design, review-finding, build-ledger and real execution roles remain
explicitly unrecorded where no governing source was found. An empty answer
means no known applicable record within that coverage; it proves no absence.
Use Ripwire and the existing documentation router to continue investigation.

`--staged` selects staged change paths against worktree knowledge. It does not
attest a staged source tree or an execution. Each answer identifies its source
snapshot, HEAD/tree, worktree and dirty state. A relevant edit, index/membership
change or concurrent mutation cannot silently reuse another snapshot.

Queries default to depth six and an 8192-byte UTF-8 envelope, including its
newline. `--max-depth 0` returns only direct matches with the same ordering and
fields. Both `--format json` and `--format text` currently use the canonical
JSON envelope. Partial results carry omitted counts and a continuation handle;
pass it with `--continuation` and the same query. Handles bind query and snapshot.
A record too large for the requested budget produces an explicit refusal.

## Read authority and evidence separately

Check coverage boundaries, reviewed provenance, current anchor integrity and
semantic applicability before relying on a result. Changed anchors require
review; dates and document headings cannot establish precedence. A `mentions`
path is discovery, not governing authority. Accepted supersession and unresolved
conflicts remain visible instead of allowing a newest-header guess.

A check definition is not an execution. Evidence reports origin, source
compatibility, environment and execution independently. A reported pass and a
matching hash cannot authenticate it. Missing Git history, incomplete dependency
closure or an unknown environment cannot establish qualification. The graph
neither authorizes receipt adoption nor skips tests; the existing validators
retain that authority. No real authenticated receipts are indexed in this pilot.

## Maintain records in the change that alters their meaning

The [repository skill](../../.agents/skills/engineering-knowledge/SKILL.md)
contains the agent workflow. Canonical routing/catalog schemas remain unchanged;
read-only adapters import their identities. Authored semantic records live in
[records.toml](../../validation/knowledge/records.toml), reviewed coverage in
[coverage.toml](../../validation/knowledge/coverage.toml), and adjudicated conflicts
in [conflicts.toml](../../validation/knowledge/conflicts.toml). Review the actual
source passage before changing its digest, attribution, scope or lifecycle.
A mechanical digest refresh alone does not establish semantic review.

Supply one existing identity per PR-body line, or a concrete no-change reason:

```text
Knowledge-Obligation: plurx:obligation:preserve.server.learned-decode-limit-expiry
Knowledge-Decision: plurx:decision:routing.plan-ceiling
Knowledge-Impact: reviewed-no-change; the patch changes formatting only in the reviewed scope
```

Missing acknowledgment remains advisory during the observation window. Invalid
supplied identities fail existing PR preflight. Acknowledgment and no-change
reasons never satisfy or remove an obligation. The preflight receives the body
as inert environment data; it does not execute prose or edit the description.
Local inspection uses `scripts/knowledge lint --body-file <description> --base
<ref>`. Without a supplied body, lint reports capture as unchecked.

Schema/reference and source-snapshot boundaries are exercised by the focused
knowledge unit modules and the real-record CLI smoke test. Existing unit
discovery owns them; there is no new CI lane. The SQLite projection is local,
rebuildable data beneath `target/knowledge`, with current/prior bundles retained.
No graph service, private CI-artifact ingestion or product feature switch is
required. The exact amendment for an external documentation skill is
[provided in the repository](../../validation/knowledge/house-skill-amendment.md);
its maintained installation path has not been verified.

## Evaluate before expanding

The [approved plan](../features/ENGINEERING-KNOWLEDGE-GRAPH-PILOT.md#9-evaluation--separate-knowledge-capture-from-graph-traversal)
compares existing navigation, direct curated records and traversal on twelve
sealed cases, three repeats per arm. A source proxy enforces exact visible
passages; solvers receive no host filesystem, network, terminal or gold access.
Patch acceptance needs a separately verified isolated executor, frozen
toolchain, exact-family dependency prerequisite and trusted completed outcome.
A version probe or bare nonzero exit cannot distinguish a patch assertion
failure from unavailable infrastructure. Unsupported acceptance families remain
unavailable and uncacheable; they cannot count as failed patches or justify
paid comparison runs known to lack acceptance. Finite measured budgets,
independent blind scoring and all failed runs belong in the result.

Capture enforcement cannot graduate until ten actual eligible PRs and fourteen
calendar days after advisory capture lands. Neither artificial PRs nor a short
benchmark replaces that observation. Check the status page before treating any
pilot milestone as qualified or expanding to another subsystem.
