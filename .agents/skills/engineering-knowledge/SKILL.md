---
name: engineering-knowledge
description: Use for engineering code, documentation, and review changes in this plurx repository when scoped knowledge records may constrain the work; inspect their authority and limits without treating mentions or empty coverage as proof.
---

# Engineering knowledge

Use this workflow for changes in the curated engineering pilot scopes. Read
the repository [contributor rules](../../../AGENTS.md) and the
[knowledge pilot contract](../../../docs/features/ENGINEERING-KNOWLEDGE-GRAPH-PILOT.md)
for the governing boundaries; do not copy the record corpus into this skill.

Before a read-only investigation, query the relevant file with
`scripts/knowledge context --path <file>` and add `--symbol <name>` when useful.
For patches and reviews, also inspect
`scripts/knowledge obligations --base origin/main --worktree`; use
`--staged` when the staged change is the review scope. Check the returned
snapshot/revision, authority, scope and coverage boundaries, and any conflicts
before relying on a record. `--staged` selects staged change paths against
the explicitly identified worktree knowledge snapshot; it is not staged-tree
execution evidence.

An empty or uncurated result means the answer is unknown within that boundary,
not that no contract, decision, or obligation exists. A `mentions` edge, file
name, PR label, or self-asserted reviewer identity does not grant authority.
Trace accepted claims to their governing source and attribute findings to the
actual reviewed evidence.

Keep distinct what a source defines, what code implements, what check ran,
who authored or reviewed it, where a receipt came from, and which source,
environment, or compatibility it covers. A check definition or reported
success does not establish execution or authenticated CI qualification.

When the change accepts or revises a semantic finding, decision, or obligation,
update the record and its source-backed references in the same change. Preserve
stable IDs and explicit conflict disposition or supersession. Do not invent
acceptance, promote advisory capture into a gate, or weaken existing validators.

The pilot's optional PR-body fields are one known ID per line:

```text
Knowledge-Obligation: plurx:obligation:<id>
Knowledge-Decision: plurx:decision:<id>
Knowledge-Impact: reviewed-no-change; <concrete reason>
```

Missing fields remain advisory at first; a supplied invalid ID fails lint. A
`reviewed-no-change` reason records impact review and never clears a matched
obligation. Do not auto-edit the PR body.
