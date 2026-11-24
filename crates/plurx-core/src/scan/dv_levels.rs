//! Which Dolby Vision display-metadata levels a title carries anywhere, and
//! whether it is a CM v4.0 grade — the "In the file" row of Playback info.
//!
//! A reporter, nothing more. It is recorded by its own background pass into
//! its own probe-document key, never into [`super::dv_title::DvTitleStats`]:
//! that document is hashed into the display-target identity and owned by the
//! title scanner, and a fact that only describes the title must not be able
//! to move either. No playback decision reads this record.
//!
//! The key is an application graft (`plurx_` root key), so an ordinary probe
//! republish replaces the document without it, and the size/mtime fence on
//! the graft refuses a write for a source that changed under the pass.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const PROBE_FIELD: &str = "plurx_dv_levels_v1";
pub const SCHEMA: u32 = 1;
/// The largest RPU NAL the pass parses; the processing contract bounds an
/// RPU to the same size.
const MAX_RPU_NAL: usize = 4098;

/// The stored record. `Recorded` lists every level seen in at least one
/// parsed RPU; `Unreadable` stops the pass retrying a source it cannot read
/// and still renders as "Unavailable".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum DvLevelsRecord {
    Recorded {
        schema: u32,
        /// Extension-metadata level numbers, ascending.
        levels: Vec<u8>,
        /// A CM v4.0 grade: some RPU carries CM v4.0 display metadata (L254).
        cm_v40: bool,
        /// RPUs parsed; a recorded title always has at least one.
        rpus: u64,
        /// RPUs the parser refused. They add no level.
        unparsed: u64,
    },
    Unreadable {
        schema: u32,
        /// Bounded, for an operator; never shown to a viewer.
        reason: String,
    },
}

/// The title's levels as the reporter uses them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DvTitleLevels {
    pub levels: BTreeSet<u8>,
    pub cm_v40: bool,
}

impl DvLevelsRecord {
    pub fn unreadable(reason: impl Into<String>) -> Self {
        let mut reason: String = reason.into();
        reason.retain(|c| !c.is_control());
        if reason.len() > 256 {
            let mut end = 256;
            while !reason.is_char_boundary(end) {
                end -= 1;
            }
            reason.truncate(end);
        }
        Self::Unreadable {
            schema: SCHEMA,
            reason,
        }
    }

    /// Whether the record is one the pass could have written. A record that
    /// is not is treated as absent, never as "no levels".
    fn well_formed(&self) -> bool {
        match self {
            Self::Recorded {
                schema,
                levels,
                rpus,
                unparsed,
                ..
            } => {
                *schema == SCHEMA
                    && *rpus > 0
                    && unparsed <= rpus
                    && !levels.is_empty()
                    && levels.windows(2).all(|pair| pair[0] < pair[1])
            }
            Self::Unreadable { schema, reason } => *schema == SCHEMA && reason.len() <= 256,
        }
    }

    /// The record in a probe document; `None` when absent, malformed or from
    /// another schema.
    pub fn from_probe_json(document: &str) -> Option<Self> {
        let document: serde_json::Value = serde_json::from_str(document).ok()?;
        Self::from_probe_value(&document)
    }

    pub fn from_probe_value(document: &serde_json::Value) -> Option<Self> {
        let record: Self = serde_json::from_value(document.get(PROBE_FIELD)?.clone()).ok()?;
        record.well_formed().then_some(record)
    }

    /// The levels a recorded title carries; `None` for an unreadable one.
    pub fn title_levels(&self) -> Option<DvTitleLevels> {
        match self {
            Self::Recorded { levels, cm_v40, .. } => Some(DvTitleLevels {
                levels: levels.iter().copied().collect(),
                cm_v40: *cm_v40,
            }),
            Self::Unreadable { .. } => None,
        }
    }
}

/// Folds every RPU of a title into the level set.
#[derive(Debug, Default)]
pub struct DvLevelsAccumulator {
    levels: BTreeSet<u8>,
    cm_v40: bool,
    rpus: u64,
    unparsed: u64,
}

impl DvLevelsAccumulator {
    /// One HEVC NAL unit, header included. Anything other than an RPU
    /// (UNSPEC62) is ignored.
    pub fn observe_nal(&mut self, nal: &[u8]) {
        if nal.len() < 3 || (nal[0] >> 1) & 0x3f != 62 {
            return;
        }
        if nal.len() > MAX_RPU_NAL {
            self.rpus += 1;
            self.unparsed += 1;
            return;
        }
        self.rpus += 1;
        match dolby_vision::rpu::dovi_rpu::DoviRpu::parse_unspec62_nalu(nal) {
            Ok(rpu) => {
                if let Some(dm) = rpu.vdr_dm_data.as_ref() {
                    use dolby_vision::rpu::extension_metadata::DmData;
                    use dolby_vision::rpu::extension_metadata::WithExtMetadataBlocks;
                    for data in [dm.cmv29_metadata.as_ref(), dm.cmv40_metadata.as_ref()]
                        .into_iter()
                        .flatten()
                    {
                        let blocks = match data {
                            DmData::V29(meta) => meta.blocks_ref(),
                            DmData::V40(meta) => {
                                self.cm_v40 = true;
                                meta.blocks_ref()
                            }
                        };
                        self.levels.extend(blocks.iter().map(|block| block.level()));
                    }
                }
            }
            Err(_) => self.unparsed += 1,
        }
    }

