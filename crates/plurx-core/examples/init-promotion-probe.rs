//! Does the *promoted* init survive a reposition?
//!
//! M0-P0 clause (d) proved init identity across generations, including a
//! generation started with `-ss`. It hashes `Unit::Init` — ffmpeg's raw
//! `ftyp`+`moov`, and the probe says so in as many words: "the same bytes
//! `FragmentReader` publishes as `Unit::Init` ... not 'everything before the
//! first moof'".
//!
//! That is not the byte string served to a viewer. `copyseg` mutates the init
//! before writing it, at `copyseg.rs:339` and `:351`:
//!
//! ```text
//! promote_hevc_parameter_sets(&mut init, &fragment)
//! promote_hdr10_static_metadata(&mut init, &fragment)
//! ```
//!
//! Both read **the first fragment of that generation**, and a repositioned
//! generation's first fragment is a different fragment of the film. So plan
//! §2.2's "every later generation's init must be byte-identical" governs a
//! byte string nothing has measured, and the measurement everyone is relying
//! on was taken on a different one.
//!
//! This runs the production-shaped video copy pipe once, then promotes the
//! init from each IDR fragment in turn and compares. If every IDR yields the
//! same promoted init the rule is safe as written. One that differs makes
//! §2.2 a design question rather than a naming one.
//!
//! ```bash
//! cargo run -p plurx-core --example init-promotion-probe -- <file.mkv>...
//! ```

use std::process::{Command, Stdio};

use plurx_core::fmp4::{
    promote_hdr10_static_metadata, promote_hevc_parameter_sets, CutClass, FragmentReader, Init,
    Unit,
};

/// The video branch of the production copy pipe, as `copyseg` builds it.
fn pipe_to_fmp4(path: &str) -> Vec<u8> {
    let output = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            path,
            "-map",
            "0:v:0",
            "-c:v",
            "copy",
            "-bsf:v",
            "hevc_mp4toannexb,dump_extra",
            "-an",
            "-sn",
            "-f",
            "mp4",
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            "-avoid_negative_ts",
            "make_zero",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .expect("running ffmpeg");
    output.stdout
}

/// Same, without the annexb filters — some builds refuse the chain above on a
/// stream that is already length-prefixed.
fn pipe_plain(path: &str) -> Vec<u8> {
    let output = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            path,
            "-map",
            "0:v:0",
            "-c:v",
            "copy",
            "-an",
            "-sn",
            "-f",
            "mp4",
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            "-avoid_negative_ts",
            "make_zero",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .expect("running ffmpeg");
    output.stdout
}

