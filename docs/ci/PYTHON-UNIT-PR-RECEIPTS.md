# Python unit receipts — one success per test per PR

**Status:** open · **Decision:** Paul, 2026-10-01 · **Scope:** effort preflight

Companion to [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md): this
policy changes repeated Python unit execution, not current-source compilation.

The effort preflight discovers the validation and operations suites on every
candidate. Each discovered test ID must have passed once for this repository
and PR. Previously successful IDs are not executed again after a head or base
refresh. Failed tests and newly introduced IDs execute. A skipped test, an
expected failure, discovery error, empty suite or unknown outcome is not a
success. Removing a test does not erase its historical evidence.

## Identity and source remain explicit

Manual dispatch must match exactly one open PR with the dispatched head SHA,
branch, same head/base repository, and an `effort/` base. Receipts are isolated
by repository numeric ID, PR number and suite-qualified test ID. Branch and
base are validated inside each journal; a mismatch refuses reuse.

Every success retains its original commit and workflow run. Reuse is
historical unit evidence, never a statement that the test executed on the
current source. Compiler, history, catalog and static web checks continue on
the current candidate. There is no bypass flag or product setting.

## Publish before execution, preserve after failure

The workflow uses the existing pinned Forgejo artifact action. A start marker
is uploaded before any unit execution. Each individual success updates a
bounded JSON journal with an atomic file replacement; the final journal is
uploaded even when another test fails. PR receipt preflights serialize and
are never automatically cancelled. Fresh dispatches are retries; re-running
a prior workflow job is refused because artifact/run-attempt identity would
be ambiguous.

Restoration binds each artifact to actual workflow run, named PR receipt job,
source SHA and start marker. It unions completed journals rather than trusting
the most recent cache snapshot. Duplicate, corrupt, incomplete, expired or
missing journals stop the lane. In particular, a missing cache is not evidence
that no test has passed. API requests have timeouts, bounded pagination and
body sizes; redirects are refused, and ZIPs contain exactly `receipt.json`.
Archives are read in memory, never extracted into caller-selected paths.

Local passes can also enter a journal, without rerunning them in the first
gate. A hash-named JSON document in `validation/python-unit-local/` retains
its original worktree test-file hash, command, result and discovered IDs.
It applies only to its named repository and task branch. An authenticated
repository writer must attest its exact SHA-256 in a PR comment:

```text
Python-Unit-Receipt: {"repository":1,"pr":123,"suite":"validation","sha256":"<64 hex digits>"}
```

The API must confirm that the comment author's numeric identity has owner,
admin or write permission. Forgejo 16.0.3 restricts that permission endpoint
to admins or the queried user, so the automatic job token may receive 403.
Only that exact collaborator-permission 403 can use the source-reviewed
`validation/python-unit-attestors.json` enrollment: repository1/user1/login
pjunod, independently verified as owner by ROOT on 2026-10-01 through the
actual authenticated user and collaborator-permission APIs. Changing the
enrollment requires a fresh actual role check and review. Other statuses,
identities, repositories, reader roles and unavailable evidence still refuse.
The safe diagnostic reports HTTP status and normalized API path, never token,
headers, response body or query. The old generic error did not identify its
actual denied endpoint; the next runtime diagnostic must establish that.
Every claim binds repository, PR, suite and hash.
Worktree successes retain null commit/run values and the original source
hash plus attestation comment ID; they never pretend the base commit or
current commit executed the tests. A relevant local receipt awaiting
attestation blocks execution, so missing comments cannot trigger duplicates.

Artifacts retain 90 days of evidence. An expired journal blocks reuse rather
than silently rerunning units. Keep the original run/job IDs and recover the
original attributable journal before retrying; do not delete receipt artifacts
to manufacture a first run. A process stopped between success and durable
publication leaves an incomplete attempt and requires evidence recovery.

The preflight job has a fixed literal name; Forgejo resolves job names before
`needs` outputs are available. Repository/ref concurrency serializes its one
validated open PR, while start/final artifact names use the prepare step's own
validated PR key. Journals bind repository, PR, branch, base, run and source;
branch reuse cannot import another PR's successes. Missing current-attempt
identity or evidence remains blocking, including unknown empty-name jobs.

First gate API3705/UI3684 failed prepare at source a3b795a37 before any unit or
start marker. Its one explicit recovery record binds job39306, actual failure
metadata, exact workflow/receipt-code hashes and original log SHA-256. Skipped
start/final artifact main steps plus reviewed source ordering prove zero units;
the record imports no success. Every unknown or contradictory attempt refuses
instead of treating an absent journal as a fresh run. This is not a generic
failed-attempt exemption.

Non-method fixture errors (module/class setup, teardown or cleanup) remain
explicit unresolved evidence even when every method already succeeded. A
retry refuses those journals and reports the originating run and fixture ID;
zero pending methods cannot turn a failed fixture green. Recovery needs
attributable failed-fixture-only diagnostic/correction evidence while retaining
all method successes. Do not delete journals, reset PR scope or rerun successful
methods to clear the blocker. If exact fixture reconstruction is impossible,
report it; this mechanism has no generic recovery bypass.