    /// The record for a source read to its end. A title with no parsed RPU
    /// carrying display metadata has nothing to report, and says why.
    pub fn finish(self) -> DvLevelsRecord {
        if self.levels.is_empty() {
            return DvLevelsRecord::unreadable(if self.rpus == 0 {
                "no Dolby Vision RPU in the selected video stream"
            } else {
                "no parsable Dolby Vision display metadata in any RPU"
            });
        }
        DvLevelsRecord::Recorded {
            schema: SCHEMA,
            levels: self.levels.into_iter().collect(),
            cm_v40: self.cm_v40,
            rpus: self.rpus,
            unparsed: self.unparsed,
        }
    }
}

/// Splits an Annex B byte stream into NAL units as it arrives, holding at
/// most one partial NAL between reads.
#[derive(Debug, Default)]
pub struct AnnexBSplitter {
    pending: Vec<u8>,
    /// The pending buffer holds a NAL whose start code has been seen.
    started: bool,
}

impl AnnexBSplitter {
    /// Feeds bytes; calls `nal` for every NAL unit completed by them. A NAL
    /// longer than `max_held` is dropped as it streams (an RPU never is one),
    /// so a huge slice cannot grow the buffer without bound.
    pub fn push(&mut self, mut bytes: &[u8], max_held: usize, nal: &mut impl FnMut(&[u8])) {
        while !bytes.is_empty() {
            let base = self.pending.len().saturating_sub(3);
            self.pending.extend_from_slice(bytes);
            bytes = &[];
            let mut search = base;
            loop {
                let Some(offset) = find_start_code(&self.pending[search..]) else {
                    break;
                };
                let code_at = search + offset;
                if self.started {
                    emit(&self.pending[..code_at], nal);
                }
                self.pending.drain(..code_at + 3);
                self.started = true;
                search = 0;
            }
            if !self.started {
                // Garbage before the first start code: keep only a possible
                // partial start code.
                let keep = self.pending.len().min(2);
                self.pending.drain(..self.pending.len() - keep);
            } else if self.pending.len() > max_held {
                // Not an RPU; keep the tail that may begin the next start code.
                let keep = 2;
                self.pending.drain(..self.pending.len() - keep);
                self.started = false;
            }
        }
    }

    /// The final NAL at end of stream.
    pub fn finish(self, nal: &mut impl FnMut(&[u8])) {
        if self.started {
            emit(&self.pending, nal);
        }
    }
}

fn find_start_code(bytes: &[u8]) -> Option<usize> {
    bytes.windows(3).position(|window| window == [0, 0, 1])
}

