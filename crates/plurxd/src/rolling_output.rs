//! Bounded metadata from committed rolling full-mux objects.
//! This is an observation, not retained-artifact or new-session authority.
use std::collections::BTreeMap;

use plurx_core::output_measurement::{CompleteOutputRates, FullOutputMeasurement, WireDuration};
use sha2::{Digest, Sha256};

const MAX_OBJECTS: usize = 8193;
const MAX_PLAYLIST: usize = 1 << 20;

#[derive(Clone, Debug)]
pub(crate) struct CommittedObject {
    pub(crate) bytes: u64,
    pub(crate) digest: [u8; 32],
}

/// One writing incarnation. Replacement/duplicate bodies cannot contribute
/// to an earlier incarnation's cost. No payload buffers are retained here.
pub(crate) struct RollingOutputMeasurement {
    nonce: uuid::Uuid,
    objects: BTreeMap<String, CommittedObject>,
    refused: bool,
}

impl Default for RollingOutputMeasurement {
    fn default() -> Self {
        Self {
            nonce: uuid::Uuid::new_v4(),
            objects: BTreeMap::new(),
            refused: false,
        }
    }
}

impl RollingOutputMeasurement {
    pub(crate) fn committed(&mut self, name: &str, object: CommittedObject) {
        if self.refused {
            return;
        }
        if !flat_name(name)
            || object.bytes == 0
            || self.objects.len() >= MAX_OBJECTS
            || self.objects.contains_key(name)
        {
            self.refused = true;
            self.objects.clear();
            return;
        }
        self.objects.insert(name.to_owned(), object);
    }

    /// Only after the owner proves normal completion and final publication.
    /// Caller must separately retain/revalidate the complete artifact before
    /// these observed numbers can become any frozen wire/candidate fact.
    pub(crate) fn complete(&self, playlist: &[u8]) -> Option<CompleteOutputRates> {
        if self.refused || playlist.len() > MAX_PLAYLIST {
            return None;
        }
        let text = std::str::from_utf8(playlist).ok()?;
        if text.lines().next()? != "#EXTM3U" {
            return None;
        }
        let mut target = None;
        let mut sequence = None;
        let mut pending = None;
        let mut durations = Vec::new();
        let mut objects = Vec::new();
        let mut transport_stream = true;
        let mut fragmented_mp4 = true;
        let mut init = None;
        let mut ended = false;
        // A seek suffix is an output observation, never a complete-title cost.
        let mut next = Some(0);
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if ended {
                return None;
            }
            if line == "#EXT-X-ENDLIST" {
                if pending.is_some() {
                    return None;
                }
                ended = true;
            } else if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
                if target.replace(value.parse::<u64>().ok()?).is_some() {
                    return None;
                }
            } else if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
                if sequence.replace(value.parse::<u64>().ok()?).is_some() || sequence != Some(0) {
                    return None;
                }
            } else if let Some(value) = line.strip_prefix("#EXTINF:") {
                if pending
                    .replace(WireDuration::parse(value.strip_suffix(',')?)?)
                    .is_some()
                {
                    return None;
                }
            } else if let Some(value) = line.strip_prefix("#EXT-X-MAP:URI=\"") {
                let name = value.strip_suffix('"')?;
                if init.replace(self.objects.get(name)?).is_some() {
                    return None;
                }
            } else if line.starts_with("#EXT-X-MAP:")
                || line.starts_with("#EXT-X-BYTERANGE:")
                || line.starts_with("#EXT-X-KEY:")
                || line.starts_with("#EXT-X-DISCONTINUITY")
            {
                return None;
            } else if !line.starts_with('#') {
                if !flat_name(line) || objects.len() >= 8192 {
                    return None;
                }
                let stem = line.strip_prefix("seg")?.split_once('.')?.0;
                transport_stream &= line.ends_with(".ts");
                fragmented_mp4 &= line.ends_with(".m4s") || line.ends_with(".mp4");
                let index = stem.parse::<u64>().ok()?;
                if next.is_some_and(|expected| expected != index) {
                    return None;
                }
                next = Some(index.checked_add(1)?);
                durations.push(pending.take()?);
                objects.push(self.objects.get(line)?);
            }
        }
        if !ended
            || pending.is_some()
            || objects.is_empty()
            || match init {
                Some(_) => !fragmented_mp4,
                None => !transport_stream,
            }
            || self.objects.len() != objects.len().checked_add(usize::from(init.is_some()))?
        {
            return None;
        }
        let mut hash = Sha256::new();
        hash.update(b"plurx/rolling-observed-full-output/v1\0");
        hash.update(self.nonce.as_bytes());
        hash.update((playlist.len() as u64).to_le_bytes());
        hash.update(playlist);
        for object in init.into_iter().chain(objects.iter().copied()) {
            hash.update(object.bytes.to_le_bytes());
            hash.update(object.digest);
        }
        let identity = hash.finalize().into();
        let mut reducer = FullOutputMeasurement::new(identity, target?, &durations);
        for (entry, object) in objects.iter().enumerate() {
            reducer.observe(identity, entry, object.bytes);
        }
        reducer.rates(true).ok()
    }
}

