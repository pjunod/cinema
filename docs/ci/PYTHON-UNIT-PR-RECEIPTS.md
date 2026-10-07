# Python unit receipts — one success per test per PR

**Status:** open · **Decision:** Paul, 2026-10-01 · **Scope:** effort and main preflight

Companion to [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md): this
policy changes repeated Python unit execution, not current-source compilation.

## Main preflight — authenticated legacy outcomes and declared inputs

The main fast lane executes Python through
[`main_unit_receipts.py`](../../validation/main_unit_receipts.py). Its prepare
step calls
[`main_preflight_adoption.py`](../../validation/main_preflight_adoption.py)
to authenticate the legacy bridge and prepare the separate Node journal.
PR #845 also applies the reviewed
[`main-preflight-inputs.json`](../../validation/main-preflight-inputs.json).
Each Python ID binds its method/local-fixture AST and declared production
inputs to every retained Python success, including successes first recorded
by the generic runner. The manifest records actual read closures, including filename
inventories, absent paths, executable modes and narrowly declared Git-history
consumers. Unknown witnesses refuse before execution; changing one sibling
method does not invalidate unchanged methods through a whole test-file hash.

PR #845 has one bounded legacy importer: run4275/job43709/attempt1, source
`0c5ab4890316da40d44806eba9125ac2e1fead0e`. It authenticates the original
same-repository ready PR event, API job success, original executable workflow,
checkout identity and the 208075-byte raw log with SHA-256
`272d7509dc94961593f77ad3961f98ee2be097a167baff07e8e1eb0cd0eadd0b`.
It reconstructs exactly 293 validation and 735 operations IDs from immutable
source and requires one ordered progress event per ID: 1026 successes and
two skips. Summary totals alone cannot import a pass. The two skipped IDs
retain their history and remain pending current controls. Linux x86_64,
Python 3.12 and Node 22.23.2 are the proven legacy environment; the Node
setup is pinned to that version. No unobserved Python patch is claimed.

The preflight installs one job-local Python 3.12.15 Linux x86_64 runtime from
the immutable October 3 public release. HTTPS download, 34,285,590-byte bound
and SHA-256 `731af898886c5f821890dc901eca3c651cca8e51fa7308c159d12a1194aeac91`
are checked before extraction. The job verifies the Python family/platform,
adds only that runtime to its PATH and removes its exact owned temporary
directory in an always step. This preserves the proven Python 3.12 family
without claiming original patch, SQLite or OpenSSL equivalence or changing
shared host packages. Node remains pinned to 22.23.2.

Future journals bind their exact schema, repository/PR, source, producer and
manifest blobs, actual terminal job and attempt1. Own outcomes require an
original executable outcome event; inherited records preserve the original
run/job/attempt/source/outcome/environment chain. The declared input digest
is independently verified against the producing journal's reviewed manifest.
Both start and final artifact publications must exist. A missing final
journal after an attempted preflight stops reuse before any positive ID can
replay. Authenticated all-skipped draft runs contain no unit execution and
need no journal. Live PR head, base, open state and readiness are checked
before prepare and each phase so queued stale events cannot execute units.

Forgejo's `synchronized` event action and the `synchronize` action both retain
the same repository, PR, source, base and readiness checks. Run4294/job43864/
attempt1 failed at that action check on source
`289c9c5f3d6c5bdd48ff937c0c76476c95e9b7b2`, before any unit or journal.
Its one bounded recovery verifies the actual terminal run and complete nine-job
inventory, original ready PR event/base, immutable workflow/producer/input/helper
hashes, and original 139814-byte log SHA-256
`2db26ec8a677447202a8fa03e3895db12e840c4c931d186e242b7a6a6083fd95`.
That log must contain the exact early refusal followed by skipped start and
final publication steps, with no unit outcomes, suite summaries or successful
uploads; run, start and final artifacts must all be absent. This recovery
imports zero successes and creates no journal. It admits no other failed
attempt or changed evidence. The two new fake-API controls remain reserved
for their first actual candidate-lane execution.

