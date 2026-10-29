# Python unit receipts — one success per test per PR

**Status:** open · **Decision:** Paul, 2026-10-01 (once-per-PR successes);
2026-10-11 (history never blocks a PR) · **Scope:** effort and main preflight

Companion to [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md): this
policy changes repeated Python unit execution, not current-source compilation.

## The rule

A test that already passed in an earlier run of the same PR does not run
again, as long as that success can be **authenticated** and still **applies**
to the current source. Anything that cannot be authenticated is simply **not
adopted**: its tests run again and the lane carries on. History never refuses
the lane.

Only the current run's own integrity refuses:

- a re-run job attempt (`GITHUB_RUN_ATTEMPT` other than 1);
- a current run that already published a journal;
- a checkout that is not the event's source, or a worktree that differs from it;
- an API origin that differs from the server (credential forwarding);
- unsafe paths, and the size bounds that protect the runner.

The cost of being wrong in the "not adopted" direction is re-running some
Python units. The cost of being wrong in the "refuse" direction was a PR that
could never pass again.

### Why the rule changed (2026-10-11)

Until then, restore accounted for every earlier run of the PR. Each one
needed a validated journal or a proof that it executed nothing, and anything
else refused the lane. Any evidence shape nobody had anticipated wedged the
PR for good. Examples:

- a crash in prepare before the start marker;
- a force-pushed commit whose objects are no longer in the checkout;
- a new refusal message;
- an incomplete journal;
- a re-run attempt.

Each wedge was then cleared by a one-off recovery bound to that exact
repository, PR and run. Twenty-odd modules and descriptors piled up, every
one for a PR that has since merged. PR #997 was the last case: a force push
left an earlier journal's commit unreachable, prepare crashed, and the next
run refused because the crashed run had no journal. The bespoke recoveries
are gone. One rule covers every case.

## Main-lane live identity

