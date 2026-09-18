# Weekly review remediation — build the missing security, recovery, and playback contracts

**Status:** Open implementation handoff, 2026-09-09. Findings refer to the frozen
review revision below; implementation and current-head qualification remain open.
**Owner:** Sol implementation task, with Paul owning product policy decisions.
**Executes:** The September 3–9 architecture, security, and end-to-end review.
**Companion:** [Settings navigation and Developer build](../features/SETTINGS-NAVIGATION-AND-DEVELOPER-IMPLEMENTATION.md).

## 1. Deliver a bounded correction pass, then stop

**Paul's final direction.** Keep development moving. This is one practical
correction pass plus a documented backlog, not a mandate to implement an entire
security platform. Do not make completion depend on every recommendation below.
Do not add feature gates: an explicit enable/disable option with requirements and
observed status in Developer is enough. Missing qualification, receipts,
benchmarks, fleet approval, or test runs must not prevent an enabled feature from
being attempted. Requirements are information, not a hidden policy engine.

Authentication, authorization, valid input handling, actual resource availability,
and protocol correctness still govern individual operations. These are not
certification gates. Keep that distinction explicit: report a real failed
operation; do not refuse to try because a qualification record is absent.

Use the current high-velocity CI/CD process unchanged: no extra review panel,
extra main gate, mandatory broad runtime suite, or automatic full-test schedule.
Collect focused evidence appropriate to the changed behavior. A recommendation
for testing is not an instruction to wire that test into the promotion gate.

This handoff is separate from the approved settings layout. It carries forward
all fifteen findings and eight broader improvements from the review.
It includes inherited limitations because the request covered the whole system,
but does not attribute every limitation to the week's changes.

| Review boundary | Recorded value |
|---|---|
| Window | September 3–9, 2026, America/New_York |
| Baseline before the window | `1d8788d87cb9c9cff330ed985429917aca554ed3` |
| Frozen reviewed main | `4cef0da740c797364023284520adad8a93b172cd` |
| Last included merge | #224, September 9 at 20:50 EDT |
| Inventory | 864 changed files; 239,508 additions and 35,935 deletions |
| History | 1,371 reachable commits including merges; 124 first-parent entries |
| Excluded | Uncommitted changes, unmerged work, and later main commits |

The review inspected the full inventory and traced selected high-risk paths.
It was not an audit of every changed line, a fresh dependency vulnerability scan,
or evidence of a compromise. The original local report and source snapshot may
be available in `tmp/review-2026-09-09/`; they are optional reference material,
not dependencies for this handoff. Historical evidence can be recovered from
the exact reviewed Git revision. Source links below navigate the checkout and
may differ from that revision; locate symbols, not historical line numbers.

**First deliverable.** On current intended main, record its exact SHA and
revalidate each item in section 2. For every item record present, already fixed,
needs reproduction, or superseded, together with a source reference and evidence.
An existing fix needs verification, not a duplicate implementation. Preserve
unrelated local CI and documentation changes. The settings prototype is local
uncommitted design work and was excluded from the frozen review.

**Scope boundaries.** Do not redesign the media product, replace Raft, split the
system into microservices, change Live TV fencing semantics, or weaken
authentication or physical-fencing protections merely to simplify these fixes.
Remove readiness/certification-only enablement gates encountered in the selected
feature paths, while preserving actual operation error handling. Do not carry
credentials or `.git` into a compiler container. This handoff does not authorize
production deployment, destructive recovery drills on real data, or mutation of
live account credentials. Use isolated fixtures for security and recovery tests.

## 2. Keep every finding traceable

P1 means early work for a high-impact security or recovery gap; P2 means a
material defect or hardening improvement. Priority is scheduling guidance, not
a claim of exploitability in every installation.

