# Windows port — live implementation status

**Status:** merged; native proof open · **Effort:** `effort/windows-port` ·
**Updated:** 2026-09-14

Companion to [WINDOWS-PORT-PLAN.md](WINDOWS-PORT-PLAN.md) (the original
decisions and milestone acceptance checks) and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (how this effort reaches
`main`) — this page records what is true of the current implementation. Update
it in the same commit whenever a milestone, blocker, decision, or evidence
result changes.

## 1. Outcome — a native Windows server, not a compatibility wrapper

The effort delivers `plurxd.exe` for Windows 10 1809+/Server 2019+ x64 on
NTFS: console and service launch · scanning · direct play · software HLS ·
NVENC and Quick Sync where the node proves them · a release ZIP · permanent
Windows compile coverage.

The browser, Android, and Apple clients already use the server's HTTP API and
do not need Windows-specific builds.

## 2. Progress — milestone evidence decides the state

| Milestone | State | Current fact | Next proof |
|---|---|---|---|
| M1 · compile and CI | complete | The Windows MSVC workspace builds locally. Forgejo effort, fast-lane, and full workflows pin the x86_64 MSVC SDK/CRT and compile normal plus test targets without executing them. | Keep the Windows compile lane green; execute native behavior only on a real Windows runner. |
| M2 · boot, scan, software play | built · native proof open | Secure Windows filesystem operations, scanner-bound direct delivery, exact ffmpeg path re-verification, complete Job Object ownership, suspend/resume, sharing-aware cleanup, targeted console shutdown, and the native smoke script are implemented. | Run `scripts/windows-smoke.ps1` on a Windows x64 host and retain its logs. |
| M3 · service and package | built · clean-VM proof open | SCM install/run/uninstall with truthful pending/running states, owner-and-SYSTEM service-data ACLs, ProgramData defaulting, long-path manifest, Windows ZIP packaging, firewall runbook, and CI artifact retention are implemented. | Clean-VM install, reboot, discovery, and uninstall transcript. |
| M4 · NVENC and Quick Sync | code-ready · hardware proof open | Existing probe-gated NVENC/QSV paths are portable and Windows readiness is visible without becoming a feature gate. | Boot probes and two-second segments on real NVIDIA and Intel hardware. |
| M5 · AMD AMF | deferred | Implement only after M4 exposes the real validation cost. | Separately recorded AMD hardware receipt. |

## 3. Decisions — changes from the 2026-08-29 plan are explicit

1. **D2 path handoff is approved.** Windows revalidates the held source
   identity immediately before ffmpeg opens the path and confines mutable
   outputs to owner/SYSTEM-only scratch. The remaining pathname race is a
   documented Windows platform difference; it is not described as equivalent
   to Unix descriptor handoff.
2. **Forgejo CI uses `cargo-xwin`.** The repository no longer has the plan's
   assumed GitHub-hosted Windows runner. Existing agents can compile the MSVC
   target with Microsoft CRT/SDK inputs supplied by `cargo-xwin`; a native VM
   remains mandatory for runtime and service evidence. The x86_64 SDK/CRT
   versions are exact, their download uses the build path's bounded retries,
   and cold jobs get 30 minutes because the original deadline expired on run
   `2065`. No persistent SDK cache is claimed while runner-approved external
   volumes are unavailable; checkout cleaning makes a workspace cache a lie.
3. **Readiness is advisory.** Windows-specific experimental capabilities get
   an explicit enable control under Settings → Developer. The same surface
   states each requirement and whether this node meets it, but a red readiness
   result does not override the operator's saved choice.
4. **One final adversarial review.** Milestones use focused compile/regression
   evidence and proper commits. The accumulated effort gets one adversarial
   review only when it is ready to target `main`; corrections land before the
   fast lane runs.

## 4. Evidence — exact commands and honest interpretations

