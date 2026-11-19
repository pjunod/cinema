//! Linux strict-parser custody fixtures shared with the daemon regression.
#![allow(clippy::disallowed_methods)]
use super::*;

#[cfg(target_os = "linux")]
pub fn build_static_probe(path: &std::path::Path, mode: u8) {
    build_static_probe_with_private_path(path, mode, Path::new("/unavailable-parent-marker"));
}

#[cfg(target_os = "linux")]
pub(super) fn build_static_probe_with_private_path(path: &Path, mode: u8, private_path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let source = path.with_extension("c");
    std::fs::write(
            &source,
            r#"
#if !defined(__x86_64__) && !defined(__aarch64__)
#error unsupported test architecture
#endif

#define NR_READ 0
#define NR_WRITE 1
#define NR_CLOSE 3
#define NR_LSEEK 8
#define NR_NANOSLEEP 35
#define NR_FORK 57
#define NR_EXECVE 59
#define NR_FCHMOD 91
#define NR_FCNTL 72
#define NR_SETPGID 109
#define NR_SETSID 112
#define NR_SENDMSG 46
#define NR_EXIT 60
#define NR_MEMFD_CREATE 319
#define NR_EXECVEAT 322
#define NR_OPENAT 257
#define NR_PTRACE 101
#define NR_PROCESS_VM_READV 310
#define NR_GETDENTS64 217
#define NR_PREAD64 17
#define NR_READV 19
#define NR_PREADV 295
#define NR_PREADV2 327
#define NR_MMAP 9
#define NR_MUNMAP 11
#define NR_SENDFILE 40
#define NR_SPLICE 275
#define NR_COPY_FILE_RANGE 326
#define NR_READAHEAD 187
#define NR_FADVISE64 221
#define NR_IO_SETUP 206
#define NR_IO_DESTROY 207
#define NR_IO_GETEVENTS 208
#define NR_IO_SUBMIT 209
#define NR_IO_CANCEL 210
#define NR_IO_PGETEVENTS 333
#define NR_IO_URING_SETUP 425
#define NR_IO_URING_ENTER 426
#define NR_IO_URING_REGISTER 427
#if defined(__aarch64__)
#undef NR_READ
#undef NR_WRITE
#undef NR_CLOSE
#undef NR_LSEEK
#undef NR_NANOSLEEP
#undef NR_FORK
#undef NR_EXECVE
#undef NR_FCHMOD
#undef NR_FCNTL
#undef NR_SETPGID
#undef NR_SETSID
#undef NR_SENDMSG
#undef NR_EXIT
#undef NR_MEMFD_CREATE
#undef NR_EXECVEAT
#undef NR_OPENAT
#undef NR_PTRACE
#undef NR_PROCESS_VM_READV
#undef NR_GETDENTS64
#define NR_OPENAT 56
#define NR_PTRACE 117
#define NR_PROCESS_VM_READV 270
#define NR_GETDENTS64 61
#undef NR_PREAD64
#undef NR_READV
#undef NR_PREADV
#undef NR_PREADV2
#undef NR_MMAP
#undef NR_MUNMAP
#undef NR_SENDFILE
#undef NR_SPLICE
#undef NR_COPY_FILE_RANGE
#define NR_PREAD64 67
#define NR_READV 65
#define NR_PREADV 69
#define NR_PREADV2 286
#define NR_MMAP 222
#define NR_MUNMAP 215
#define NR_SENDFILE 71
#define NR_SPLICE 76
#define NR_COPY_FILE_RANGE 285
#undef NR_READAHEAD
#undef NR_FADVISE64
#undef NR_IO_SETUP
#undef NR_IO_DESTROY
#undef NR_IO_GETEVENTS
#undef NR_IO_SUBMIT
#undef NR_IO_CANCEL
#undef NR_IO_PGETEVENTS
#define NR_READAHEAD 213
#define NR_FADVISE64 223
#define NR_IO_SETUP 0
#define NR_IO_DESTROY 1
#define NR_IO_SUBMIT 2
#define NR_IO_CANCEL 3
#define NR_IO_GETEVENTS 4
#define NR_IO_PGETEVENTS 292
#define NR_READ 63
#define NR_WRITE 64
#define NR_CLOSE 57
#define NR_LSEEK 62
#define NR_NANOSLEEP 101
#define NR_FORK 220
#define NR_EXECVE 221
#define NR_FCHMOD 52
#define NR_FCNTL 25
#define NR_SETPGID 154
#define NR_SETSID 157
#define NR_SENDMSG 211
#define NR_EXIT 93
#define NR_MEMFD_CREATE 279
#define NR_EXECVEAT 281
#endif
#define AT_EMPTY_PATH 0x1000
#define FD_CLOEXEC 1
#define F_DUPFD 0
#define F_SETFD 2
#define SIGCHLD 17
#define SOL_SOCKET 1
#define SCM_RIGHTS 1

struct probe_iovec { void *base; unsigned long length; };
struct probe_msghdr {
    void *name;
    unsigned name_length;
    struct probe_iovec *iov;
    unsigned long iov_length;
    void *control;
    unsigned long control_length;
    int flags;
};
struct probe_cmsghdr { unsigned long length; int level; int type; };
struct probe_control { struct probe_cmsghdr header; int fd; int padding; };

#if defined(__x86_64__)
static long syscall6(long number, long a0, long a1, long a2, long a3, long a4, long a5) {
    long result;
    register long r10 __asm__("r10") = a3;
    register long r8 __asm__("r8") = a4;
    register long r9 __asm__("r9") = a5;
    __asm__ volatile("syscall" : "=a"(result) : "a"(number), "D"(a0), "S"(a1), "d"(a2), "r"(r10), "r"(r8), "r"(r9) : "rcx", "r11", "memory");
    return result;
}
__asm__(".global _start\n.type _start,@function\n_start:\n mov %rsp,%rdi\n andq $-16,%rsp\n call probe_main\n");
#else
static long syscall6(long number, long a0, long a1, long a2, long a3, long a4, long a5) {
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x3 __asm__("x3") = a3;
    register long x4 __asm__("x4") = a4;
    register long x5 __asm__("x5") = a5;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5), "r"(x8) : "memory");
    return x0;
}
__asm__(".global _start\n.type _start,%function\n_start:\n mov x0,sp\n bl probe_main\n");
#endif