| ID | Priority / confidence at reviewed head | Proposed outcome; scope in section 3 | Package |
|---|---|---|---|
| F01 | P1, inherited; three client paths traced | Sign Out attempts server revocation; session lifecycle becomes explicit | R03 |
| F02 | P1, source-confirmed new coordination cost | Healthy quorum can revoke ordinary credentials without all-member availability | R03 |
| F03 | P1, conditional active-network attack; no live exploit run | Revocation acknowledgements authenticate the responder and exact exchange | R04 |
| F04 | P1, inherited documented trust boundary | Cluster TLS verifies per-node identity and current authorization | R04 |
| F05 | P1, explicitly documented gap | Consistent portable backup and supported catastrophic restore | R05 |
| F06 | P1, explicitly documented gap | Supported daemon-owned local administrator recovery | R03 |
| F07 | P1, isolation gap enlarged by Live TV; no parser exploit identified | Untrusted media workers cannot inherit daemon secrets and unrestricted authority | R06 |
| F08 | P2, source-confirmed resource risk | Bounded password hashing off async workers with admission control | R02 |
| F09 | P2, focused web reproducer executed | Handoff preserves mute and playback rate | R01 |
| F10 | P2, Apple source hypothesis requiring device reproduction | Fresh commit alignment and bounded incumbent recovery | R01 |
| F11 | P2, Android parameter omission confirmed; timing needs device evidence | Preserve playback parameters and prove final alignment | R01 |
| F12 | P2, source-confirmed deadline/publication gaps | Cancellable bounded guide refresh; stale work cannot replace newer results | R02 |
| F13 | P2, source-confirmed unbounded optional probe | Measurement cannot indefinitely hold startup or abandon its child | R02 |
| F14 | P2, workflow portability risk; recheck ongoing CI work | Workspace and fuzz audit verdicts survive Forgejo reporting differences | R07 |
| F15 | P2, mixed inherited/new supply-chain risks | Pinned inputs, constrained runners, authenticated release artifacts | R07 |
| A01 | Architectural improvement | Smaller internal ownership boundaries and one playback authority | R08 |
| A02 | Cross-cutting improvement | Shared deadline, cancellation, admission, and child ownership contracts | R02 |
| A03 | Performance hypothesis; benchmark first | Cheap warm decode-fact lookup without weakening artifact identity | R08 |
| A04 | Product and state-model improvement | Requested, pending, and effective playback state are distinct | R01 + companion UI plan |
| A05 | Operational improvement | Explicit Live TV owner recovery and idempotent ambiguous-start handling | R09 |
| A06 | Validation improvement | Shared protocol conformance plus real-device qualification | R01 + section 13 |
| A07 | Documentation improvement | Current security and operations claims match evidence | All packages + R08 |
| A08 | Security design improvement | Scoped credentials, redaction, rotation, and explicit offline limits | R03, R04, R05, R07 |

**Evidence corrections to preserve.** F10 is not a demonstrated Apple-device
failure. F03 does not establish arbitrary admin mutation access: the stale proof
is for scoped diagnostic reads, has a five-minute lifetime at the reviewed
revision, and can be cleared sooner by background projection. F04's WebSocket
challenge does not transmit the raw shared secret; the issue is relay across
unauthenticated TLS channels. F13's outer startup already races shutdown; the
missing contracts are bounded measurement and child cleanup on cancellation.
F14 must be reconciled with the user's ongoing CI edits before changing workflows.

## 3. Select the small fixes; retain the large designs as a backlog

The detailed packages below preserve the full review so useful ideas are not
lost. **They are specifications for selected work, not nine mandatory projects.**
Use the following boundary to decide what Sol builds now.

| Work | First correction pass | Stop or defer when |
|---|---|---|
| R00 baseline | Recheck findings against current main; skip already fixed items | Do not repeat the entire weekly review |
| R01 playback | Web mute/rate; Android parameter transfer; focused Apple/alignment investigation | Native timing is not reproduced, device unavailable, or a new player architecture is needed |
| R02 bounds | Bounded hashing, guide cancellation/publication, startup child lifetime | Avoid a generic framework rewrite; extract only the small helpers these fixes share |
| R03 auth | All-client server logout integration; truthful offline result | Quorum authorization redesign, expiry/inventory, and local recovery are separate backlog work |
| R04 peers | Signed acknowledgement correction if supported by a narrow compatible protocol change | Fleet enrollment/mTLS or substantial mixed-version migration is required |
| R05 recovery | Preserve the gap and proposed restore contract in this handoff | Full backup/restore is a separately scoped capability, not a prerequisite for this pass |
| R06 media | Remove unnecessary inherited environment/descriptors where safely demonstrable | New worker identities, sandbox packaging, and GPU isolation need a separate delivery |
| R07 CI/release | Reconcile existing CI work; repair audit verdict handling and narrow pin/credential issues | New attestation infrastructure, registry migration, or runner replacement is required |
| R08 structure/perf/docs | Correct stale claims touched by fixes; reuse existing boundaries | Broad refactors and cache redesign wait for measured need |
| R09 Live TV ops | Document the operational gap and preserve existing fencing | New durable start reconciliation is separate unless an existing primitive makes the fix small |
| Feature readiness | Remove readiness-only blocking in selected feature paths; keep enable/disable and advisory status | Do not build another qualification or override system |

**Execution order.** Make the definite client corrections first, then bounded
server work, then narrow peer/audit corrections. Group adjacent edits into a few
coherent changes; do not turn every finding into a separate ceremony. A
hypothesis gets one focused reproduction attempt using the available harness or
device. If evidence is unavailable, record the exact remaining test and move on.
Do not spend repeated cycles inventing a speculative fix.

**Stop condition.** Finish when the selected small corrections compile, their
focused behavior checks pass, and the report states which high-impact risks
remain. Deferred work is not fixed, but it does not block completing this pass.
Do not automatically proceed into the large designs. Paul can choose them later
as concrete, separately bounded capabilities using the specifications here.

