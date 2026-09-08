//! Media fixtures for the tests, built by the local ffmpeg on first use.
//!
//! Two crates need the same bytes: `plurx_core::fmp4` proves the reader and
//! the merger against them, and `plurxd::copyseg` drives a whole session
//! through them. Generating them in one place means the two suites cannot
//! drift onto subtly different GOP structures and disagree about what the
//! classifier should have said.
//!
//! Compiled only for tests — `#[cfg(test)]` inside this crate, and behind the
//! `fixtures` feature that `plurxd`'s dev-dependencies turn on. It is not part
//! of the shipped library.
//!
//! Everything lands in `target/fixtures/` and is reused across runs. The
//! sources are small on purpose (12 s of 640x360): CI pays for every x265
//! encode, and none of the properties under test — GOP structure, box layout,
//! sample timing — care how big the picture is.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn ffmpeg() -> String {
    std::env::var("PLURX_FFMPEG")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "ffmpeg".to_owned())
}

pub fn ffprobe() -> String {
    std::env::var("PLURX_FFPROBE")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "ffprobe".to_owned())
}

/// Fail in terms of the dependency that is missing, rather than in terms of
/// whatever a parser made of an empty file. plurxd shells out to ffmpeg at
/// runtime, so this is a thing to install, not a test to skip: skipping would
/// let CI report green on paths it never ran.
pub fn require_ffmpeg() {
    let bin = ffmpeg();
    let ok = Command::new(&bin)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok();
    assert!(
        ok,
        "these tests need ffmpeg; running `{bin}` failed. Install it \
         (`apt-get install ffmpeg`) or point PLURX_FFMPEG at a build — plurxd \
         requires it at runtime too"
    );
}

/// Run a command, or fail with what it said rather than with its exit code.
pub fn run(cmd: &mut Command) -> Vec<u8> {
    let out = cmd.output().expect("running ffmpeg");
    assert!(
        out.status.success(),
        "{:?} failed: {}",
        cmd,
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

pub fn fixture_dir() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.pop(); // repo root
    p.push("target");
    p.push("fixtures");
    p
}

/// The GOP shape each fixture exists to have. The classifier's whole job is to
/// tell these apart, so each is spelled out rather than left to the encoder's
/// defaults.
fn x265_params(kind: &str) -> &'static str {
    match kind {
        // Every keyframe a CRA with a RASL leading picture: the disc-remux
        // shape, and the source of the bug.
        "open-gop" => {
            "keyint=42:min-keyint=42:open-gop=1:bframes=4:b-pyramid=2:\
             scenecut=0:repeat-headers=1:log-level=none"
        }
        // Every keyframe an IDR: the control.
        "closed-gop" => {
            "keyint=42:min-keyint=42:open-gop=0:bframes=4:scenecut=0:\
             repeat-headers=1:log-level=none"
        }
        // Open GOP with no B-frames, so every CRA is clean — the case a lazy
        // classifier would call dirty, and the one that gives a session real
        // clean cut points to find.
        "clean-cra" => {
            "keyint=42:min-keyint=42:open-gop=1:bframes=0:scenecut=0:\
             repeat-headers=1:log-level=none"
        }
        other => panic!("no x265 params for fixture {other}"),
    }
}

/// One advisory lock shared by every test binary using `target/fixtures`.
///
/// A process-local mutex did not order `cargo test`'s separate target
/// processes. The first attempted repair used a hard link as a no-replace
/// publish, but removing the temporary link after the destination became
/// visible changed the shared inode's `ctime`. A reader could fence the source
/// in that interval and then reject it as changed. Holding this lock from the
/// absence check through the final rename makes the destination visible only
/// after its metadata has reached its final state.
fn fixture_publication_lock(dir: &Path) -> File {
    std::fs::create_dir_all(dir).expect("creating target/fixtures");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".publication.lock"))
        .expect("opening the fixture publication lock");
    lock.lock().expect("locking fixture publication");
    lock
}

