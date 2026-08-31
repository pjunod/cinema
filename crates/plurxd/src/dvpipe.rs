//! The Profile 7 → 8.1 producer: two ffmpegs with plurx's own stage between
//! them.
//!
//! An ordinary copy session is one ffmpeg reading the source and writing a
//! fragmented MP4 down a pipe. A converting session is the same thing with the
//! middle opened up:
//!
//! ```text
//!  stage one  ──▶  convert  ──▶  stage two  ──▶  the fragment reader
//!  reads the       rewrites      remuxes to      (unchanged: it is the
//!  file, Annex     every RPU     fMP4, audio     same fMP4 stream a
//!  B out           to 8.1        from the file   plain copy produces)
//! ```
//!
//! Everything downstream of `stage two` is untouched — `vodgen`, the
//! segmenter, the init identity — because the bytes it reads are the same
//! *kind* of bytes. What changes is only which RPUs are inside them.
//!
//! **The slot holds stage one, not stage two.** `ProducerSlot` signals its
//! child to suspend and resume the producer (the ahead-window guard), and the
//! process worth signalling is the one reading the disk. Suspending stage one
//! back-pressures the rest through two pipes for free, whereas suspending
//! stage two would leave stage one filling a kernel buffer and then blocking
//! anyway — the same result, one process later, with the pacing flags applied
//! to something that is not the disk.
//!
//! Teardown follows the same chain and needs no bookkeeping: killing stage one
//! closes its stdout, the conversion sees EOF and closes stage two's stdin,
//! and stage two exits. Nothing is left behind for a reaper to find.

use std::process::Stdio;

use plurx_core::transcode::dvconvert::{self, Converted, DvConvertError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdout, Command};

/// How much of stage one's output to hold before converting a batch.
///
/// The conversion is per-NAL and the carried remainder is bounded by one NAL
/// unit ([`dvconvert::whole_units_prefix`]), so this number does not bound
/// correctness — it trades syscalls against latency. 256 KiB is a few frames
/// of a 4K remux: small enough that the first segment is not waiting on it,
/// large enough that a two-hour film is not several hundred thousand writes.
const READ_CHUNK: usize = 256 * 1024;

/// A spawned converting producer.
pub struct ConvertingProducer {
    /// Stage one — the child the slot attaches and signals.
    pub child: Child,
    /// Stage two's stdout: the fragmented MP4 the reader consumes.
    pub stdout: ChildStdout,
}

/// What the conversion observed, reported once the stream ends.
///
/// Carried out of the bridging task rather than returned, because the task
/// outlives the spawn: the caller has a producer to attach long before the
/// last RPU is rewritten. The session's reasons read it when it settles.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The stream converted. `rpus: 0` is included here and is *not* an error
    /// at this layer — see [`dvconvert::convert_annex_b`]; whether a source
    /// believed to be Profile 7 carrying no RPUs is a refusal is a question
    /// for the caller, which has the file's stored facts.
    Converted(Converted),
    /// An RPU could not be read, converted, or was a profile this stage
    /// refuses. The stream stops here rather than continuing unconverted: a
    /// mixed stream — some RPUs saying 7, the rest saying 8.1 — is worse than
    /// either answer and no decoder is required to survive it.
    Refused(String),
    /// The pipe broke before the stream ended: stage two exited, the session
    /// was torn down, or a write failed.
    Interrupted(String),
}