**Architecture decisions only when needed.** For a selected change crossing a
trust boundary, add a short decision paragraph to the relevant existing doc:
actor, authority, failure behavior, migration, and residual risk. Do not create a
standalone threat-model/ADR project as a prerequisite for simple corrections.
For later R03–R06 work, explicitly decide diagnostic access during quorum loss,
node identity, backup custody, and worker privileges before implementing those
boundaries. These choices matter to those capabilities, not to the web mute fix.

**Coordination.** R01 and the companion both touch the web file. Port their edits
selectively and integrate without overwriting either. Native behavior evidence
is useful to the UI's advisory status, but must not become an enablement gate.

## 4. R01 — preserve playback intent through a prepared handoff

**Delivery:** Small corrections now; native timing redesign only with reproduced evidence.
**Owns:** F09, F10, F11, A04, A06.
**Source map:** Web `commitPreparedReplacement` in
[index.html](../../crates/plurxd/src/web/index.html); prepared replacement paths in
[Apple PlayerController](../../clients/apple/Sources/PlayerController.swift) and
[Android Controller](../../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt);
server authority in [playback_control.rs](../../crates/plurxd/src/playback_control.rs).

**Observed boundary.** The web commit explicitly unmutes the spare and does not
copy playback rate. Apple prepares a paused successor using an early film-time
anchor, later measures the incumbent, and replaces its item without a clearly
proven final realignment or retained incumbent item. Android initially aligns
once, checks buffered runway at readiness, then copies volume without copying
playback parameters. Buffer coverage is not proof that the successor's current
presentation position matches the incumbent at commit.

**Build steps.**

1. Define a handoff intent snapshot: requested playing/paused state, mute,
   volume, rate, Android pitch, selected audio/subtitle intent, presentation
   mode, current generation, and authoritative film-time mapping. Distinguish
   user intent from transient player state. Do not overwrite a newer pause,
   seek, track choice, or rate change with an older preparation snapshot.
2. Fix the web parameter transfer directly. Read the current incumbent state
   at the commit boundary, apply it before exposing the successor, and keep
   existing generation and actual buffer/presentation checks. Qualification records
   do not control enablement. Never introduce an audible frame
   from a muted session.
3. Instrument Apple and Android with monotonic preparation/commit timestamps,
   incumbent and successor film positions, seek completion, buffered runway,
   first presented frame, generation, and rollback result. Redact session
   capabilities. Reproduce before deciding the exact native seek strategy.
4. For a confirmed alignment gap, perform a final alignment using the current
   incumbent and the platform's actual seek semantics. Account for the incumbent
   advancing while an asynchronous seek completes. Recheck generation,
   cancellation, presentation drift, and runway before committing.
5. Retain a viable incumbent until the successor proves presentation, or define
   a bounded reopen/rollback protocol if a platform cannot retain the old item.
   Apple item replacement must have an explicit failure path. Preserve Android's
   existing retained-player rollback instead of replacing it with reopen-only.
6. Define shared conformance traces while retaining platform-specific player
   adapters. A shared protocol does not imply identical AVPlayer and Media3
   implementation details.

**Acceptance.** Web mute, rate, pause, and changes made during preparation survive
commit. Native tests cover preparation delays of 2, 8, and 12 seconds, changed
rate, seek during preparation, cancellation, first-frame failure, and insufficient
runway. Record observed drift and rollback latency against documented existing
product tolerances; if no tolerance exists, choose and document one before
claiming seamless behavior. A native hypothesis may be closed as not reproduced
with evidence and a retained conformance case, not a speculative rewrite.

**UI coordination.** The companion moves the quality-switch setting and improves
readiness; it does not fix these handoff behaviors or certify devices. Keep
requested/effective/pending state truthful. Coordinate edits to the shared web
file so neither task overwrites the other's changes.

## 5. R02 — bound work from admission through cleanup

**Delivery:** Bounded corrections now; no general-purpose executor rewrite.
**Owns:** F08, F12, F13, A02.
**Source map:** [auth.rs](../../crates/plurxd/src/http/auth.rs),
[live_tv.rs](../../crates/plurxd/src/live_tv.rs),
[decoder_health.rs](../../crates/plurxd/src/decoder_health.rs), and
[startup](../../crates/plurxd/src/main.rs).

### 5.1 Password verification

Move Argon2 verification from the async request worker into bounded blocking
execution. Acquire admission before scheduling expensive work. A permit remains
owned by the actual hash computation until it ends, even if its HTTP request is
cancelled; releasing on request drop would allow abandoned hashes to exceed the
limit. Bound pending admission as well as active work. An unlimited
`spawn_blocking` queue is not a fix.

Keep comparable invalid-credential behavior for unknown accounts and wrong
passwords. Bound accepted password input consistently across login and password
creation without silently invalidating existing supported credentials. Add a
rate policy that combines source and account signals without enabling easy
account lockout or username enumeration. Trust forwarding headers only from
configured trusted proxies. Define deliberate 429/rate and 503/capacity responses
with retry guidance. Do not log submitted credentials.