Run4299/job43909/attempt1 on `ef65129def6f8286976f8ff2a18394c0c0183704`
failed the legacy environment check before units or journals. Its separate
bounded recovery authenticates the complete nine-job terminal inventory,
same ready event/base, original source hashes and 141038-byte raw log SHA-256
`758d98b2e40c73432f8ca328fda7c5cf8aa1eedf4cf88f2a0e5e7af47f323e2c`.
The exact environment refusal, absent unit outcomes and absent start/final/run
artifacts establish zero execution; no outcome is imported.

The bridge returns full authenticated original provenance before filtering
current applicability, so changed inputs invalidate a method without losing
its historical attribution. Only adapter-era runs authenticated by the bridge
are omitted from generic bootstrap; unknown attempted runs still refuse.
Generic start/final journals keep the original Python run/commit attribution,
and PR #845 adds runtime and immutable producer-blob provenance. Both journals
must match the authenticated original workflow and current runtime. Tracked
worktree changes refuse execution before discovery. The adapter retains the
seven Node script identities and executes only pending scripts; it does not
execute Python alongside the generic runner. Both start and final artifact
pairs are published independently, including after failures.

Fixture, import, discovery and runner-abort errors remain in the durable
journal even when earlier methods passed. They block later reuse until
attributable evidence is recovered; retained method successes cannot hide a
failed class or module teardown. A phase-in-progress marker is persisted
before Python discovery/fixtures and before each pending Node script; a
timeout or hard cancellation retains it. Normal completion removes the
marker atomically while preserving explicit fixture errors and individual
successes. Failed Python methods remain individually pending. Existing
history, catalog, static contracts,
compiler checks and promotion requirements still apply to current source.

**How to read output:** `adopted` counts applicable historical success IDs;
`pending` counts current methods still needing execution. `Changed declared
inputs/local fixture` identifies a specific invalidated ID, not a new blanket
suite run. An evidence refusal requires inspection of the named original
run/job/log/journal; deleting evidence does not create a first attempt.

**Granularity limits:** the seven Node receipts identify successful scripts,
not individual TAP cases. A failed or aborted Node script may contain passed
cases; its original log is preserved and its journal blocks automatic replay
until that partial evidence is recovered. Rust retries are not covered by
this Python/Node adapter. No Rust test success is inferred from run4275, and
these receipts cannot authorize repeating successful Rust cases. Compiler
parallelism is bounded with `CARGO_BUILD_JOBS=1`; Windows compilation waits
for the Rust gate to finish because their code generation exhausted a shared
14 GiB guest. Main's Rust fast lane compiles all targets and runs Clippy;
full Rust unit suites remain in manual full CI.

Both preflights discover the validation and operations suites on every
candidate. Each discovered test ID must have passed once for this repository
and PR. Applicable successful methods are not executed again after a head or
base refresh. A changed assertion body or shared local fixture is not proved
by an old success under the same ID. Failed, newly introduced and genuinely
source-invalidated methods execute; unchanged sibling methods stay retained.
A skipped test, an
expected failure, discovery error, empty suite or unknown outcome is not a
success. Main preserves ordinary unittest acceptance of legitimate skips and
expected failures, recording skips separately without caching them as passes.
Effort keeps its existing stricter policy. Removing a test does not erase its historical evidence.

## Identity and source remain explicit

Manual dispatch must match exactly one open PR with the dispatched head SHA,
branch, same head/base repository, and an `effort/` base. Receipts are isolated
by repository numeric ID, PR number and suite-qualified test ID. Branch and
base are validated inside each journal; a mismatch refuses reuse.

Every success retains its original commit and workflow run. Reuse is
historical unit evidence, never a statement that the test executed on the
current source. Compiler, history, catalog and static web checks continue on
the current candidate. There is no bypass flag or product setting.

## Main continuation — authenticated cutover and separate journals

Since 2026-10-07, [main_unit_receipts.py](../../validation/main_unit_receipts.py)
uses the shared bounded API, source applicability and success recorder with
its own workflow scope, artifact names and workspace directory. The event
must identify an open same-repository PR into `main` at the exact checked-out
head. Historical runs are queried by `refs/pull/<number>/head` and each run's
authenticated event payload, repository, workflow, PR, head, base, job and
attempt are checked. An effort receipt cannot satisfy main evidence.

