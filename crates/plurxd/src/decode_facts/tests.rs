use super::*;

#[cfg(target_os = "macos")]
#[test]
fn invalid_parser_initialization_removes_the_immutable_snapshot() {
    let root = crate::test_tempdir().expect("private source root");
    let snapshots = root.path().join("snapshots");
    std::fs::create_dir(&snapshots).expect("private snapshot directory");
    let artifact = root.path().join("invalid.wasm");
    // Valid magic followed by an invalid section; initialization must fail
    // with a parser error or unsupported signing, and both paths clean up.
    std::fs::write(&artifact, b"\0asm\x01\0\0\0\xff").expect("malformed parser");
    let source = std::fs::File::open(artifact).expect("held source");
    assert!(snapshot_executable_in(&source, Some(&snapshots)).is_err());
    assert_eq!(
        std::fs::read_dir(&snapshots)
            .expect("snapshot entries")
            .count(),
        0
    );
}

#[cfg(target_os = "macos")]
#[test]
fn successful_snapshot_drop_removes_the_immutable_file() {
    let root = crate::test_tempdir().expect("private source root");
    let snapshots = root.path().join("snapshots");
    std::fs::create_dir(&snapshots).expect("private snapshot directory");
    let artifact = root.path().join("native-fixture");
    std::fs::write(&artifact, b"native fixture bytes").expect("source bytes");
    let source = std::fs::File::open(artifact).expect("held source");
    let snapshot = snapshot_executable_in(&source, Some(&snapshots)).expect("snapshot");
    let immutable_path = snapshot.path().to_path_buf();
    assert!(
        std::fs::remove_file(&immutable_path).is_err(),
        "snapshot retains immutability until its owner drops"
    );
    drop(snapshot);
    assert!(!immutable_path.exists());
    assert_eq!(
        std::fs::read_dir(&snapshots)
            .expect("snapshot entries")
            .count(),
        0
    );
}

#[test]
fn idet_verdict_uses_the_final_multi_frame_summary_and_a_strict_ninety_percent_bound() {
    let overruled = b"noise\n[Parsed_idet_2] Multi frame detection: TFF: 0 BFF: 0 Progressive: 271 Undetermined: 29\n";
    assert_eq!(
        parse_idet_verdict(overruled),
        Some(InterlaceVerdict::FlagOverruled)
    );
    let confirmed =
        b"[Parsed_idet_2] Multi frame detection: TFF: 60 BFF: 0 Progressive: 0 Undetermined: 0\n";
    assert_eq!(
        parse_idet_verdict(confirmed),
        Some(InterlaceVerdict::FlagConfirmed)
    );
    let boundary =
        b"[Parsed_idet_2] Multi frame detection: TFF: 10 BFF: 0 Progressive: 90 Undetermined: 0\n";
    assert_eq!(
        parse_idet_verdict(boundary),
        Some(InterlaceVerdict::FlagConfirmed),
        "the plan requires more than ninety percent"
    );
    assert_eq!(parse_idet_verdict(b"no summary"), None);
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "needs the shipped ffmpeg and ffprobe; set PLURX_FFMPEG and PLURX_FFPROBE"]
async fn descriptor_bound_idet_overrules_misflag_and_confirms_interlace() {
    let ffmpeg = std::env::var("PLURX_FFMPEG").expect("PLURX_FFMPEG");
    let ffprobe = std::env::var("PLURX_FFPROBE").expect("PLURX_FFPROBE");
    let root = crate::test_tempdir().expect("tempdir");
    let identity = DecodeProbeIdentity::discover_fixture(&ffprobe)
        .await
        .expect("probe identity");
    let fixtures = [
        (
            "misflagged.mkv",
            "testsrc=size=640x360:rate=30000/1001:duration=10",
            "setfield=tff",
            InterlaceVerdict::FlagOverruled,
            ScanType::Progressive,
        ),
        (
            "interlaced.mkv",
            "testsrc2=size=640x360:rate=60000/1001:duration=4",
            "tinterlace=mode=interleave_top,setfield=tff",
            InterlaceVerdict::FlagConfirmed,
            ScanType::Interlaced(plurx_core::domain::FieldOrder::TffCoded),
        ),
    ];
    for (name, source, filter, expected_verdict, expected_scan) in fixtures {
        let media = root.path().join(name);
        let status = std::process::Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg(source)
            .args(["-vf", filter, "-c:v", "ffv1"])
            .arg(&media)
            .status()
            .expect("generate fixture");
        assert!(status.success(), "generate {name}");
        let facts = DecodeFactCache::new()
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(Arc::new(
                    std::fs::File::open(&media).expect("open fixture"),
                )),
                None,
                ProbeStreamSelection::Absolute(0),
                PROBE_DEADLINE,
                None,
            )
            .await
            .expect("descriptor-bound facts");
        assert_eq!(facts.interlace_verdict(), expected_verdict, "{name}");
        assert_eq!(facts.scan_type(), expected_scan, "{name}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn unknown_hevc_scan_is_refined_once_without_fresh_document_cache_downgrade() {
    use std::io::{Seek, SeekFrom};
    let root = crate::test_tempdir().expect("HEVC scan fixture setup and refinement");
    let probe = root.path().join("ffprobe-hevc-scan");
    let mut header: serde_json::Value = serde_json::from_str(include_str!(
        "../../../plurx-core/tests/fixtures/hevc-progressive-configuration.json"
    ))
    .expect("HEVC scan fixture setup and refinement");
    header["streams"][0]["index"] = serde_json::json!(4);
    let document = serde_json::json!({"streams":[{"index":4,"codec_type":"video","codec_name":"hevc",
            "profile":"Main 10","pix_fmt":"yuv420p10le","width":3840,"height":2160,
            "avg_frame_rate":"24000/1001","r_frame_rate":"24000/1001"}],"format":{"format_name":"matroska"}});
    executable(
        &probe,
        &format!(
            r###"#!/bin/sh
if test "$1" = "-version"; then echo 'ffprobe version hevc-scan-fixture'; exit 0; fi
case " $* " in
  *" -show_data "*)
    test "$4" = "4" || exit 91
    echo scan >> "$PLURX_TEST_PROBE_PATH.calls"
    cat /dev/fd/3 >/dev/null || exit 92
    printf '%s\n' '{header}'
    ;;
  *)
    cat /dev/fd/3 >/dev/null || exit 93
    printf '%s\n' '{document}'
    ;;
esac
"###
        ),
    );
    let identity = DecodeProbeIdentity::discover_fixture(
        probe
            .to_str()
            .expect("HEVC scan fixture setup and refinement"),
    )
    .await
    .expect("HEVC scan fixture setup and refinement");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"held HEVC source").expect("HEVC scan fixture setup and refinement");
    let handle =
        Arc::new(std::fs::File::open(&media).expect("HEVC scan fixture setup and refinement"));
    let mut offset = handle
        .try_clone()
        .expect("HEVC scan fixture setup and refinement");
    offset
        .seek(SeekFrom::Start(3))
        .expect("HEVC scan fixture setup and refinement");
    let cache = DecodeFactCache::new();
    let mut digest = None;
    for _ in 0..2 {
        let prepared = cache
            .probe_source_document(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&handle)),
                None,
                ProbeStreamSelection::Absolute(4),
                PROBE_DEADLINE,
                None,
            )
            .await
            .expect("HEVC scan fixture setup and refinement");
        let facts = cache
            .refine_source_document(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&handle)),
                None,
                prepared,
                PROBE_DEADLINE,
                None,
            )
            .await
            .expect("HEVC scan fixture setup and refinement");
        assert_eq!(facts.scan_type(), ScanType::Progressive);
        assert_eq!(facts.field_order(), None);
        assert_eq!(
            offset
                .stream_position()
                .expect("HEVC scan fixture setup and refinement"),
            3
        );
        if let Some(previous) = &digest {
            assert_eq!(facts.facts_digest(), previous);
        }
        digest = Some(facts.facts_digest().to_owned());
    }
    assert_eq!(
        std::fs::read_to_string(probe.with_extension("calls"))
            .expect("HEVC scan fixture setup and refinement")
            .lines()
            .count(),
        1
    );
}

/// One rule for both planning routes, including the case that used to
/// split them: a file whose cover art is a video stream. Whatever the rule
/// selects, both routes select it, and the catalog row is applied only when
/// the selected stream is the film.
#[test]
fn both_planning_routes_select_one_stream_through_one_rule() {
    let json = serde_json::json!({"streams": [
        {"index": 2, "codec_type": "audio", "codec_name": "aac"},
        {"index": 4, "codec_type": "video", "codec_name": "mjpeg", "profile": "Baseline",
         "pix_fmt": "yuvj420p", "width": 600, "height": 600,
         "avg_frame_rate": "25/1", "r_frame_rate": "25/1", "color_transfer": "bt709",
         "disposition": {"attached_pic": 1}},
        {"index": 7, "codec_type": "video", "codec_name": "hevc", "profile": "Main 10",
         "pix_fmt": "yuv420p10le", "width": 3840, "height": 2160,
         "avg_frame_rate": "24/1", "r_frame_rate": "24/1", "color_transfer": "smpte2084",
         "disposition": {"attached_pic": 0}}
    ]});
    let index = absolute_video_ordinal(&json, 0).expect("0:v:0 resolves");
    assert_eq!(index, 4, "FFmpeg's 0:v:0 is the attached picture here");

    let catalog = DecodeCatalogMetadata::new(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
        plurx_core::domain::DolbyVisionFacts {
            profile: Some(8),
            level: Some(6),
            bl_compat_id: Some(1),
            el_present: Some(false),
            rpu_present: Some(true),
        },
    )
    .expect("catalog facts");

    let identity = |byte: char| {
        plurx_core::transcode::DecodeSourceIdentity::from_sha256(byte.to_string().repeat(64))
            .expect("identity")
    };
    // The bound route's identity and the stored route's identity differ by
    // construction; the facts they produce must not.
    let bound =
        legacy_ordinal_facts(&json, identity('c'), index, Some(&catalog)).expect("bound facts");
    let stored =
        legacy_ordinal_facts(&json, identity('d'), index, Some(&catalog)).expect("stored facts");

    assert_eq!(bound.input_video_stream(), 4);
    assert_eq!(stored.input_video_stream(), 4);
    assert_eq!(
        bound.facts_digest(),
        stored.facts_digest(),
        "one stream, one description, so one artifact name"
    );
    assert_eq!(
        bound.dynamic_range(),
        Some("sdr"),
        "the film's Dolby Vision row does not describe its cover art"
    );

    // And when the ordinal does land on the film, the catalog applies.
    let film = legacy_ordinal_facts(&json, identity('c'), 7, Some(&catalog)).expect("film");
    assert_eq!(film.input_video_stream(), 7);
    assert_eq!(film.dynamic_range(), Some("dolby_vision"));
}

/// Plan P-02 §3.2: the probe execs from inside its own `pre_exec`, so a
/// closure registered after it never runs. The child priority must be
/// registered first; the launcher's own registration lands after it.
/// (The exec-ordering test in `process::priority` shows both outcomes.)
#[test]
fn the_child_priority_is_registered_before_the_probe_execs_from_pre_exec() {
    let source = include_str!("../decode_facts.rs");
    let configure = source
        .find("fn configure_probe_execution(")
        .expect("configure_probe_execution");
    let body = &source[configure..];
    let apply = body
        .find("priority::apply(command, class)")
        .expect("configure_probe_execution registers the child priority");
    let exec = body
        .find("command.pre_exec(move ||")
        .expect("the closure that execs the probe");
    assert!(
        apply < exec,
        "the priority must be registered before the closure that execs"
    );
}