**Evidence.** Saturated login traffic stays within measured CPU/memory and queue
bounds while legitimate playback/control latency remains acceptable. Cancelled
requests cannot release hashing capacity early. Unknown-user timing and status
behavior do not expose an account inventory.

### 5.2 Live TV guide refresh

At the reviewed revision, up to 64 sequential page requests each have an
individual timeout, DNS is outside that fetch budget, manual and background
refresh do not share one admission path, and an older completion can replace a
newer cache. The generation filter can hide stale content but cannot recover the
newer good cache it displaced. The background loop awaits refresh before
observing shutdown and does not promptly wake on changed configuration.

Use one operation context with a monotonic total deadline and cancellation for
configuration change and shutdown. Include resolution, network requests, bounded
body reads, and parsing in the budget. Preserve URL restrictions, pinned
resolution/SSRF defenses, redirect policy, and byte caps. Parsing remains bounded
and must not block the async executor with unbounded CPU work.

Unify manual and background admission with single-flight or a small explicit
queue. Identify publication by configuration generation **and** refresh sequence;
one generation can have overlapping requests too. Publish only if the operation
is still current, through a compare-and-publish rule. A late cancelled result
cannot replace cache contents or title projections. Define partial success when
a valid bulk guide is available but extension pages exceed the remaining budget.
Report freshness, truncation, and errors without exposing provider credentials.

**Evidence.** Fake DNS, slow pages, stalled parsing, a configuration change,
shutdown, two same-generation refreshes, and old-after-new completion all have
deterministic expected results. Healthy cache remains usable on a failed refresh.
Measure the chosen total deadline and cancellation latency; do not multiply a
per-request timeout into an accidental many-minute operation.

### 5.3 Optional startup measurement and process ownership

The FFmpeg measurement uses unbounded `Command.output()` capture and an
unbounded wait; artifact hashing also needs a size/time contract. Use a shared
process supervisor with total and per-step deadlines, capped stdout/stderr,
explicit environment, child ownership on cancellation, and kill/reap handling.
Use process-group cleanup where descendants can be created. Account for pipe
backpressure after reaching an output cap; merely stopping reads can deadlock
the child. Inspect oversized or changed artifacts safely and classify measurement
failure as unavailable evidence rather than fabricated qualification.

The outer startup already selects shutdown against the probe. Preserve that
behavior and prove that cancelling it cleans up the child. Optional measurement
failure must follow a documented bounded startup policy and never silently enable
a passed qualification status. Missing measurement must not override an explicit
feature enablement or prevent an attempt; report unavailable evidence and handle
actual launch failures normally. Keep admission until underlying blocking/process
work ends.

**Evidence.** Hanging child, excessive output, ignored termination, cancellation,
oversized artifact, and failed probe leave no owned processes and yield a bounded,
truthful startup result. Reuse this supervisor in R06 where appropriate without
making every platform use one unsuitable launch mechanism.

## 6. R03 — make session revocation and local recovery coherent

**Delivery:** Section 6.1 now; sections 6.2–6.4 are separately scoped backlog.
**Owns:** F01, F02, F06, A08.
**Source map:** [server logout](../../crates/plurxd/src/http/auth.rs),
[revocation coordinator](../../crates/plurxd/src/http/internal_auth_revocation.rs),
[store authentication](../../crates/plurx-core/src/store/hiqlite.rs),
[membership roster](../../crates/plurx-core/src/cluster/membership.rs),
[Apple AppModel](../../clients/apple/Sources/AppModel.swift),
[Android AppViewModel](../../clients/android/app/src/main/java/tv/plurx/app/ui/AppViewModel.kt),
and the web logout function. Read [SECURITY.md](../SECURITY.md) before changing
cache-proof or recovery claims.

### 6.1 Ship the client integration correction first

Capture the current server origin and token, submit a bounded authenticated
`POST /api/v1/auth/logout`, then clear local credentials and session state on
all outcomes. Never send the captured token to a newly selected server. Prevent
late completion from clearing a newer login. Distinguish confirmed server
revocation from offline/local-only sign-out in accessible UI without preventing
the user from leaving the session. Never persist a revoked token merely to retry
later unless an explicitly reviewed secure queue design requires it.

**Acceptance.** Each real client signs in, retains a test copy of its bearer,
signs out, and proves that bearer receives 401 after confirmed revocation.
Cover offline sign-out, server failure, slow response, and sign-out followed by
immediate new sign-in. A local storage assertion alone is insufficient.

### 6.2 Separate ordinary authorization from outage diagnostics

The reviewed coordinator deliberately requires every committed member, including
learners, to acknowledge cache exclusion before a security mutation. That makes
a healthy quorum unable to revoke credentials during a minority outage. Its
fail-closed intent is valid; simply skipping an unavailable peer is unsafe while
that peer can still accept a cached administrator proof.

**Recommended architecture.** Ordinary bearer authorization, including remote
administrator diagnostic access, requires a quorum-authoritative decision.
Outage diagnostics use an independently OS-authenticated local daemon interface.
Durable token deletion or authentication-generation changes commit through
quorum. Isolated nodes fail closed for ordinary bearer access and cannot keep
accepting stale privileges from a process-local proof cache.

