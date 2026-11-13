//! Disk F+ authoring from the title scanner's exact source packet clock.
//! The sidecar binds each decode-order RPU to its source PTS and original
//! bytes; a raw HEVC stream alone has no presentation clock.
use dolby_vision::rpu::{dovi_rpu::DoviRpu, ConversionMode};
use plurx_core::{
    store::dv_refit_jobs::{RefitCurveScene, RefitCurveTable, RefitVerdict},
    transcode::{dv_refit::apply_to_rpu, dvconvert::MAX_RPU_NAL_BYTES},
};
use sha2::{Digest, Sha256};
use std::{fs, io::{BufWriter, Write}, path::Path};

const MAX_RPU_STREAM_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TIMELINE_BYTES: u64 = 96 * 1024 * 1024;
const MAX_FRAMES: usize = 1_000_000;

type Hash = [u8; 32];

fn bounded_read(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path).map_err(|e| format!("RPU metadata: {e}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > max {
        return Err("RPU metadata exceeds its bounded file size".into());
    }
    fs::read(path).map_err(|e| format!("reading RPU metadata: {e}"))
}

/// dovi_tool's RPU.bin is Annex B UNSPEC62 NALs, one per source picture.
fn units(bytes: &[u8], mut visit: impl FnMut(&[u8], usize) -> Result<(), String>) -> Result<usize, String> {
    let starts: Vec<usize> = bytes.windows(3).enumerate()
        .filter_map(|(i, w)| (w == [0, 0, 1]).then_some(i))
        .collect();
    if starts.is_empty() || starts[0] > 1 || starts.len() > MAX_FRAMES {
        return Err("RPU stream has no bounded Annex B units".into());
    }
    for (ordinal, &start) in starts.iter().enumerate() {
        let end = starts.get(ordinal + 1).copied().unwrap_or(bytes.len());
        // The fourth zero of a four-byte start code belongs to the next
        // delimiter, never to the preceding RPU.
        let end = if end < bytes.len() && end > start + 3 && bytes[end - 1] == 0 {
            end - 1
        } else { end };
        let nal = &bytes[start + 3..end];
        if nal.len() < 3 || nal.len() > MAX_RPU_NAL_BYTES || nal[..2] != [0x7c, 0x01] {
            return Err(format!("invalid RPU NAL at decode ordinal {ordinal}"));
        }
        visit(nal, ordinal)?;
    }
    Ok(starts.len())
}

fn timeline(path: &Path, table: &RefitCurveTable) -> Result<Vec<(i64, Hash)>, String> {
    let bytes = bounded_read(path, MAX_TIMELINE_BYTES)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "non-UTF8 RPU timeline")?;
    let mut lines = text.lines();
    let expected_base = table.scenes.first().ok_or("fitted title has no scenes")?.time_base.as_str();
    if lines.next() != Some(format!("# {expected_base}").as_str()) {
        return Err("RPU timeline time base differs from stored scenes".into());
    }
    let mut rows = Vec::new();
    for line in lines {
        let (pts, hash) = line.split_once(' ').ok_or("malformed RPU timeline")?;
        let pts = pts.parse::<i64>().map_err(|_| "invalid RPU timeline PTS")?;
        let hash = hex::decode(hash).map_err(|_| "invalid RPU timeline hash")?;
        let hash: Hash = hash.try_into().map_err(|_| "invalid RPU timeline hash size")?;
        if rows.len() == MAX_FRAMES { return Err("RPU timeline frame bound exceeded".into()); }
        rows.push((pts, hash));
    }
    if rows.len() != usize::try_from(table.title.frames).unwrap_or(usize::MAX) {
        return Err("RPU timeline frame count differs from fitted title".into());
    }
    Ok(rows)
}

fn scene_for_pts(scenes: &[RefitCurveScene], pts: i64) -> Result<&RefitCurveScene, String> {
    let index = scenes.partition_point(|scene| scene.first_pts <= pts);
    let scene = index.checked_sub(1).and_then(|i| scenes.get(i))
        .ok_or("RPU PTS precedes fitted scene coverage")?;
    if index == scenes.len() && pts >= scene.end_pts {
        return Err("RPU PTS exceeds fitted scene coverage".into());
    }
    Ok(scene)
}