#[test]
fn production_pre_exec_transitive_source_audit_rejects_allocation_and_panic_forms() {
    fn item_body<'a>(source: &'a str, needle: &str) -> &'a str {
        let start = source
            .find(needle)
            .unwrap_or_else(|| panic!("missing {needle}"));
        let open = source[start..]
            .find('{')
            .map(|offset| start + offset)
            .unwrap_or_else(|| panic!("missing body for {needle}"));
        let mut depth = 0_usize;
        for (offset, byte) in source.as_bytes()[open..].iter().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &source[open..=open + offset];
                    }
                }
                _ => {}
            }
        }
        panic!("unterminated body for {needle}")
    }

    let source = include_str!("../decode_facts.rs");
    for item in [
        "command.pre_exec(move ||",
        "fn is_production(self)",
        "fn bootstrap_interrupts(self)",
        "fn export_socket_fd(self)",
        "fn start_probe_session(",
        "fn duplicate_child_fd(",
        "fn assign_child_fd(",
        "fn install_child_fd(",
        "fn install_probe_child_fds(",
        "fn consume_linux_bootstrap_interrupt(",
        "fn ensure_linux_probe_deadline(",
        "fn confirm_linux_probe_fork(",
        "fn wait_for_linux_fd(",
        "fn mark_unrelated_fds_close_on_exec(",
        "fn install_linux_filesystem_execute_denial(",
        "fn send_linux_seccomp_listener(",
        "fn install(&self)",
        "fn install_listener(&self)",
        "fn execute_held_probe(&self)",
    ] {
        let body = item_body(source, item);
        for allocating in [
            "std::io::Error::new(",
            "std::io::Error::other(",
            "format!(",
            "to_owned(",
            "to_string(",
            "to_vec(",
            "Box::new(",
            "Vec::new(",
            "Vec::with_capacity(",
            "String::new(",
            "String::from(",
            "CString::new(",
            "OsString::from(",
            "vec![",
            ".push(",
            ".extend(",
            ".collect(",
            ".collect::<",
            "panic!(",
            "assert!(",
            "assert_eq!(",
        ] {
            assert!(
                !body.contains(allocating),
                "post-fork item {item} contains allocating or panic path {allocating}"
            );
        }
    }
}

#[cfg(unix)]
fn executable(path: &std::path::Path, body: &str) {
    crate::write_test_executable(path, body, 0o700);
}

#[cfg(target_os = "linux")]
fn build_static_probe(path: &std::path::Path, mode: u8) {
    build_static_probe_with_private_path(path, mode, Path::new("/unavailable-parent-marker"));
}