If remote diagnostics during quorum loss are retained, explicitly define a
separate restricted credential, issuance rules, route scope, lifetime, maximum
stale privilege window, and UI wording. That is a policy alternative requiring
a documented decision; do not preserve a general administrator bearer cache
while claiming immediate global revocation.

**Build steps.** Inventory every authorization guard and cached proof consumer.
Define the authority/freshness requirement per route. Centralize enforcement so
HTTP handlers cannot accidentally bypass the generation or revocation rule.
Design the durable migration and membership capability marker before removing
the current all-member exclusion guard. Older nodes must be upgraded, fenced,
or prevented from serving the superseded authorization mode before activation.
Cancellation and coordinator loss must not leave a security mutation reported
successful without durable containment. Record an explicit refusal when quorum
is unavailable.

**Acceptance.** Logout, reset, demotion, and deletion succeed with a healthy quorum
and an offline voter or learner; stale credentials fail on every serving ordinary
API path. Cover membership change, joining/rejoining node, leader change,
cancellation, restart, and mixed versions. Verify any separately chosen stale
diagnostic-access bound. Do not assert both zero stale access and availability
without a mechanism that enforces them.

### 6.3 Add daemon-owned local administrator recovery

The documented activated-store reset command refuses unsafe offline sidecar
mutation. Preserve that refusal until a supported daemon path exists. Add an
OS-authenticated local control interface with socket/pipe permissions and peer
identity checks appropriate to each supported host. A public HTTP endpoint with
a special bypass header is not a local recovery channel.

Run password/reset mutations through the live daemon and authoritative store;
revoke relevant existing sessions and redact audit events. Require appropriate
local operator authority, report the target account and result, and avoid
passwords in command arguments or logs. During quorum loss, expose only scoped
local diagnostics; credential mutation waits for quorum or R05's supported
recovery process. Never write directly into an activated database.

### 6.4 Complete the credential lifecycle

Add session inventory, creation/last-use metadata at an appropriate write rate,
expiry and renewal policy, and revoke-one/revoke-other-session actions. Define
old non-expiring-token migration and client renewal behavior before enforcing
expiry. Store credentials using platform-appropriate secure storage; inventory
browser exposure and limit what tokens can authorize. Separate correlation IDs
from bearer capabilities in URLs, telemetry, and support bundles. Test redaction
with canary credentials across normal and error paths.

Document the limits of offline leases and downloaded media. Revoking a server
credential cannot erase bytes already copied to an untrusted device; do not
promise DRM or remote deletion as a side effect of Sign Out.

## 7. R04 — authenticate peer exchanges and node identity

**Delivery:** Narrow acknowledgement correction now if compatible; node identity
and fleet transition are separately scoped backlog.
**Owns:** F03, F04, A08.
**Source map:** [peer_transport.rs](../../crates/plurxd/src/http/peer_transport.rs),
[revocation handlers](../../crates/plurxd/src/http/internal_auth_revocation.rs),
[Hiqlite TLS](../../vendor/hiqlite/src/tls.rs), and
[challenge_response.rs](../../vendor/hiqlite/src/network/challenge_response.rs).

**Bounded correction.** If the revocation protocol remains after R03, use the
existing exact-request-and-response authentication mechanism, including receiver
response signing. Bind status, bounded body, request nonce, path, and target
identity; verify the existing mechanism actually covers the required fields.
Audit other control operations that accept an `ExactRequest` response as evidence.
Ensure direct peer traffic does not unexpectedly inherit ambient HTTP proxies.

Reproduce in an isolated proxy harness: suppressed Begin with forged 204,
replayed response, wrong target, altered status/body, and Begin/End between
background projection polls. Prove acknowledgement rejection separately from
whether a cached diagnostic proof remains usable. The latter is the limited
impact claimed by F03, not arbitrary administrative writes.

**Transport architecture.** Replace accept-all automatic TLS verification with
verified per-node identity using mTLS or pinned keys and a documented enrollment
model. Reuse existing node identity where it is suitable. Bind permitted peers
to current membership and cluster identity. Define trust anchors, certificate
names/identities, validity, renewal overlap, clock failure, node removal,
compromise rotation, and loss of the signing authority. Group-secret possession
alone must not authorize impersonating any current or removed node.

**Migration.** Add capability/version reporting before strict activation. Publish
the supported old/new combinations and activation preconditions. Signed-response
verification failures must never fall back to unsigned success. Once strict
identity mode is activated, rollback to an accepting verifier is not a safe
rollback; recovery requires compatible trusted binaries or an explicit documented
re-enrollment process. Test restart and interrupted rotation, not only first boot.
Support public HTTPS setup as part of the client bearer transport story.