/// Build and atomically publish one absent fixture while every other process
/// is excluded from the absence check.
fn publish_fixture_if_absent(destination: &Path, stem: &str, build: impl FnOnce(&Path)) -> bool {
    let dir = destination
        .parent()
        .expect("a fixture destination has a parent directory");
    let _publication = fixture_publication_lock(dir);
    if destination.exists() {
        return false;
    }
    let temporary = scratch_path(dir, stem);
    build(&temporary);
    std::fs::rename(&temporary, destination).unwrap_or_else(|error| {
        panic!("publishing {}: {error}", destination.display());
    });
    true
}

/// A scratch name no other process will also be writing.
///
/// The pid and counter keep a temporary left by a killed or panicked builder
/// from becoming the next run's output.
fn scratch_path(dir: &Path, stem: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    dir.join(format!("{stem}.{}.{n}.tmp", std::process::id()))
}

/// Path to a source fixture, generating it on first use.
///
/// Known kinds: `open-gop`, `closed-gop`, `clean-cra`, `h264`, `vp9`.
pub fn source(kind: &str) -> PathBuf {
    require_ffmpeg();
    let dir = fixture_dir();
    let path = dir.join(format!("{kind}.mkv"));
    let stem = format!("{kind}.mkv");
    publish_fixture_if_absent(&path, &stem, |temporary| {
        let mut cmd = Command::new(ffmpeg());
        cmd.args(["-y", "-v", "error"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=24:duration=12",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=12",
            ]);
        match kind {
            "h264" => {
                cmd.args(["-c:v", "libx264", "-preset", "ultrafast"])
                    .args([
                        "-x264-params",
                        "keyint=42:min-keyint=42:open_gop=1:bframes=3",
                    ])
                    .args(["-c:a", "aac"]);
            }
            // The only codec pair the headless Chromium in this container can
            // actually decode, so the browser end-to-end has to be built on it.
            // VP9 has no leading pictures, so it cannot reproduce the discard —
            // what it proves is the serving contract.
            "vp9" => {
                cmd.args(["-c:v", "libvpx-vp9", "-b:v", "1M", "-deadline", "realtime"])
                    .args(["-cpu-used", "8", "-g", "48", "-c:a", "libopus"]);
            }
            _ => {
                cmd.args(["-c:v", "libx265", "-preset", "ultrafast"])
                    .args(["-x265-params", x265_params(kind)])
                    .args(["-c:a", "aac"]);
            }
        }
        // `-f matroska` because the temp name has no extension ffmpeg knows;
        // rename exposes it only once the encode and metadata are complete.
        cmd.args(["-pix_fmt", "yuv420p", "-ac", "2", "-shortest"])
            .args(["-f", "matroska"])
            .arg(temporary);
        run(&mut cmd);
    });
    path
}

/// The production pipe command — the same arguments `copy_pipe_args` builds —
/// run against a fixture, cached beside the source.
pub fn pipe(kind: &str) -> Vec<u8> {
    let src = source(kind);
    let out = pipe_path(kind);
    let stem = out
        .file_name()
        .expect("the pipe cache path always names a file")
        .to_string_lossy()
        .into_owned();
    let mut generated = None;
    publish_fixture_if_absent(&out, &stem, |temporary| {
        let mut cmd = Command::new(ffmpeg());
        cmd.args(["-hide_banner", "-loglevel", "error"])
            .arg("-i")
            .arg(&src)
            .args(["-map", "0:v:0", "-map", "0:a:0?", "-sn", "-c:v", "copy"]);
        match kind {
            "h264" | "vp9" => {}
            _ => {
                cmd.args(["-tag:v", "hvc1"])
                    .args(["-bsf:v", "filter_units=remove_types=32-34"]);
            }
        }
        // VP9 in fMP4 needs Opus alongside it; everything else re-encodes to AAC
        // exactly as a copy session does when the client cannot take the source.
        if kind == "vp9" {
            cmd.args(["-c:a", "copy"]);
        } else {
            cmd.args(["-c:a", "aac", "-b:a", "256k"]);
        }
        cmd.args(["-avoid_negative_ts", "make_zero"])
            .args([
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            ])
            .args(["-use_editlist", "0"])
            .args(["-f", "mp4", "pipe:1"]);
        let bytes = run(&mut cmd);
        std::fs::write(temporary, &bytes).expect("caching the pipe output");
        generated = Some(bytes);
    });
    generated.unwrap_or_else(|| std::fs::read(&out).expect("reading the cached pipe output"))
}