fn digest(bytes: &[u8]) -> String {
    // FNV-1a. Not cryptographic and not required to be: this compares byte
    // strings this process produced moments apart.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: init-promotion-probe <file.mkv>...");
        std::process::exit(2);
    }

    let mut any_differed = false;

    for path in &files {
        let mut feed = pipe_to_fmp4(path);
        if feed.is_empty() {
            feed = pipe_plain(path);
        }
        if feed.is_empty() {
            println!("{path}: ffmpeg produced nothing — skipped");
            continue;
        }

        let mut reader = FragmentReader::new();
        reader.push(&feed);
        let mut base: Option<Init> = None;
        let mut fragments = Vec::new();
        while let Ok(Some(unit)) = reader.next_unit() {
            match unit {
                Unit::Init(init) => base = Some(init),
                Unit::Fragment(fragment) => fragments.push(fragment),
                Unit::Trailer => {}
            }
        }
        let Some(base) = base else {
            println!("{path}: no moov — skipped");
            continue;
        };

        // Every fragment a repositioned generation could legally start at: a
        // clean one, which is exactly what the plan cuts in front of.
        let starts: Vec<usize> = fragments
            .iter()
            .enumerate()
            .filter(|(_, fragment)| classify_clean(fragment, &base))
            .map(|(index, _)| index)
            .collect();

        if starts.len() < 2 {
            println!("{path}: only {} clean start(s) — skipped", starts.len());
            continue;
        }

        let mut seen: Vec<(usize, String, bool, bool)> = Vec::new();
        for index in &starts {
            let mut init = base.clone();
            let hevc = promote_hevc_parameter_sets(&mut init, &fragments[*index]).unwrap_or(false);
            let hdr = promote_hdr10_static_metadata(&mut init, &fragments[*index]).unwrap_or(false);
            seen.push((*index, digest(&init.bytes), hevc, hdr));
        }

        // What promotion WOULD copy, measured whether or not it fired.
        //
        // `promote_hevc_parameter_sets` only runs on a source whose `hvcC`
        // carries zero NAL arrays -- a WEB-DL shape a libx265 encode does not
        // produce, so a synthetic corpus can never exercise it directly. But
        // what it copies is the parameter set NALs from the starting
        // fragment's first sample, so whether those vary across legal starting
        // fragments decides whether the promoted init varies. That is
        // measurable here on any source that carries them in band.
        let nal_sets: Vec<(usize, Option<String>)> = starts
            .iter()
            .map(|index| {
                (
                    *index,
                    in_band_parameter_sets(&fragments[*index], &base).map(|nals| digest(&nals)),
                )
            })
            .collect();
        let carrying: Vec<&(usize, Option<String>)> =
            nal_sets.iter().filter(|entry| entry.1.is_some()).collect();
        let nal_verdict = if carrying.is_empty() {
            "no in-band parameter sets at any start -- promotion is a no-op \
             on this source and this file cannot answer the question"
                .to_owned()
        } else if carrying.len() != nal_sets.len() {
            any_differed = true;
            format!(
                "IN-BAND PARAMETER SETS PRESENT AT ONLY {} OF {} STARTS -- a \
                 generation starting at one of the others promotes nothing",
                carrying.len(),
                nal_sets.len()
            )
        } else {
            let first_nals = &carrying[0].1;
            let differing = carrying.iter().filter(|e| &e.1 != first_nals).count();
            if differing == 0 {
                format!(
                    "in-band parameter sets BYTE-IDENTICAL at all {} starts",
                    carrying.len()
                )
            } else {
                any_differed = true;
                format!(
                    "IN-BAND PARAMETER SETS DIFFER at {differing} of {} starts",
                    carrying.len()
                )
            }
        };

        let first = seen[0].1.clone();
        let differing: Vec<&(usize, String, bool, bool)> =
            seen.iter().filter(|entry| entry.1 != first).collect();

        let raw = digest(&base.bytes);
        let promoted_changed_anything = seen.iter().any(|entry| entry.2 || entry.3);

        println!("── {path}");
        println!(
            "   fragments {} · clean starts {} · raw init {raw}",
            fragments.len(),
            starts.len()
        );
        println!(
            "   promotion did something: {promoted_changed_anything} \
             (hevc {} / hdr10 {})",
            seen.iter().filter(|e| e.2).count(),
            seen.iter().filter(|e| e.3).count()
        );
        println!("   {nal_verdict}");
        if differing.is_empty() {
            println!(
                "   promoted init identical from all {} starts",
                starts.len()
            );
        } else {
            any_differed = true;
            println!(
                "   PROMOTED INIT DIFFERS: {} of {} starts disagree with fragment {}",
                differing.len(),
                starts.len(),
                starts[0]
            );
            for entry in differing.iter().take(5) {
                println!("      fragment {:>4} -> {}", entry.0, entry.1);
            }
        }
    }

    println!();
    if any_differed {
        println!(
            "VERDICT: at least one file's promoted init would depend on which \
             fragment the generation started at."
        );
        println!(
            "         Plan §2.2's byte-identity rule would refuse a \
             legitimate reposition on that file."
        );
    } else {
        println!(
            "VERDICT: the promoted init did not depend on the starting \
             fragment for any file measured."
        );
        println!(
            "         §2.2 is safe as written FOR THIS CORPUS. It is a \
             synthetic corpus; see the caveat in VOD-M2-QUESTIONS.md §2."
        );
    }
}

/// The VPS/SPS/PPS NAL units carried in a fragment's first video sample,
/// concatenated — exactly the bytes `promote_hevc_parameter_sets` copies into
/// `hvcC`. `None` when the sample carries none.
fn in_band_parameter_sets(fragment: &plurx_core::fmp4::Fragment, init: &Init) -> Option<Vec<u8>> {
    let video = init.video()?;
    let length_size = video.nal_length_size as usize;
    if length_size == 0 {
        return None;
    }
    let track = fragment.track(video.id)?;
    let run = track.runs.first()?;
    let sample = run.samples.first()?;
    let end = run.data_offset.checked_add(sample.size as usize)?;
    let bytes = fragment.bytes.get(run.data_offset..end)?;

    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + length_size <= bytes.len() {
        let mut len = 0usize;
        for offset in 0..length_size {
            len = (len << 8) | bytes[pos + offset] as usize;
        }
        pos += length_size;
        let Some(nal) = bytes.get(pos..pos + len) else {
            break;
        };
        pos += len;
        // HEVC NAL type is bits 1..6 of the first header byte.
        if let Some(first) = nal.first() {
            let kind = (first >> 1) & 0x3f;
            if (32..=34).contains(&kind) {
                out.extend_from_slice(nal);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

fn classify_clean(fragment: &plurx_core::fmp4::Fragment, init: &Init) -> bool {
    matches!(
        plurx_core::fmp4::classify(fragment, init),
        CutClass::CleanIdr | CutClass::CleanCra
    )
}