**Acceptance.** Valid peers connect; wrong-cluster, unknown, removed, expired,
wrong-identity, and replayed credentials fail. An active relay cannot establish
an authenticated application channel under another member's identity. Capture
no private keys or shared secrets in evidence. Update the trusted-LAN assumptions
in the security guide only after the stronger contract is actually enforced.

## 8. R05 — deliver backup and catastrophic restore as one capability

**Delivery:** Backlog specification; do not build as a prerequisite for this pass.
**Owns:** F05 and restoration aspects of A08.
**Starting contract:** [OPERATIONS.md](../OPERATIONS.md). The pre-activation SQLite
copies are frozen import inputs, not current restore points for an activated
cluster. Replication does not recover accidental deletion or permanent majority
loss by itself.

**Build steps.**

1. Define a supported consistency boundary using the store's actual snapshot
   and applied-log guarantees. Include all authoritative state and explicitly
   inventory what is reconstructed, externally stored, or intentionally excluded.
   Do not copy a live database directory and label it a portable snapshot.
2. Version a portable manifest: cluster identity, schema/format version,
   snapshot/log boundary, creation time, component inventory, sizes, hashes,
   encryption metadata, and compatibility constraints. Checksums detect damage;
   authentication and custody protect against malicious replacement.
3. Implement bounded export, atomic completion, interruption cleanup, protected
   temporary files, and off-cluster destination/retention support. Establish key
   custody, encryption and restore access, with no plaintext credentials in logs
   or command arguments. State what the backup contains that grants authority.
4. Implement inspect/verify and restore into a fresh isolated cluster. Validate
   completeness and compatibility before mutation. Restore must define the new
   cluster/epoch and node identity relationship, account/session policy, and how
   stale surviving members are fenced from serving or rejoining incorrectly.
5. Restore authoritative state first, then rebuild derived state with visible
   progress and refusals for incomplete dependencies. Inventory media objects,
   offline artifacts, job state, and external integrations separately from the
   database. A database success message must not imply all media was backed up.
6. Write a disaster-recovery runbook with preservation steps, explicit operator
   checkpoints, supported version combinations, and abort/restart behavior.
   Do not make a surviving single voter force-authoritative without a proven
   consistent recovery source and fencing protocol.

**Acceptance.** Isolated drills cover total cluster loss, permanent majority loss,
corrupted/truncated backup, incompatible version, interrupted export and restore,
missing encryption key, and a stale member returning. Compare account, library,
playback/history, and relevant cluster state against the fixture. Verify the
chosen credential invalidation/rotation policy. Measure actual RPO and RTO and
state exclusions; do not invent service-level promises before measurement.

**Rollback.** An incomplete restore never serves as a valid cluster. Preserve the
original evidence and data, abandon the isolated target safely, and retry from a
verified source. Export remains compatible with documented retained backups
across upgrades, or the release explicitly supplies and tests a migration path.

## 9. R06 — isolate media workers from daemon authority

**Delivery:** Narrow environment/descriptor hygiene now where safe; full isolation
is a separately scoped capability.
**Owns:** F07; builds on R02.
**Source map:** `live_ffmpeg_command` in
[live_tv.rs](../../crates/plurxd/src/live_tv.rs), other FFmpeg/ffprobe launch sites,
and the existing scanner constraints described in [SECURITY.md](../SECURITY.md).

Inventory every parser/decoder process and its input, environment, descriptors,
filesystem, network, device access, and limits. Live tuner data is untrusted media
input even when its configured host is on the LAN. The reviewed Live TV child
uses daemon privileges and inherited environment; `kill_on_drop` is useful
lifecycle control but is not isolation.

Define a narrow worker launch contract: explicit environment allowlist, no
unneeded daemon/database keys or open descriptors, read-only scoped inputs,
private bounded scratch/output, required network destinations only, and bounded
CPU, memory, disk, process count, and wall time. Validate worker output and keep
paths under daemon-controlled roots. Use a distinct OS identity, container, or
sandbox appropriate to the deployment. Document equivalent minimum isolation
for each supported packaging mode and fail clearly when required setup is
missing.

GPU decoding may require device nodes, drivers, dynamic libraries, and hardware
capabilities. Do not blindly copy a static scanner sandbox that prevents valid
GPU execution. Test the actual worker/artifact/deployment combination and state residual
shared-kernel/driver risk. These test results inform Developer requirements/status;
the presence of a test receipt must not control runtime feature enablement. A
selected sandbox must enforce its actual isolation contract and report setup
errors, rather than silently launching with unrestricted daemon privilege.

**Acceptance.** Canary secrets and database files are unreadable by the worker;
unneeded network and filesystem writes are denied; valid software and hardware
playback still function. Flooded output, malformed input, crash, daemon shutdown,
and resource exhaustion remain bounded and cleaned up. This is boundary testing,
not a claim to have discovered or eliminated every codec vulnerability.

## 10. R07 — separate audit verdicts from reporting and authenticate releases