static unsigned long length(const char *text) {
    unsigned long count = 0;
    while (text[count]) count++;
    return count;
}

static int same(const char *left, const char *right) {
    while (*left && *right && *left == *right) { left++; right++; }
    return *left == *right;
}

static void write_text(long fd, const char *text) {
    syscall6(NR_WRITE, fd, (long)text, length(text), 0, 0, 0);
}

static __attribute__((noreturn)) void finish(long status) {
    syscall6(NR_EXIT, status, 0, 0, 0, 0, 0);
    for (;;) {}
}

static const char facts[] = "{\"streams\":[{\"index\":4,\"codec_type\":\"video\",\"codec_name\":\"h264\",\"profile\":\"High\",\"pix_fmt\":\"yuv420p\",\"width\":1920,\"height\":1080,\"avg_frame_rate\":\"24000/1001\",\"r_frame_rate\":\"24000/1001\",\"disposition\":{\"attached_pic\":0}}]}\n";

#if PROBE_MODE == 24
static __attribute__((noreturn, noinline, used)) void source_thread_reader(void) {
    static char byte;
    syscall6(NR_READ, 3, (long)&byte, 1, 0, 0, 0);
    for (;;) { unsigned long delay[2] = { 60, 0 }; syscall6(NR_NANOSLEEP, (long)delay, 0, 0, 0, 0, 0); }
}
extern long make_source_thread(void *stack);
#if defined(__x86_64__)
__asm__(".text\n.global make_source_thread\nmake_source_thread:\n"
        "mov %rdi,%rsi\nmov $0x50f00,%edi\nxor %edx,%edx\nxor %r10d,%r10d\nxor %r8d,%r8d\n"
        "mov $56,%eax\nsyscall\ntest %rax,%rax\njnz 1f\ncall source_thread_reader\nud2\n1: ret\n");