#[cfg(target_os = "linux")]
fn build_static_probe_with_private_path(path: &Path, mode: u8, private_path: &Path) {
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

#[cfg(unix)]
#[test]
fn qualified_probe_identity_refuses_indirect_wrappers() {
    let root = crate::test_tempdir().expect("tempdir");
    let wrapper = root.path().join("ffprobe-wrapper");
    executable(&wrapper, "#!/bin/sh\nexec /usr/bin/ffprobe \"$@\"\n");
    let wrapper = std::fs::File::open(wrapper).expect("open wrapper");
    #[cfg(target_os = "linux")]
    assert!(matches!(
        require_direct_probe_executable(&wrapper),
        Err(DecodeFactError::ProbeIdentity(_))
    ));
    #[cfg(target_os = "macos")]
    assert!(matches!(
        require_direct_probe_executable(&wrapper),
        Err(DecodeFactError::ProbeIdentity(_))
    ));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    assert_eq!(
        require_direct_probe_executable(&wrapper),
        Err(DecodeFactError::UnsupportedPlatform)
    );

    let current = std::fs::File::open(std::env::current_exe().expect("current executable"))
        .expect("open current native executable");
    #[cfg(target_os = "linux")]
    assert!(matches!(
        require_direct_probe_executable(&current),
        Err(DecodeFactError::ProbeIdentity(reason))
            if reason.contains("statically linked")
    ));
    #[cfg(target_os = "macos")]
    assert!(matches!(
        require_direct_probe_executable(&current),
        Err(DecodeFactError::ProbeIdentity(_))
    ));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    assert_eq!(
        require_direct_probe_executable(&current),
        Err(DecodeFactError::UnsupportedPlatform)
    );
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
#[tokio::test]
async fn production_discovery_refuses_unsupported_unix_within_its_deadline() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("native-looking-ffprobe");
    executable(
        &probe,
        "#!/bin/sh\nprintf '%s\\n' 'unsupported Unix probe must not execute'\n",
    );
    let result = tokio::time::timeout(
        IDENTITY_DEADLINE + VERSION_DEADLINE,
        DecodeProbeIdentity::discover(probe.to_str().expect("probe path")),
    )
    .await
    .expect("unsupported Unix discovery remains bounded");
    assert!(matches!(result, Err(DecodeFactError::UnsupportedPlatform)));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn production_discovery_requires_the_packaged_capability_parser() {
    let root = crate::test_tempdir().expect("package");
    let probe = root.path().join("ffprobe");
    executable(&probe, "#!/bin/sh\ntouch \"$0.executed\"\n");
    let result = DecodeProbeIdentity::discover(probe.to_str().expect("path")).await;
    assert!(
        matches!(result, Err(DecodeFactError::ProbeIdentity(reason)) if reason.contains("packaged capability-bound source parser"))
    );
    assert!(!root.path().join("ffprobe.executed").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn self_contained_elf_has_no_external_parser_dependency() {
    let root = crate::test_tempdir().expect("tempdir");
    let artifact = root.path().join("static-ffprobe");
    let mut elf = vec![0_u8; 64 + 56];
    elf[..4].copy_from_slice(b"\x7fELF");
    elf[4] = 2;
    elf[5] = 1;
    elf[6] = 1;
    elf[16..18].copy_from_slice(&2_u16.to_le_bytes());
    elf[18..20].copy_from_slice(&62_u16.to_le_bytes());
    elf[20..24].copy_from_slice(&1_u32.to_le_bytes());
    elf[32..40].copy_from_slice(&64_u64.to_le_bytes());
    elf[52..54].copy_from_slice(&64_u16.to_le_bytes());
    elf[54..56].copy_from_slice(&56_u16.to_le_bytes());
    elf[56..58].copy_from_slice(&1_u16.to_le_bytes());
    elf[64..68].copy_from_slice(&1_u32.to_le_bytes());
    std::fs::write(&artifact, elf).expect("write structural static ELF");
    let artifact = std::fs::File::open(artifact).expect("open structural static ELF");
    require_direct_probe_executable(&artifact).expect("static ELF has no external closure");
}

#[cfg(target_os = "linux")]
#[test]
fn executable_classification_is_bound_to_the_sealed_snapshot() {
    let root = crate::test_tempdir().expect("tempdir");
    let artifact = root.path().join("mutable-probe");
    let mut elf = vec![0_u8; 64 + 56];
    elf[..4].copy_from_slice(b"\x7fELF");
    elf[4] = 2;
    elf[5] = 1;
    elf[6] = 1;
    elf[16..18].copy_from_slice(&2_u16.to_le_bytes());
    elf[18..20].copy_from_slice(&62_u16.to_le_bytes());
    elf[20..24].copy_from_slice(&1_u32.to_le_bytes());
    elf[32..40].copy_from_slice(&64_u64.to_le_bytes());
    elf[52..54].copy_from_slice(&64_u16.to_le_bytes());
    elf[54..56].copy_from_slice(&56_u16.to_le_bytes());
    elf[56..58].copy_from_slice(&1_u16.to_le_bytes());
    elf[64..68].copy_from_slice(&1_u32.to_le_bytes());
    std::fs::write(&artifact, elf).expect("write initially accepted ELF");
    let opened = std::fs::File::open(&artifact).expect("open initially accepted ELF");
    require_direct_probe_executable(&opened).expect("initial bytes qualify structurally");
    std::fs::copy(small_dynamic_executable(), &artifact)
        .expect("replace configured bytes in place");
    let snapshot = snapshot_executable(&opened).expect("snapshot replacement bytes");
    assert!(matches!(
        require_direct_probe_executable(snapshot.as_file()),
        Err(DecodeFactError::ProbeIdentity(reason)) if reason.contains("statically linked")
    ));
}

/// A real dynamically linked executable well under
/// `MAX_PROBE_EXECUTABLE_BYTES`. The debug test binary itself is not: it
/// grew past 512 MiB, so snapshotting `current_exe()` tested its size, not
/// the sealing or static-link rules these tests are about.
#[cfg(target_os = "linux")]
fn small_dynamic_executable() -> std::path::PathBuf {
    ["/usr/bin/true", "/bin/true"]
        .iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.is_file())
        .unwrap_or_else(|| std::env::current_exe().expect("current executable"))
}

#[cfg(target_os = "linux")]
#[test]
fn sealed_snapshot_refuses_in_place_mutation() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::FileExt;

    let current = std::fs::File::open(small_dynamic_executable()).expect("open a small executable");
    let snapshot = snapshot_executable(&current).expect("sealed executable snapshot");
    let seals = unsafe { libc::fcntl(snapshot.as_file().as_raw_fd(), libc::F_GET_SEALS) };
    assert_eq!(seals & libc::F_SEAL_WRITE, libc::F_SEAL_WRITE);
    assert!(
        snapshot.as_file().write_at(b"X", 0).is_err(),
        "sealed snapshot bytes must reject an in-place overwrite"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn production_probe_executes_sealed_bytes_and_collects_bound_json() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("static-ffprobe");
    build_static_probe(&probe, 0);
    let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
        .await
        .expect("production probe identity and version");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"production-bound source").expect("media");
    let source = Arc::new(std::fs::File::open(media).expect("open media"));
    let facts = DecodeFactCache::new()
        .get_or_probe(
            &identity,
            DecodeFactSource::isolated(source),
            None,
            ProbeStreamSelection::Absolute(4),
            Duration::from_secs(5),
            None,
        )
        .await
        .expect("sealed production collection");
    assert_eq!(facts.input_video_stream(), 4);
    assert_eq!(facts.codec(), Some("h264"));
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires a compiled daemon in PLURX_TEST_NAMESPACE_BOOTSTRAP and unprivileged user namespaces"]
async fn namespace_probe_executes_bound_source_and_denies_secondary_images() {
    assert!(
        std::env::var_os("PLURX_TEST_NAMESPACE_BOOTSTRAP").is_some(),
        "provide the exact compiled daemon bootstrap"
    );
    let root = crate::test_tempdir().expect("tempdir");
    for mode in 0..=3 {
        let probe = root.path().join(format!("namespace-probe-{mode}"));
        build_static_probe(&probe, mode);
        let identity = DecodeProbeIdentity::discover_with_mode(
            probe.to_str().expect("probe path"),
            ProbeLaunchMode::ProductionNamespace,
        )
        .await
        .expect("namespace-bound sealed discovery and one-shot exec");
        if mode == 0 {
            let media = root.path().join("namespace-media.bin");
            std::fs::write(&media, b"production-bound source").expect("media");
            let source = Arc::new(std::fs::File::open(media).expect("source"));
            let facts = DecodeFactCache::new()
                .get_or_probe(
                    &identity,
                    DecodeFactSource::isolated(source),
                    None,
                    ProbeStreamSelection::Absolute(4),
                    Duration::from_secs(5),
                    None,
                )
                .await
                .expect("namespace-bound exact FD source collection");
            assert_eq!(facts.input_video_stream(), 4);
            assert_eq!(facts.codec(), Some("h264"));
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires a compiled daemon in PLURX_TEST_NAMESPACE_BOOTSTRAP and unprivileged user namespaces"]
async fn namespace_probe_hides_parent_files_and_reaps_descendants() {
    assert!(std::env::var_os("PLURX_TEST_NAMESPACE_BOOTSTRAP").is_some());
    let root = crate::test_tempdir().expect("tempdir");
    let marker = root.path().join("parent-private-marker");
    std::fs::write(&marker, b"must not enter probe namespace").expect("parent marker");
    for mode in [7, 8] {
        let probe = root.path().join(format!("isolated-probe-{mode}"));
        build_static_probe_with_private_path(&probe, mode, &marker);
        let identity = DecodeProbeIdentity::discover_with_mode(
            probe.to_str().expect("probe path"),
            ProbeLaunchMode::ProductionNamespace,
        )
        .await
        .expect("isolated production bootstrap");
        let media = root.path().join(format!("media-{mode}"));
        std::fs::write(&media, b"production-bound source").expect("media");
        let source = Arc::new(std::fs::File::open(media).expect("source"));
        let facts = tokio::time::timeout(
            Duration::from_secs(7),
            DecodeFactCache::new().get_or_probe(
                &identity,
                DecodeFactSource::isolated(source),
                None,
                ProbeStreamSelection::Absolute(4),
                Duration::from_secs(5),
                None,
            ),
        )
        .await
        .expect(
            "namespace child retaining pipes for60s must be killed within unchanged probe deadline",
        )
        .expect("isolated parser facts");
        assert_eq!(facts.codec(), Some("h264"));
    }
    assert_eq!(
        std::fs::read(marker).expect("parent marker remains"),
        b"must not enter probe namespace"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires a compiled daemon in PLURX_TEST_NAMESPACE_BOOTSTRAP and unprivileged user namespaces"]
async fn namespace_probe_cancellation_kills_started_descendants() {
    assert!(std::env::var_os("PLURX_TEST_NAMESPACE_BOOTSTRAP").is_some());
    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("cancellable-namespace-probe");
    build_static_probe(&probe, 9);
    let identity = DecodeProbeIdentity::discover_with_mode(
        probe.to_str().expect("probe path"),
        ProbeLaunchMode::ProductionNamespace,
    )
    .await
    .expect("isolated production bootstrap");
    let media = root.path().join("owned-cancellation-marker-source");
    std::fs::write(&media, b"production-bound source").expect("media");
    // This owned test source is deliberately writable solely to signal that
    // the hostile descendant has started, before cancellation is requested.
    let source = Arc::new(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&media)
            .expect("owned marker source"),
    );
    let cleanup_gate = Arc::new(tokio::sync::Semaphore::new(1));
    let probe_gate = Arc::clone(&cleanup_gate);
    let token = tokio_util::sync::CancellationToken::new();
    let probe_token = token.clone();
    let task = tokio::spawn(async move {
        DecodeFactCache::new()
            .get_or_probe(
                &identity,
                DecodeFactSource::new(source, probe_gate, TEST_FACT_WORK),
                None,
                ProbeStreamSelection::Absolute(4),
                Duration::from_secs(10),
                Some(&probe_token),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !std::fs::read(&media)
            .expect("owned source marker")
            .starts_with(b"namespace-child-started")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual descendant must start before cancellation");
    token.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("cancellation must not wait for the descendant60s sleep")
        .expect("probe task joins");
    assert!(matches!(result, Err(DecodeFactError::Cancelled)));
    let _reaped_source = tokio::time::timeout(Duration::from_secs(2), cleanup_gate.acquire_owned())
        .await
        .expect("descendant cleanup must reap before returning source ownership")
        .expect("source ownership remains available");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn production_bootstrap_eintr_retries_share_the_absolute_deadline() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("static-ffprobe");
    build_static_probe(&probe, 0);
    let mut identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
        .await
        .expect("production probe identity and version");
    identity.launch_mode = ProbeLaunchMode::ProductionBootstrapInterrupted;
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"production-bound source").expect("media");
    let source = Arc::new(std::fs::File::open(media).expect("open media"));
    let started = std::time::Instant::now();
    let facts = DecodeFactCache::new()
        .get_or_probe(
            &identity,
            DecodeFactSource::isolated(source),
            None,
            ProbeStreamSelection::Absolute(4),
            Duration::from_secs(5),
            None,
        )
        .await
        .expect("all injected bootstrap EINTR paths retry successfully");
    assert_eq!(facts.input_video_stream(), 4);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "ready send, acknowledgement, first receive, and first response retries remain bounded"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn persistent_bootstrap_eintr_expires_and_reclaims_supervisor_ownership() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("static-ffprobe");
    build_static_probe(&probe, 0);
    let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
        .await
        .expect("production probe identity and version");

    for phase in [
        LinuxBootstrapPhase::ReadySend,
        LinuxBootstrapPhase::AcknowledgementSend,
        LinuxBootstrapPhase::FirstNotificationReceive,
        LinuxBootstrapPhase::FirstNotificationInvalidated,
        LinuxBootstrapPhase::FirstNotificationResponse,
    ] {
        assert_eq!(INTERRUPTED_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
        let ownership = Arc::new(tokio::sync::Semaphore::new(1));
        let started = std::time::Instant::now();
        let result = probe_version_with_deadline_on(
            &identity.executable_snapshot,
            identity.executable(),
            ProbeLaunchMode::ProductionBootstrapInterruptedUntilDeadline(phase),
            Duration::from_millis(100),
            Arc::clone(&ownership),
        )
        .await;
        assert!(
            result.is_err(),
            "phase {phase:?} must not pass an interrupted bootstrap"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(75),
            "phase {phase:?} must keep retrying against the launch deadline"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "phase {phase:?} must fail at its absolute deadline"
        );
        let _ownership = tokio::time::timeout(
            Duration::from_secs(5),
            Arc::clone(&ownership).acquire_owned(),
        )
        .await
        .unwrap_or_else(|_| panic!("phase {phase:?} cleanup did not return ownership"))
        .expect("version ownership semaphore remains open");
        assert_eq!(
            INTERRUPTED_SUPERVISOR_OWNERS.load(Ordering::Acquire),
            0,
            "phase {phase:?} supervisor is reclaimed only after cleanup"
        );
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn persistent_steady_response_eintr_cannot_wedge_detached_cleanup() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("second-exec-static-ffprobe");
    build_static_probe(&probe, 2);
    let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
        .await
        .expect("production probe identity and version");
    assert_eq!(STEADY_RESPONSE_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
    STEADY_RESPONSE_DEADLINE_INTERRUPT_HITS.store(0, Ordering::Release);
    STEADY_RESPONSE_DEADLINE_EXITS.store(0, Ordering::Release);
    let ownership = Arc::new(tokio::sync::Semaphore::new(1));
    // The launch deadline is the subject here, so it cannot be replaced by
    // an event, but it must outlast the launch it bounds: the second exec
    // notification only arrives once the probe has been forked, handed
    // its filter and exec'd twice, and a loaded runner took longer than
    // the 100 ms this used to allow. The injected interruptions then hold
    // the response loop until this deadline, whatever its length.
    let deadline = Duration::from_secs(3);
    let started = std::time::Instant::now();
    // The reap below is held until this test releases it, after the call
    // returns. A caller that joined its cleanup instead of detaching would
    // therefore wait forever, so the call gets its own outer bound: that
    // regression fails here, not as a hung test binary.
    let returned = tokio::time::timeout(
        deadline + Duration::from_secs(20),
        probe_version_with_deadline_on(
            &identity.executable_snapshot,
            identity.executable(),
            ProbeLaunchMode::ProductionSteadyResponseInterruptedUntilDeadline,
            deadline,
            Arc::clone(&ownership),
        ),
    )
    .await
    .expect("the caller must detach its held cleanup at the launch deadline");
    assert_eq!(returned, Err(DecodeFactError::Deadline));
    assert!(
            STEADY_RESPONSE_DEADLINE_INTERRUPT_HITS.load(Ordering::Acquire) > 1,
            "the production supervisor must receive the second exec notification and consume persistent response interruptions"
        );
    assert!(
        started.elapsed() >= deadline,
        "only the shared launch deadline ends persistent response interruptions"
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        while STEADY_RESPONSE_DEADLINE_EXITS.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the response loop itself must observe the shared deadline");
    // The caller detached at the deadline: its reap is held (see
    // `STEADY_RESPONSE_REAP_RELEASE`), and it has returned all the same,
    // leaving the version owner and the supervisor with the cleanup.
    assert!(
        Arc::clone(&ownership).try_acquire_owned().is_err(),
        "the version owner remains held during deliberately slow reap"
    );
    assert_eq!(STEADY_RESPONSE_SUPERVISOR_OWNERS.load(Ordering::Acquire), 1);
    STEADY_RESPONSE_REAP_RELEASE.add_permits(1);
    let _ownership = tokio::time::timeout(Duration::from_secs(20), ownership.acquire_owned())
        .await
        .expect("steady-response cleanup cannot wedge supervisor join")
        .expect("version ownership returns after reap and supervisor teardown");
    assert_eq!(STEADY_RESPONSE_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn supervisor_stop_bounds_persistent_steady_response_interrupts() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("second-exec-stop-static-ffprobe");
    build_static_probe(&probe, 2);
    let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
        .await
        .expect("production probe identity and version");
    assert_eq!(
        STEADY_RESPONSE_STOP_SUPERVISOR_OWNERS.load(Ordering::Acquire),
        0
    );
    STEADY_RESPONSE_STOP_INTERRUPT_HITS.store(0, Ordering::Release);
    STEADY_RESPONSE_STOP_EXITS.store(0, Ordering::Release);
    STEADY_RESPONSE_STOP_DEADLINE_EXITS.store(0, Ordering::Release);
    let ownership = Arc::new(tokio::sync::Semaphore::new(1));
    // Long enough that a loaded runner's launch cannot reach it: whether
    // the stop signal or the deadline released the loop is read from the
    // loop's own exit counters below, not from how long the call took.
    probe_version_with_deadline_on(
        &identity.executable_snapshot,
        identity.executable(),
        ProbeLaunchMode::ProductionSteadyResponseInterruptedUntilStop,
        Duration::from_secs(20),
        Arc::clone(&ownership),
    )
    .await
    .expect("the supervisor stop signal releases the response loop before deadline");
    assert!(
        STEADY_RESPONSE_STOP_INTERRUPT_HITS.load(Ordering::Acquire) >= 1,
        "the stop-path proof must first consume a steady response interruption"
    );
    assert!(
        STEADY_RESPONSE_STOP_EXITS.load(Ordering::Acquire) >= 1,
        "the response loop itself must observe the supervisor stop signal"
    );
    assert_eq!(
        STEADY_RESPONSE_STOP_DEADLINE_EXITS.load(Ordering::Acquire),
        0,
        "the stop signal, not the long launch deadline, must release the response loop"
    );
    let _ownership = ownership
        .try_acquire_owned()
        .expect("stop-path supervisor and version ownership return before completion");
    assert_eq!(
        STEADY_RESPONSE_STOP_SUPERVISOR_OWNERS.load(Ordering::Acquire),
        0
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn production_probe_allows_only_the_initial_bound_exec() {
    let root = crate::test_tempdir().expect("tempdir");
    for (mode, name) in [
        (1, "path-launcher"),
        (2, "direct-and-proc-memfd-launcher"),
        (3, "fd-reuse-launcher"),
    ] {
        let probe = root.path().join(name);
        build_static_probe(&probe, mode);
        DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
            .await
            .unwrap_or_else(|error| panic!("{name} must enter and observe blocked exec: {error}"));
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn production_probe_descendants_cannot_leave_the_kill_domain() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("session-escape-probe");
    build_static_probe(&probe, 4);
    let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
        .await
        .expect("production probe identity");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"source").expect("media");
    let source = Arc::new(std::fs::File::open(media).expect("open media"));
    tokio::time::timeout(
        Duration::from_millis(1_500),
        DecodeFactCache::new().get_or_probe(
            &identity,
            DecodeFactSource::isolated(source),
            None,
            ProbeStreamSelection::Absolute(4),
            Duration::from_secs(2),
            None,
        ),
    )
    .await
    .expect("escaped child must not retain the probe pipes")
    .expect("denied setsid child remains in the killed process group");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn production_probe_cannot_export_the_bound_source_descriptor() {
    use std::os::fd::{AsRawFd, FromRawFd as _};

    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("fd-export-probe");
    build_static_probe(&probe, 5);
    let mut identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
        .await
        .expect("production probe identity");
    let (exporter, receiver) = std::os::unix::net::UnixDatagram::pair().expect("socket pair");
    let exporter_fd = unsafe { libc::fcntl(exporter.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    assert!(
        exporter_fd >= 10,
        "reserve a collision-free export descriptor"
    );
    let exporter_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(exporter_fd) };
    identity.launch_mode = ProbeLaunchMode::ProductionFdExport(exporter_fd.as_raw_fd());
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"source").expect("media");
    let source = Arc::new(std::fs::File::open(media).expect("open media"));
    DecodeFactCache::new()
        .get_or_probe(
            &identity,
            DecodeFactSource::isolated(source),
            None,
            ProbeStreamSelection::Absolute(4),
            Duration::from_secs(5),
            None,
        )
        .await
        .expect("post-bootstrap sendmsg is denied");
    receiver
        .set_nonblocking(true)
        .expect("nonblocking receiver");
    let mut byte = [0_u8; 1];
    assert!(
        receiver.recv(&mut byte).is_err_and(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )
        }),
        "the external process must receive neither payload nor SCM_RIGHTS"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn pidfd_open_failure_detaches_bounded_version_cleanup() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("static-ffprobe");
    build_static_probe(&probe, 0);
    let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
        .await
        .expect("production identity");
    assert_eq!(PIDFD_OPEN_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
    let ownership = Arc::new(tokio::sync::Semaphore::new(1));
    let started = std::time::Instant::now();
    assert_eq!(
        probe_version_with_deadline_on(
            &identity.executable_snapshot,
            identity.executable(),
            ProbeLaunchMode::ProductionPidfdOpenFailure,
            Duration::from_millis(100),
            Arc::clone(&ownership),
        )
        .await,
        Err(DecodeFactError::Deadline)
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "pidfd-open cleanup must remain behind the caller deadline"
    );
    assert!(
        Arc::clone(&ownership).try_acquire_owned().is_err(),
        "version ownership remains held until explicit reap"
    );
    assert!(
        PIDFD_OPEN_SUPERVISOR_OWNERS.load(Ordering::Acquire) == 1,
        "production supervisor ownership remains attached through reap"
    );
    let _ownership = tokio::time::timeout(Duration::from_secs(2), ownership.acquire_owned())
        .await
        .expect("detached pidfd-open cleanup finishes")
        .expect("version ownership returns after reap");
    assert_eq!(PIDFD_OPEN_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn delayed_supervisor_handshake_obeys_the_version_deadline() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("static-ffprobe");
    build_static_probe(&probe, 0);
    let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
        .await
        .expect("production identity");
    assert_eq!(DELAYED_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
    let ownership = Arc::new(tokio::sync::Semaphore::new(1));
    let started = std::time::Instant::now();
    assert_eq!(
        probe_version_with_deadline_on(
            &identity.executable_snapshot,
            identity.executable(),
            ProbeLaunchMode::ProductionSupervisorDelay,
            Duration::from_millis(100),
            Arc::clone(&ownership),
        )
        .await,
        Err(DecodeFactError::Deadline)
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "the caller must not block in the pre-exec notification handshake"
    );
    assert!(Arc::clone(&ownership).try_acquire_owned().is_err());
    assert_eq!(DELAYED_SUPERVISOR_OWNERS.load(Ordering::Acquire), 1);
    let _ownership = tokio::time::timeout(Duration::from_secs(2), ownership.acquire_owned())
        .await
        .expect("detached launch owner finishes")
        .expect("version ownership returns after launch cleanup");
    assert_eq!(DELAYED_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn failed_listener_receive_cannot_leak_or_deadlock_spawn() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("static-ffprobe");
    build_static_probe(&probe, 0);
    let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
        .await
        .expect("production identity");
    assert_eq!(FAILED_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
    let ownership = Arc::new(tokio::sync::Semaphore::new(1));
    let started = std::time::Instant::now();
    let result = probe_version_with_deadline_on(
        &identity.executable_snapshot,
        identity.executable(),
        ProbeLaunchMode::ProductionSupervisorReceiveFailure,
        Duration::from_secs(1),
        Arc::clone(&ownership),
    )
    .await;
    assert!(
        !matches!(result, Err(DecodeFactError::Deadline)),
        "the injected receiver failure must actively unblock spawn"
    );
    assert!(started.elapsed() < Duration::from_millis(500));
    let _ownership = ownership
        .try_acquire_owned()
        .expect("failed launch returns version ownership");
    assert_eq!(FAILED_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn pidfd_read_failure_retains_source_ownership_until_reap() {
    use std::io::{Seek, SeekFrom};

    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("static-ffprobe");
    build_static_probe(&probe, 6);
    let mut identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
        .await
        .expect("production identity");
    identity.launch_mode = ProbeLaunchMode::ProductionPidfdReadFailure;
    assert_eq!(PIDFD_READ_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"0123456789").expect("media");
    let mut opened = std::fs::File::open(media).expect("open media");
    opened.seek(SeekFrom::Start(3)).expect("set source offset");
    let source = Arc::new(opened);
    let ownership = Arc::new(tokio::sync::Semaphore::new(1));
    let cache = DecodeFactCache::new();
    let started = std::time::Instant::now();
    assert_eq!(
        cache
            .get_or_probe(
                &identity,
                DecodeFactSource::new(Arc::clone(&source), Arc::clone(&ownership), TEST_FACT_WORK,),
                None,
                ProbeStreamSelection::Absolute(4),
                Duration::from_millis(400),
                None,
            )
            .await,
        Err(DecodeFactError::Deadline)
    );
    assert!(
        started.elapsed() < Duration::from_millis(800),
        "pidfd-read cleanup must remain behind the caller deadline"
    );
    assert!(
        Arc::clone(&ownership).try_acquire_owned().is_err(),
        "source ownership remains held until explicit reap"
    );
    assert!(
        PIDFD_READ_SUPERVISOR_OWNERS.load(Ordering::Acquire) == 1,
        "production supervisor ownership remains attached through reap"
    );
    assert!(
        cache.metrics().prometheus().contains(
            "plurx_decode_facts_phase_seconds_count{phase=\"collection\",outcome=\"deadline\"} 0"
        ),
        "the detached collection must not publish a phase sample before slow reap finishes"
    );
    let _ownership = tokio::time::timeout(Duration::from_secs(2), ownership.acquire_owned())
        .await
        .expect("detached pidfd-read cleanup finishes")
        .expect("source ownership returns after reap");
    tokio::time::timeout(Duration::from_millis(250), async {
            loop {
                if cache.metrics().prometheus().contains(
                    "plurx_decode_facts_phase_seconds_count{phase=\"collection\",outcome=\"deadline\"} 1"
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the collection sample appears after the detached owner finishes cleanup");
    assert_eq!(PIDFD_READ_SUPERVISOR_OWNERS.load(Ordering::Acquire), 0);
    assert_eq!(
        source
            .try_clone()
            .expect("source view")
            .stream_position()
            .expect("restored source offset"),
        3
    );
}

#[cfg(unix)]
#[tokio::test]
async fn blocked_source_identity_returns_deadline_without_releasing_probe_ownership() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        "#!/bin/sh\nprintf '%s\\n' 'ffprobe version source-identity'\n",
    );
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("fixture identity");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"source").expect("media");
    let source =
        DecodeFactSource::isolated(Arc::new(std::fs::File::open(media).expect("open media")))
            .with_identity_delay(Duration::from_secs(1));
    let cache = DecodeFactCache::new();
    let ownership = Arc::clone(&cache.probe_gate);
    let started = std::time::Instant::now();
    assert_eq!(
        cache
            .get_or_probe(
                &identity,
                source,
                None,
                ProbeStreamSelection::FirstPlayable,
                Duration::from_millis(100),
                None,
            )
            .await,
        Err(DecodeFactError::Deadline)
    );
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(
        Arc::clone(&ownership).try_acquire_owned().is_err(),
        "the blocked source syscall must retain the single probe lane"
    );
    let _ownership = tokio::time::timeout(Duration::from_secs(2), ownership.acquire_owned())
        .await
        .expect("detached source observation finishes")
        .expect("probe ownership returns after the syscall owner exits");
}

/// M8's shipped-shape test for the decode-fact source: the production
/// constructor (no-op hooks) runs the blocked-identity test's scenario —
/// same fixture probe and source, with no observation delayed. It checks
/// that the probe runs and the single probe lane is free when the call
/// returns. Its budget is the fixture hang guard, so process scheduling
/// on a loaded runner cannot turn it into a deadline test; the
/// neighboring blocked-source test checks the 100 ms deadline.
/// Acceptance runs it in the release profile
/// (`cargo test --release -p plurxd decode_fact_source_shipped_shape`).
#[cfg(unix)]
#[tokio::test]
async fn decode_fact_source_shipped_shape() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        "#!/bin/sh\nprintf '%s\\n' 'ffprobe version source-identity'\n",
    );
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("fixture identity");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"source").expect("media");
    let source = DecodeFactSource::new(
        Arc::new(std::fs::File::open(media).expect("open media")),
        Arc::new(tokio::sync::Semaphore::new(1)),
        TEST_FACT_WORK,
    );
    let cache = DecodeFactCache::new();
    let ownership = Arc::clone(&cache.probe_gate);
    // This checks the shipped FD/source shape and probe-lane release,
    // not a wall-clock latency promise. A loaded runner can spend seconds
    // scheduling the background-class shell before it prints its fixed
    // output, so the budget is only a hang guard. Dedicated deadline
    // regressions below retain their short budgets.
    let result = cache
        .get_or_probe(
            &identity,
            source,
            None,
            ProbeStreamSelection::FirstPlayable,
            FIXTURE_HANG_GUARD,
            None,
        )
        .await;
    assert!(
        matches!(result, Err(DecodeFactError::InvalidJson(_))),
        "the undelayed observation finishes inside the budget, so the fixture probe runs \
             and its (non-JSON) output is what fails, not the deadline: {result:?}"
    );
    assert!(
        Arc::clone(&ownership).try_acquire_owned().is_ok(),
        "an undelayed observation has returned the single probe lane"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn production_probe_refuses_an_executable_source_fd() {
    use std::os::unix::fs::PermissionsExt;

    let root = crate::test_tempdir().expect("tempdir");
    let probe = root.path().join("static-ffprobe");
    build_static_probe(&probe, 0);
    let identity = DecodeProbeIdentity::discover(probe.to_str().expect("probe path"))
        .await
        .expect("production probe identity");
    let source_path = root.path().join("executable-media");
    std::fs::write(&source_path, b"not executable content").expect("source");
    std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o500))
        .expect("mark source executable");
    let source = Arc::new(std::fs::File::open(source_path).expect("open source"));
    let error = DecodeFactCache::new()
        .get_or_probe(
            &identity,
            DecodeFactSource::isolated(source),
            None,
            ProbeStreamSelection::Absolute(4),
            Duration::from_secs(5),
            None,
        )
        .await
        .expect_err("executable source must be refused before FD 3 installation");
    assert!(matches!(
        error,
        DecodeFactError::SourceMetadata(reason) if reason.contains("executable mode")
    ));
}

#[cfg(unix)]
#[test]
fn process_group_termination_is_consumed_exactly_once() {
    let guard = ProbeSessionGuard::new(Some(libc::pid_t::MAX));
    let terminator = guard.terminator();
    assert_eq!(terminator.take_process_group(), Some(libc::pid_t::MAX));
    assert_eq!(terminator.take_process_group(), None);
    drop(guard);
}

#[cfg(unix)]
#[tokio::test]
async fn facts_are_collected_from_the_held_descriptor_and_cached_by_build() {
    let root = crate::test_tempdir().expect("tempdir");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"bound source").expect("media");
    let source = Arc::new(std::fs::File::open(&media).expect("open media"));
    std::fs::remove_file(&media).expect("unlink path after opening");
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then
  printf '%s\n' 'ffprobe version test-build'
  exit 0
fi
test ! -e "$PLURX_TEST_PROBE_PATH.used" || exit 93
touch "$PLURX_TEST_PROBE_PATH.used"
test "$8" = "/dev/fd/3" || exit 91
test "$(cat "$8")" = "bound source" || exit 92
printf '%s\n' '{"streams":[{"index":4,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24000/1001","r_frame_rate":"24000/1001","disposition":{"attached_pic":0}}]}'
"###,
    );
    let cache = DecodeFactCache::with_capacity(2);
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("probe identity");
    let first = cache
        .get_or_probe(
            &identity,
            DecodeFactSource::isolated(Arc::clone(&source)),
            None,
            ProbeStreamSelection::Absolute(4),
            PROBE_DEADLINE,
            None,
        )
        .await
        .expect("bound facts");
    assert_eq!(first.input_video_stream(), 4);
    let mut offset_view = source.try_clone().expect("clone source descriptor");
    assert_eq!(
        std::io::Seek::stream_position(&mut offset_view).expect("source offset"),
        0,
        "fact collection must restore the held descriptor's file offset"
    );
    let cached = cache
        .get_or_probe(
            &identity,
            DecodeFactSource::isolated(source),
            None,
            ProbeStreamSelection::Absolute(4),
            PROBE_DEADLINE,
            None,
        )
        .await
        .expect("cache hit does not respawn");
    assert_eq!(cached.facts_digest(), first.facts_digest());
}

#[cfg(unix)]
#[tokio::test]
async fn flagged_source_runs_idet_on_the_same_descriptor_before_releasing_its_offset_lane() {
    use std::io::{Seek, SeekFrom};

    let root = crate::test_tempdir().expect("tempdir");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"bound interlace source").expect("media");
    let source = Arc::new(std::fs::File::open(&media).expect("open media"));
    let mut offset_view = source.try_clone().expect("offset view");
    offset_view.seek(SeekFrom::Start(3)).expect("set offset");
    std::fs::remove_file(&media).expect("unlink path after opening");
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version idet-bound'; exit 0; fi
if test "$1" = "-hide_banner"; then
  test -r /dev/fd/3 || exit 94
  printf '%s\n' 'video'
  printf '%s\n' '[Parsed_idet_2] Multi frame detection: TFF: 0 BFF: 0 Progressive: 91 Undetermined: 9' >&2
  exit 0
fi
test "$8" = "/dev/fd/3" || exit 91
cat "$8" >/dev/null || exit 92
printf '%s\n' '{"streams":[{"index":4,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"30000/1001","r_frame_rate":"30000/1001","field_order":"tt","disposition":{"attached_pic":0}}]}'
"###,
    );
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("probe identity");
    let facts = DecodeFactCache::new()
        .get_or_probe(
            &identity,
            DecodeFactSource::isolated(source),
            None,
            ProbeStreamSelection::Absolute(4),
            PROBE_DEADLINE,
            None,
        )
        .await
        .expect("bound facts and idet verdict");
    assert_eq!(facts.interlace_verdict(), InterlaceVerdict::FlagOverruled);
    assert_eq!(facts.scan_type(), ScanType::Progressive);
    assert_eq!(
        offset_view.stream_position().expect("restored offset"),
        3,
        "both children must finish before the shared offset is restored"
    );
}

#[cfg(unix)]
fn full_document_probe_fixture(root: &Path, field_order: &str) -> PathBuf {
    let probe = root.join("ffprobe-document");
    let document = serde_json::json!({
        "streams":[{"index":4,"codec_type":"video","codec_name":"h264",
            "profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,
            "avg_frame_rate":"30000/1001","r_frame_rate":"30000/1001",
            "field_order":field_order,"disposition":{"attached_pic":0}}],
        "format":{"format_name":"matroska","duration":"12.0"},
        "chapters":[{"id":0,"start_time":"0.0","end_time":"12.0"}]
    });
    executable(
        &probe,
        &format!(
            r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version document-fixture'; exit 0; fi
if test "$1" = "-hide_banner"; then
  echo idet >> "$PLURX_TEST_PROBE_PATH.idet"
  printf '%s\n' '[Parsed_idet_2] Multi frame detection: TFF: 0 BFF: 0 Progressive: 91 Undetermined: 9' >&2
  exit 0
fi
test "$8" = "/dev/fd/3" || exit 91
cat "$8" >/dev/null || exit 92
echo probe >> "$PLURX_TEST_PROBE_PATH.calls"
printf '%s\n' '{document}'
"###
        ),
    );
    probe
}

#[cfg(unix)]
#[tokio::test]
async fn fresh_source_document_and_decoder_facts_share_one_stream_probe() {
    use std::io::{Seek, SeekFrom};
    for field_order in ["progressive", "tt"] {
        let root = crate::test_tempdir().expect("document fixture");
        let probe = full_document_probe_fixture(root.path(), field_order);
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("path"))
            .await
            .expect("sealed fixture identity");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"descriptor-bound document").expect("source");
        let handle = Arc::new(std::fs::File::open(&media).expect("source handle"));
        let mut offset = handle.try_clone().expect("offset witness");
        offset.seek(SeekFrom::Start(3)).expect("initial offset");
        let calls = probe.with_extension("calls");
        let idet = probe.with_extension("idet");
        // The previous two independent owners each needed a stream probe.
        DecodeFactCache::new()
            .probe_source_document(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&handle)),
                None,
                ProbeStreamSelection::Absolute(4),
                PROBE_DEADLINE,
                None,
            )
            .await
            .expect("old verification owner");
        DecodeFactCache::new()
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&handle)),
                None,
                ProbeStreamSelection::Absolute(4),
                PROBE_DEADLINE,
                None,
            )
            .await
            .expect("old independent planning owner");
        assert_eq!(
            std::fs::read_to_string(&calls)
                .expect("old calls")
                .lines()
                .count(),
            2
        );
        std::fs::write(&calls, "").expect("reset measured launches");
        if idet.exists() {
            std::fs::remove_file(&idet).expect("reset idet witness");
        }
        let cache = DecodeFactCache::new();
        let prepared = cache
            .probe_source_document(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&handle)),
                None,
                ProbeStreamSelection::Absolute(4),
                PROBE_DEADLINE,
                None,
            )
            .await
            .expect("full source verification");
        let document: serde_json::Value =
            serde_json::from_str(&prepared.document).expect("full document");
        assert_eq!(document["format"]["format_name"], "matroska");
        assert_eq!(document["chapters"].as_array().expect("chapters").len(), 1);
        assert_eq!(
            plurx_core::scan::probe::reporter_of(&document),
            Some("ffprobe version document-fixture")
        );
        assert!(
            !idet.exists(),
            "source verification must not consume the optional refinement budget"
        );
        assert_eq!(
            offset.stream_position().expect("offset after full probe"),
            3
        );
        let facts = cache
            .refine_source_document(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&handle)),
                None,
                prepared,
                PROBE_DEADLINE,
                None,
            )
            .await
            .expect("same-document decoder planning");
        assert_eq!(facts.input_video_stream(), 4);
        assert_eq!(facts.scan_type(), ScanType::Progressive);
        assert_eq!(
            std::fs::read_to_string(&calls)
                .expect("new calls")
                .lines()
                .count(),
            1,
            "source verification and decoder facts must use one stream FFprobe, not two"
        );
        assert_eq!(idet.exists(), field_order == "tt");
        assert_eq!(
            offset.stream_position().expect("offset after refinement"),
            3
        );
        cache
            .probe_source_document(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&handle)),
                None,
                ProbeStreamSelection::Absolute(4),
                PROBE_DEADLINE,
                None,
            )
            .await
            .expect("fresh verification on warm cache");
        assert_eq!(
            std::fs::read_to_string(&calls)
                .expect("fresh calls")
                .lines()
                .count(),
            2,
            "cached facts must never stand in for fresh source verification"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn source_verification_keeps_decoder_refinement_failures_separate() {
    for missing_stream in [false, true] {
        let root = crate::test_tempdir().expect("document failure fixture");
        let probe = full_document_probe_fixture(root.path(), "tt");
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("path"))
            .await
            .expect("identity");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"unchanged source").expect("source");
        let handle = Arc::new(std::fs::File::open(media).expect("source"));
        let cache = DecodeFactCache::new();
        let prepared = cache
            .probe_source_document(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&handle)),
                None,
                ProbeStreamSelection::Absolute(if missing_stream { 9 } else { 4 }),
                PROBE_DEADLINE,
                None,
            )
            .await
            .expect("source verification still succeeds");
        assert!(prepared.document.contains("chapters"));
        let result = cache
            .refine_source_document(
                &identity,
                DecodeFactSource::isolated(handle),
                None,
                prepared,
                if missing_stream {
                    PROBE_DEADLINE
                } else {
                    Duration::ZERO
                },
                None,
            )
            .await;
        assert!(
            matches!(result, Err(DecodeFactError::InvalidFacts(_))) && missing_stream
                || matches!(result, Err(DecodeFactError::Deadline)) && !missing_stream
        );
        assert!(
            cache.entries.lock().await.is_empty(),
            "unfinished refinement must not poison the facts cache"
        );
        assert_eq!(
            std::fs::read_to_string(probe.with_extension("calls"))
                .expect("calls")
                .lines()
                .count(),
            1
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn prepared_source_document_refuses_changed_source_and_probe_before_refinement() {
    for replace_probe in [false, true] {
        let root = crate::test_tempdir().expect("document identity fixture");
        let probe = full_document_probe_fixture(root.path(), "tt");
        let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("path"))
            .await
            .expect("identity");
        let media = root.path().join("media.bin");
        std::fs::write(&media, b"old source").expect("source");
        let handle = Arc::new(std::fs::File::open(&media).expect("source"));
        let cache = DecodeFactCache::new();
        let prepared = cache
            .probe_source_document(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&handle)),
                None,
                ProbeStreamSelection::Absolute(4),
                PROBE_DEADLINE,
                None,
            )
            .await
            .expect("prepared source");
        if replace_probe {
            std::fs::write(&probe, b"changed executable").expect("replace probe");
        } else {
            std::fs::write(&media, b"changed source length").expect("replace source");
        }
        let result = cache
            .refine_source_document(
                &identity,
                DecodeFactSource::isolated(handle),
                None,
                prepared,
                PROBE_DEADLINE,
                None,
            )
            .await;
        assert!(
            matches!(result, Err(DecodeFactError::ProbeChanged)) && replace_probe
                || matches!(result, Err(DecodeFactError::SourceChanged)) && !replace_probe
        );
        assert!(
            !probe.with_extension("idet").exists(),
            "refused identity must not launch refinement"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_idet_reaps_the_second_child_before_returning_the_offset_lane() {
    use std::io::{Seek, SeekFrom};

    let root = crate::test_tempdir().expect("tempdir");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"cancelled interlace source").expect("media");
    let source = Arc::new(std::fs::File::open(&media).expect("open media"));
    let mut offset_view = source.try_clone().expect("offset view");
    offset_view.seek(SeekFrom::Start(5)).expect("set offset");
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version idet-cancel'; exit 0; fi
if test "$1" = "-hide_banner"; then
  touch "$PLURX_TEST_PROBE_PATH.idet"
  sleep 30
  exit 95
fi
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"30000/1001","r_frame_rate":"30000/1001","field_order":"tt","disposition":{"attached_pic":0}}]}'
"###,
    );
    let marker = probe.with_extension("idet");
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("probe identity");
    let offset_gate = Arc::new(tokio::sync::Semaphore::new(1));
    let cancelled = tokio_util::sync::CancellationToken::new();
    let child_cancelled = cancelled.clone();
    let child_gate = Arc::clone(&offset_gate);
    let task = tokio::spawn(async move {
        DecodeFactCache::new()
            .get_or_probe(
                &identity,
                DecodeFactSource::new(source, child_gate, TEST_FACT_WORK),
                None,
                ProbeStreamSelection::Absolute(0),
                PROBE_DEADLINE,
                Some(&child_cancelled),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("idet child started");
    cancelled.cancel();
    assert_eq!(
        task.await.expect("collection task"),
        Err(DecodeFactError::Cancelled)
    );
    let _offset_owner = tokio::time::timeout(Duration::from_secs(2), offset_gate.acquire_owned())
        .await
        .expect("idet cleanup returns the offset lane")
        .expect("offset lane remains open");
    assert_eq!(offset_view.stream_position().expect("restored offset"), 5);
}

#[cfg(unix)]
#[tokio::test]
async fn legacy_selection_binds_attached_art_and_catalog_to_the_resolved_absolute_stream() {
    let root = crate::test_tempdir().expect("tempdir");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"multi-video source").expect("media");
    let source = Arc::new(std::fs::File::open(&media).expect("open media"));
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version stream-selection'; exit 0; fi
printf '%s\n' '{"streams":[{"index":4,"codec_type":"video","codec_name":"mjpeg","profile":"Baseline","pix_fmt":"yuvj420p","width":600,"height":600,"avg_frame_rate":"25/1","r_frame_rate":"25/1","color_transfer":"bt709","disposition":{"attached_pic":1}},{"index":7,"codec_type":"video","codec_name":"hevc","profile":"Main 10","pix_fmt":"yuv420p10le","width":3840,"height":2160,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"smpte2084","disposition":{"attached_pic":0}}]}'
"###,
    );
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("probe identity");
    let cache = DecodeFactCache::new();
    let attached = cache
        .get_or_probe(
            &identity,
            DecodeFactSource::isolated(Arc::clone(&source)),
            None,
            ProbeStreamSelection::LegacyVideoOrdinal(0),
            Duration::from_secs(2),
            None,
        )
        .await
        .expect("legacy ordinal zero includes attached video");
    assert_eq!(attached.input_video_stream(), 4);
    assert_eq!(attached.codec(), Some("mjpeg"));

    let catalog = DecodeCatalogMetadata::new(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
        plurx_core::domain::DolbyVisionFacts {
            profile: Some(8),
            level: Some(6),
            bl_compat_id: Some(1),
            el_present: Some(false),
            rpu_present: Some(true),
        },
    )
    .expect("catalog facts");
    let attached_with_catalog = cache
        .get_or_probe(
            &identity,
            DecodeFactSource::isolated(Arc::clone(&source)),
            Some(&catalog),
            ProbeStreamSelection::LegacyVideoOrdinal(0),
            Duration::from_secs(2),
            None,
        )
        .await
        .expect("first-playable catalog is not applied to attached art");
    assert_eq!(attached_with_catalog.input_video_stream(), 4);
    assert_eq!(attached_with_catalog.dynamic_range(), Some("sdr"));
    let playable = cache
        .get_or_probe(
            &identity,
            DecodeFactSource::isolated(source),
            Some(&catalog),
            ProbeStreamSelection::LegacyVideoOrdinal(1),
            Duration::from_secs(2),
            None,
        )
        .await
        .expect("catalog binds after ordinal resolves to an absolute stream");
    assert_eq!(playable.input_video_stream(), 7);
    assert_eq!(playable.codec(), Some("hevc"));
    assert_eq!(playable.dynamic_range(), Some("dolby_vision"));
    assert_eq!(playable.dolby_vision().profile, Some(8));
}

#[cfg(unix)]
#[tokio::test]
async fn missing_probe_identity_is_refused_before_fact_collection() {
    let error = DecodeProbeIdentity::discover_fixture("plurx-no-such-ffprobe")
        .await
        .expect_err("missing probe");
    assert!(matches!(error, DecodeFactError::ProbeIdentity(_)));
}

#[cfg(unix)]
#[tokio::test]
async fn a_replaced_probe_cannot_reuse_cached_facts() {
    let root = crate::test_tempdir().expect("tempdir");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"source").expect("media");
    let source = Arc::new(std::fs::File::open(&media).expect("open"));
    let probe = root.path().join("ffprobe-test");
    let body = r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version one'; exit 0; fi
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###;
    executable(&probe, body);
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("identity");
    let cache = DecodeFactCache::new();
    cache
        .get_or_probe(
            &identity,
            DecodeFactSource::isolated(Arc::clone(&source)),
            None,
            ProbeStreamSelection::FirstPlayable,
            Duration::from_secs(2),
            None,
        )
        .await
        .expect("initial facts");
    executable(&probe, &body.replace("version one", "version two"));
    assert_eq!(
        cache
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(source),
                None,
                ProbeStreamSelection::FirstPlayable,
                Duration::from_secs(2),
                None,
            )
            .await,
        Err(DecodeFactError::ProbeChanged)
    );
}