/// Where [`pipe`] caches its output, for tests that hand the path to ffprobe.
pub fn pipe_path(kind: &str) -> PathBuf {
    // Keep the cache key tied to the authored timeline. Older fixture files
    // contain movie edit lists and therefore expose different first-frame
    // timestamps even though their decoded pictures are identical.
    fixture_dir().join(format!("{kind}.edit-free.pipe.mp4"))
}

/// The `open-gop` source with chapters attached.
///
/// Every disc remux has chapters and no `lavfi` fixture does, which is exactly
/// how a chapter track reached production unnoticed: ffmpeg's mp4 muxer turns
/// chapters into a QuickTime `text` track plus a `chpl` box, so the segmenter's
/// pipe carried a third track its predecessor never did, and Safari refused the
/// stream. Anything comparing the two muxers has to be asked on a source that
/// has them.
pub fn source_with_chapters() -> PathBuf {
    let src = source("open-gop");
    let dir = fixture_dir();
    let path = dir.join("chaptered.mkv");
    publish_fixture_if_absent(&path, "chaptered.mkv", |temporary| {
        let meta = dir.join("chapters.ffmeta");
        // Built line by line rather than as one continued literal: ffmetadata
        // rejects a section header with leading whitespace, and Rust's `\`
        // continuation keeps the source indentation inside the string.
        let mut chapters = String::from(";FFMETADATA1\n");
        for (i, (start, end)) in [(0, 4000), (4000, 8000), (8000, 12_000)]
            .into_iter()
            .enumerate()
        {
            chapters.push_str("[CHAPTER]\n");
            chapters.push_str("TIMEBASE=1/1000\n");
            chapters.push_str(&format!("START={start}\n"));
            chapters.push_str(&format!("END={end}\n"));
            chapters.push_str(&format!("title=Chapter {}\n", i + 1));
        }
        std::fs::write(&meta, chapters).expect("writing the chapter metadata");
        run(Command::new(ffmpeg())
            .args(["-y", "-v", "error", "-i"])
            .arg(&src)
            .arg("-i")
            .arg(&meta)
            .args(["-map_metadata", "1", "-map", "0", "-c", "copy"])
            .args(["-f", "matroska"])
            .arg(temporary));
    });
    path
}

/// One real Profile 7 RPU, captured 2026-08-30 from
/// `Nosferatu (2024) Remux-2160p.mkv` on nuc4 through
/// `-c:v copy -bsf:v hevc_mp4toannexb,filter_units=remove_types=63`.
///
/// A real one rather than a synthetic one because the question the conversion
/// answers is whether a library's idea of a Profile 7 RPU matches what a disc
/// remux actually carries.
///
/// It lives in a file rather than a literal because a hand-wrapped copy has
/// now lost bytes twice — once in `dvconvert`'s own tests and once again when
/// it was transcribed into a second module. Both times the length assertion
/// below caught it and the failure read as "invalid mapping_idc", which is a
/// long way from "you dropped three bytes".
const REAL_P7_RPU: &str = include_str!("../../../tests/playback/dv-p7-rpu.hex");

/// That RPU as a bare NAL unit — no start code, no length prefix.
pub fn p7_rpu() -> Vec<u8> {
    let hex = REAL_P7_RPU.trim();
    assert_eq!(
        hex.len(),
        734,
        "the fixture lost bytes on its way into the test"
    );
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
        .collect()
}

