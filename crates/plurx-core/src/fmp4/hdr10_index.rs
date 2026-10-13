//! Whole-title HDR10 metadata for immutable copy initialization segments.
use super::*;

/// Binds both local and shared indexes to the full-sample metadata scan.
pub const HDR10_INDEX_METADATA_REVISION: u8 = 1;

#[derive(Default)]
pub struct Hdr10IndexMetadata {
    mastering: Option<Vec<u8>>,
    light: Option<[u16; 2]>,
}

impl Hdr10IndexMetadata {
    /// Inspect every sample, including non-random-access pictures. Samples are
    /// never rewritten; their per-sequence SEIs remain authoritative in-band.
    pub fn observe_fragment(&mut self, fragment: &Fragment, init: &Init) -> Result<(), Fmp4Error> {
        let Some(video) = init.video().filter(|v| v.codec == Some(VideoCodec::Hevc)) else {
            return Ok(());
        };
        let Some(track) = fragment.track(video.id) else {
            return Ok(());
        };
        for run in &track.runs {
            let mut offset = run.data_offset;
            for sample in &run.samples {
                let end = offset
                    .checked_add(sample.size as usize)
                    .ok_or_else(|| Fmp4Error::Malformed("HDR sample size overflow".into()))?;
                let bytes = fragment
                    .bytes
                    .get(offset..end)
                    .ok_or_else(|| Fmp4Error::Malformed("HDR sample outside fragment".into()))?;
                self.observe_sample(bytes, video.nal_length_size)?;
                offset = end;
            }
        }
        Ok(())
    }

    fn observe_sample(&mut self, bytes: &[u8], length_size: u8) -> Result<(), Fmp4Error> {
        let mut result = Ok(());
        visit_length_prefixed_hevc_nals(bytes, length_size, |framed, kind| {
            if result.is_ok() {
                match kind {
                    Some(39) => result = self.observe_nal(&framed[usize::from(length_size)..]),
                    None => result = malformed("truncated HEVC NAL header"),
                    _ => {}
                }
            }
        })?;
        result
    }

