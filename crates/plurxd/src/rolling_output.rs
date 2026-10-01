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
        let mut pending = None;
        let mut durations = Vec::new();
        let mut objects = Vec::new();
        let mut init = None;
        let mut ended = false;
        let mut next = None;
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
                let index = stem.parse::<u64>().ok()?;
                if next.is_some_and(|expected| expected != index) {
                    return None;
                }
                next = Some(index.checked_add(1)?);
                durations.push(pending.take()?);
                objects.push(self.objects.get(line)?);
            }
        }
        if !ended || pending.is_some() || objects.is_empty() || init.is_none() {
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