#else
__asm__(".text\n.global make_source_thread\nmake_source_thread:\n"
        "mov x1,x0\nmov x0,#0x0f00\nmovk x0,#5,lsl #16\nmov x2,xzr\nmov x3,xzr\nmov x4,xzr\n"
        "mov x8,#220\nsvc #0\ncbnz x0,1f\nbl source_thread_reader\nbrk #0\n1: ret\n");
#endif
#endif

void probe_main(unsigned long *stack) {
    long argc = (long)stack[0];
    char **argv = (char **)(stack + 1);
#if PROBE_MODE == 0
    if (argc > 1 && same(argv[1], "-v")) {
        char source[64];
        syscall6(NR_LSEEK, 3, 0, 0, 0, 0, 0);
        long count = syscall6(NR_READ, 3, (long)source, sizeof(source) - 1, 0, 0, 0);
        if (count != 23) finish(84);
        source[count] = 0;
        if (!same(source, "production-bound source")) finish(85);
    }
#elif PROBE_MODE == 1
    char *next[] = { "/usr/bin/false", 0 };
    write_text(1, "entered:path\n");
    syscall6(NR_EXECVE, (long)next[0], (long)next, 0, 0, 0, 0);
    write_text(1, "blocked:path\n");
#elif PROBE_MODE == 2
    if (argc > 1 && same(argv[1], "copied")) finish(92);
    write_text(1, "entered:memfd\n");
    long copy = syscall6(NR_MEMFD_CREATE, (long)"copy", 0, 0, 0, 0, 0);
    if (copy < 0) finish(81);
    syscall6(NR_LSEEK, 4, 0, 0, 0, 0, 0);
    char buffer[4096];
    for (;;) {
        long count = syscall6(NR_READ, 4, (long)buffer, sizeof(buffer), 0, 0, 0);
        if (count < 0) finish(82);
        if (count == 0) break;
        if (syscall6(NR_WRITE, copy, (long)buffer, count, 0, 0, 0) != count) finish(83);
    }
    syscall6(NR_FCHMOD, copy, 0500, 0, 0, 0, 0);
    syscall6(NR_LSEEK, copy, 0, 0, 0, 0, 0);
    char *next[] = { "copy", "copied", 0 };
    syscall6(NR_EXECVEAT, copy, (long)"", (long)next, 0, AT_EMPTY_PATH, 0);
    syscall6(NR_EXECVEAT, 4, (long)"/proc/self/fd/5", (long)next, 0, AT_EMPTY_PATH, 0);
    write_text(1, "blocked:memfd\n");
#elif PROBE_MODE == 3
    if (argc > 1 && same(argv[1], "reentered")) {
        long rebound = syscall6(NR_FCNTL, 5, F_DUPFD, 4, 0, 0, 0);
        char *again[] = { "replacement", "copied", 0 };
        syscall6(NR_EXECVEAT, rebound, (long)"", (long)again, 0, AT_EMPTY_PATH, 0);
        finish(93);
    }
    long copy = syscall6(NR_MEMFD_CREATE, (long)"replacement", 0, 0, 0, 0, 0);
    if (copy < 0) finish(86);
    syscall6(NR_LSEEK, 4, 0, 0, 0, 0, 0);
    char buffer[4096];
    for (;;) {
        long count = syscall6(NR_READ, 4, (long)buffer, sizeof(buffer), 0, 0, 0);
        if (count < 0) finish(87);
        if (count == 0) break;
        if (syscall6(NR_WRITE, copy, (long)buffer, count, 0, 0, 0) != count) finish(88);
    }
    syscall6(NR_FCHMOD, copy, 0500, 0, 0, 0, 0);
    syscall6(NR_LSEEK, copy, 0, 0, 0, 0, 0);
    long changed = syscall6(NR_FCNTL, 4, F_SETFD, FD_CLOEXEC, 0, 0, 0);
    char *next[] = { "sealed", "reentered", 0 };
    if (changed == 0) {
        syscall6(NR_EXECVEAT, 4, (long)"", (long)next, 0, AT_EMPTY_PATH, 0);
        finish(94);
    }
    write_text(1, "blocked:fd-reuse\n");
#elif PROBE_MODE == 4
    if (argc > 1 && same(argv[1], "-version")) {
        write_text(1, facts);
        finish(0);
    }
    long child;
#if defined(__x86_64__)
    child = syscall6(NR_FORK, 0, 0, 0, 0, 0, 0);
#else
    child = syscall6(NR_FORK, SIGCHLD, 0, 0, 0, 0, 0);
#endif
    if (child < 0) finish(89);
    if (child == 0) {
        long escaped_session = syscall6(NR_SETSID, 0, 0, 0, 0, 0, 0);
        long escaped_group = syscall6(NR_SETPGID, 0, 0, 0, 0, 0, 0);
        unsigned long delay[2] = { 0, 500000000 };
        syscall6(NR_NANOSLEEP, (long)delay, 0, 0, 0, 0, 0);
        if (escaped_session >= 0 || escaped_group >= 0) write_text(1, "escaped-session\n");
        finish(0);
    }
#elif PROBE_MODE == 5
    if (argc > 1 && same(argv[1], "-version")) {
        write_text(1, facts);
        finish(0);
    }
    char byte = 'x';
    struct probe_iovec io = { &byte, 1 };
    struct probe_control control = {
        { sizeof(struct probe_cmsghdr) + sizeof(int), SOL_SOCKET, SCM_RIGHTS },
        3,
        0
    };
    struct probe_msghdr message = { 0, 0, &io, 1, &control, sizeof(control), 0 };
    if (syscall6(NR_SENDMSG, 5, (long)&message, 0, 0, 0, 0) >= 0) finish(95);
#elif PROBE_MODE == 6
    write_text(1, facts);
    if (!(argc > 1 && same(argv[1], "-version"))) {
        unsigned long delay[2] = { 5, 0 };
        syscall6(NR_NANOSLEEP, (long)delay, 0, 0, 0, 0, 0);
    }
#elif PROBE_MODE == 7
    if (syscall6(NR_OPENAT, -100, (long)PROBE_PRIVATE_PATH, 0, 0, 0, 0) >= 0) finish(96);
    if (syscall6(NR_OPENAT, -100, (long)PROBE_PARENT_ROOT_PATH, 0, 0, 0, 0) >= 0) finish(97);
    if (syscall6(NR_OPENAT, -100, (long)("/proc/self/root" PROBE_PRIVATE_PATH), 0, 0, 0, 0) >= 0) finish(100);
    if (syscall6(NR_OPENAT, -100, (long)PROBE_PARENT_PROC_PATH, 0, 0, 0, 0) >= 0) finish(101);
    if (syscall6(NR_OPENAT, -100, (long)"/proc/1", 0, 0, 0, 0) >= 0) finish(102);
    // Enumerate all inherited capabilities, not just low-numbered bootstrap
    // and supervisor FDs. The directory opened for this check is the sole
    // allowed descriptor above the held source/parser pair.
    long directory = syscall6(NR_OPENAT, -100, (long)"/proc/self/fd", 0, 0, 0, 0);
    if (directory < 0) finish(103);
    char entries[4096];
    for (;;) {
        long count = syscall6(NR_GETDENTS64, directory, (long)entries, sizeof(entries), 0, 0, 0);
        if (count < 0) finish(104);
        if (count == 0) break;
        for (long offset = 0; offset < count;) {
            if (count - offset < 20) finish(105);
            unsigned short size = *(unsigned short *)(entries + offset + 16);
            if (size < 20 || size > count - offset) finish(106);
            const char *name = entries + offset + 19;
            long fd = 0;
            long index = 0;
            while (19 + index < size && name[index] >= '0' && name[index] <= '9') {
                if (fd > 214748364) finish(107);
                fd = fd * 10 + name[index++] - '0';
            }
            if (index > 0 && (19 + index >= size || name[index] != 0 || (fd > 4 && fd != directory))) finish(108);
            offset += size;
        }
    }
    syscall6(NR_CLOSE, directory, 0, 0, 0, 0, 0);
    if (!(argc > 1 && same(argv[1], "-version"))) {
        long source_fd = syscall6(NR_OPENAT, -100, (long)"/proc/self/fd/3", 0, 0, 0, 0);
        if (source_fd < 0) finish(109);
        char source[64];
        long count = syscall6(NR_READ, source_fd, (long)source, sizeof(source) - 1, 0, 0, 0);
        if (count != 23) finish(110);
        source[count] = 0;
        if (!same(source, "production-bound source")) finish(111);
        syscall6(NR_CLOSE, source_fd, 0, 0, 0, 0, 0);
    }
    if (syscall6(NR_PTRACE, 0, 0, 0, 0, 0, 0) != -1) finish(98);
    if (syscall6(NR_PROCESS_VM_READV, PROBE_PARENT_PID, 0, 0, 0, 0, 0) != -1) finish(99);
#elif PROBE_MODE == 8 || PROBE_MODE == 9
    if (argc > 1 && same(argv[1], "-version")) {
        write_text(1, facts);
        finish(0);
    }
    long child;
#if defined(__x86_64__)
    child = syscall6(NR_FORK, 0, 0, 0, 0, 0, 0);
#else
    child = syscall6(NR_FORK, SIGCHLD, 0, 0, 0, 0, 0);
#endif
    if (child < 0) finish(89);
    if (child == 0) {
#if PROBE_MODE == 9
        syscall6(NR_LSEEK, 3, 0, 0, 0, 0, 0);
        write_text(3, "namespace-child-started");
#endif
        unsigned long delay[2] = { 60, 0 };
        syscall6(NR_NANOSLEEP, (long)delay, 0, 0, 0, 0, 0);
        finish(0);
    }
#if PROBE_MODE == 9
    unsigned long delay[2] = { 60, 0 };
    syscall6(NR_NANOSLEEP, (long)delay, 0, 0, 0, 0, 0);
#endif
#elif PROBE_MODE == 13
    if (argc > 1 && same(argv[1], "-version")) {
        write_text(1, "ffprobe version admitted-large-document\n");
        finish(0);
    }
    /* Valid JSON whitespace belongs to the actual document pipe drain even
       though canonical document serialization removes it afterward. */
    for (long block = 0; block < 8192; ++block) {
        write_text(1, "                                ");
    }
    write_text(1, facts);
    finish(0);
#elif PROBE_MODE == 10 || PROBE_MODE == 11 || PROBE_MODE == 12
    if (argc > 1 && same(argv[1], "-version")) {
        write_text(1, "ffprobe version admitted-static\n");
        finish(0);
    }
#if PROBE_MODE == 10
    write_text(1, "{\"streams\":[{\"index\":4,\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"profile\":\"Main\",\"pix_fmt\":\"yuv420p\",\"width\":1920,\"height\":1080,\"avg_frame_rate\":\"24/1\",\"r_frame_rate\":\"24/1\",\"disposition\":{\"attached_pic\":0}}]}\n");
#elif PROBE_MODE == 11
    write_text(1, "{\"streams\":[{\"index\":4,\"codec_type\":\"video\",\"codec_name\":\"h264\",\"profile\":\"High\",\"pix_fmt\":\"yuv420p\",\"width\":1920,\"height\":1080,\"avg_frame_rate\":\"24/1\",\"r_frame_rate\":\"24/1\",\"field_order\":\"tt\",\"disposition\":{\"attached_pic\":0}}]}\n");
#else
    write_text(1, "{invalid-json\n");
#endif
    finish(0);
#elif PROBE_MODE == 24
    if (argc > 1 && same(argv[1], "-version")) { write_text(1, facts); finish(0); }
    static char thread_stack[65536] __attribute__((aligned(16)));
    if (make_source_thread(thread_stack+sizeof(thread_stack)) < 0) finish(146);
    char byte;
    syscall6(NR_READ, 3, (long)&byte, 1, 0, 0, 0);
    for (;;) { unsigned long delay[2] = {60,0}; syscall6(NR_NANOSLEEP, (long)delay, 0, 0, 0, 0, 0); }
#elif PROBE_MODE >= 20 && PROBE_MODE <= 23
    if (argc > 1 && same(argv[1], "-version")) { write_text(1, facts); finish(0); }
    char byte = 0;
#if PROBE_MODE == 20
    if (syscall6(NR_LSEEK, 3, 0, 0, 0, 0, 0) != 0) finish(112);
    if (syscall6(NR_READ, 3, (long)&byte, 1, 0, 0, 0) != 1 || byte != 'p') finish(113);
    long alias = syscall6(NR_FCNTL, 3, F_DUPFD, 10, 0, 0, 0);
    if (alias < 0) finish(114);
    if (syscall6(NR_READ, alias, (long)&byte, 1, 0, 0, 0) != 1 || byte != 'r') finish(115);
    long reopened = syscall6(NR_OPENAT, -100, (long)"/dev/fd/3", 0, 0, 0, 0);
    if (reopened < 0) finish(116);
    if (syscall6(NR_PREAD64, reopened, (long)&byte, 1, 22, 0, 0) != 1 || byte != 'e') finish(117);
    if (syscall6(NR_READ, reopened, (long)&byte, 1, 0, 0, 0) != 1 || byte != 'p') finish(118);
    if (syscall6(NR_LSEEK, alias, 22, 0, 0, 0, 0) != 22) finish(119);
    if (syscall6(NR_READ, 3, (long)&byte, 1, 0, 0, 0) != 1 || byte != 'e') finish(120);
    if (syscall6(NR_READ, alias, (long)&byte, 0, 0, 0, 0) != 0) finish(121);
#elif PROBE_MODE == 21
    static char buffer[1048577];
    if (syscall6(NR_READ, 3, (long)buffer, sizeof(buffer), 0, 0, 0) != -7) finish(122);
#elif PROBE_MODE == 22
    long mapped = syscall6(NR_MMAP, 0, 4096, 1, 2, 3, 0);
    if (mapped != -1) finish(123);
    long anonymous = syscall6(NR_MMAP, 0, 4096, 3, 0x22, -1, 0);
    if (anonymous < 0) finish(124);
    *(char *)anonymous = 'a';
    if (syscall6(NR_MUNMAP, anonymous, 4096, 0, 0, 0, 0) != 0) finish(125);
    struct probe_iovec io = { &byte, 1 };
    if (syscall6(NR_READV, 3, (long)&io, 1, 0, 0, 0) != -1) finish(126);
    if (syscall6(NR_PREADV, 3, (long)&io, 1, 0, 0, 0) != -1) finish(127);
    if (syscall6(NR_PREADV2, 3, (long)&io, 1, 0, 0, 0) != -1) finish(128);
    if (syscall6(NR_SENDFILE, 1, 3, 0, 1, 0, 0) != -1) finish(129);
    if (syscall6(NR_SPLICE, 3, 0, 1, 0, 1, 0) != -1) finish(130);
    if (syscall6(NR_COPY_FILE_RANGE, 3, 0, 1, 0, 1, 0) != -1) finish(131);
    if (syscall6(NR_READAHEAD, 3, 0, 4096, 0, 0, 0) != -1) finish(134);
    if (syscall6(NR_FADVISE64, 3, 0, 4096, 3, 0, 0) != -1) finish(135);
    if (syscall6(NR_FADVISE64, 3, 0, 4096, 1, 0, 0) != 0) finish(136);
    if (syscall6(NR_IO_SETUP, 1, 0, 0, 0, 0, 0) != -1) finish(137);
    if (syscall6(NR_IO_DESTROY, 0, 0, 0, 0, 0, 0) != -1) finish(138);
    if (syscall6(NR_IO_SUBMIT, 0, 0, 0, 0, 0, 0) != -1) finish(139);
    if (syscall6(NR_IO_CANCEL, 0, 0, 0, 0, 0, 0) != -1) finish(140);
    if (syscall6(NR_IO_GETEVENTS, 0, 0, 0, 0, 0, 0) != -1) finish(141);
    if (syscall6(NR_IO_PGETEVENTS, 0, 0, 0, 0, 0, 0) != -1) finish(142);
    if (syscall6(NR_IO_URING_SETUP, 1, 0, 0, 0, 0, 0) != -1) finish(143);
    if (syscall6(NR_IO_URING_ENTER, 0, 0, 0, 0, 0, 0) != -1) finish(144);
    if (syscall6(NR_IO_URING_REGISTER, 0, 0, 0, 0, 0, 0) != -1) finish(145);
#elif PROBE_MODE == 23
    static char buffer[1048576];
    if (syscall6(NR_READ, 3, (long)buffer, sizeof(buffer), 0, 0, 0) != 23) finish(132);
    if (syscall6(NR_PREAD64, 3, (long)buffer, 32768, 0, 0, 0) != 23) finish(133);
#endif
#endif
    write_text(1, facts);
    finish(0);
}
"#,
        )
        .expect("write static probe source");
    let status = std::process::Command::new("cc")
        .arg("-nostdlib")
        .arg("-static")
        .arg("-fno-stack-protector")
        .arg("-fno-pie")
        .arg("-no-pie")
        .arg("-Wl,--build-id=none")
        .arg("-Wl,-e,_start")
        .arg(format!("-DPROBE_MODE={mode}"))
        .arg(format!(
            "-DPROBE_PRIVATE_PATH={:?}",
            private_path.to_str().expect("private marker path")
        ))
        .arg(format!(
            "-DPROBE_PARENT_ROOT_PATH={:?}",
            format!(
                "/proc/{}/root{}",
                std::process::id(),
                private_path.display()
            )
        ))
        .arg(format!("-DPROBE_PARENT_PID={}", std::process::id()))
        .arg(format!(
            "-DPROBE_PARENT_PROC_PATH={:?}",
            format!("/proc/{}", std::process::id())
        ))
        .arg(&source)
        .arg("-o")
        .arg(path)
        .status()
        .expect("start static probe compiler");
    assert!(status.success(), "compile static production-path probe");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500))
        .expect("make static probe executable");
}