For a PR that began before main receipts existed, the adapter imports one
latest exhaustive legacy baseline. The authenticated default unittest log
must bind the source checkout, terminal counts and every failed method ID to
an exhaustive AST inventory reconstructed from that commit's tracked files.
Class inheritance and supported unittest decorators are source-bound; custom
loaders, custom runners, dynamic discovery, missing source, count mismatch,
errors, skips and ambiguous outcomes refuse migration. A failed subtest
invalidates its whole named method. A terminal validation failure establishes
only validation successes: operations have not run. When both suites have
terminal results, each inventory is checked separately against its own log.
This generic migration has no run-specific pass lists. PR #845's older
skip-bearing baseline uses the independently authenticated bridge above;
generic dot-log migration remains strict for other PRs.

The latest authenticated legacy job may be a retry: its exact source and
exhaustive terminal log still bind the import. Receipt-era retries remain
refused, including skipped jobs, because artifact identity is ambiguous.

Older pre-receipt attempts are outside that migration baseline. Their missing
logs or journals are not converted into passes or assertions of zero
execution. After the source workflow adopts receipts, every executed attempt
requires authenticated start and final journals, even when units fail.
An authenticated cancelled preflight with integer `task_id=0` was never
assigned to a runner and contributes no test evidence. It must have neither
a final journal nor an attempt-start artifact. Historical prepare refusals
can also establish zero execution, but only with the admitted immutable
workflow and runner hashes, exact-source checkout evidence, a preparation
refusal followed by a skipped start-marker step, and a terminal failed-job
log. Partial logs, started journals and unadmitted source still refuse. This
distinguishes job lifecycle from lost test evidence; it neither deletes prior
runs nor fabricates successes. The two lifecycle cases are covered in
[`test_main_unit_receipts.py`](../../tests/validation/test_main_unit_receipts.py).
An incomplete final journal from a failed or cancelled attempt preserves its
individually recorded positive successes; it makes no claim about unrecorded
execution. A missing journal or unresolved fixture error refuses continuation;
retain and recover its actual evidence. Inherited successes require an individually
matching authenticated original journal or the authenticated legacy baseline,
then current method/local-fixture applicability before reuse.

Main queues concurrent PR attempts instead of cancelling the run publishing
its journal. The existing current-head/base promotion guard rejects a stale
candidate. Current history, catalog, Regression-Test fields, Node contracts
and affected compiler gates continue normally. The run command explicitly
declares `--suite-dir tests/validation --suite-dir tests/operations`; the CLI
refuses any other list, and the regression-field checker derives the executed
suite coverage from those declarations. Output separates discovered methods,
historical passes and pending execution; it never claims a retained method
executed on the current source. Unknown method outcomes refuse completion.

The controls live in
[test_main_unit_receipts.py](../../tests/validation/test_main_unit_receipts.py):
exhaustive failure accounting, metadata mismatches, unsupported historical
discovery, skip semantics, no execution of retained successes, failed units
and silent custom runners. The main migration accepts only source-proven
method/local-fixture evidence; the dependency limits below still apply.

## Current applicability — genuine history is not proof of new assertions

CLI `prepare` supplies the mandatory source-applicability consumer before
either CI or locally attested candidates enter the retained union. It reads
the recorded commit's immutable test blob and the dispatched commit's blob
from the local full-history checkout; the existing workflow already uses
`fetch-depth: 0`. It neither fetches source nor transfers a credential. The
current worktree must also equal the dispatched blob. A production `run`
requires that current prepare marker and rechecks its retained pass map before
discovery or method execution, so an unfiltered historical map cannot skip
new assertions. Explicitly injected fake suites remain a test seam, not a CLI
flag or production execution path.