**Delivery:** Narrow audit and input/credential fixes now; new release trust
infrastructure remains separately scoped backlog.
**Owns:** F14, F15, A08.
**Source map:** [rust-audit workflow](../../.github/workflows/rust-audit.yml),
[release workflow](../../.github/workflows/publish-release.yml),
the assessed revision's `.github/buildkitd.toml`,
[Dockerfile](../../Dockerfile), and
[development pipeline](../DEVELOPMENT_PIPELINE.md).

**Audit path.** Reconcile current CI changes first. The reviewed workspace audit
uses a direct CLI wrapper while fuzz and scheduled reporting retain assumptions
about GitHub APIs. Establish one deterministic scanner invocation and parsed
result model for every governed lockfile, including fuzz. Separate the security
verdict from Forgejo/GitHub annotation or issue publication. Define what fails
for actionable advisories, informational notices, database/network failure, and
reporting failure. A failed reporter must not transform a known vulnerability
into success or erase its artifact.

Use fixture scanner outputs to test clean, vulnerable, informational, malformed,
and transport-error cases. Verify the chosen Forgejo reporting API with a safe
fixture; do not claim an actual missed report without evidence. Preserve the
current manually dispatched full-suite policy. Do not restore automatic full
test schedules as a side effect of fixing audit reporting.

**Release trust.** Inventory mutable action references, base images, downloaded
binaries, caches, registry hops, and deployment inputs. Pin reviewed action
commits and artifact/image digests, with an intentional update process. Preserve
existing good controls such as verified tool checksums and non-root runtime.
Review checkout credential persistence, workflow permissions, runner host access,
and credential lifetime. Trusted self-hosted runners still need containment of
a compromised build: constrain secrets, isolate workloads, and prefer disposable
execution environments where practical.

Replace unauthenticated registry transport with verified TLS under the deployment
trust model. Produce SBOM and provenance for the actual release artifact, sign
or attest with controlled release identity, and verify expected identity and
digest before rollout. A SHA-256 file served beside a mutable binary is not alone
proof of publisher identity. Pinning an FFmpeg base also stabilizes the artifact
identity used for decoder qualification; update that evidence when it changes.

**Acceptance.** Both audit scopes preserve their verdict when reporting fails.
A changed tag cannot silently change a pinned build input. Tampered artifacts,
wrong signer/identity, and digest mismatches fail deployment verification. The
supported emergency rollback verifies an older trusted artifact, not an unsigned
replacement. Document actual guarantees and gaps; no fresh CVE scan was part of
the original review.

## 11. R08 — improve internal boundaries and measured warm-path performance

**Delivery:** Touched documentation now; broad refactors/cache changes only for a
subsequently selected, measured problem.
**Owns:** A01, A03, A07.
**Source map:** [decode_facts.rs](../../crates/plurxd/src/decode_facts.rs),
[playback_control.rs](../../crates/plurxd/src/playback_control.rs),
[HLS handlers](../../crates/plurxd/src/http/hls.rs),
[live_tv.rs](../../crates/plurxd/src/live_tv.rs), and the web application.

**Modularity.** Extract cohesive internal units at the ownership boundaries
already exposed by fixes: authentication decisions versus diagnostic access,
transport versus protocol transitions, process lifetime versus command selection,
guide acquisition versus publication, and player intent versus platform effects.
Keep state transitions testable with explicit inputs and outcomes. Avoid moving
large functions into new files without reducing shared mutable state, or adding
abstractions that duplicate the server's playback authority. Integrate one
boundary at a time with behavior evidence; a service rewrite is not required.

**Decode facts.** At the reviewed revision, warm lookup can hash three whole
binaries through a serialized gate before finding a cache hit. Measure first:
bytes read, identity time, lock wait, cache-hit latency, and concurrent session
start latency with representative artifacts. Compare first use and warm use.
If material, introduce an immutable qualified artifact identity snapshot with
explicit invalidation at install/change boundaries and a bounded cache. Do not
use mtime alone as proof of identity, remove compatibility keys, or reuse facts
across an artifact replacement. Artifact identity governs whether a cached fact
is applicable, not whether an explicitly enabled feature is allowed to run.
Missing facts remain advisory. Preserve current input-size and memory bounds.
Test replacement during lookup, corruption, concurrent first use, restart, and
changed qualification policy. Record before/after performance rather than
promising improvement solely from a new cache.

**Documentation.** Update current SECURITY, OPERATIONS, API, FEATURES, validation,
and applicable status claims as each package lands. In particular remove stale
scanner-only threat descriptions, false logout rotation guidance, and unsupported
restore claims. Keep historical reviews historical. Use behavior or boundary
tests for security claims; avoid source-text assertions that reward retaining
obsolete wording. Every new/moved document needs index and point ownership.

## 12. R09 — make Live TV ownership and ambiguous starts operable

**Delivery:** Backlog unless a narrow existing-primitive correction is available.
**Owns:** A05; coordinates with the companion's Live TV settings surface.

Keep the existing single-owner and physical-fencing safety model. An unreachable
owner is not proof it stopped using the tuner. Show effective owner, generation,
readiness, drain state, blockers, and supported recovery actions in ordinary
language. Reuse the existing backend recovery contract and avoid duplicated
ownership policy in clients.

