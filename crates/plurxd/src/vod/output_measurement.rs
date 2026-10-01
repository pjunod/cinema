//! Metadata from successful full-mux publications, never index estimates.
use super::*;
use plurx_core::output_measurement::{CompleteOutputRates, FullOutputMeasurement, WireDuration};

/// A rendition incarnation's observer. Missing/adopted bytes do not count.
/// Complete consumption is deliberately separate from observation.
pub(super) struct PublishedOutputMeasurement {
    nonce: uuid::Uuid,
    observation: Option<OutputObservation>,
    refused: bool,
}

struct OutputObservation {
    preimage: OutputOrigin,
    origin: [u8; 32],
    epoch: u64,
    muxer_init: String,
    served_init: String,
    reducer: FullOutputMeasurement,
    complete_tail: bool,
    members: Vec<Option<ObservedOutputMember>>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservedOutputMember {
    pub(super) bytes: u64,
    pub(super) digest: [u8; 32],
    pub(super) publication: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CompleteOutputObservation {
    pub(super) preimage: OutputOrigin,
    pub(super) rates: CompleteOutputRates,
    pub(super) epoch: u64,
    pub(super) served_init: String,
    pub(super) members: Vec<ObservedOutputMember>,
}

/// Original execution identity. Restart validation never substitutes a new
/// observer nonce, process recipe or publication epoch into this preimage.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OutputOrigin {
    pub(super) nonce: [u8; 16],
    pub(super) epoch: u64,
    pub(super) source_version: String,
    pub(super) recipe_key: String,
    pub(super) playlist: Vec<u8>,
    pub(super) muxer_init: String,
    pub(super) served_init: String,
}

impl OutputOrigin {
    pub(super) fn identity(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"plurx/vod-published-output/v1\0");
        hash.update(self.nonce);
        hash.update(self.epoch.to_le_bytes());
        for field in [
            self.source_version.as_bytes(),
            self.recipe_key.as_bytes(),
            self.playlist.as_slice(),
            self.muxer_init.as_bytes(),
            self.served_init.as_bytes(),
        ] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field);
        }
        hash.finalize().into()
    }
}

impl Default for PublishedOutputMeasurement {
    fn default() -> Self {
        Self {
            nonce: uuid::Uuid::new_v4(),
            observation: None,
            refused: false,
        }
    }
}

impl PublishedOutputMeasurement {
    /// Invoked under the existing manifest fence AFTER successful disk commit.
    /// An ambiguous/repeated/different-origin observation loses measurement
    /// authority but never rejects ordinary playable output.
    pub(super) fn observe(
        &mut self,
        rendition: &Rendition,
        init: &InitIdentity,
        epoch: u64,
        entry: u32,
        member: ObservedOutputMember,
    ) {
        let bytes = member.bytes;
        if self.refused {
            return;
        }
        let Some(source) = rendition.source.as_ref() else {
            self.refused = true;
            return;
        };
        if let Some(observation) = self.observation.as_mut() {
            if observation.epoch != epoch
                || observation.muxer_init != init.muxer_init
                || observation.served_init != init.served_init
            {
                self.refused = true;
                return;
            }
            observation
                .reducer
                .observe(observation.origin, entry as usize, bytes);
            if let Some(slot) = observation.members.get_mut(entry as usize) {
                *slot = Some(member);
            }
            return;
        }
        let mut hash = Sha256::new();
        // Length-delimited fields, exact source version, selected recipe,
        // full immutable playlist and served init, plus execution incarnation.
        hash.update(b"plurx/vod-published-output/v1\0");
        hash.update(self.nonce.as_bytes());
        hash.update(epoch.to_le_bytes());
        for field in [
            source.object_version().as_bytes(),
            rendition.key.as_bytes(),
            &rendition.playlist,
            init.muxer_init.as_bytes(),
            init.served_init.as_bytes(),
        ] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field);
        }
        let origin: [u8; 32] = hash.finalize().into();
        if self.observation.is_none() {
            let Ok(playlist) = std::str::from_utf8(&rendition.playlist) else {
                self.refused = true;
                return;
            };
            if rendition.plan.len() > 8192 || !playlist.ends_with("#EXT-X-ENDLIST\n") {
                self.refused = true;
                return;
            }
            let durations: Option<Vec<_>> = playlist
                .lines()
                .filter_map(|line| line.strip_prefix("#EXTINF:"))
                .map(|duration| WireDuration::parse(duration.strip_suffix(',')?))
                .collect();
            let Some(durations) = durations.filter(|d| d.len() == rendition.plan.len()) else {
                self.refused = true;
                return;
            };
            self.observation = Some(OutputObservation {
                preimage: OutputOrigin {
                    nonce: *self.nonce.as_bytes(),
                    epoch,
                    source_version: source.object_version().to_owned(),
                    recipe_key: rendition.key.clone(),
                    playlist: rendition.playlist.to_vec(),
                    muxer_init: init.muxer_init.clone(),
                    served_init: init.served_init.clone(),
                },
                origin,
                epoch,
                muxer_init: init.muxer_init.clone(),
                served_init: init.served_init.clone(),
                reducer: FullOutputMeasurement::new(
                    origin,
                    u64::from(rendition.plan.target_duration.max(1)),
                    &durations,
                ),
                complete_tail: false,
                members: vec![None; durations.len()],
            });
        }
        if let Some(observation) = self.observation.as_mut() {
            if origin != observation.origin || epoch != observation.epoch {
                self.refused = true;
                return;
            }
            observation.reducer.observe(origin, entry as usize, bytes);
            if let Some(slot) = observation.members.get_mut(entry as usize) {
                *slot = Some(member);
            }
        }
    }

    /// A normal generation completion alone is insufficient; every entry,
    /// including tail, must still be present and successfully observed.
    pub(super) fn complete(&mut self, epoch: u64, complete_retained: bool) {
        if let Some(observation) = self.observation.as_mut() {
            if observation.epoch == epoch && complete_retained {
                observation.complete_tail = true;
            }
        }
    }

    pub(super) fn complete_rates(&self) -> Option<CompleteOutputRates> {
        if self.refused {
            return None;
        }
        let observation = self.observation.as_ref()?;
        observation.reducer.rates(observation.complete_tail).ok()
    }

    pub(super) fn complete_observation(&self) -> Option<CompleteOutputObservation> {
        let rates = self.complete_rates()?;
        let observation = self.observation.as_ref()?;
        Some(CompleteOutputObservation {
            preimage: observation.preimage.clone(),
            rates,
            epoch: observation.epoch,
            served_init: observation.served_init.clone(),
            members: observation
                .members
                .iter()
                .cloned()
                .collect::<Option<Vec<_>>>()?,
        })
    }

    pub(super) fn matches_origin(&self, identity: &[u8; 32], epoch: u64, init: &str) -> bool {
        self.observation.as_ref().is_some_and(|observation| {
            &observation.origin == identity
                && observation.epoch == epoch
                && observation.served_init == init
        })
    }
}