The fingerprint binds the named method's normalized AST, decorators and the
module's non-test fixture context: class bases/decorators, setup/teardown,
helper methods, module fixtures and import declarations. Line numbers,
comments and sibling test method bodies are excluded. Sibling decorators,
default expressions, annotations and type-parameter metadata remain as
body-stripped declaration stubs, in their original order: they can mutate a
fixture when defined or inspected. Bare sibling declarations without that
metadata are omitted. Thus adding or changing
a sibling method does not invalidate a method whose assertions and local
context are unchanged. Shared context changes conservatively invalidate the
methods in that module. Local base classes are source-bound; unsupported
external/dynamic bases, custom discovery or class execution semantics refuse
rather than inventing an applicability result. Removed IDs remain in the
original historical artifacts and do not become pending methods.

Each candidate is checked **before** union, not after the first old ID wins.
A newer matching CI or local candidate can therefore satisfy the current
method even if an older candidate changed or its source is unavailable.
Unknown evidence is deferred only until other authenticated candidates have
been considered. If no applicable candidate resolves it, prepare refuses
before any method runs. An old known-changed body alone becomes pending;
missing source, unsupported normalization or a missing original worktree
witness never authorizes replaying every historical success.

A null-commit worktree proof applies only if its attested whole-file SHA256
equals the actual current file. A mismatch cannot be narrowed to a method
without the original bytes; recover that hash-bound witness or a genuine
current candidate instead of guessing. A local proof with a recorded commit
must also match that commit's exact file hash. Writer authentication and
the existing 64-proof bound remain unchanged. Accepted passes keep their
actual original commit/run/comment; no current execution is fabricated.

**Bounds:** source files ≤1 MiB; cache ≤512 files /32 MiB of source and
1,000,000 AST nodes, with ≤100,000 nodes per file; inheritance depth <16;
fingerprints ≤40,000. Each local Git read has a five-second timeout. Current
POSIX file readers use no-follow/nonblocking opens and verify regular files
before bounded reads. Exhausted bounds refuse evidence; they do not grow
automatically. These are input/cache bounds, not a reserved OS RSS or CPU
allocation.

**How to read output:** `Unit applicability changed` names only a
source-invalidated ID and its old/current fingerprint. Original journals are
unchanged. An `applicability unavailable` refusal names the first unresolved
ID and its evidence boundary; repair that boundary, never reset PR identity
or delete journals to turn unknown history into new work.

**Deliberate limit:** this is a method/local-fixture source guard, not a
universal semantic cache. Import declarations are bound, but imported product
implementations, arbitrary files scanned by a test, environment values and
external services are not transitively fingerprinted. Their applicability
still requires identified source/input evidence and targeted coordination;
this mechanism must not label arbitrary dependency changes proved. The raw
`restore` API without an applicability consumer remains historical diagnostic
attribution only; only the guarded CLI prepare/run path qualifies retained
passes for production execution.

The one combined fake-source control
`tests/validation/test_python_unit_receipts.py::test_current_applicability_rejects_changed_bodies_and_retains_bound_candidates`
exercises actual restore filtering, local/CI candidate selection, changed-only
execution, preserved siblings/provenance, fixture/decorator/base changes,
null-file witnesses, missing/dynamic source, bounds and the production guard.
Its synthetic API/source/runner fixtures are not an actual workflow run or a
claim of arbitrary production-dependency coverage.

The separately new focused edge control
`tests/validation/test_python_unit_receipts.py::test_sibling_definition_metadata_binds_fixture_without_replaying_bodies`
executes only controlled synthetic class definitions to demonstrate decorator,
positional/keyword-default and parameter/return-annotation fixture mutations,
including async sibling declarations. It refuses their changed applicability
while retaining body-only edits. The earlier combined control remains actual
historical evidence; it is not replayed or relabelled as covering this edge.

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