/// A four-byte start code leaves a trailing zero on the previous NAL, and
/// trailing zero bytes are never part of a NAL's payload.
fn emit(bytes: &[u8], nal: &mut impl FnMut(&[u8])) {
    let end = bytes
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |i| i + 1);
    if end > 0 {
        nal(&bytes[..end]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dolby_vision::rpu::dovi_rpu::DoviRpu;
    use dolby_vision::rpu::extension_metadata::blocks::{
        ExtMetadataBlock, ExtMetadataBlockLevel2, ExtMetadataBlockLevel8,
    };
    use dolby_vision::rpu::extension_metadata::{CmV40DmData, DmData};

    const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/dv-runtime/p8-piecewise.nal");

    fn rpu(cm4: bool, extra: Vec<ExtMetadataBlock>) -> Vec<u8> {
        let mut rpu = DoviRpu::parse_unspec62_nalu(FIXTURE).expect("fixture");
        let dm = rpu.vdr_dm_data.as_mut().expect("display metadata");
        dm.remove_metadata_level(2);
        dm.remove_metadata_level(8);
        dm.remove_metadata_level(10);
        dm.cmv40_metadata = cm4.then(|| DmData::V40(CmV40DmData::new_with_l254_402()));
        for block in extra {
            dm.add_metadata_block(block).expect("block");
        }
        rpu.modified = true;
        rpu.write_hevc_unspec62_nalu().expect("RPU")
    }

    fn levels_of(nal: &[u8]) -> BTreeSet<u8> {
        let rpu = DoviRpu::parse_unspec62_nalu(nal).expect("parses");
        let dm = rpu.vdr_dm_data.expect("dm");
        (0..=255u8)
            .filter(|level| dm.get_block(*level).is_some())
            .collect()
    }

    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = vec![0, 0, 0, 1, 0x40, 0x01, 0xaa]; // a VPS-shaped NAL first
        for nal in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
        out
    }

    #[test]
    fn the_union_of_every_rpu_is_recorded_with_its_cm_version() {
        let cm29 = rpu(
            false,
            vec![ExtMetadataBlock::Level2(ExtMetadataBlockLevel2 {
                target_max_pq: 2081,
                ..Default::default()
            })],
        );
        let cm40 = rpu(
            true,
            vec![ExtMetadataBlock::Level8(ExtMetadataBlockLevel8 {
                length: 13,
                target_display_index: 1,
                ..Default::default()
            })],
        );
        let mut accumulator = DvLevelsAccumulator::default();
        let mut splitter = AnnexBSplitter::default();
        let stream = annexb(&[&cm29, &cm40]);
        // One byte at a time: start codes split across reads still split.
        for byte in &stream {
            splitter.push(std::slice::from_ref(byte), 1 << 20, &mut |nal| {
                accumulator.observe_nal(nal)
            });
        }
        splitter.finish(&mut |nal| accumulator.observe_nal(nal));
        let mut expected = levels_of(&cm29);
        expected.extend(levels_of(&cm40));
        assert!(expected.contains(&2) && expected.contains(&8) && expected.contains(&254));
        let record = accumulator.finish();
        assert_eq!(
            record,
            DvLevelsRecord::Recorded {
                schema: SCHEMA,
                levels: expected.iter().copied().collect(),
                cm_v40: true,
                rpus: 2,
                unparsed: 0,
            }
        );
        let document = serde_json::json!({ PROBE_FIELD: record });
        assert_eq!(
            DvLevelsRecord::from_probe_json(&document.to_string()),
            Some(record.clone())
        );
        assert_eq!(
            record.title_levels(),
            Some(DvTitleLevels {
                levels: expected,
                cm_v40: true
            })
        );
    }

    #[test]
    fn a_cm29_title_is_not_v40_and_a_title_without_rpus_is_unreadable() {
        let mut accumulator = DvLevelsAccumulator::default();
        AnnexBSplitter::default().push(&annexb(&[&rpu(false, vec![])]), 1 << 20, &mut |nal| {
            accumulator.observe_nal(nal)
        });
        // The last NAL is only completed by finish; push alone parsed none.
        assert!(matches!(
            accumulator.finish(),
            DvLevelsRecord::Unreadable { .. }
        ));

        let mut accumulator = DvLevelsAccumulator::default();
        let mut splitter = AnnexBSplitter::default();
        splitter.push(&annexb(&[&rpu(false, vec![])]), 1 << 20, &mut |nal| {
            accumulator.observe_nal(nal)
        });
        splitter.finish(&mut |nal| accumulator.observe_nal(nal));
        let DvLevelsRecord::Recorded { cm_v40, levels, .. } = accumulator.finish() else {
            panic!("recorded");
        };
        assert!(!cm_v40 && !levels.contains(&254));

        let mut accumulator = DvLevelsAccumulator::default();
        accumulator.observe_nal(&[0x7c, 0x01, 0xff, 0xff]);
        assert_eq!(
            accumulator.finish(),
            DvLevelsRecord::Unreadable {
                schema: SCHEMA,
                reason: "no parsable Dolby Vision display metadata in any RPU".into()
            }
        );
    }

    #[test]
    fn an_oversized_nal_is_dropped_without_holding_it() {
        let rpu = rpu(false, vec![]);
        let mut stream = vec![0, 0, 1, 0x26, 0x01];
        stream.extend(std::iter::repeat_n(0x55, 10_000));
        stream.extend_from_slice(&[0, 0, 1]);
        stream.extend_from_slice(&rpu);
        let mut seen = Vec::new();
        let mut splitter = AnnexBSplitter::default();
        for chunk in stream.chunks(512) {
            splitter.push(chunk, 4096, &mut |nal| seen.push(nal.to_vec()));
            assert!(splitter.pending.len() <= 4096 + 512);
        }
        splitter.finish(&mut |nal| seen.push(nal.to_vec()));
        assert_eq!(seen, vec![rpu]);
    }

    #[test]
    fn malformed_or_foreign_records_read_as_absent() {
        for value in [
            serde_json::json!({"outcome":"recorded","schema":1,"levels":[],"cm_v40":false,"rpus":1,"unparsed":0}),
            serde_json::json!({"outcome":"recorded","schema":1,"levels":[2,1],"cm_v40":false,"rpus":1,"unparsed":0}),
            serde_json::json!({"outcome":"recorded","schema":2,"levels":[1],"cm_v40":false,"rpus":1,"unparsed":0}),
            serde_json::json!({"outcome":"recorded","schema":1,"levels":[1],"cm_v40":false,"rpus":0,"unparsed":0}),
            serde_json::json!({"outcome":"recorded","schema":1,"levels":[1],"cm_v40":false,"rpus":1,"unparsed":0,"extra":1}),
        ] {
            let document = serde_json::json!({ PROBE_FIELD: value });
            assert_eq!(DvLevelsRecord::from_probe_value(&document), None, "{value}");
        }
        assert_eq!(DvLevelsRecord::from_probe_json("{}"), None);
        let unreadable = DvLevelsRecord::unreadable("x".repeat(300) + "\n");
        assert!(unreadable.well_formed());
        assert_eq!(unreadable.title_levels(), None);
    }
}