#[cfg(unix)]
fn generation_probe_fixture(root: &std::path::Path) -> std::path::PathBuf {
    let probe = root.join("ffprobe-generation");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version generation'; exit 0; fi
echo probe >> "$PLURX_TEST_PROBE_PATH.calls"
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","field_order":"progressive","color_transfer":"bt709","disposition":{"attached_pic":0}}],"format":{"format_name":"matroska"}}'
"###,
    );
    probe
}

/// S-13, lab 2026-10-11: each encoded-VOD start verifies its held source
/// (`probe_source_document`, a fresh collection by design) and then
/// refines its decoder plan from that document; three full-content
/// SHA-256s of the bound FFprobe per validation, under the process-wide
/// gate, put 117-162 ms of gate wait on concurrent warm starts. Within a
/// validation generation neither start may hash the probe at all.
#[cfg(unix)]
#[tokio::test]
async fn warm_starts_within_a_generation_hash_no_probe_content() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = generation_probe_fixture(root.path());
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("identity");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"warm source").expect("media");
    let handle = Arc::new(std::fs::File::open(&media).expect("source"));
    let cache = DecodeFactCache::new();
    let start = || async {
        let prepared = cache
            .probe_source_document(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&handle)),
                None,
                ProbeStreamSelection::LegacyVideoOrdinal(0),
                PROBE_DEADLINE,
                None,
            )
            .await
            .expect("fresh source verification");
        cache
            .refine_source_document(
                &identity,
                DecodeFactSource::isolated(Arc::clone(&handle)),
                None,
                prepared,
                PROBE_DEADLINE,
                None,
            )
            .await
            .expect("decoder refinement")
    };
    let hits = |cache: &DecodeFactCache| {
        cache.metrics().lookups[DecodeFactLookupResult::Hit.index()].load(Ordering::Relaxed)
    };
    start().await;
    let hits_after_first = hits(&cache);
    start().await;
    assert_eq!(
        hits(&cache),
        hits_after_first + 1,
        "the second start's refinement is a cache hit"
    );
    assert_eq!(
        identity.content_verifications(),
        0,
        "two warm starts inside one generation hashed the probe"
    );
    assert_eq!(
        std::fs::read_to_string(probe.with_extension("calls"))
            .expect("launches")
            .lines()
            .count(),
        2,
        "one verification child per start, none for the refinement"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn generation_boundary_forces_a_content_hash() {
    let root = crate::test_tempdir().expect("tempdir");
    let probe = generation_probe_fixture(root.path());
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("identity");
    identity
        .validate_current(PROBE_DEADLINE, None)
        .await
        .expect("tuple-only validation");
    assert_eq!(identity.content_verifications(), 0);
    identity.expire_content_verification();
    identity
        .validate_current(PROBE_DEADLINE, None)
        .await
        .expect("content validation at the boundary");
    assert_eq!(identity.content_verifications(), 1);
    // A clone shares the generation it just renewed.
    identity
        .clone()
        .validate_current(PROBE_DEADLINE, None)
        .await
        .expect("tuple-only validation after renewal");
    assert_eq!(identity.content_verifications(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn same_size_in_place_mutation_is_caught_within_one_lookup() {
    use std::os::unix::fs::FileExt;

    let root = crate::test_tempdir().expect("tempdir");
    let probe = generation_probe_fixture(root.path());
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("identity");
    let length = std::fs::metadata(&probe).expect("probe metadata").len();
    // Overwrite one byte of the trailing newline through a separate
    // descriptor: same length, same inode, new content and ctime.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&probe)
        .expect("separate writer")
        .write_at(b" ", length - 1)
        .expect("same-size write");
    assert_eq!(std::fs::metadata(&probe).expect("after").len(), length);
    assert_eq!(
        identity.validate_current(PROBE_DEADLINE, None).await,
        Err(DecodeFactError::ProbeChanged)
    );
    assert_eq!(
        identity.content_verifications(),
        1,
        "the tuple difference forced the content hash before the verdict"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn transient_path_swap_cannot_change_the_executable_object() {
    let root = crate::test_tempdir().expect("tempdir");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"source").expect("media");
    let source = std::fs::File::open(&media).expect("open source");
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version held-a'; exit 0; fi
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
    );
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("identity");
    let original = root.path().join("ffprobe-original");
    std::fs::rename(&probe, &original).expect("hold original inode under another name");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version transient-b'; exit 0; fi
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"hevc","profile":"Main","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
    );
    let observation = source_observation(&source).expect("source identity");
    let facts = collect(
        &identity,
        DecodeFactCollectionSource {
            handle: Arc::new(source),
            observation,
            offset_permit: Arc::new(tokio::sync::Semaphore::new(1))
                .acquire_owned()
                .await
                .expect("source offset permit"),
            work: TEST_FACT_WORK,
            projection: ProbeProjection::Decoder,
            seed: None,
        },
        None,
        ProbeStreamSelection::FirstPlayable,
        Duration::from_secs(2),
        None,
    )
    .await
    .expect("held executable runs even while its path names another program")
    .facts
    .expect("valid typed facts");
    assert_eq!(facts.codec(), Some("h264"));
    std::fs::remove_file(&probe).expect("remove transient executable");
    std::fs::rename(&original, &probe).expect("restore original path");
    assert_eq!(
        probe_file_identity(&probe)
            .expect("restored executable identity")
            .content_digest,
        identity.file.content_digest
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn snapshot_path_swap_cannot_change_the_executable_object() {
    let root = crate::test_tempdir().expect("tempdir");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"source").expect("media");
    let source = std::fs::File::open(&media).expect("open source");
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version snapshot-a'; exit 0; fi
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
    );
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("identity");
    let snapshot_path = identity.executable_snapshot.path().to_owned();
    let held_path = snapshot_path.with_extension("held");
    assert!(
        std::fs::rename(&snapshot_path, &held_path).is_err(),
        "the immutable snapshot path must refuse replacement"
    );
    let observation = source_observation(&source).expect("source identity");
    let facts = collect(
        &identity,
        DecodeFactCollectionSource {
            handle: Arc::new(source),
            observation,
            offset_permit: Arc::new(tokio::sync::Semaphore::new(1))
                .acquire_owned()
                .await
                .expect("source offset permit"),
            work: TEST_FACT_WORK,
            projection: ProbeProjection::Decoder,
            seed: None,
        },
        None,
        ProbeStreamSelection::FirstPlayable,
        Duration::from_secs(2),
        None,
    )
    .await
    .expect("held snapshot descriptor remains the execution object")
    .facts
    .expect("valid typed facts");
    assert_eq!(facts.codec(), Some("h264"));
    assert!(
        !probe.with_extension("snapshot-b").exists(),
        "replacement at the snapshot pathname must never execute"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn source_is_revalidated_after_post_probe_build_validation() {
    let root = crate::test_tempdir().expect("tempdir");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"source").expect("media");
    let source = Arc::new(std::fs::File::open(&media).expect("open source"));
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version source-fence'; exit 0; fi
touch "$PLURX_TEST_PROBE_PATH.started"
while ! test -f "$PLURX_TEST_PROBE_PATH.release"; do sleep 0.01; done
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
touch "$PLURX_TEST_PROBE_PATH.done"
"###,
    );
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("identity");
    let cache = Arc::new(DecodeFactCache::new());
    let task_cache = Arc::clone(&cache);
    let task_identity = identity.clone();
    let task_source = Arc::clone(&source);
    let task = tokio::spawn(async move {
        task_cache
            .get_or_probe(
                &task_identity,
                DecodeFactSource::isolated(task_source),
                None,
                ProbeStreamSelection::FirstPlayable,
                Duration::from_secs(4),
                None,
            )
            .await
    });
    for _ in 0..500 {
        if probe.with_extension("started").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(probe.with_extension("started").exists(), "probe started");
    let hash_barrier = identity_gate()
        .acquire_owned()
        .await
        .expect("identity gate");
    std::fs::write(probe.with_extension("release"), b"").expect("release probe output");
    for _ in 0..150 {
        if probe.with_extension("done").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(probe.with_extension("done").exists(), "probe completed");
    std::fs::write(&media, b"changed source").expect("mutate source after probe");
    drop(hash_barrier);
    assert_eq!(
        task.await.expect("join collector"),
        Err(DecodeFactError::SourceChanged)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn metrics_count_each_phase_once() {
    let root = crate::test_tempdir().expect("tempdir");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"source").expect("media");
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version phase-metrics'; exit 0; fi
sleep 0.2
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
    );
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("identity");
    let cache = DecodeFactCache::new();
    for _ in 0..2 {
        cache
            .get_or_probe(
                &identity,
                DecodeFactSource::isolated(Arc::new(
                    std::fs::File::open(&media).expect("open source"),
                )),
                None,
                ProbeStreamSelection::FirstPlayable,
                Duration::from_secs(2),
                None,
            )
            .await
            .expect("miss and warm hit both resolve");
    }
    let metrics = cache.metrics().prometheus();
    for (phase, count) in [
        ("gate_wait", 2),
        ("identity_validation", 2),
        ("source_observation", 3),
        ("collection", 1),
        ("final_validation", 1),
    ] {
        assert!(
                metrics.contains(&format!(
                    "plurx_decode_facts_phase_seconds_count{{phase=\"{phase}\",outcome=\"ok\"}} {count}"
                )),
                "missing expected phase count for {phase}:\n{metrics}"
            );
    }
    assert!(metrics.contains("plurx_decode_facts_lookups_total{result=\"hit\"} 1"));
    assert!(metrics.contains("plurx_decode_facts_lookups_total{result=\"miss_collected\"} 1"));
    assert!(metrics.contains("plurx_decode_facts_lookups_total{result=\"refused\"} 0"));
    for phase in ["gate_wait", "identity_validation", "source_observation"] {
        assert!(
            metrics.contains(&format!(
                "plurx_decode_facts_hit_phase_seconds_count{{phase=\"{phase}\"}} 1"
            )),
            "the warm-hit phase must be joinable to the hit result for {phase}:\n{metrics}"
        );
    }
    for phase in ["collection", "final_validation"] {
        assert!(
            metrics.contains(&format!(
                "plurx_decode_facts_hit_phase_seconds_count{{phase=\"{phase}\"}} 0"
            )),
            "the deliberately slow miss must not contaminate hit-only {phase}:\n{metrics}"
        );
    }
}

#[tokio::test]
async fn cancelled_identity_hash_keeps_the_single_worker_admission() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let cancelled = tokio_util::sync::CancellationToken::new();
    let worker_active = Arc::clone(&active);
    let worker_maximum = Arc::clone(&maximum);
    let task_gate = Arc::clone(&gate);
    let task_cancelled = cancelled.clone();
    let first = tokio::spawn(async move {
        run_bounded_identity_task_on(
            task_gate,
            Duration::from_secs(1),
            Some(&task_cancelled),
            move || {
                let now = worker_active.fetch_add(1, Ordering::SeqCst) + 1;
                worker_maximum.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(150));
                worker_active.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancelled.cancel();
    assert_eq!(
        first.await.expect("join cancelled identity waiter"),
        Err(DecodeFactError::Cancelled)
    );
    assert_eq!(
        run_bounded_identity_task_on(
            Arc::clone(&gate),
            Duration::from_millis(20),
            None,
            || Ok(())
        )
        .await,
        Err(DecodeFactError::Deadline)
    );
    tokio::time::sleep(Duration::from_millis(160)).await;
    run_bounded_identity_task_on(gate, Duration::from_secs(1), None, || Ok(()))
        .await
        .expect("lane reopens after the cancelled worker completes");
    assert_eq!(maximum.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn caller_deadline_detaches_cleanup_without_releasing_its_ownership() {
    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = Arc::clone(&gate)
        .acquire_owned()
        .await
        .expect("owned cleanup permit");
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let cleanup = tokio::spawn(async move {
        let _permit = permit;
        let _ = released.await;
        7_u8
    });

    assert_eq!(
        await_owned_collection(cleanup, Duration::from_millis(20), None).await,
        Err(DecodeFactError::Deadline)
    );
    assert!(
        Arc::clone(&gate).try_acquire_owned().is_err(),
        "detached cleanup must retain its owned permit"
    );
    release.send(()).expect("release detached cleanup");
    let _returned = tokio::time::timeout(Duration::from_secs(1), Arc::clone(&gate).acquire_owned())
        .await
        .expect("detached cleanup finishes")
        .expect("cleanup permit is returned");
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_same_key_misses_spawn_one_probe() {
    let root = crate::test_tempdir().expect("tempdir");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"source").expect("media");
    let source = Arc::new(std::fs::File::open(&media).expect("open"));
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version one'; exit 0; fi
test ! -e "$PLURX_TEST_PROBE_PATH.used" || exit 93
touch "$PLURX_TEST_PROBE_PATH.used"
sleep 1
printf '%s\n' '{"streams":[{"index":4,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
    );
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("identity");
    let cache = Arc::new(DecodeFactCache::new());
    let first_cache = Arc::clone(&cache);
    let first_source = Arc::clone(&source);
    let first_identity = identity.clone();
    let first = tokio::spawn(async move {
        first_cache
            .get_or_probe(
                &first_identity,
                DecodeFactSource::isolated(first_source),
                None,
                ProbeStreamSelection::Absolute(4),
                PROBE_DEADLINE,
                None,
            )
            .await
    });
    let second = cache.get_or_probe(
        &identity,
        DecodeFactSource::isolated(source),
        None,
        ProbeStreamSelection::Absolute(4),
        PROBE_DEADLINE,
        None,
    );
    let (first, second) = tokio::join!(first, second);
    let first = first.expect("join first").expect("first facts");
    let second = second.expect("second facts");
    assert_eq!(first.facts_digest(), second.facts_digest());
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_while_waiting_for_probe_admission_is_bounded() {
    let root = crate::test_tempdir().expect("tempdir");
    let first_media = root.path().join("first.bin");
    let second_media = root.path().join("second.bin");
    std::fs::write(&first_media, b"first").expect("first media");
    std::fs::write(&second_media, b"second").expect("second media");
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version one'; exit 0; fi
touch "$PLURX_TEST_PROBE_PATH.started"
sleep 1
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
    );
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("identity");
    let cache = Arc::new(DecodeFactCache::new());
    let first_cache = Arc::clone(&cache);
    let first_identity = identity.clone();
    let first = tokio::spawn(async move {
        first_cache
            .get_or_probe(
                &first_identity,
                DecodeFactSource::isolated(Arc::new(
                    std::fs::File::open(first_media).expect("first source"),
                )),
                None,
                ProbeStreamSelection::FirstPlayable,
                PROBE_DEADLINE,
                None,
            )
            .await
    });
    for _ in 0..(PROBE_DEADLINE.as_millis() / 10) {
        if probe.with_extension("started").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        probe.with_extension("started").exists(),
        "first probe started within the production deadline"
    );
    let cancellation = tokio_util::sync::CancellationToken::new();
    let waiter = cache.get_or_probe(
        &identity,
        DecodeFactSource::isolated(Arc::new(
            std::fs::File::open(second_media).expect("second source"),
        )),
        None,
        ProbeStreamSelection::FirstPlayable,
        PROBE_DEADLINE,
        Some(&cancellation),
    );
    tokio::pin!(waiter);
    tokio::select! {
        result = &mut waiter => panic!("waiter returned before cancellation: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(25)) => {}
    }
    cancellation.cancel();
    assert_eq!(waiter.await, Err(DecodeFactError::Cancelled));
    first.await.expect("join first").expect("first facts");
}

#[cfg(unix)]
#[tokio::test]
async fn running_cancellation_reaps_before_restoring_and_releasing_the_source() {
    use std::io::{Seek, SeekFrom};

    let root = crate::test_tempdir().expect("tempdir");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"0123456789").expect("media");
    let mut opened = std::fs::File::open(&media).expect("open source");
    opened.seek(SeekFrom::Start(3)).expect("set source offset");
    let source = Arc::new(opened);
    let offset_gate = Arc::new(tokio::sync::Semaphore::new(1));
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version running-cancel'; exit 0; fi
( sleep 1
  dd if="$8" of=/dev/null bs=1 skip=6 count=1 >/dev/null 2>&1
  touch "$PLURX_TEST_PROBE_PATH.descendant"
) &
touch "$PLURX_TEST_PROBE_PATH.started"
wait
"###,
    );
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("identity");
    let cancellation = tokio_util::sync::CancellationToken::new();
    let cache = DecodeFactCache::new();
    let collection = cache.get_or_probe(
        &identity,
        DecodeFactSource::new(
            Arc::clone(&source),
            Arc::clone(&offset_gate),
            TEST_FACT_WORK,
        ),
        None,
        ProbeStreamSelection::FirstPlayable,
        Duration::from_secs(8),
        Some(&cancellation),
    );
    tokio::pin!(collection);
    for _ in 0..100 {
        if probe.with_extension("started").exists() {
            break;
        }
        tokio::select! {
            result = &mut collection => panic!("collector returned before cancellation: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
    }
    assert!(probe.with_extension("started").exists(), "probe started");
    assert!(offset_gate.clone().try_acquire_owned().is_err());
    cancellation.cancel();
    assert_eq!(collection.await, Err(DecodeFactError::Cancelled));
    assert!(
        offset_gate.clone().try_acquire_owned().is_err(),
        "caller cancellation must not release ownership before cleanup"
    );
    let _permit = tokio::time::timeout(Duration::from_secs(1), offset_gate.clone().acquire_owned())
        .await
        .expect("cleanup must finish after cancellation")
        .expect("source lease released only after child reap");
    let mut view = source.try_clone().expect("source view");
    assert_eq!(view.stream_position().expect("restored source offset"), 3);
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(
        !probe.with_extension("descendant").exists(),
        "cancellation must terminate every inherited-descriptor descendant"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn normal_completion_kills_descendants_before_reaping_the_group_leader() {
    let root = crate::test_tempdir().expect("tempdir");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"source").expect("media");
    let source = Arc::new(std::fs::File::open(media).expect("open source"));
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version normal-group-exit'; exit 0; fi
( sleep 1; touch "$PLURX_TEST_PROBE_PATH.descendant" ) &
printf '%s\n' '{"streams":[{"index":4,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","disposition":{"attached_pic":0}}]}'
"###,
    );
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("identity");
    DecodeFactCache::new()
        .get_or_probe(
            &identity,
            DecodeFactSource::isolated(source),
            None,
            ProbeStreamSelection::Absolute(4),
            Duration::from_secs(3),
            None,
        )
        .await
        .expect("facts");
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(
        !probe.with_extension("descendant").exists(),
        "normal completion must kill the process group while the leader PID remains anchored"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn source_lease_wait_is_charged_to_the_probe_deadline() {
    let root = crate::test_tempdir().expect("tempdir");
    let media = root.path().join("media.bin");
    std::fs::write(&media, b"source").expect("media");
    let source = Arc::new(std::fs::File::open(&media).expect("open source"));
    let offset_gate = Arc::new(tokio::sync::Semaphore::new(1));
    let held = Arc::clone(&offset_gate)
        .acquire_owned()
        .await
        .expect("hold source lease");
    let probe = root.path().join("ffprobe-test");
    executable(
        &probe,
        r###"#!/bin/sh
if test "$1" = "-version"; then printf '%s\n' 'ffprobe version lease-deadline'; exit 0; fi
touch "$PLURX_TEST_PROBE_PATH.started"
sleep 5
printf '%s\n' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":1920,"height":1080,"avg_frame_rate":"24/1","r_frame_rate":"24/1","color_transfer":"bt709","disposition":{"attached_pic":0}}]}'
"###,
    );
    let identity = DecodeProbeIdentity::discover_fixture(probe.to_str().expect("probe path"))
        .await
        .expect("identity");
    let cache = DecodeFactCache::new();
    let started = std::time::Instant::now();
    let collection = cache.get_or_probe(
        &identity,
        DecodeFactSource::new(source, offset_gate, TEST_FACT_WORK),
        None,
        ProbeStreamSelection::FirstPlayable,
        Duration::from_secs(2),
        None,
    );
    tokio::pin!(collection);
    tokio::select! {
        result = &mut collection => panic!("collector returned while source lease was held: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
    }
    drop(held);
    assert_eq!(collection.await, Err(DecodeFactError::Deadline));
    assert!(
        started.elapsed() < Duration::from_millis(3_400),
        "waiting for the source lease must not restart the full probe budget"
    );
    assert!(
        probe.with_extension("started").exists(),
        "the regression must exercise collection after lease acquisition"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn crossed_source_and_executable_fds_are_duplicated_before_assignment() {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let root = crate::test_tempdir().expect("tempdir");
    let source_path = root.path().join("source");
    let executable_path = root.path().join("executable");
    std::fs::write(&source_path, b"bound-source\n").expect("source bytes");
    std::fs::write(&executable_path, b"held-probe\n").expect("probe bytes");
    let source = std::fs::File::open(source_path).expect("source");
    let executable = std::fs::File::open(executable_path).expect("executable");
    let source_fd = source.as_raw_fd();
    let executable_fd = executable.as_raw_fd();
    let mut command = std::process::Command::new("/bin/sh");
    command.args([
        "-c",
        "test \"$(cat <&3)\" = bound-source && test \"$(cat <&4)\" = held-probe",
    ]);
    unsafe {
        command.pre_exec(move || {
            let source_cross = duplicate_child_fd(source_fd)?;
            let executable_cross = match duplicate_child_fd(executable_fd) {
                Ok(duplicate) => duplicate,
                Err(error) => {
                    libc::close(source_cross);
                    return Err(error);
                }
            };
            let crossed = assign_child_fd(executable_cross, 3)
                .and_then(|()| assign_child_fd(source_cross, HELD_PROBE_FD));
            libc::close(source_cross);
            libc::close(executable_cross);
            crossed?;
            install_probe_child_fds(HELD_PROBE_FD, 3)
        });
    }
    assert!(command.status().expect("run descriptor check").success());
    drop((source, executable));
}

#[tokio::test]
async fn bounded_reader_drains_but_reports_oversize() {
    let bytes = vec![b'x'; 65];
    let (retained, overflow) = read_bounded(bytes.as_slice(), 64).await.expect("read");
    assert_eq!(retained.len(), 64);
    assert!(overflow);
}

#[test]
fn legacy_video_ordinals_preserve_the_command_builders_exact_mapping() {
    let json = serde_json::json!({"streams": [
        {"index": 2, "codec_type": "audio"},
        {"index": 4, "codec_type": "video", "disposition": {"attached_pic": 1}},
        {"index": 7, "codec_type": "video", "disposition": {"attached_pic": 0}}
    ]});
    assert_eq!(absolute_video_ordinal(&json, 0), Some(4));
    assert_eq!(absolute_video_ordinal(&json, 1), Some(7));
    assert_eq!(absolute_video_ordinal(&json, 2), None);
}

#[test]
fn source_frame_projection_is_bounded_selected_and_separate_from_raw_streams() {
    let raw = ProbeProjection::SourceDocument.arguments();
    assert!(raw.iter().any(|arg| arg == "-show_streams"));
    assert!(!raw
        .iter()
        .any(|arg| arg == "-show_frames" || arg == "-select_streams" || arg == "-read_intervals"));
    let frames = ProbeProjection::SourceFrames(7).arguments();
    assert!(frames
        .windows(2)
        .any(|pair| pair == ["-select_streams", "7"]));
    assert!(frames
        .windows(2)
        .any(|pair| pair == ["-read_intervals", "%+#3"]));
    assert!(frames.windows(2).any(|pair| pair == ["-threads", "1"]));
    assert!(!frames.iter().any(|arg| arg == "-show_streams"));
    assert!(ProbeProjection::SourceFrames(7).output_limit() <= MAX_PROBE_STDOUT_BYTES);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn source_frame_sidecar_keeps_raw_audio_and_refuses_changed_source_probe_or_stream() {
    let root = crate::test_tempdir().expect("frame sidecar");
    let path = root.path().join("ffprobe-frame-sidecar");
    let raw = serde_json::json!({"streams":[
        {"index":7,"codec_type":"video","codec_name":"h264","profile":"High","pix_fmt":"yuv420p","width":64,"height":64,
         "avg_frame_rate":"24/1","r_frame_rate":"24/1","field_order":"progressive"},
        {"index":9,"codec_type":"audio","codec_name":"aac"}],"format":{"format_name":"matroska"}});
    let frames = serde_json::json!({"frames":[{"stream_index":7,"side_data_list":[{"side_data_type":"Mastering display metadata","min_luminance":"1/10000"}]}]});
    let script = format!(
        r###"#!/bin/sh
if test "$1" = "-version"; then echo 'ffprobe version frame-sidecar'; exit 0; fi
cat /dev/fd/3 >/dev/null || exit 91
case " $* " in
 *" -show_frames "*) printf '%s\n' '{frames}' ;;
 *) printf '%s\n' '{raw}' ;;
esac
"###
    );
    executable(&path, &script);
    let probe = DecodeProbeIdentity::discover_fixture(path.to_str().expect("path"))
        .await
        .expect("identity");
    let media = root.path().join("source.bin");
    std::fs::write(&media, b"held source").expect("source");
    let handle = Arc::new(std::fs::File::open(&media).expect("held file"));
    let cache = Arc::new(DecodeFactCache::new());
    let prepared = cache
        .probe_source_document(
            &probe,
            DecodeFactSource::isolated(Arc::clone(&handle)),
            None,
            ProbeStreamSelection::LegacyVideoOrdinal(0),
            Duration::from_secs(5),
            None,
        )
        .await
        .expect("raw observation");
    let raw_before = prepared.document.clone();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&raw_before).expect("raw")["streams"]
            .as_array()
            .expect("streams")
            .len(),
        2
    );
    assert!(matches!(
        cache.source_frame_probe(
            &probe,
            DecodeFactSource::isolated(Arc::clone(&handle)),
            None,
            &prepared,
            8
        ),
        Err(DecodeFactError::InvalidFacts(_))
    ));
    let frame_probe = cache
        .source_frame_probe(
            &probe,
            DecodeFactSource::isolated(Arc::clone(&handle)),
            None,
            &prepared,
            7,
        )
        .expect("selected sidecar");
    assert_eq!(
        crate::vodencode::dv_runtime::held_source_minimum(
            Ok(Some(50)),
            Some(frame_probe.as_ref()),
            None
        )
        .await
        .expect("stream floor"),
        Some(50)
    );
    assert!(
        frame_probe.document.get().is_none(),
        "known stream floor never samples frames"
    );
    assert_eq!(
        crate::vodencode::dv_runtime::held_source_minimum(
            Ok(None),
            Some(frame_probe.as_ref()),
            None
        )
        .await
        .expect("real held frame minimum without xy"),
        Some(1)
    );
    assert_eq!(
        frame_probe.document(None).await.expect("frame observation"),
        &frames
    );
    assert_eq!(prepared.document, raw_before, "raw document unchanged");
    assert_eq!(
        cache
            .entries
            .lock()
            .await
            .get(&prepared.key)
            .expect("decoder cache retained")
            .input_video_stream(),
        7
    );
    let replacement = cache
        .source_frame_probe(
            &probe,
            DecodeFactSource::isolated(Arc::clone(&handle)),
            None,
            &prepared,
            7,
        )
        .expect("new lazy sidecar");
    std::fs::write(&media, b"source changed before lazy frame observation").expect("change");
    assert!(matches!(
        replacement.document(None).await,
        Err(DecodeFactError::SourceChanged)
    ));
    let changed_probe = cache
        .source_frame_probe(
            &probe,
            DecodeFactSource::isolated(handle),
            None,
            &prepared,
            7,
        )
        .expect("probe-bound sidecar");
    executable(&path, &(script + "# changed parser image\n"));
    assert!(matches!(
        changed_probe.document(None).await,
        Err(DecodeFactError::ProbeChanged)
    ));
}
