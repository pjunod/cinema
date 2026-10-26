# Async qualification phases and whole web cohort retention

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
is pinned. The exact fbbf and observed-main 087 shape families both include
Acorn and the transitively invoked startup-lab helper. Every row under
`tests/web` and `tests/playback` is a reviewed shape, regardless of suffix:
source, bytecode, native modules, extensionless files, package members and data
that can select further inputs. These are raw supported families, with no
applicability normalization: the reviewed changed inputs
still produce a genuinely different key. Executable and loader inputs under
`crates/plurxd/src/web` include `.js`, `.mjs`, `.cjs`, `.ts` (including `.d.ts`),
`.html` and `.json`, with exact member sets. Their changes require closure
review before fresh execution or retention: tests execute production bodies,
HTML selects code and TypeScript configuration can introduce imports. CSS,
images and webmanifest resources remain ordinary raw inputs; their changes
produce a new key without weakening the executable guard. Unknown code, dependencies,
alternate case-folded Make selectors, unsupported workflow shape or untracked closure inputs require closure review;
they never become a silent first-run escape. Ordinary supported non-executable
resource changes produce a different execution key. Baseline files within the
test roots require review too; changed baseline comparison metadata still
affects the raw key. The
key includes event base, resolved merge-base and baseline bytes. Only the
semantic web job recipe is bound; unrelated Rust workflow edits do not change
this cohort's applicability.

The bounded scripts-level resolver inventory binds names, types and modes,
not unrelated script bodies. New Python file/package shadows, including
transitive `socket` imports, and Node's extensionless `playback-frame-clock`
selector require closure review. Finite repository-local ancestor
`package.json` scopes bind reviewed presence, mode and bytes or exact absence,
including scopes above the complete source roots. Live checks apply too;
there is no host-level package crawl.

Committed cache rows remain reviewed shapes. Existing normal untracked
`__pycache__` directories at the scripts and playback Python search roots may
remain untouched only with bounded regular `.pyc` members and no aliases.
Their bytes are not declared equivalent to source. Every fresh child receives
an explicit reserved absent `PYTHONPYCACHEPREFIX` beneath its owned output and
`PYTHONDONTWRITEBYTECODE=1`. A sys-only observation confirms the effective
policy; the prefix must remain absent. This redirects reads away from old
source caches and prevents writes, including for Python spawned by Node.
The fixed policy and observed interpreter are bound in fresh receipts without
the run-specific absolute prefix. Retention executes no current test helper;
legacy cache behavior remains unrecorded. Tracked source and live selectors
are rechecked before the final cohort receipt.

The bootstrap origin is the successful web job in run 5964, job 57771, task
23006, attempt 1 at `fbbf4078`. Overall Rust failure does not erase that web
success. Its complete raw log SHA-256, exact checkout, API event/job and
reviewed original job recipe authenticate the execution. Matching known
inputs with missing or altered origin evidence refuse; they never rerun the
completed cohort as fallback.

The versioned runtime contract requires Linux/X64 host execution and the
recorded Node 22.23.4, npm 10.9.9, Git 2.55.0 and locked TypeScript 6.0.3.
The current setup-node recipe provisions exactly 22.23.4; the legacy recipe
remains its historical major-22 declaration, authenticated by the observed
22.23.4 execution. A cached different patch is unsupported. CLI refusals expose
only a fixed stage and allowlisted reason code, without exception or API text.
Python stdlib orchestration and Make's fixed dispatch use an explicit
compatibility contract. Historical distro/kernel, Python/Make versions and
immutable host/image identity were not recorded and remain unknown. Current
Python implementation/version and Make version are recorded diagnostically;
version-1 adoption requires the declared stdlib planning/summary/loopback HTTP
policy, with no fabricated historical Python identity. Retention identifies
the current declared/locked TypeScript without claiming it executed; fresh qualification
checks the installed compiler's actual version after the existing installer.
The selected startup tests also execute the pinned Python stdlib planning and
local HTTP fixture paths. Browser, FFmpeg generation and host-proc helper modes
remain outside the audited calls; adding such a call requires closure review.

Version-1 archives bind fresh or retained receipts and the original cohort
log to the authenticated upload. A retained receipt points directly to the
legacy execution or an authenticated version-1 fresh execution; retained
chains and cycles refuse. Pre-v1 heads without eligible cohort evidence are
outside this protocol, not proven passes or input differences; they do not
poison new-key execution through current-family parsing. A matching v1
producer with missing, invalid or ambiguous evidence still refuses. Raw source
manifests and log bytes remain original.

Web producer validation recognizes the exact complete reviewed predecessor
adapter SHA-256 `fa61446a54fb35cf1c836dd6dac8b5fe90dfb239326345c8bdedcc33aa4485ef`
and the current complete adapter source. Each receipt's adapter hash binds its
own producer source. Unknown changed bytes refuse; the execution-policy,
runtime, recipe, source, archive, upload and raw log/key checks remain exact.
No arbitrary historical source is made compatible by matching selected text.

Effective-baseline applicability copies each independently reconstructed
manifest and removes only `baseline.event_base`. It preserves the actual Git
merge base, baseline byte hash and every other source/member/mode/runtime/
namespace/policy field. Both baseline commit identities and the exact baseline
shape are validated first. Different raw event bases still produce different
raw execution keys; a retained current receipt keeps its own key and points
directly to the original execution with its unchanged key, receipt and log.
The authenticated original inputs travel internally alongside that pointer.
Different effective baselines or other covered inputs cannot match, and
retained-to-retained chains remain forbidden. The predecessor's stricter raw
key equality for its own retained receipts is still checked.

Producer eligibility precedes source compatibility: an authenticated terminal
failed or cancelled web job with a complete artifact listing containing no
cohort artifact offers no accepted whole-cohort result. Its execution remains
unknown; this classification claims neither zero execution nor passing cases.
Its negative-only identity check permits a typed task ID of zero for an
unassigned terminal job, while requiring stable positive job/repository/run
identities and exactly attempt 1. Current and successful producer checks still
require a positive assigned task; booleans, negative and noninteger task IDs
refuse. Artifact-bearing or ambiguous jobs cannot use the negative predicate.
It permits inspection of older eligible evidence without requiring a failed
nonproducer's old adapter to match the current one. Success with missing
evidence, malformed or artifact-bearing producers, ambiguous metadata and
active jobs retain the refusal rules. Positive result authentication and
source compatibility remain unchanged.

The receipt reports current applicability, policy/adapter identity and origin
provenance. It does not fabricate current build products or test events.