fn flat_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
        && name != "."
        && name != ".."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_actual_transport_stream_cost_requires_complete_unambiguous_container() {
        let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:4.000000,\nseg00000.ts\n#EXTINF:2.000000,\nseg00001.ts\n#EXT-X-ENDLIST\n";
        let mut observed = RollingOutputMeasurement::default();
        observed.committed(
            "seg00000.ts",
            CommittedObject {
                bytes: 1880,
                digest: [1; 32],
            },
        );
        assert!(
            observed.complete(playlist.as_bytes()).is_none(),
            "actual tail must commit"
        );
        observed.committed(
            "seg00001.ts",
            CommittedObject {
                bytes: 940,
                digest: [2; 32],
            },
        );
        let rates = observed
            .complete(playlist.as_bytes())
            .expect("self-initializing MPEG-TS needs no fake init");
        assert_eq!(rates.wire_bytes, 2820);
        assert_eq!(rates.average_bps, 3760);
        assert_eq!(rates.rfc_peak_bps, 3760);
        let ambiguous = playlist.replace("#EXTINF:4", "#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4");
        observed.committed(
            "init.mp4",
            CommittedObject {
                bytes: 100,
                digest: [3; 32],
            },
        );
        assert!(
            observed.complete(ambiguous.as_bytes()).is_none(),
            "TS plus fMP4 MAP cannot establish full mux container"
        );
        let mut mixed = RollingOutputMeasurement::default();
        mixed.committed(
            "seg00000.ts",
            CommittedObject {
                bytes: 1880,
                digest: [1; 32],
            },
        );
        mixed.committed(
            "seg00001.m4s",
            CommittedObject {
                bytes: 940,
                digest: [2; 32],
            },
        );
        assert!(mixed
            .complete(playlist.replace("seg00001.ts", "seg00001.m4s").as_bytes())
            .is_none());
    }

    #[test]
    fn rolling_complete_cost_refuses_seek_suffix_and_omitted_committed_tail() {
        let full = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4.000000,\nseg000000.m4s\n#EXT-X-ENDLIST\n";
        let object = |value| CommittedObject {
            bytes: 100,
            digest: [value; 32],
        };
        let mut observed = RollingOutputMeasurement::default();
        observed.committed("init.mp4", object(1));
        observed.committed("seg000000.m4s", object(2));
        assert!(
            observed.complete(full.as_bytes()).is_some(),
            "zero-origin complete control"
        );
        assert!(observed
            .complete(
                full.replace("MEDIA-SEQUENCE:0", "MEDIA-SEQUENCE:7")
                    .as_bytes()
            )
            .is_none());
        let mut suffix = RollingOutputMeasurement::default();
        suffix.committed("init.mp4", object(1));
        suffix.committed("seg000007.m4s", object(2));
        assert!(
            suffix
                .complete(full.replace("seg000000", "seg000007").as_bytes())
                .is_none(),
            "suffix cannot masquerade as title"
        );
        observed.committed("seg000001.m4s", object(3));
        assert!(
            observed.complete(full.as_bytes()).is_none(),
            "known committed tail cannot be omitted"
        );
    }

    #[test]
    fn rolling_committed_mux_observation_requires_complete_unchanged_incarnation() {
        let playlist = b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4.000000,\nseg000000.m4s\n#EXTINF:2.000000,\nseg000001.m4s\n#EXT-X-ENDLIST\n";
        let mut observed = RollingOutputMeasurement::default();
        let object = |bytes| CommittedObject {
            bytes,
            digest: [bytes as u8; 32],
        };
        observed.committed("init.mp4", object(100));
        observed.committed("seg000000.m4s", object(1000));
        assert!(observed.complete(playlist).is_none());
        observed.committed("seg000001.m4s", object(2000));
        let rates = observed
            .complete(playlist)
            .expect("all actual mux bodies and tail committed");
        assert_eq!(rates.wire_bytes, 3000);
        assert_eq!(rates.duration_micros, 6_000_000);
        assert_eq!(rates.average_bps, 4000);
        assert_eq!(rates.rfc_peak_bps, 8000);
        assert_eq!(rates.segment_burst_bps, 8000);
        assert!(observed
            .complete(&playlist[..playlist.len() - "#EXT-X-ENDLIST\n".len()])
            .is_none());
        let discontinuous = String::from_utf8(playlist.to_vec())
            .expect("ASCII playlist fixture")
            .replace("seg000001", "seg000002");
        assert!(observed.complete(discontinuous.as_bytes()).is_none());
        observed.committed("seg000001.m4s", object(2001));
        assert!(
            observed.complete(playlist).is_none(),
            "replacement cannot inherit old cost"
        );
        let reset = RollingOutputMeasurement::default();
        assert!(
            reset.complete(playlist).is_none(),
            "new lane cannot inherit prior commits"
        );
    }
}