/// The same fMP4 stream with a real Profile 7 RPU appended to every video
/// sample.
///
/// No fixture encoder produces Dolby Vision — x265 in the CI image has no
/// `--dolby-vision-rpu` and the corpus is 640x360 lavfi — so a stream the
/// conversion can actually run on has to be built. Appending a real RPU to
/// each sample of a real pipe's output is the closest honest approximation:
/// the framing, the `trun` shapes and the sample sizes are ffmpeg's, and the
/// bytes the converter rewrites are a disc's.
///
/// It uses [`crate::fmp4::rewrite_video_samples`] to do the injecting, which
/// is also what the conversion uses. That is deliberate and it is not
/// circular: what these fixtures test is that the conversion is *wired* — that
/// it runs, that its refusals refuse — and every consumer reads the result
/// back through the ordinary [`crate::fmp4::FragmentReader`], which would not
/// parse a fragment this had corrupted. The rewrite's own correctness is
/// proved directly in `fmp4`'s tests, against bytes it did not build.
pub fn with_dolby_vision_rpus(stream: &[u8]) -> Vec<u8> {
    use crate::fmp4::{FragmentReader, Init, Unit};

    let rpu = p7_rpu();
    let mut reader = FragmentReader::new();
    reader.push(stream);
    let mut out: Vec<u8> = Vec::with_capacity(stream.len() * 2);
    let mut init: Option<Init> = None;
    loop {
        match reader.next_unit().expect("parsing the fixture stream") {
            Some(Unit::Init(parsed)) => {
                out.extend_from_slice(&parsed.bytes);
                init = Some(parsed);
            }
            Some(Unit::Fragment(mut fragment)) => {
                let init = init.as_ref().expect("an init before any fragment");
                let video = init.video().expect("a video track");
                let width = usize::from(video.nal_length_size);
                assert!(matches!(width, 1 | 2 | 4), "an hvcC length width");
                crate::fmp4::rewrite_video_samples(
                    &mut fragment,
                    &init.tracks,
                    video.id,
                    |sample| {
                        let mut next = Vec::with_capacity(sample.len() + width + rpu.len());
                        next.extend_from_slice(sample);
                        next.extend_from_slice(&(rpu.len() as u64).to_be_bytes()[8 - width..]);
                        next.extend_from_slice(&rpu);
                        Ok(next)
                    },
                )
                .expect("injecting the RPUs");
                out.extend_from_slice(&fragment.bytes);
            }
            // Handled after the loop: `Unit::Trailer` carries no bytes, and
            // the trailer has to survive because a generation's end is only
            // trustworthy when ffmpeg's `mfra` says so.
            Some(Unit::Trailer) => {}
            None => break,
        }
    }
    if let Some(at) = trailer_at(stream) {
        // Verbatim, stale offsets and all. `tfra` indexes `moof` positions
        // this rewrite has moved — and so does production: ffmpeg writes the
        // trailer and plurx rewrites the fragments behind it. Nothing reads
        // the entries (the trailer is recognised so it is never mistaken for
        // payload, and never published), so a fixture that "fixed" them would
        // be less like the stream a session actually carries, not more.
        out.extend_from_slice(&stream[at..]);
    }
    out
}

/// The same stream with the RPU in one fragment made unreadable.
///
/// The NAL header is left alone so the unit is still routed as an RPU and has
/// to be *refused*, not skipped. `at_fragment` is zero-based and counts
/// fragments, so a caller can put the damage past the landing window — which
/// is the case that matters: the landing matcher only reads the first few
/// fragments, so a refusal after it is the last thing standing between a
/// Profile 7 RPU and a client holding a Profile 8.1 playlist.
pub fn with_one_unconvertible_rpu(stream: &[u8], at_fragment: usize) -> Vec<u8> {
    use crate::fmp4::{FragmentReader, Init, Unit};

    let rpu = p7_rpu();
    let mut reader = FragmentReader::new();
    reader.push(stream);
    let mut out: Vec<u8> = Vec::with_capacity(stream.len());
    let mut init: Option<Init> = None;
    let mut index = 0usize;
    let mut broke = false;
    loop {
        match reader.next_unit().expect("parsing the fixture stream") {
            Some(Unit::Init(parsed)) => {
                out.extend_from_slice(&parsed.bytes);
                init = Some(parsed);
            }
            Some(Unit::Fragment(mut fragment)) => {
                if index == at_fragment {
                    let init = init.as_ref().expect("an init before any fragment");
                    let video = init.video().expect("a video track").id;
                    let track = fragment.track(video).expect("a video track").clone();
                    let mut end = None;
                    for run in &track.runs {
                        let mut at = run.data_offset;
                        for sample in &run.samples {
                            at += sample.size as usize;
                            end = Some(at);
                        }
                    }
                    let end = end.expect("a sample");
                    assert_eq!(
                        &fragment.bytes[end - rpu.len()..end],
                        &rpu[..],
                        "this stream was not built by `with_dolby_vision_rpus`"
                    );
                    fragment.bytes[end - rpu.len() + 2..end].fill(0xff);
                    broke = true;
                }
                out.extend_from_slice(&fragment.bytes);
                index += 1;
            }
            Some(Unit::Trailer) => {}
            None => break,
        }
    }
    assert!(broke, "the stream has no fragment {at_fragment}");
    if let Some(at) = trailer_at(stream) {
        out.extend_from_slice(&stream[at..]);
    }
    out
}

