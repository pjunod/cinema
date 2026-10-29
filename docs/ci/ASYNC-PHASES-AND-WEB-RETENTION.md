# Async qualification phases and authenticated web commands

**Status:** open — implementation reviewed; qualification pending.

The phase recovery follows run 5964's Rust job deadline; its incomplete product
exploration is not a proof result. This document defines the new transport and
execution boundaries, not a claim that qualification has completed.

## Separate budgets, unchanged proof limits

The main fast lane compiles and lints in `fast Rust gate`, then runs selected
async qualification in the dependent `fast async driver refinement gate`. Each
job has its own 90-minute budget. Both are promotion dependencies; selected
qualification must succeed. They use the same pinned toolchain, FFmpeg setup,
one compilation worker and persistent cache lane. No target tree is copied.

Each job selects its own bounded Cargo home; the cache lane does not guarantee
a registry handoff between containers. Immediately after proof-job cache
selection, a separate provisioning step runs `cargo +1.97.1 fetch --locked
--manifest-path Cargo.toml`, repeats the fetch with `--offline`, and checks
`git diff --exit-code -- Cargo.lock`. It uses that selected Cargo home and
fetches the workspace lock's full target set. Provisioning failure stops before
evidence preparation or any proof case. The existing offline case commands
remain unchanged; there is no online retry after a case failure or copied credential
handoff. The workflow change genuinely invalidates earlier invocation-bound
case rows, including run 6034's accepted selector witness; original evidence
is preserved without granting it acceptance for the changed invocation.

The 20,000 Store identities, 256 MiB wire, 512 MiB artifact, 1800-second case
limit, TLC 4 GiB heap and one TLC worker remain unchanged.

The oracle measures host canonical/domain/invariant/control/replayed-apply
costs separately. It removes only the second parent canonical/domain read
pair after the demand savepoint. The full first parent checks, replay refusal
atomicity, successor checks, exact rollback checks and poisoning remain.

## Ordinary PR authority preserves the recorded baseline

The ordinary main fast lane authorizes the checked-out, API-authenticated PR
head. The shared `main_preflight_adoption.validate_live_pr` check is used by
preflight/Python/controller context and the async, web and native adapters.
It requires the same typed PR/repository identities, head SHA and head ref,
open/non-draft state and base ref `main`. Both recorded and live base SHAs must
be well formed; a live main advance does not replace the authenticated event
baseline or require the tips to stay equal. Initial and final run/event/head
checks remain strict. Effort/manual qualification still binds its expected
head and base tips separately; this ordinary-lane rule does not alter it.

Async invocation provenance explicitly includes the shared adoption module.
Historical event authentication is separate from current live authority, and
an old producer's head is never compared to today's PR head. A head-only pass
proves that source head; it is not a merged-tree proof of later upstream bytes.

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

## One web controller preserves command origins

`validation.web_unit_receipts` is the single active web controller. It may
retain independently authenticated original passes for the fixed
`jsconfig-generated`, `player-typedef` and `suite` commands, or execute the
actual raw command when history is unavailable or inapplicable. Each report
under `.main-preflight-receipts/web-unit` distinguishes adopted from executed.
Only a successful actual raw execution emits its original-pass marker.
An adopted pass creates no new original execution or wholly fresh cohort.

Syntax parsing and TypeScript are fixed, non-adoptable current commands in
that same controller. The authenticated PR event base and resolved merge-base
select the TypeScript baseline; the installed compiler must report6.0.3.
Node22.23.4, npm10.9.9 and Git2.55.0 remain the reviewed concrete runtime.
The exact workflow recipe and controller/helper source are authenticated
before current execution and before historical adoption. Unknown current
shapes refuse; they never become an unreviewed fresh fallback.

`validation.web_cohort_retention` now supplies only shared guards; obsolete
cohort readers, CLI and execution orchestration are removed.
The workflow publishes no new `web-cohort-*` artifacts. Historical archives
and their original logs remain unchanged and do not acquire successor credit.

The shared guard retains the closed executable/helper/dependency manifest,
complete test/playback member sets, source-loader extensions, scripts resolver
namespace, ancestor Node package selectors and exact Make selection. Ignored
or untracked inputs, symlinks, unsupported imports and interpreter environment
selectors refuse before adoption or execution. The specific Main5ac9 source
family includes the reviewed subtitle web/test change and merged rolling-owner
census; merely changing a hash does not authorize another family.

API authentication stays in the parent. Raw syntax, TypeScript, Node and Make
children receive the credential-scrubbed environment, an owned absent
`PYTHONPYCACHEPREFIX` and `PYTHONDONTWRITEBYTECODE=1`. Existing bounded regular
source caches remain untouched and are not equated to source. Live selectors,
source and manifest are checked again after command execution and before an
adoption report; a failed current guard cannot enter historical fallback.
No new runtime or qualification result is claimed by this source integration.