    fn observe_nal(&mut self, nal: &[u8]) -> Result<(), Fmp4Error> {
        for (kind, payload) in sei_messages(nal)? {
            match kind {
                137 => {
                    if payload.len() != 24 || self.mastering.as_ref().is_some_and(|v| v != &payload)
                    {
                        return malformed("mastering-display metadata changes within the title");
                    }
                    self.mastering = Some(payload);
                }
                144 => {
                    let bytes: [u8; 4] = payload.try_into().map_err(|_| {
                        Fmp4Error::Malformed("content-light metadata is not four bytes".into())
                    })?;
                    let next = [
                        u16::from_be_bytes([bytes[0], bytes[1]]),
                        u16::from_be_bytes([bytes[2], bytes[3]]),
                    ];
                    self.light = Some(match self.light {
                        None => next,
                        Some(old) => std::array::from_fn(|i| {
                            // Zero means unspecified, not a measured zero. A
                            // partly unknown film cannot claim a known maximum.
                            if old[i] == 0 || next[i] == 0 {
                                0
                            } else {
                                old[i].max(next[i])
                            }
                        }),
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn promotion_nals(&self) -> Vec<Vec<u8>> {
        let mut result = Vec::new();
        if let Some(mastering) = &self.mastering {
            result.push(encode_sei(&[0x4e, 1], &[(137, mastering.clone())]));
        }
        if let Some(light) = self.light {
            let payload = light.into_iter().flat_map(u16::to_be_bytes).collect();
            result.push(encode_sei(&[0x4e, 1], &[(144, payload)]));
        }
        result
    }
}

fn sei_messages(nal: &[u8]) -> Result<Vec<(usize, Vec<u8>)>, Fmp4Error> {
    if nal.len() < 3 {
        return malformed("truncated prefix SEI");
    }
    let mut rbsp = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &byte in &nal[2..] {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        rbsp.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    let mut pos = 0;
    let mut result = Vec::new();
    while pos < rbsp.len() {
        if rbsp[pos] == 0x80 && rbsp[pos + 1..].iter().all(|b| *b == 0) {
            return Ok(result);
        }
        let kind = read_sei_number(&rbsp, &mut pos)
            .ok_or_else(|| Fmp4Error::Malformed("invalid SEI type".into()))?;
        let size = read_sei_number(&rbsp, &mut pos)
            .ok_or_else(|| Fmp4Error::Malformed("invalid SEI size".into()))?;
        let end = pos
            .checked_add(size)
            .filter(|end| *end <= rbsp.len())
            .ok_or_else(|| Fmp4Error::Malformed("truncated SEI payload".into()))?;
        result.push((kind, rbsp[pos..end].to_vec()));
        pos = end;
    }
    malformed("SEI trailing bits missing")
}

// SEI fields are byte-summed numbers, not 16-bit lengths. Unrelated user-data
// messages can exceed 64 KiB and must not disqualify an otherwise valid title.
fn read_sei_number(bytes: &[u8], pos: &mut usize) -> Option<usize> {
    let mut result = 0usize;
    loop {
        let byte = *bytes.get(*pos)?;
        *pos += 1;
        result = result.checked_add(usize::from(byte))?;
        if byte != 255 {
            return Some(result);
        }
    }
}

fn encode_sei(header: &[u8], messages: &[(usize, Vec<u8>)]) -> Vec<u8> {
    let mut rbsp = Vec::new();
    for (kind, payload) in messages {
        for mut value in [*kind, payload.len()] {
            while value >= 255 {
                rbsp.push(255);
                value -= 255;
            }
            rbsp.push(value as u8);
        }
        rbsp.extend_from_slice(payload);
    }
    rbsp.push(0x80);
    let mut nal = header.to_vec();
    let mut zeros = 0;
    for byte in rbsp {
        if zeros >= 2 && byte <= 3 {
            nal.push(3);
            zeros = 0;
        }
        nal.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    nal
}

/// A resumed muxer may have copied the landing scene's CLL into hvcC/clli.
/// Replace those declarations with the stored whole-title values before normal
/// promotion fills missing fields. Preserve every other init message and box.
pub fn normalize_hdr10_content_light(
    init: &mut Init,
    inputs: &[Vec<u8>],
) -> Result<bool, Fmp4Error> {
    let mut metadata = Hdr10IndexMetadata::default();
    for nal in inputs {
        metadata.observe_nal(nal)?;
    }
    let Some(light) = metadata.light else {
        return Ok(false);
    };
    let payload: Vec<u8> = light.into_iter().flat_map(u16::to_be_bytes).collect();
    let Some(location) = locate_hvcc(&init.bytes)? else {
        return Ok(false);
    };
    let entry = location.ancestors[1];
    let children = entry.start + entry.header_len + 78..location.sample_entry_end;
    let mut changed = false;
    for (at, box_header) in find_children(&init.bytes, children, b"clli")? {
        let range = at.start + box_header.header_len..at.start + box_header.size;
        if range.len() != 4 {
            return malformed("clli payload is not four bytes");
        }
        changed |= init.bytes[range.clone()] != payload;
        init.bytes[range].copy_from_slice(&payload);
    }
    let record = &init.bytes[location.payload.clone()];
    hvcc_nal_arrays(record)?;
    let mut rewritten = record[..23].to_vec();
    let mut pos = 23;
    for _ in 0..record[22] {
        let kind = record[pos] & 63;
        let count = u16::from_be_bytes([record[pos + 1], record[pos + 2]]);
        rewritten.extend_from_slice(&record[pos..pos + 3]);
        pos += 3;
        for _ in 0..count {
            let len = usize::from(u16::from_be_bytes([record[pos], record[pos + 1]]));
            pos += 2;
            let nal = &record[pos..pos + len];
            pos += len;
            let mut replacement = None;
            if kind == 39 {
                let mut messages = sei_messages(nal)?;
                let mut replaced = false;
                for (typ, bytes) in &mut messages {
                    if *typ == 144 && *bytes != payload {
                        *bytes = payload.clone();
                        replaced = true;
                    }
                }
                if replaced {
                    let bytes = encode_sei(&nal[..2], &messages);
                    if bytes != nal {
                        replacement = Some(bytes);
                    }
                }
            }
            let bytes = replacement.as_deref().unwrap_or(nal);
            let size = u16::try_from(bytes.len())
                .map_err(|_| Fmp4Error::Unsupported("canonical HDR SEI too large".into()))?;
            rewritten.extend_from_slice(&size.to_be_bytes());
            rewritten.extend_from_slice(bytes);
        }
    }
    if rewritten != record {
        let old_len = location.payload.len();
        let new_len = rewritten.len();
        init.bytes.splice(location.payload, rewritten);
        for ancestor in location.ancestors {
            if new_len > old_len {
                grow_box(&mut init.bytes, ancestor, new_len - old_len)?;
            } else if old_len > new_len {
                shrink_box(&mut init.bytes, ancestor, old_len - new_len)?;
            }
        }
        changed = true;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(max: u16, average: u16) -> Vec<u8> {
        encode_sei(
            &[0x4e, 1],
            &[(
                144,
                [max, average]
                    .into_iter()
                    .flat_map(u16::to_be_bytes)
                    .collect(),
            )],
        )
    }

    #[test]
    fn title_light_metadata_merges_maxima_without_losing_unknown_or_mastering_conflicts() {
        let mut meta = Hdr10IndexMetadata::default();
        for (max, average) in [(230, 197), (283, 200), (281, 139)] {
            meta.observe_nal(&light(max, average))
                .expect("valid HDR fixture");
        }
        assert_eq!(meta.light, Some([283, 200]));
        meta.observe_nal(&light(0, 150)).expect("valid HDR fixture");
        meta.observe_nal(&light(1000, 190))
            .expect("valid HDR fixture");
        assert_eq!(
            meta.light,
            Some([0, 200]),
            "unspecified is not a measured maximum"
        );
        let mastering = encode_sei(&[0x4e, 1], &[(137, vec![0; 24])]);
        meta.observe_nal(&mastering).expect("valid HDR fixture");
        meta.observe_nal(&mastering).expect("valid HDR fixture");
        assert!(meta
            .observe_nal(&encode_sei(&[0x4e, 1], &[(137, vec![1; 24])]))
            .is_err());
        assert!(meta
            .observe_nal(&encode_sei(&[0x4e, 1], &[(144, vec![0; 3])]))
            .is_err());
    }

    #[test]
    fn title_light_metadata_rejects_truncated_sample_framing() {
        let nal = light(230, 197);
        let mut framed = (nal.len() as u32).to_be_bytes().to_vec();
        framed.extend(nal);
        let mut metadata = Hdr10IndexMetadata::default();
        metadata
            .observe_sample(&framed, 4)
            .expect("valid HDR fixture");
        framed.extend([0, 0, 0, 10, 0x4e, 1]);
        assert!(metadata.observe_sample(&framed, 4).is_err());
        assert!(metadata.observe_sample(&[0, 0, 0], 4).is_err());
    }

    #[test]
    fn title_light_metadata_ignores_large_unrelated_sei_messages() {
        let nal = encode_sei(
            &[0x4e, 1],
            &[(5, vec![7; 70_000]), (144, vec![1, 27, 0, 200])],
        );
        let mut metadata = Hdr10IndexMetadata::default();
        metadata.observe_nal(&nal).expect("valid HDR fixture");
        assert_eq!(metadata.light, Some([283, 200]));
    }

    #[test]
    fn title_light_metadata_makes_different_landing_inits_identical() {
        let mut reader = FragmentReader::new();
        reader.push(&crate::testfixtures::pipe("closed-gop"));
        let Some(Unit::Init(muxer)) = reader.next_unit().expect("valid HDR fixture") else {
            panic!("init");
        };
        let mut early = muxer.clone();
        let mut late = muxer;
        promote_hdr10_static_metadata_from(&mut early, &[light(230, 197)])
            .expect("valid HDR fixture");
        promote_hdr10_static_metadata_from(&mut late, &[light(283, 200)])
            .expect("valid HDR fixture");
        assert_ne!(early.bytes, late.bytes);
        let mut meta = Hdr10IndexMetadata::default();
        meta.observe_nal(&light(230, 197))
            .expect("valid HDR fixture");
        meta.observe_nal(&light(283, 200))
            .expect("valid HDR fixture");
        let inputs = PromotionInputs {
            hdr10_sei: meta.promotion_nals(),
            ..Default::default()
        };
        promote_from(&mut early, &inputs).expect("valid HDR fixture");
        promote_from(&mut late, &inputs).expect("valid HDR fixture");
        assert_eq!(
            early.bytes, late.bytes,
            "hvcC and clli both use the title maxima"
        );
        let same = early.bytes.clone();
        promote_from(&mut early, &inputs).expect("valid HDR fixture");
        assert_eq!(early.bytes, same, "canonical promotion is idempotent");
        let location = locate_hvcc(&early.bytes)
            .expect("valid HDR fixture")
            .expect("valid HDR fixture");
        let entry = location.ancestors[1];
        let (at, header) = find_child(
            &early.bytes,
            entry.start + entry.header_len + 78..location.sample_entry_end,
            b"clli",
        )
        .expect("valid HDR fixture")
        .expect("valid HDR fixture");
        assert_eq!(
            &early.bytes[at.start + header.header_len..at.start + header.size],
            &[1, 27, 0, 200]
        );
    }
}
