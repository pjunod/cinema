# Native entry — what is protected before executable code runs?

**Status:** open · restricted launcher primitive qualified locally · 2026-11-08.
Companion to [owned containment](ADAPTIVE-PLAYBACK-OWNED-CONTAINMENT.md) and the
[canonical ledger](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md). Production graph
integration, complete worker ownership, M0 and real trials remain open.

## Restrictions precede the executable

The existing atomic launcher creates the child inside its original configured
cgroup using clone3 and retains the original domain and pidfd. Its new prepared
policy combines read-only Landlock object grants with an architecture-checked,
default-deny syscall filter. The first policy requires an unprivileged x86-64
launcher and Landlock ABI 6 or later. Unsupported enforcement refuses; it does
not execute an unrestricted replacement.

Landlock handles filesystem mutation rights, including truncate and device
ioctl, and scopes signals and abstract Unix sockets. The filter denies namespace
entry, descriptor import, ptrace, sockets, unlisted syscalls and limit mutation.
Classic BPF cannot inspect clone3's pointer payload, so clone3 returns ENOSYS;
the separately constrained legacy clone path permits ordinary threads but
rejects namespace and parent-changing flags. Foreign syscall architectures are
rejected. Inherited capabilities and ambient capabilities are cleared before
execution, with no_new_privs already set.

The filesystem policy grants read access to actual supplied file/directory
objects and execute access to explicit executable/interpreter objects. No write
rule exists. Readable library directories in the fixture are not a claim that a
production dependency inventory or original library-file custody is complete.
The kernel contracts are [Landlock](https://docs.kernel.org/userspace-api/landlock.html)
and [seccomp BPF](https://docs.kernel.org/userspace-api/seccomp_filter.html);
syscall filtering alone does not provide the filesystem policy.

## Finite descriptor roles preserve the held objects

The launcher now supports the fixed finite input mapping: fd 3, optional fd 4,
and the same held executable as fd 9. Its standard input is the fixed null
input. All input and executable objects must be read-only; a writable inherited
file cannot be made safe by applying filesystem restrictions later. The fixed
input count/capacity is checked before launch. Child descriptor installation
uses only the fixed pre-exec syscall sequence, with no arbitrary callback.

The prepared child borrows the original spec, preventing argument, environment
and policy references from outliving their original storage. Original domain,
pidfd wait, exec-error reporting, output pipes and descendant extinction remain
distinct facts. EOF on the exec-error pipe does not certify execution or whole
resource closure.

## Qualification remains tied to its source and environment

On intermediate `17817d0a`, the emitted-policy interpreter case and the actual
delegated-kernel escape case pass. The real child is denied unlisted file
reads/writes, controller writes, namespace changes, descriptor import, ptrace
and socket creation, while an ordinary thread completes.

On `3789a962`, the real restricted two-input FFmpeg case produces fragmented
MP4 through the held fd 3/4/9 mapping. Its first attempt failed at the loader:
the host lacked libOpenCL, which existed in the compiler image. An owned fixture
runtime copies the original bundled libraries and 31 non-core dependencies from
that existing image. The original bundle and host packages remain unchanged.
The successful rerun uses the same source and executable. This environment
correction is not a production dependency qualification.

Successor `0e5e1918` adds the explicit spec lifetime and writable-input refusal;
focused strict Clippy and all 17 final same-source cases pass: four new cases
and 13 existing native compatibility cases, zero failed or ignored. The exact
Cargo-reported executable has SHA-256
`06cecdf32c85081ceffac4c22dea197c7dfdcfc8f4c7d127f1918b2eb23d79a3`.
Normal hooks, exact committed checks and the manual task gate remain required.
Earlier passing scopes and reruns add no distinct credit.

Kernel cases run explicitly rather than silently remaining ignored. The runner
uses the exact Cargo-reported executable and records its hash and source. It
refuses a lab with a server process and verifies its owned transient ancestor:
18 GiB memory, zero swap, four CPUs and 1024 tasks. The control process moves to
its own child before memory/pids are enabled for workload children. No compiler
runs uncapped on a voter, no repository credential enters the lab, and no
persistent host configuration changes.

## Production entry is still a separate integration boundary

The production profile constructor remains unavailable. The current selected
finite production route still uses its existing launcher. These kernel cases
qualify a prerequisite primitive, not its production graph placement or a
complete producer memory envelope.

Before that route changes, its original domain setup, rule objects, transient
copies, pidfd, exec-error pipe and cleanup records need prepaid graph ownership
before their effects. NativeDomainCore alone retains 16 file interfaces, plus
three in the shared factory, with further launch-time descriptors; the current
graph's three-pipe budget is not that allowance. Recovery must retain or refuse
on the original authority rather than reopen a path and invent a close proof.

Whole planning/parsing/pump ownership, library-file custody, allocator/executor
and complete native/kernel peaks, all-family census and full R/S/F closure remain
open. Configured memory.max and a successful tiny fixture are not KnownMemoryPeak.
No full M0, trial eligibility, current-main qualification, Main promotion or
deployment is claimed.