| Date | Candidate | Command | Result | How to read it |
|---|---|---|---|---|
| 2026-09-13 | `04cbb2e4` | `rustup run 1.97.1 cargo check --workspace --locked --all-targets` | pass · 1m20s | The untouched mainline compiles on the checkout host with the repository-pinned compiler. |
| 2026-09-13 | `04cbb2e4` | `AWS_LC_SYS_NO_ASM=1 rustup run 1.97.1 cargo xwin check --workspace --locked --exclude plurx-cluster-check --target x86_64-pc-windows-msvc` | expected fail · 31 Plurx source errors | The MSVC SDK/CRT loop is established. Failures are now port work, not missing host headers. `AWS_LC_SYS_NO_ASM` affects this check-only build because the checkout host has no NASM; release CI may install NASM instead. |
| 2026-09-13 | `codex/windows-port-m1` | `AWS_LC_SYS_NO_ASM=1 rustup run 1.97.1 cargo xwin build --workspace --locked --exclude plurx-cluster-check --target x86_64-pc-windows-msvc` | pass · 2m20s clean build | The Windows MSVC workspace compiles and links, excluding the intentionally Linux-only cluster fault tool. This is not native runtime evidence. |
| 2026-09-13 | `codex/windows-port-m1` | `rustup run 1.97.1 cargo check --workspace --locked --all-targets` | pass · 41s | The current milestone preserves the existing macOS compile surface. |
| 2026-09-13 | `codex/windows-port-m2` | `AWS_LC_SYS_NO_ASM=1 rustup run 1.97.1 cargo xwin check --workspace --locked --exclude plurx-cluster-check --target x86_64-pc-windows-msvc` | pass · 43s | The complete runtime/service source compiles for MSVC. This is not native behavior evidence. |
| 2026-09-13 | `codex/windows-port-m2` | `rustup run 1.97.1 cargo xwin build --workspace --release --locked --exclude plurx-cluster-check --target x86_64-pc-windows-msvc` | pass · 1m34s warm build | The optimized PE links with assembly enabled; its embedded resource contains `longPathAware=true`. The release ZIP and SHA-256 receipt were assembled and verified. |
| 2026-09-13 | `4d03946b` | Forgejo effort gate `2052` | pass | M1 policy, native Rust compile, and clean-container Windows compile passed before PR 305 merged. |
| 2026-09-13 | `01bf515d` | Forgejo effort gate `2056` | pass | The consolidated runtime, service, packaging, documentation, and ownership inventory passed policy plus native Rust and Windows compile before PR 308 merged. |
| 2026-09-13 | `43bd8abb` | pinned `cargo xwin check` and optimized `cargo xwin build` commands above | pass | The exact effort tree after merging current `main` compiles and links for Windows with Rust 1.97.1; this is the frozen promotion candidate before adversarial review. |
| 2026-09-14 | `b3e5f8dd..20c297d8` | one requested adversarial review | 11 findings addressed | Corrections cover service/key ACLs, Windows free-space proofs, direct-play no-reparse opens, complete child Job ownership, truthful SCM states, permanent-DV Windows execution, atomic recovery publication, handle-based ACL changes, bounded smoke cleanup, targeted Ctrl-Break, and an explicit advisory Developer enable control. |
| 2026-09-14 | `b3e5f8dd..20c297d8` | `rustup run 1.97.1 cargo check --workspace --locked --all-targets` | pass · 1m14s clean target | The review corrections preserve the native workspace compile surface. This is compile evidence, not the requested fast lane. |
| 2026-09-14 | `b3e5f8dd..20c297d8` | `AWS_LC_SYS_NO_ASM=1 rustup run 1.97.1 cargo xwin check --workspace --locked --exclude plurx-cluster-check --target x86_64-pc-windows-msvc` | pass · 13s warm target | The review corrections compile for MSVC with Rust 1.97.1. Native behavior is still unproved. |
| 2026-09-14 | `452c920a` | Forgejo main fast lane `2061` | pass | Post-review policy and contract preflight, Rust gate, Windows MSVC compile, embedded-web syntax, and the aggregate Main promotion gate all passed. Unaffected mobile compile jobs were correctly skipped. |
| 2026-09-14 | `8027480d` | Forgejo main fast lane `2065` | infrastructure fail | A fresh runner spent the 15-minute job allowance installing `cargo-xwin` and downloading the MSVC CRT/SDK. No plurx compiler or test failure occurred. The repair pins x86_64 SDK `10.0.26100` and CRT `14.44.17.14`, uses the build path's eight download retries, raises the cold-run allowance to 30 minutes, and compiles all Windows test targets without executing them. |
| 2026-09-14 | `ce82873b` | Forgejo main fast lane `2068` | corrective compile fail | The same cold runner completed the pinned CRT download in 9m14s inside the new 30-minute allowance. Compiling test targets then exposed a Unix-only `MetadataExt` assertion in the shared fixture suite; the repair now compares held handles through the production cross-platform file-identity API. |

Passing compilation includes test-only code and proves type and platform
linkage coverage. It does not execute those tests and therefore does not
prove Windows filesystem, service-control, process-job, discovery, or hardware
behavior; those remain red until their native evidence is recorded above.

## 5. Known blockers — infrastructure, not hidden scope

- No native Windows runner is currently registered in Forgejo. The PowerShell
  smoke harness is ready; add its manually dispatched workflow only after a
  real runner with the `Windows`, `X64`, `lab`, and `ffmpeg-6` labels is
  registered in `validation/runner-fleet.toml`.
- An ARM Windows 11 VMware guest exists on the checkout host, but x64 emulation
  is not a substitute for the missing NVIDIA/Intel x64 hardware receipts. A
  headless start on 2026-09-13 stopped at VMware's encrypted-VM password
  boundary, so no guest command or runtime claim was fabricated.
- M4 needs Windows NVIDIA and Intel hardware. The implementation may land
  before those machines exist; the milestone cannot be marked complete from
  argument-builder tests alone.

## 6. Guardrails — what this effort will not pretend

- A cross-compiled executable is not a runtime receipt.
- Missing readiness evidence is not an invisible feature gate.
- Unix secure-filesystem behavior is not weakened to make Windows easier.
- No MSI, winget package, ARM64 target, or second media engine is added here.
- Secrets, deploy keys, tokens, build outputs, SDK caches, and agent-local
  status never enter the repository.