/// Spawn the two stages and the conversion between them.
///
/// `configure` is the caller's hook for what this module has no business
/// knowing: on Unix, the `pre_exec` that hands a stage the attested file
/// descriptor. It is called for **both** stages, and both need it — stage one
/// reads the picture from the source and stage two reads the audio from it,
/// and a stage two that opened the file by name could pair one file's audio
/// with another file's video if the path were replaced between the two spawns.
/// The two stages seek that descriptor independently, which is the whole
/// reason the audio is a second open rather than a second map.
///
/// `on_outcome` is called exactly once, when the conversion stops for any
/// reason.
pub fn spawn<C, F>(
    ffmpeg: &str,
    source_args: &[String],
    output_args: &[String],
    configure: C,
    on_outcome: F,
) -> Result<ConvertingProducer, String>
where
    C: Fn(&mut Command),
    F: FnOnce(Outcome) + Send + 'static,
{
    let mut one = Command::new(ffmpeg);
    one.args(source_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    configure(&mut one);
    let mut one = one
        .spawn()
        .map_err(|error| format!("spawning the source stage: {error}"))?;
    let Some(annex_b) = one.stdout.take() else {
        return Err("the source stage started without a stdout".to_owned());
    };

    let mut two = Command::new(ffmpeg);
    two.args(output_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    configure(&mut two);
    let mut two = two
        .spawn()
        .map_err(|error| format!("spawning the output stage: {error}"))?;
    let (Some(sink), Some(stdout)) = (two.stdin.take(), two.stdout.take()) else {
        return Err("the output stage started without a stdin or a stdout".to_owned());
    };

    // The bridging task owns stage two's handle, so the child is reaped when
    // the conversion ends however it ends. It is detached deliberately: the
    // caller has a producer to attach and a viewer waiting, and this task's
    // lifetime is the stream's, not the spawn's.
    tokio::spawn(async move {
        let outcome = bridge(annex_b, sink).await;
        // Stage two is finished with its input either way. Waiting rather
        // than killing lets a clean end flush the last fragment; a broken one
        // has already exited.
        if matches!(outcome, Outcome::Converted(_)) {
            let _ = two.wait().await;
        } else {
            let _ = two.kill().await;
        }
        on_outcome(outcome);
    });

    Ok(ConvertingProducer { child: one, stdout })
}

/// Read Annex B from stage one, convert, write to stage two, until one end
/// stops.
async fn bridge<R, W>(mut annex_b: R, mut sink: W) -> Outcome
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    // `carried` holds the tail of the last read: the NAL unit that was still
    // arriving. It is bounded by one unit, never by the stream — a two-hour 4K
    // remux is tens of gigabytes and buffering it is not an option that
    // exists.
    let mut carried: Vec<u8> = Vec::with_capacity(READ_CHUNK * 2);
    let mut chunk = vec![0u8; READ_CHUNK];
    let mut converted: Vec<u8> = Vec::with_capacity(READ_CHUNK * 2);
    let mut report = Converted::default();

    loop {
        let read = match annex_b.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                return Outcome::Interrupted(format!("reading the source stage: {error}"))
            }
        };
        carried.extend_from_slice(&chunk[..read]);

        let cut = dvconvert::whole_units_prefix(&carried);
        if cut == 0 {
            // Not one whole unit yet. This is normal at the very start of a
            // stream and after a short read; it cannot loop forever, because
            // any unit that never completes ends with the stream and is
            // converted by the flush below.
            continue;
        }
        match dvconvert::convert_annex_b(&carried[..cut], &mut converted) {
            Ok(batch) => absorb(&mut report, batch),
            Err(error) => return refusal(error),
        }
        if let Err(error) = sink.write_all(&converted).await {
            return Outcome::Interrupted(format!("writing to the output stage: {error}"));
        }
        carried.drain(..cut);
    }

    // End of stream: whatever is left is a whole unit, because there is no
    // more of it coming.
    if !carried.is_empty() {
        match dvconvert::convert_annex_b(&carried, &mut converted) {
            Ok(batch) => absorb(&mut report, batch),
            Err(error) => return refusal(error),
        }
        if let Err(error) = sink.write_all(&converted).await {
            return Outcome::Interrupted(format!("writing to the output stage: {error}"));
        }
    }
    // Closing the sink is what tells stage two the stream is over; without it
    // the muxer waits for more input and never writes its last fragment.
    if let Err(error) = sink.shutdown().await {
        return Outcome::Interrupted(format!("closing the output stage's input: {error}"));
    }
    Outcome::Converted(report)
}

/// Fold one batch's report into the stream's.
///
/// The first batch that saw an RPU decides the source facts, for the reason
/// [`dvconvert::convert_annex_b`] gives: the badge names what the title *was*,
/// and a stream spliced from two sources must not have that answer depend on
/// where the reads happened to fall.
fn absorb(report: &mut Converted, batch: Converted) {
    report.rpus += batch.rpus;
    if report.source_profile.is_none() {
        report.source_profile = batch.source_profile;
        report.enhancement_layer = batch.enhancement_layer;
    }
}