Inventory the start path's durable operation identity and response-loss behavior.
Where missing, add a caller-stable idempotency key scoped to authorized caller
and request intent, with durable bounded retention and a status lookup. Repeated
requests with the same key must reconcile one operation; changed intent with the
same key must refuse. A timeout means outcome unknown until reconciled, not
permission to allocate another tuner session. Status access must enforce the
same authorization boundary and avoid disclosing bearer capabilities.

Define cancellation and cleanup when a caller disappears, when the owner changes,
and after daemon restart. Test lost success responses, duplicate starts, delayed
old-generation completion, restart, drain, and fenced takeover. Show a recoverable
pending/unknown state to the client while reconciliation runs. No automatic
unfenced takeover or independent client heuristic may bypass server authority.

## 13. Prove completion without changing the repository's merge policy

### 13.1 Focused evidence for the work actually selected

| Boundary, when implemented | Relevant focused evidence |
|---|---|
| Client sessions | Real UI sign-out plus retained-token rejection; offline and late-response cases |
| Authorization | Quorum/minority/learner/mixed-version matrix and route-scope assertions |
| Peer transport | Forgery/replay/wrong-identity tests plus rotation and strict-mode transition |
| Playback | Web behavioral regression; native traces and physical-device qualification for timing |
| Resource work | Deadline, cancellation, queue saturation, output cap, kill/reap and memory bounds |
| Recovery | Isolated export/restore drills, consistency comparisons, stale-node fencing, RPO/RTO |
| Worker isolation | Denied secret/file/network access and valid qualified playback |
| CI/release | Fixture audit verdicts; artifact identity, attestation, and rollback verification |
| Product design | Actual UI states reflect requested, pending, effective, failed, and unavailable outcomes |

A test that only asserts the current broken behavior is a reproducer, not a
regression test. Convert reproducers into the desired contract and verify they
fail on the relevant old behavior. Use deterministic fakes for time and network
ordering, then hardware for claims that depend on real decoders or player APIs.
Do not label simulator success as physical-device qualification. If a device is
unavailable, retain that limitation rather than blocking the entire correction
pass. Neither test outcome nor a missing qualification receipt disables the
feature in software.

Evidence records identify source SHA, artifact identities, platform/device,
command or procedure, outcome, and limitations. Keep secrets out of logs and
support bundles. Link evidence from each finding's disposition. Deferred work
remains explicitly open with owner, reason, residual risk, and next action; it
must not be counted as implemented.

### 13.2 Branching, compilation, and bookkeeping

Follow [AGENTS.md](../../AGENTS.md),
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md), and the
[agent compile loop](../ci/AGENT-COMPILE-LOOP.md) in their current versions.
Prefer a small coherent correction PR when the selected scope fits. If it
actually becomes a multi-task effort, use one temporary `effort/<project>` branch
with task branches targeting it. Non-main task PRs do not request the main-bound
adversarial review or run its fast lane. Do not create an effort structure merely
because this document contains a long backlog.

Before any Rust edits, establish the repository-pinned Rust 1.97.1 compiler loop
and verify `rustc --version`. If compilation is remote, send committed source
using `git archive`, never credentials or `.git`. Compile affected code as it
changes. After integrating current main, archive and check that exact source
again. Run required check, Clippy, and formatting before the main-bound PR leaves
draft; an older snapshot's success is not current-head evidence.

Corrective changes require direct regression-test evidence or the prescribed
`validation/regressions.d/<first-commit-prefix>-<slug>.toml` record. Maintain
functionality-point ownership and the docs index in the same applicable commits.
Advance Apple/Android build counters for shipped native changes; a workspace
release additionally aligns both marketing versions and both counters. Keep
pre-commit hooks disabled.

For an effort, freeze task merges and integrate current main first. Open the
resulting main-bound PR (or the small independent correction PR) as draft.
Request exactly one adversarial agent review, address its findings,
mark ready, let the automatic fast lane complete, and merge only after the
current head's green **Main promotion gate**. The author verifies fixes; do not
request a second review. Full runtime suites belong to the separate manual
sweep and are not a new pre-merge gate. Targeted implementation evidence does
not change that policy.

### 13.3 Sol's final handoff and stop point

Deliver the selected correction commits and PR, focused current-head evidence,
and a disposition matrix for F01–F15 and A01–A08. Use explicit labels: fixed,
already fixed and verified, not reproduced, or deferred. Link changed source and
state remaining high-impact risks in plain language. Only implemented persistent
or protocol changes need migration/rollback instructions in this pass.

Finish the bounded pass without silently starting the backlog. Completion means
the selected fixes and their bookkeeping are finished, not that every architecture
idea in this document has shipped. Keep the full backlog available for Paul's
later choices. No new runtime readiness gate, extra CI lane, additional review
round, or broad test-sweep prerequisite is part of this handoff.