pub(super) fn author(input: &Path, timeline_path: &Path, output: &Path, table: &RefitCurveTable) -> Result<Vec<Hash>, String> {
    if table.title.verdict != RefitVerdict::Fitted || table.scenes.is_empty() {
        return Err("disk F+ author requires a fitted title".into());
    }
    let rows = timeline(timeline_path, table)?;
    let bytes = bounded_read(input, MAX_RPU_STREAM_BYTES)?;
    let file = fs::OpenOptions::new().write(true).create_new(true).open(output)
        .map_err(|e| format!("creating fitted RPU file: {e}"))?;
    let mut writer = BufWriter::new(file);
    let mut expected = Vec::with_capacity(rows.len());
    let count = units(&bytes, |nal, ordinal| {
        let (pts, source_hash) = rows.get(ordinal).ok_or("more extracted RPUs than source pictures")?;
        let mut actual: Hash = [0; 32];
        actual.copy_from_slice(&Sha256::digest(nal));
        if &actual != source_hash {
            return Err(format!("source RPU hash differs at decode ordinal {ordinal}"));
        }
        let scene = scene_for_pts(&table.scenes, *pts)?;
        let mut rpu = DoviRpu::parse_unspec62_nalu(nal)
            .map_err(|e| format!("source RPU {ordinal}: {e}"))?;
        if rpu.dovi_profile != 7 {
            return Err(format!("source RPU {ordinal} is not Profile 7"));
        }
        rpu.convert_with_mode(ConversionMode::To81)
            .map_err(|e| format!("converting RPU {ordinal}: {e}"))?;
        if let Some(curves) = &scene.curves {
            apply_to_rpu(&mut rpu, curves)
                .map_err(|e| format!("fitting RPU {ordinal}: {e}"))?;
        }
        let written = rpu.write_hevc_unspec62_nalu()
            .map_err(|e| format!("writing RPU {ordinal}: {e}"))?;
        writer.write_all(&[0, 0, 0, 1]).map_err(|e| e.to_string())?;
        writer.write_all(&written).map_err(|e| e.to_string())?;
        let mut hash: Hash = [0; 32];
        hash.copy_from_slice(&Sha256::digest(&written));
        expected.push(hash);
        Ok(())
    })?;
    if count != rows.len() { return Err("extracted RPU count differs from source pictures".into()); }
    writer.flush().map_err(|e| format!("flushing fitted RPUs: {e}"))?;
    writer.into_inner().map_err(|e| format!("closing fitted RPUs: {e}"))?
        .sync_all().map_err(|e| format!("syncing fitted RPUs: {e}"))?;
    Ok(expected)
}

pub(super) fn verify_readback(path: &Path, expected: &[Hash]) -> Result<(), String> {
    let bytes = bounded_read(path, MAX_RPU_STREAM_BYTES)?;
    let count = units(&bytes, |nal, ordinal| {
        let want = expected.get(ordinal).ok_or("replacement has extra RPUs")?;
        DoviRpu::parse_unspec62_nalu(nal)
            .map_err(|e| format!("replacement RPU {ordinal} did not parse: {e}"))?;
        let mut actual: Hash = [0; 32];
        actual.copy_from_slice(&Sha256::digest(nal));
        if &actual != want {
            return Err(format!("replacement RPU {ordinal} differs from fitted curves"));
        }
        Ok(())
    })?;
    if count != expected.len() { return Err("replacement dropped fitted RPUs".into()); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reordered_decode_packets_select_scenes_by_presentation_time() {
        let scene = |first, end| RefitCurveScene {
            first_frame: 0, frames: 2, first_pts: first, end_pts: end,
            time_base: "1/1000".into(), curves: None,
        };
        let scenes = [scene(100, 200), scene(200, 300)];
        let decode_order_pts = [100, 240, 180, 200];
        let starts: Vec<i64> = decode_order_pts.into_iter()
            .map(|pts| scene_for_pts(&scenes, pts).unwrap().first_pts)
            .collect();
        assert_eq!(starts, [100, 200, 100, 200]);
        assert!(scene_for_pts(&scenes, 99).is_err());
        assert!(scene_for_pts(&scenes, 300).is_err());
    }

    #[test]
    fn annex_b_units_never_guess_a_missing_frame() {
        let valid = [0,0,0,1,0x7c,1,0x55,0,0,0,1,0x7c,1,0x66];
        let mut bodies = Vec::new();
        assert_eq!(units(&valid, |nal, _| { bodies.push(nal.to_vec()); Ok(()) }), Ok(2));
        assert_eq!(bodies, [vec![0x7c,1,0x55], vec![0x7c,1,0x66]]);
        assert!(units(&valid[..6], |_,_| Ok(())).is_err());
    }
}
