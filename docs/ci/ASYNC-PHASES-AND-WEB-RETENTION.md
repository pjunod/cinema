# Async qualification phases and whole web cohort retention

Status: implemented, independent source review and affected execution pending.
The phase recovery follows run 5964's Rust job deadline; its incomplete product
exploration is not a proof result. This document defines the new transport and
execution boundaries, not a claim that qualification has completed.

## Separate budgets, unchanged proof limits

The main fast lane compiles and lints in `fast Rust gate`, then runs selected
async qualification in the dependent `fast async driver refinement gate`. Each
job has its own 90-minute budget. Both are promotion dependencies; selected
qualification must succeed. They use the same pinned toolchain, FFmpeg setup,
one compilation worker and persistent cache lane. No target tree is copied.
The 20,000 Store identities, 256 MiB wire, 512 MiB artifact, 1800-second case
limit, TLC 4 GiB heap and one TLC worker remain unchanged.

The oracle measures host canonical/domain/invariant/control/replayed-apply
costs separately. It removes only the second parent canonical/domain read
pair after the demand savepoint. The full first parent checks, replay refusal
atomicity, successor checks, exact rollback checks and poisoning remain.

## Checkpoint before graph-product

`scripts/check-async-driver-refinement build-prefix` derives its cases from
the ordered `CASES` registry up to `graph-product`. Every selected case must
validate through ordinary exact case reuse. The journal remains incomplete;
valid later rows already present are preserved but cannot establish whole
completion. The workflow snapshots and uploads the immutable prefix before
starting full `build-inputs`, which revalidates the same local successes.
The final complete or partial snapshot has a different output directory and
artifact name. A cancellation that prevents final upload leaves the earlier
prefix upload intact.

The async adapter alone accepts positive attempts. It authenticates the
current ready same-repository PR, live head/base, checkout and exact active
proof job. Worker and preflight attempt-1 policies remain unchanged. Current
and future attempts cannot be restored. Forgejo lists the latest task; older
candidate attempts are discovered from bounded run artifact names, then use
the documented job-log `attempt` query. Their trusted first runner preamble
binds the historical task. A later authenticated attempt establishes that an
older attempt is no longer active; its old overall result stays unrecorded.
The latest attempt of another run must have an authenticated terminal status.

Each phase binds API run/job/attempt/artifact IDs, name and size, the uniquely
matched upload transaction SHA-256, checkout and snapshot source/task/file
hashes. Final evidence is preferred; invalid final evidence can fall back to
the separately authenticated prefix. Every restored case is still untrusted
until the runner rechecks source, tool, command, logs, capture and graph.
Invocation paths remain exact: this phase/transport change invalidates the
old fifteen cases. The pre-timeout SSH copy is supplementary evidence only.

## The one whole web cohort

`validation.web_cohort_retention` keeps the real web job selected. Its wrapper
retains a supported complete earlier execution or runs the unchanged syntax,
TypeScript and Node/Make commands for new inputs under
`--fresh-on-new-inputs`. It creates no current test pass events on retention.
API tokens stay in the parent; fresh children receive no API credentials.

The source-reconstructed closure contains every tracked regular path/mode/blob
and SHA-256 under the audited test, playback, daemon-source and fixture roots,
plus their external contract data, helpers, Make/Cargo parse inputs and locked
TypeScript configuration. The reviewed executable/helper/member-set aggregate
is pinned. Unknown code, dependencies, alternate case-folded Make selectors,
unsupported workflow shape or untracked closure inputs require closure review;
they never become a silent first-run escape. Ordinary supported production,
resource or baseline input changes produce a different execution key. The
key includes event base, resolved merge-base and baseline bytes. Only the
semantic web job recipe is bound; unrelated Rust workflow edits do not change
this cohort's applicability.

The bootstrap origin is the successful web job in run 5964, job 57771, task
23006, attempt 1 at `fbbf4078`. Overall Rust failure does not erase that web
success. Its complete raw log SHA-256, exact checkout, API event/job and
reviewed original job recipe authenticate the execution. Matching known
inputs with missing or altered origin evidence refuse; they never rerun the
completed cohort as fallback.

The versioned runtime contract requires Linux/X64 host execution and the
recorded Node 22.23.4, npm 10.9.9, Git 2.55.0 and locked TypeScript 6.0.3.
Python stdlib orchestration and Make's fixed dispatch use an explicit
compatibility contract. Historical distro/kernel, Python/Make versions and
immutable host/image identity were not recorded and remain unknown. Current
Python/Make versions are diagnostics. Retention identifies the current
declared/locked TypeScript without claiming it executed; fresh qualification
checks the installed compiler's actual version after the existing installer.
Optional browser, FFmpeg and host-proc helper modes are outside the audited
executed calls; adding such a call requires a new closure review.

Version-1 archives bind fresh or retained receipts and the original cohort
log to the authenticated upload. A retained receipt points directly to the
legacy execution or an authenticated version-1 fresh execution; retained
chains and cycles refuse. Raw source manifests and log bytes remain original.
The receipt reports current applicability, policy/adapter identity and origin
provenance. It does not fabricate current build products or test events.
