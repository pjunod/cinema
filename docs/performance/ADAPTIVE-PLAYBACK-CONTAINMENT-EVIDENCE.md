# Adaptive containment evidence — what each check establishes

**Status:** open. Companion to [ADAPTIVE-PLAYBACK-OWNED-CONTAINMENT.md](ADAPTIVE-PLAYBACK-OWNED-CONTAINMENT.md).

## Qualification records distinct boundaries

The native lab uses a task-owned transient delegated scope beneath an explicit
18 GiB/no-swap/four-CPU ancestor. The control process moves to its own child
before enabling memory/pids for workload children. The actual limit readbacks
are verified; no build runs uncapped on a voter. Committed source reaches the
pinned Rust 1.97.1 compiler through source-only archives.

Nine daemon cases pass on staged source tree
`c45428b43d1010f6aa8eb94afb40c65aa7951ce4`, zero failed/ignored. They include
three new original-driver cases (same task through Adopt, cancelled preparation
and cleanup waiter, aborted task retains full charge), four affected
Adopt/fence cases, real finite lifecycle (80.90 s including invocation) and
graceful restart (87.76 s including invocation). Those four daemon source files
are unchanged on the committed native candidate.

On `fd81da243a3d8d1012ca9a0bf0686ec884d54243`, all 35 focused core cases pass:

| Scope | Passed | Evidence boundary |
|---|---:|---|
| Owner census | 14 | Includes callback Drop/unwind, finalization, references, overflow and atomic reservation |
| Domain validation/custody | 8 | Synthetic refusals/ownership and bounded control parsing |
| Sealed launcher validation | 3 | Argument capacity and original pidfd status handling |
| Actual delegated kernel | 10 | Empty/removal/reuse; atomic birth; fork/thread refusal; private OOM; surviving `setsid` descendant; dropped owner, exec failure and pre-clone refusal |

The ten kernel cases ran explicitly with `--ignored`, each reporting zero
ignored tests under verified ancestor caps; ordinary fixture-file success is
not substituted for enforcement. The private child entry runs only through
the pinned executable owned by those parent cases. All 44 selected core/daemon
cases pass, zero failed/ignored, at their stated sources.

Pinned workspace/all-target strict Clippy, formatting, catalog and JavaScript
syntax pass; task paperwork passes 14/14. Independent source reviews cover the
domain/launcher, census and driver boundaries. Task
[PR #1375](http://forge.lan:3000/noirr/plurx/pulls/1375) merged into the effort at
`edb5b79ff902b7eea5fe95d8954b1e5af99a5752` after all eight jobs of Effort
development gate 6437 passed. Its landing tree matches the qualified candidate.

Kernel enforcement, ownership regressions and exact-source compiler results
are recorded separately. Authored fixture cases do not establish
production coverage, startup/cleanup bounds, M0 acceptance or trial eligibility.
The effort remains draft; V1–V5 and Main promotion remain open.