fn refusal(error: DvConvertError) -> Outcome {
    Outcome::Refused(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::transcode::dvconvert::EnhancementLayer;

    /// The same real Profile 7 RPU `plurx_core::transcode::dvconvert` is
    /// tested against, from the same file, so the two cannot drift — a
    /// transcribed copy of it has already lost bytes once.
    const REAL_P7_RPU: &str = include_str!("../../../tests/playback/dv-p7-rpu.hex");

    fn rpu_bytes() -> Vec<u8> {
        let hex = REAL_P7_RPU.trim();
        assert_eq!(hex.len(), 734, "the fixture lost bytes in the file");
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
            .collect()
    }

    /// A stream of `n` frames: a slice, then an RPU, repeated.
    fn stream(frames: usize) -> Vec<u8> {
        let rpu = rpu_bytes();
        let mut out = Vec::new();
        for n in 0..frames {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&[0x26, 0x01, 0xaf, n as u8]);
            out.extend_from_slice(&[0, 0, 1]);
            out.extend_from_slice(&rpu);
        }
        out
    }

    /// The bridge converts a whole stream regardless of where the reads fall.
    ///
    /// This is the property the whole design rests on: a pipe hands over
    /// whatever has arrived, so an RPU is split across two reads routinely,
    /// and a stage that mishandled that would refuse a good RPU as unreadable
    /// or emit a truncated one. Reading one byte at a time is the worst case
    /// and the cheapest way to prove it.
    #[tokio::test]
    async fn a_stream_converts_the_same_however_the_reads_are_chopped() {
        let source = stream(6);

        let mut whole = Vec::new();
        let report = bridge(&source[..], &mut whole).await;
        let Outcome::Converted(whole_report) = report else {
            panic!("{report:?}");
        };
        assert_eq!(whole_report.rpus, 6);
        assert_eq!(whole_report.source_profile, Some(7));
        assert_eq!(whole_report.enhancement_layer, EnhancementLayer::Full);

        // The same stream through a reader that yields one byte per call.
        let mut byte_at_a_time = Vec::new();
        let report = bridge(OneByteAtATime(&source[..], 0), &mut byte_at_a_time).await;
        let Outcome::Converted(chopped) = report else {
            panic!("{report:?}");
        };
        assert_eq!(chopped.rpus, 6, "no RPU was lost at a read boundary");
        assert_eq!(chopped.source_profile, Some(7));
        assert_eq!(
            byte_at_a_time, whole,
            "the bytes out must not depend on the bytes in per read"
        );

        // …and the output really is converted, not passed through.
        assert_ne!(byte_at_a_time, source);
    }

    /// A stream with no RPUs is not an error here.
    ///
    /// Deciding that a source believed to be Profile 7 carrying none is a
    /// refusal needs the file's stored facts, which this layer does not have.
    #[tokio::test]
    async fn a_stream_with_no_rpus_reports_zero_rather_than_failing() {
        let source = [0, 0, 0, 1, 0x26, 0x01, 0xaf, 0x00].to_vec();
        let mut out = Vec::new();
        let outcome = bridge(&source[..], &mut out).await;
        let Outcome::Converted(report) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(report.rpus, 0);
        assert_eq!(out, source, "not one byte moves");
    }

    /// An RPU the conversion refuses stops the stream and names why.
    ///
    /// Continuing would ship a mixed stream — the first frames saying 8.1 and
    /// the rest saying 7 — which is worse than either answer.
    #[tokio::test]
    async fn a_refused_rpu_stops_the_stream_and_carries_its_reason() {
        let mut broken = rpu_bytes();
        for byte in broken.iter_mut().skip(2) {
            *byte = 0xff;
        }
        let mut source = stream(2);
        source.extend_from_slice(&[0, 0, 1]);
        source.extend_from_slice(&broken);
        // A trailing unit, so the refusal is not merely the end of the stream.
        source.extend_from_slice(&[0, 0, 0, 1, 0x26, 0x01, 0xaf, 0x09]);

        let mut out = Vec::new();
        let outcome = bridge(&source[..], &mut out).await;
        let Outcome::Refused(reason) = outcome else {
            panic!("{outcome:?}");
        };
        assert!(reason.contains("could not be read"), "{reason}");
        assert!(reason.contains("byte"), "the reason names where: {reason}");
    }

    /// A reader that hands over one byte per call — the worst case a pipe can
    /// produce, and the one a naive implementation passes tests without ever
    /// meeting.
    struct OneByteAtATime<'a>(&'a [u8], usize);

    impl tokio::io::AsyncRead for OneByteAtATime<'_> {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if self.1 >= self.0.len() {
                return std::task::Poll::Ready(Ok(()));
            }
            let byte = self.0[self.1];
            self.1 += 1;
            buf.put_slice(&[byte]);
            std::task::Poll::Ready(Ok(()))
        }
    }
}