/// Exercise the actual finish path with a deliberately held supervisor worker.
pub struct JoinedProbeFixture(ProbeExecutionSupervisor);

impl JoinedProbeFixture {
    pub fn new(
        retirement: Box<dyn RawProbeRetirement>,
        stop: Arc<AtomicBool>,
        read_cancel: tokio_util::sync::CancellationToken,
        worker: std::thread::JoinHandle<std::io::Result<()>>,
        source_failure: DecodeFactError,
    ) -> Self {
        Self(ProbeExecutionSupervisor {
            descriptors: None,
            retirement: Some(retirement),
            linux: Some(LinuxProbeExecSupervisor {
                child_sender: None,
                receiver: None,
                child_sender_fd: -1,
                stop,
                worker: Some(worker),
                read_cancel,
                source_failure: Arc::new(std::sync::Mutex::new(Some(source_failure))),
                test_counter: None,
            }),
        })
    }

    pub fn finish_joined(&mut self) -> Result<(), DecodeFactError> {
        self.0.finish_joined()
    }
}

pub struct HeldProbeWorker;

impl HeldProbeWorker {
    pub fn hold(probe: &mut DecodeProbeIdentity) -> Self {
        probe.launch_mode = ProbeLaunchMode::ProductionCompletionWorkerHeld;
        COMPLETION_WORKER_STARTED.store(false, Ordering::Release);
        COMPLETION_CLEANUP_STARTED.store(false, Ordering::Release);
        *COMPLETION_WORKER_RELEASE.0.lock().expect("worker gate") = false;
        Self
    }

    pub fn started(&self) -> bool {
        COMPLETION_WORKER_STARTED.load(Ordering::Acquire)
    }

    pub fn cleaning_up(&self) -> bool {
        COMPLETION_CLEANUP_STARTED.load(Ordering::Acquire)
    }
}

impl Drop for HeldProbeWorker {
    fn drop(&mut self) {
        *COMPLETION_WORKER_RELEASE.0.lock().expect("worker gate") = true;
        COMPLETION_WORKER_RELEASE.1.notify_all();
    }
}