/// Where the `mfra` trailer starts, by a top-level box walk.
fn trailer_at(stream: &[u8]) -> Option<usize> {
    let mut at = 0usize;
    while at + 8 <= stream.len() {
        let size = u32::from_be_bytes(stream[at..at + 4].try_into().ok()?) as usize;
        assert!(
            size >= 8,
            "a 64-bit box size is not something these fixtures emit"
        );
        if &stream[at + 4..at + 8] == b"mfra" {
            return Some(at);
        }
        at = at.checked_add(size)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Publishing a fixture that is already there must leave a reader holding
    /// it completely undisturbed — including the parts of its metadata nobody
    /// thinks of as content.
    ///
    /// This is the whole of the defect that turned four `vodserve` resurrection
    /// tests red in CI on a tree that was green here. `SourceFence::unchanged`
    /// compares an object version built from device, inode, size, mtime *and
    /// ctime*, and `rename` moves the ctime of the inode it unlinks even though
    /// every other component is untouched and the open handle keeps reading the
    /// same bytes. So the assertion below is deliberately on ctime rather than
    /// on the bytes: the bytes never were the thing that broke.
    ///
    /// A second publisher enters through the same locked absence check as the
    /// first, so it cannot replace or relink the inode a reader already holds.
    #[test]
    fn republishing_a_fixture_leaves_a_held_reader_undisturbed() {
        use std::os::unix::fs::MetadataExt;

        let dir = fixture_dir().join(
            scratch_path(Path::new(""), "publish-race")
                .file_name()
                .expect("a name"),
        );
        std::fs::create_dir_all(&dir).expect("a directory of our own");
        let published = dir.join("fixture.mkv");
        std::fs::write(&published, b"the bytes a reader already holds").expect("first publish");

        // The reader: an open handle plus the object version recorded from it,
        // exactly as a fence records one before a producer starts.
        let held = std::fs::File::open(&published).expect("holding the fixture open");
        let before = held.metadata().expect("fstat");

        // The second publisher, arriving after the first one finished.
        let built = publish_fixture_if_absent(&published, "fixture.mkv", |temporary| {
            std::fs::write(temporary, b"bytes a second encode produced").expect("second encode");
        });
        assert!(
            !built,
            "an existing fixture wins without another publication"
        );

        let after = held.metadata().expect("fstat");
        assert_eq!(
            (before.dev(), before.ino(), before.size(), before.mtime()),
            (after.dev(), after.ino(), after.size(), after.mtime()),
            "these components survive a rename too, so they are not the proof"
        );
        assert_eq!(
            (before.ctime(), before.ctime_nsec()),
            (after.ctime(), after.ctime_nsec()),
            "publishing must not unlink the inode a reader is holding: ctime is \
             part of the object version a SourceFence compares, so moving it \
             fails a producer with `source changed` for a source that did not"
        );
        assert_eq!(
            std::fs::read(&published).expect("reading the published fixture"),
            b"the bytes a reader already holds",
            "the fixture that was already published is the one that stays"
        );
        std::fs::remove_dir_all(&dir).expect("cleaning up");
    }

    #[test]
    fn publication_lock_excludes_an_independent_file_handle() {
        let dir = fixture_dir().join(
            scratch_path(Path::new(""), "publication-lock")
                .file_name()
                .expect("a name"),
        );
        let first = fixture_publication_lock(&dir);
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join(".publication.lock"))
            .expect("opening the lock through another handle");
        assert!(
            second.try_lock().is_err(),
            "another process handle must not pass the publication boundary"
        );
        drop(first);
        second
            .lock()
            .expect("the next publisher proceeds after the owner exits");
        drop(second);
        std::fs::remove_dir_all(&dir).expect("cleaning up");
    }
}
