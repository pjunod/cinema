//! Fixtures for the producer's tests: real sources muxed by `ffmpeg`,
//! with the authored PGS track the self-test also uses.

use super::*;
use plurx_core::testfixtures;

/// One subtitle track of a fixture, in stream order.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Sub {
    /// The authored PGS track: two display sets, sixteen segments.
    Pgs,
    /// The same track delayed past the end of the file, so the output
    /// `-t` cuts every packet: a real PGS track with no cues.
    EmptyPgs,
    /// A text track, which shifts every later subtitle ordinal.
    Srt,
}

pub(crate) struct Fixture {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) source: PathBuf,
    pub(crate) file: plurx_core::domain::MediaFile,
}

pub(crate) fn run_ffmpeg_to_completion(args: &[String]) {
    testfixtures::run(std::process::Command::new(testfixtures::ffmpeg()).args(args));
}

/// A sixteen-second 64x64 h264 Matroska file with `subs` in order,
/// catalogued as file `id` — distinct per test, because a ride-along
/// claims its file for the length of the pass.
pub(crate) fn fixture(id: i64, subs: &[Sub]) -> Fixture {
    testfixtures::require_ffmpeg();
    let dir = crate::test_tempdir().expect("fixture directory");
    let sup = dir.path().join("authored.sup");
    std::fs::write(&sup, SELF_TEST_PGS).expect("authored track");
    let srt = dir.path().join("text.srt");
    std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,000\nhello\n\n").expect("text track");
    let source = dir.path().join("source.mkv");
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=64x64:r=5:d=16",
        "-i",
    ]
    .map(str::to_owned)
    .to_vec();
    args.push(sup.to_string_lossy().into_owned());
    args.extend(["-itsoffset", "100", "-i"].map(str::to_owned));
    args.push(sup.to_string_lossy().into_owned());
    args.push("-i".to_owned());
    args.push(srt.to_string_lossy().into_owned());
    args.extend(["-map", "0:v"].map(str::to_owned));
    for sub in subs {
        args.push("-map".to_owned());
        args.push(
            match sub {
                Sub::Pgs => "1:s",
                Sub::EmptyPgs => "2:s",
                Sub::Srt => "3:s",
            }
            .to_owned(),
        );
    }
    args.extend(
        [
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "5",
            "-c:s",
            "copy",
            "-t",
            "16",
            "-f",
            "matroska",
        ]
        .map(str::to_owned),
    );
    args.push(source.to_string_lossy().into_owned());
    run_ffmpeg_to_completion(&args);
    let mut file = synthetic_media_file(&source, 16_000).expect("fixture media file");
    file.id = id;
    Fixture { dir, source, file }
}

/// Corrupt the first segment of `track`'s first block (Matroska track
/// numbers: video is 1, the first subtitle 2).
pub(crate) fn corrupt(fixture: &mut Fixture, track: u8) {
    let mut bytes = std::fs::read(&fixture.source).expect("fixture bytes");
    corrupt_first_segment_length(&mut bytes, track).expect("a block to corrupt");
    std::fs::write(&fixture.source, bytes).expect("corrupted fixture");
    let id = fixture.file.id;
    fixture.file = synthetic_media_file(&fixture.source, 16_000).expect("fixture media file");
    fixture.file.id = id;
}