PR #690's first gate API3747/job39644 failed prepare before any marker or
method because ROOT had not yet published the four local-pass attestations.
Comments7174–7177 subsequently authenticated those original hashes. No test
failed and no journal exists for3747; missing evidence is not silently reset.
The separate bounded descriptor
`validation/python-unit-preunit-failure3747.json` pins repository1/PR690,
branch/base, sourcee3ba7cf2c, literal receipt job/attempt1, exact original
workflow/receipt-code hashes, and115485-byte log SHA-256
`8bf64b85652ac5ac328d3f8519e9ed661ad724bbfc84f2a2e0ab3486d6b164df`.
The actual restore consumer verifies live terminal run/job/source/log facts,
the specific attestation prepare refusal before skipped start/final publication
steps, no discovery/unit/fixture phase, and empty3747 start/final/run artifacts.
It imports zero successes and never synthesizes or completes a journal.
Unknown missing journals, source/metadata/hash contradictions, started attempts,
fixture/discovery/unit failures or any3747 artifacts still refuse. Later valid
other-run journals remain separately attributable, not a contradiction of this
zero-unit attempt. Original3705 recovery remains unchanged.

Two new fake-API table methods exercise actual restore acceptance and seventeen
refusal modes; fake fixtures are not actual CI evidence. Read-only live restore
verified the real3747 zero-unit refusal and retained exactly four authenticated
local successes with their original null run/commit and comment provenance.
No methods, local initial journal or CI artifacts were created by that check.

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

## The bounded #742 lost-journal recovery retains 935 original passes

PR #742's API3915 Python job40909 **attempt1** completed successfully at
`5fa478987bf464ef6ab9b50bf49dea842acef018`. Its start marker1630 retained926
passes from run3898/source12990cf0; final1631 retained those unchanged and
added two validation and seven operations methods, each individually logged.
The completed final has935 keys and no fixture errors. Root retained both
original ZIPs before a later non-unit retry; Windows subsequently reports
attempt3, but that is not another Python execution. The live artifact API
now lists neither1630 nor1631 and returns404 for their ZIPs. The association
with that retry does not establish why the server ceased serving them.

`validation/python-unit-lost-journals742.json` embeds those exact archival ZIP
bytes, original source hashes, job/log identities and counts. The implementation
pins its whole-file SHA256 and admits only repository1/PR742, branch
`opus/client-evidence`, the existing effort base, run3915/job40909 and the
separate run3930/job41018 refusal. It cannot enroll an arbitrary run or path.
An authenticated repository writer must first attest the reviewed record hash
on **PR742**, not the recovery task PR:

```text
Python-Journal-Recovery: {"repository":1,"pr":742,"sha256":"31ef59fb7a45a480df9129499d34772fcfe071c03a284a2967caa4e8bd96ed31"}
```

Technical access to the retained ZIPs is not that authority. Missing attestation
blocks. The existing writer-permission check and its exact403 enrolled-owner
fallback apply unchanged. Neither this record nor the comment is a new unit
execution or a synthetic success journal.

Restore checks live original run/job metadata (Python attempt1/success while
the containing run failed), exact original workflow/receipt-code hashes and
log SHA256, absence of live start/final/run artifacts, both archival ZIP
hashes/sizes and single-member safety. Start publication precedes individual
events and final publication follows them. All nine unique positive events
must match the final-minus-start IDs, source/run attribution and suite counts.
The inherited926 keys must equal the retained start and still have their
separately trusted original run3898 journal. Imported maps retain every
original source; the current mandatory applicability consumer still checks
each candidate before union. Recovery is not proof of changed assertions or
arbitrary product dependencies.

API3930/job41018 at `2a299d44b38e67c029b66087f69edc4d5315222e` refused
prepare because3915's final was missing. Its exact source/log hashes, failure
attempt1, named refusal before both skipped publication steps, absence of
discovery/unit/fixture phases and empty artifact lists prove **zero units**.
That separately pinned branch imports no successes and creates no journal.
Started units, live partial artifacts, source/log contradictions, unknown runs,
ambiguous Python attempts or untrusted inherited sources continue to refuse.

The two new synthetic methods in
`tests/validation/test_python_lost_journal_recovery.py` exercise actual restore,
original provenance/applicability, archive and writer binding, phase ordering,
missing/contradictory evidence and the zero-unit branch. Their fake API bytes
are not actual CI evidence. Original935 successes are never replayed by these
controls. No workflow, runner, artifact service, retention policy or compiler
cache changes are made. Durable preservation before any future non-unit retry
remains operational work; this exact recovery is not a generic lost-artifact
fallback or permission to delete markers.

