# Windows port — live implementation status

**Status:** promotion candidate ready for adversarial review; native and hardware evidence open ·
**Effort:** `effort/windows-port` · **Updated:** 2026-09-13

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
| M1 · compile and CI | complete | The Windows MSVC workspace builds locally. Forgejo effort, fast-lane, and full workflows carry the same pinned `cargo-xwin` build. | Keep the Windows compile lane green while later milestones land. |
| M2 · boot, scan, software play | built · native proof open | Secure Windows filesystem operations, exact ffmpeg path re-verification, Job Object ownership, suspend/resume, sharing-aware cleanup, console shutdown, and the native smoke script are implemented. | Run `scripts/windows-smoke.ps1` on a Windows x64 host and retain its logs. |
| M3 · service and package | built · clean-VM proof open | SCM install/run/uninstall, ProgramData defaulting, long-path manifest, Windows ZIP packaging, firewall runbook, and CI artifact retention are implemented. | Clean-VM install, reboot, discovery, and uninstall transcript. |
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
   remains mandatory for runtime and service evidence.
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

Passing compilation proves type and platform linkage coverage. It does not
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