**How to read output:** `discovered` is the current inventory,
`historical-passes` counts retained successful IDs, and `pending` counts the
only tests executed. A receipt refusal identifies the ambiguous run or
artifact. It is a blocking evidence problem, not a passing test result.

## The bounded legacy #663 reconstruction is not individual runner output

PR #663's old gate API run 3703 / UI 3682 / job 39297 ran operations discovery
at `50c17c8783aa38f52109372dc04cb91c47863d7a`: 603 unique tests, one named
error, no skips, failures, expected failures or unexpected successes.
Exact-source loader discovery without test execution matched the static 603
IDs with zero loader errors and no custom discovery/run/constructor hooks.
Paul's delegated root accepted the resulting 602 old-source successes as an
aggregate reconstruction on 2026-10-01, not separately logged pass events.

The implementation narrowly binds this import to repository 1, PR 663,
its actual branch/base, API run/job, original log SHA-256, exact module hashes,
static inventory and exhaustive command. The sole previously failed PDB ID
retains the separately approved local Linux six-test correction evidence at
`157e0b3cfd52eae074f674379f924a6b442eb6b8`, with no invented CI run ID.
Missing or contradictory evidence refuses the import. The checked-in compact
manifest and correction receipt live beside the validation implementation;
they are historical provenance, not current-source test executions.

The same original job's validation summary reports 252 tests in 23.016 seconds,
plain `OK`, with no non-success outcomes. Exact-source no-execution discovery
and static inventory matched all 252 IDs, including the audited test-free
fixture/mixin inheritance. Those successes retain the same old-source run,
not a fresh validation-suite execution. The corrected PDB ID is admitted only
through the authenticated local-receipt path described above.

## The bounded #674 discovery-failure recovery preserves 277 real passes

On 2026-10-01, PR #674 gate API3719 / UI3698 / job39416 failed at
`cd2e2f1467ab0e77e8e9e09c90b75580d6411294` after 277 validation methods
passed individually in 22.126 seconds. Operations discovery then refused a
relative fixture import in its top-level loader context. Root's original
review35 disposition had not validated that CI loader context; the correction
verified both import contexts without rerunning any successful method.

The retained final journal is **incomplete**, has no fixture errors and has
281 keys: 277 individually logged CI validation passes plus four previously
authenticated local operations passes. Recovery never stamps it complete or
calls operations executed. The small exact descriptor
`validation/python-unit-discovery-failure3719.json` binds repository1/PR674/
branch/base, original source, terminal failure attempt1 and these live objects:

| Evidence | Exact identity and SHA-256 |
|---|---|
| Start marker | artifact1481, 755 bytes; `f2ba6e29cf1937d725017d940bab0f0f88ceed90955f911947504de5a3f6d3bb` |
| Final journal | artifact1482, 7327 bytes; `71f6ca02aee13807b3373882e2b41b3742408487470b9bb28b0736dbe109f88d` |
| Receipt job log | job39416; `1c7b7b0fd2738988a5e1aef91ebc7ad0c446e7d3882264b6b429fa665c2d0f40` |

The real restore consumer verifies live run/job/artifact metadata, both ZIP
hashes, original workflow/receipt-source hashes, and the log's phase ordering.
All 277 unique individual positive events must exactly match the new validation
journal keys and original commit/run attribution. The four unchanged start
keys must still have current authenticated local-receipt evidence; CI cannot
turn them into run3719 successes. Duplicate/missing events, foreign sources,
operations execution, fixture errors, missing/expired artifacts and every
unapproved incomplete case refuse recovery. No generic incomplete waiver or
caller-selected record exists. Raw private logs and ZIPs remain outside Git.

Future execution discovers **all suites before the first method**. A later
suite's import refusal therefore executes no earlier methods and never stamps
the attempt complete. This does not make unknown failed attempts automatically
reusable; their exact evidence still needs reviewed recovery.

Two focused synthetic-API regressions in
`tests/validation/test_python_discovery_recovery.py` exercise the actual restore
consumer, authenticated local baseline, individual-ID bijection, all-suite
preflight and fourteen refusal modes. They are not actual CI run evidence.
Separate read-only live restoration validated the genuine 277+4 keys using
13 API requests and zero test methods. Original journal/source attribution
remains unchanged; source compilation remains current-candidate evidence.

## Focused proof and limits

The small fake-fixture tests in
`tests/validation/test_python_unit_receipts.py` cover failed-only retries,
new IDs, source preservation, skips, isolation, corrupt/incomplete evidence,
unsafe archives, empty discovery, atomic publication and workflow ordering.
Run each unit regression once per PR; retry only a failure. This change does
not cache compiler or static checks, alter main qualification, or mutate a
runner service. Legacy runs without sufficiently attributable per-ID evidence
are not automatically labelled successful.

The receipt mechanism is Python-only. Duplicate effort-preflight player-input
and player-DOM executions are removed; both remain in the unchanged mandatory
`web-check` lane. That broad Node lane may run once on the first attempt, but
after a Node success the gate must not be dispatched again until the separate
static-only follow-up or attributable recovery preserves that success. These
Python receipts do not claim to cache Node tests or exempt their contracts.