Main fast-lane jobs test the PR head, not a merge preview. The
[workflow's reviewed head-only gate](../../.github/workflows/main-fast-lane.yml)
records Paul's 2026-10-11 decision: a base-tip advance alone does not invalidate
that execution. The live PR must still be open, ready, from the same head/base
repositories, targeting `main`, and at the exact checkout/event/API-run head.
Source cleanliness, first-attempt, API-origin and receipt input/provenance guards
remain unchanged. This does not waive `AGENTS.md` integration requirements:
assess and integrate required current-main changes before landing, retaining
receipts only for compatible inputs. Manual effort/full-tree qualification
continues to bind both immutable tips and ancestry.

## Where it runs

| Lane | Workflow | Producer | Journals |
|---|---|---|---|
| Effort | `effort-ci.yml` | [`python_unit_receipts.py`](../../validation/python_unit_receipts.py) | `python-unit-…` per branch PR |
| Main, Python | `main-fast-lane.yml` | [`main_unit_receipts.py`](../../validation/main_unit_receipts.py) | `main-python-units-v1-r<repo>-pr<n>` |
| Main, Node and adoption | `main-fast-lane.yml` | [`main_preflight_adoption.py`](../../validation/main_preflight_adoption.py) | `main-preflight-v1-r<repo>-pr<n>` |

Each run publishes a **start marker** before any unit can execute. Each
individual success updates a bounded JSON journal by atomic replacement. The
**final journal** is uploaded even when a test fails. A run that fails before
its units contributes no journal, and therefore nothing to adopt.

## What "authenticated" means

A prior final journal is adopted only when all of these hold. These are the
checks from before the 2026-10-11 change; the effort lane also gained the
terminal-status check:

- **Run:**
  - The artifact belongs to an authenticated workflow run of this
    repository and PR (main) or branch (effort).
  - The run's event, head and base bind to the PR.
  - The receipt job is unique, terminal (success, failure or cancelled)
    and attempt 1. On the main lane it must also have been assigned to a
    runner.
- **Start marker:** exactly one live marker exists for that run, and it
  validates against the same scope, run and commit.
- **Journal:**
  - The final journal validates against the run's scope, run ID and commit,
    with schema, producer and manifest blobs (main) and environment.
  - It keeps every record of its start marker unchanged.
  - It carries no fixture or phase errors.
  - On the main lane, the journal's commit objects must be present in the
    checkout, or the journal is not adopted. On the effort lane the journal
    is adopted, but its passes fail the applicability check below. The
    outcome is the same on both lanes: those tests run, and nothing crashes.
- **Records:**
  - An own record needs its original executable outcome event in the job log
    (main adoption journal).
  - An inherited record is kept only if the run it names was itself adopted
    and recorded it as its own. Otherwise that one record is dropped.

The main Python lane also keeps one generic second channel. If a run's final
artifact upload failed, a complete start/final snapshot pair printed in that
run's own terminal job log can stand in for it. The log must bind the
checked-out source and the pull-request event. Anything less is not adopted.

A failure of any check prints one line naming the run and the reason, ending
"its tests run again". Exceptions other than `ReceiptError` print with their
type name, so a code bug cannot quietly turn adoption off. Every run without
a final journal contributes nothing. On the main lane each one also prints a
line, because the log channel above is tried for it. Transport errors also
mean "not adopted": a connection reset, a timeout or a truncated download.
So does a failure to list history, which leaves every test pending.

The run step re-checks the retained passes before skipping them. A pass whose
re-check cannot complete is executed, never refused. A run that withdraws a
pass this way publishes a final journal that no longer matches its start
marker, so that run's journal is not adopted later.

## What "applies" means

Adopted records then pass the source-applicability check
(`SourceApplicability`) before they enter the retained set.

- **Fingerprint:** the method's normalized AST, its decorators and the
  module's non-test fixture context. That context is class bases and
  decorators, setup and teardown, helpers, module fixtures and imports.
  Sibling test bodies are excluded, so editing one method does not invalidate
  its neighbours. Shared context changes invalidate the whole module.
- **Comparison:** the recorded commit's immutable blob against the current
  commit's, read from the full-history checkout. The current worktree must
  equal the dispatched source.
- **Main lane only:** each ID also binds the declared production inputs in
  [`main-preflight-inputs.json`](../../validation/main-preflight-inputs.json).
  An ID with no reviewed witness row, or whose inputs changed, runs.
- **Undecidable cases:** unavailable historical source, unsupported dynamic
  constructs, and a current source that cannot be fingerprinted all mean the
  same thing. No candidate proves the ID, it is listed as not adopted, and
  it runs.

**Bounds:**

- source files ≤1 MiB each;
- cache ≤512 files, ≤32 MiB of source and ≤1,000,000 AST nodes (≤100,000
  per file);
- inheritance depth <16;
- fingerprints ≤40,000;
- five-second timeout per local Git read.

These protect the runner; they are not a semantic cache of imported product
code, scanned files or environment.

## Local receipts (effort lane)

A pass run locally can enter a journal without being re-run in the first
gate. A hash-named document in `validation/python-unit-local/` records its
test file hash, command, result and IDs. An authenticated repository writer
attests its exact SHA-256 in a PR comment:

```text
Python-Unit-Receipt: {"repository":1,"pr":123,"suite":"validation","sha256":"<64 hex digits>"}
```

- **Writer check:** the comment author's numeric identity must hold owner,
  admin or write permission.
- **The 403 case:** Forgejo 16 restricts the permission endpoint, so the job
  token can get a 403 there. Only that case falls back to the reviewed
  [`python-unit-attestors.json`](../../validation/python-unit-attestors.json)
  enrollment.
- **Unattested or failing receipts:** a receipt that is not attested, or an
  attestation that does not authenticate, is reported and not adopted. Its
  tests run.
- **Unsafe files:** an unsafe file in that committed directory refuses,
  because it is current source.

## Identity

- **Isolation:** receipts are keyed by repository numeric ID, PR number and
  suite-qualified test ID. Branch reuse cannot import another PR's
  successes, and an effort receipt never satisfies main.
- **Original provenance:** every adopted success keeps its original commit
  and run. Reuse is historical unit evidence, never a claim that the test ran
  on the current source.
- **Still current:** compiler, history, catalog and static web checks always
  run on the current candidate.

## How to read the output

- `discovered` is the current test inventory.
- `historical-passes` (or "adopted") counts retained successes.
- `pending` counts the tests that actually execute.
- `run N: journal not adopted — <reason>; its tests run again` is
  informational, not a failure.
- `Unit applicability changed: <id>` names a source-invalidated ID with its
  old and current fingerprints.

## Tests

- **Effort lane:**
  [`test_python_unit_receipts.py`](../../tests/validation/test_python_unit_receipts.py),
  class `ReceiptHistoryCase`.
- **Main lane:**
  [`test_main_unit_receipts.py`](../../tests/validation/test_main_unit_receipts.py)
  and
  [`test_main_preflight_adoption.py`](../../tests/validation/test_main_preflight_adoption.py).

Each one covers:

- a run that crashed in prepare;
- the PR #997 force-push sequence;
- a table of single-check breakages, each of which leaves only the valid
  sibling journal adopted;
- dropped inherited records;
- current-run refusals.

Each authentication check was deleted in turn in a scratch copy during
review. Every deletion made a test fail.