## The bounded #742 pre-unit failure preserves inherited successes

On 2026-10-03, API3994/job41507 at `b18c01904af3f33193be3ee15db88bac69c519c7`
published start1653, failed the corrective-history check before units, then
preserved final1654. Both journals are identical: 936 inherited passes attributed
only to their original runs3898,3915,3950, no fixture errors, and
`complete: false`. The recovery does not complete this attempt or attribute
any success to3994. Every retained entry still needs its separately trusted
original source and current applicability. The history check remains blocking
on the current candidate.

API4002/job41568 at `4a087b75be2ad886f664c72596c36f8cc0311914` subsequently
refused prepare on that incomplete3994 journal. Skipped start/final uploads,
no unit phase and empty live artifact lists prove that4002 imports zero passes.

The hash-pinned [two-case witness](../../validation/python-unit-inherited-failure742.json)
binds those exact PR742 scope/run/job/source/log/ZIP identities. It requires
an authenticated writer's `Python-Journal-Recovery` attestation of its hash on
PR742, using the existing writer check. It has no arbitrary enrollment or
incomplete waiver. Changed maps, new attribution, fixture errors, unknown
outcomes, missing or expired witnesses and contradictory phase evidence refuse.
The existing 64-local-proof bound is unchanged. The new synthetic control
`tests/validation/test_python_inherited_failure_recovery.py::test_exact_inherited_failure_and_empty_refusal_preserve_original_trust`
exercises actual restore and refusal paths without replaying original units.

## Focused proof and limits

The small fake-fixture tests in
`tests/validation/test_python_unit_receipts.py` cover failed-only retries,
new IDs, source preservation, skips, isolation, corrupt/incomplete evidence,
unsafe archives, empty discovery, atomic publication and workflow ordering.
Run each unit regression once per PR; retry only a failure. This change does
not cache compiler or static checks, bypass main qualification, or mutate a
runner service. Legacy runs without sufficiently attributable per-ID evidence
are not automatically labelled successful.

The receipt mechanism is Python-only. Duplicate effort-preflight player-input
and player-DOM executions are removed; both remain in the unchanged mandatory
`web-check` lane. That broad Node lane may run once on the first attempt, but
after a Node success the gate must not be dispatched again until the separate
static-only follow-up or attributable recovery preserves that success. These
Python receipts do not claim to cache Node tests or exempt their contracts.


## Ordinary main-lane continuation

`validation.main_unit_receipts` applies the same source-attribution machinery
to the main fast lane's validation and operations suites. It imports an
exhaustive legacy unittest baseline only when exact-source static discovery
and terminal counts account for every method and every non-success. Subsequent
journals retain original run/commit attribution; changed methods or local
fixtures invalidate their own evidence. This is not a claim that every possible
production dependency change can be inferred from a test's source fingerprint.

Forgejo can remove a run's artifacts when a different job in that run is
retried. The successful Python job remains attempt 1 and retains its log.
New main-lane attempts therefore write bounded, checksummed start/final journal
frames into that log as well as publishing artifacts. Log-only restoration
requires a successful Python job and a completed final snapshot; partial
snapshots remain diagnostic evidence, not an automatic failure waiver.
Restoration authenticates
the same repository, PR, branch, workflow, source and Python job, and still
requires original provenance for every inherited success. Framing detects
truncation or corruption; its checksum does not replace API authentication.

For pre-framing main receipt output, recovery accepts only the reviewed runner
source digests, a successful attempt-1 Python job and inherited successes from
older authenticated evidence. Each explicit pending method must resolve to
immutable method/fixture source in exactly one suite. Discovery and pending
counts, exhaustive verbose outcomes and terminal summaries must all agree.
Only explicit successful methods add passes; optional skips do not. Ambiguous,
incomplete or contradictory evidence stops continuation. Other receipt-policy
attempts without either artifacts or sufficient authenticated logs remain
blocking. This does not allow rerunning the Python job itself or replaying an
entire suite to replace missing evidence.
