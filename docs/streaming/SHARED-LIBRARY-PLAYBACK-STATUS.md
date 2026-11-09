# Shared-library playback — implementation and merge status

**Status:** open · **Updated:** 2026-11-09 · **Owner:** Codex integrator

Companion to [the root-cause analysis](SHARED-LIBRARY-PLAYBACK-RCA.md), which
owns the design and accepted decisions. This page records PR #1472's delivery
state. The larger [playback-start effort](PLAYBACK-START-PLAN.md) owns A1.

## Current state

PR **#1472**, branch `claude/shared-library-standalone-fixes`, remains draft.
The resumed source was `0b3c0dfc3`; the relay work and prior Opus fixes are
integrated. Final review corrections are in code candidate `dc38044c4`, with later
documentation-only commits.
No passing final fast-lane result is claimed.

The implementation covers NULL-principal consumers (§4), committed membership
(§6), RPC attribution (§7 step 2), Source admission (§8/S12), poster retries
and snapshots (§9), the HEVC index argv (S4), owned abandonment (S10), and
startup budgets and retries across the relay (S6/S7/S11).

S5 was reverted: independent subtitle extraction lacked Source custody.
S1–S3, S8 and S9 remain A1 work. Native Apple/Android startup headers require
a client release. The RX ring and deployment remain separate operations work.

## Evidence and remaining work

| Stage | State |
|---|---|
| Compiler | Pinned Rust 1.97.1 formatting, daemon/core all-target Clippy and canonical production Hiqlite Clippy passed. Building the focused executable remains pending; no focused Rust results claimed. |
| Final review | Independent Codex review of `28bc0aacc`: one P1 and one P2; both addressed below. Prior Opus review remains attributed to the earlier owner because no report was retained. |
| Existing focused tests | Latest historical run: 143 passed, two failed; pinned fixtures: four passed, three failed. These are not final-source passes. The native Source HTTP cases pass after the S5 revert; the older caption failures predate it. |
| Core | Retained focused run: 27 passed; separate replicated-store run: five passed. Applicability remains bounded to the source and features actually exercised. |
| Counterfactuals | Prior evidence covers NULL principals and Source admission. Other counterfactual execution is not established. |
| Paperwork | Prior CI failed for missing `Regression-Test:` lines; valid references are now in the PR body. Final check pending. |
| Next | Finish the exact-source compiler checks; run final focused regressions and fast lane; fix and rerun failures only; merge on green. |

The fixture investigation uses a separate owned clone. User-owned source is
not a build or scratch workspace. No watchdog, feature gate or authorization
cache is introduced by the resumption.

## Final review and dispositions

The final independent review examined `28bc0aacc` against `8ab61a779` plus the
pinned asset diagnostic. No tests or builds were run by the reviewer.

| Finding | Root cause and correction |
|---|---|
| P1 · Ready result abandoned in a retry gap | Readiness ended the abandonment phase although no accepted Start writer had claimed the result. `824e19044` preserves the existing rejoin deadline through readiness and serializes response claim against abandonment under the same mutex. Its ordinary End owner settles the Source; no watchdog is added. A real pinned test covers readiness before the window ends, and retry coverage checks an accepted result survives it. |
| P2 · Discovery restarted the peer clock | The clock was constructed after discovery, and Local fall-through parsed the unchanged allowance again. `63a92adf3` constructs it once at ingress and carries it into the Local handler. Only the discovery answer waiter is cancelled: filesystem inspection remains with accepted-driver monitors, and no Start dispatch or registration is dropped. The real Source regression covers parked discovery and delayed Local fall-through. |

The fixture repairs also establish their actual preconditions: End uses an
independent driver; a long pause occurs after scan admission rather than
expiring its five-second authority; abandonment observes the running scan
before the caller leaves. The unresolved pinned pre-session WebVTT case
retains Source warnings so a final failure can identify its extraction cause.

The first serialized type check passed, but test-target Clippy exceeded the
8 GiB container cap even without debug data. On 2026-11-09 the owner explicitly
authorized a task-specific 12 GiB, four-CPU, no-swap container on the existing
build host. This is retained in the task receipt, not a repository policy change.
A subsequent production lint failure is corrected by grouping the canonical
request, clock, rejoin window and connection custody in one transport input.
The initial focused run completed with nine passes and one failure, recorded below.

Paperwork initially passed 15 of 16 rules. The new index row crossed the 40 KiB
streaming-index budget; shortening an existing summary fixed it, and all five
checks in that failed module passed on the rerun. The other rules were not rerun.


The test executable also exceeded 12 GiB after limiting compiler affinity to
one CPU. The owner then authorized **up to 24 GiB, four CPUs, no swap** for this
task only. The existing host has 60 GiB total; its repository build policy is
unchanged. The failed executable build is being retried with four-CPU affinity,
retaining all passing compiler/lint checks. Kernel records identify cgroup
memory-limit kills, not Rust diagnostics. The four HEVC fixture `unwrap()`
lints were corrected with descriptive `expect()` messages in `dc38044c4`.

## Focused correction — pre-session text subtitles

The isolated final run on `dc38044c4` completed **9 passed, 1 failed**. Both
abandonment orderings, the accepted-response lifetime, relay/rejoin clocks and
artwork scope classification passed. The failed native-lane fixture returned 503
for pre-session WebVTT; captured Source warnings identified
`subtitle sparse read cancelled`.

The sparse range reader cancels its helper child after a read failure. That
is distinct from stopping the extraction. The correction restores the existing
whole-source demux only while both the bounded operation and its Source-read
budget remain active. The budget has a separate cancellation token, so it is
checked explicitly rather than relying on delayed supervisor propagation.
S5's asynchronous extraction remains deferred; no work is detached here.

Candidate `537b4cf2d` is in affected daemon lint and a failed-only run: the
native fixture plus the genuine operation- and budget-cancellation controls.
The nine unaffected passes and passing core/vendor lint are retained.
