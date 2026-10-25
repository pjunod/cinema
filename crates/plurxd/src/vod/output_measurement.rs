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
    fn requested_heap_bytes(&self) -> Option<usize> {
        self.source_version
            .capacity()
            .checked_add(self.recipe_key.capacity())?
            .checked_add(self.playlist.capacity())?
            .checked_add(self.muxer_init.capacity())?
            .checked_add(self.served_init.capacity())
    }
}

impl OutputObservation {
    fn requested_heap_bytes(&self) -> Option<usize> {
        self.preimage
            .requested_heap_bytes()?
            .checked_add(self.muxer_init.capacity())?
            .checked_add(self.served_init.capacity())?
            .checked_add(self.reducer.requested_heap_bytes()?)?
            .checked_add(
                self.members
                    .capacity()
                    .checked_mul(std::mem::size_of::<Option<ObservedOutputMember>>())?,
            )
    }
}

impl PublishedOutputMeasurement {
    pub(super) fn requested_heap_bytes(&self) -> Option<usize> {
        self.observation
            .as_ref()
            .map_or(Some(0), OutputObservation::requested_heap_bytes)
    }
}

/// Conservative projection for the optional full-output observation retained
/// after the first finite publication. It uses the profile's bounded entry
/// count and precomputed immutable string capacities.
pub(super) fn projected_observation_bytes(
    source_version_capacity: usize,
    recipe_key_capacity: usize,
    playlist_capacity: usize,
    muxer_init_capacity: usize,
    served_init_capacity: usize,
    entry_count: usize,
) -> Option<usize> {
    if entry_count > 8192 {
        return Some(0);
    }
    let origin = std::mem::size_of::<OutputOrigin>()
        .checked_add(source_version_capacity)?
        .checked_add(recipe_key_capacity)?
        .checked_add(playlist_capacity)?
        .checked_add(muxer_init_capacity)?
        .checked_add(served_init_capacity)?;
    let reducer = std::mem::size_of::<FullOutputMeasurement>().checked_add(
        entry_count.checked_mul(std::mem::size_of::<(WireDuration, Option<u64>)>())?,
    )?;
    let observation = std::mem::size_of::<OutputObservation>()
        .checked_add(origin)?
        .checked_add(muxer_init_capacity)?
        .checked_add(served_init_capacity)?
        .checked_add(reducer)?
        .checked_add(
            entry_count.checked_mul(std::mem::size_of::<Option<ObservedOutputMember>>())?,
        )?;
    Some(observation)
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
    pub(super) fn refuse(&mut self) {
        self.refused = true;
    }
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
        if self.refused {
            return;
        }
        let Some(source) = rendition.source.as_ref() else {
            self.refused = true;
            return;
        };
        self.observe_version(
            rendition,
            init,
            epoch,
            entry,
            member,
            source.object_version(),
        );
    }

    /// Finite output has no daemon `SourceFence`; core owns the actual held
    /// source. This is called only after its durable-publication and
    /// same-held-source postcheck event, and remains advisory measurement only.
    #[cfg(target_os = "linux")]
    pub(super) fn observe_finite(
        &mut self,
        rendition: &Rendition,
        init: &InitIdentity,
        epoch: u64,
        entry: u32,
        member: ObservedOutputMember,
    ) {
        if rendition.dir.finite_producer().is_none() {
            self.refused = true;
            return;
        }
        let Some(encoding) = rendition.recipe.encoding.as_ref() else {
            self.refused = true;
            return;
        };
        let source_version = encoding.source_object_version.as_str();
        if source_version.is_empty()
            || rendition
                .recipe
                .source_object_version
                .as_deref()
                .is_some_and(|version| version != source_version)
        {
            self.refused = true;
            return;
        }
        self.observe_version(rendition, init, epoch, entry, member, source_version);
    }

    fn observe_version(
        &mut self,
        rendition: &Rendition,
        init: &InitIdentity,
        epoch: u64,
        entry: u32,
        member: ObservedOutputMember,
        source_version: &str,
    ) {
        self.observe_parts(
            &rendition.key,
            &rendition.playlist,
            &rendition.plan,
            init,
            epoch,
            entry,
            member,
            source_version,
        );
    }

    /// Static background output supplies the same immutable recipe/playlist
    /// directly, without creating a viewer rendition or reopening a source.
    /// The caller may invoke this only for core post-publication events.
    #[cfg(target_os = "linux")]
    pub(super) fn observe_static(
        &mut self,
        recipe_key: &str,
        playlist: &[u8],
        plan: &SegmentPlan,
        init: &InitIdentity,
        epoch: u64,
        entry: u32,
        member: ObservedOutputMember,
        source_version: &str,
    ) {
        self.observe_parts(
            recipe_key,
            playlist,
            plan,
            init,
            epoch,
            entry,
            member,
            source_version,
        );
    }

    fn observe_parts(
        &mut self,
        recipe_key: &str,
        playlist_bytes: &[u8],
        plan: &SegmentPlan,
        init: &InitIdentity,
        epoch: u64,
        entry: u32,
        member: ObservedOutputMember,
        source_version: &str,
    ) {
        let bytes = member.bytes;
        if self.refused {
            return;
        }
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
            source_version.as_bytes(),
            recipe_key.as_bytes(),
            playlist_bytes,
            init.muxer_init.as_bytes(),
            init.served_init.as_bytes(),
        ] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field);
        }
        let origin: [u8; 32] = hash.finalize().into();
        if self.observation.is_none() {
            let Ok(playlist) = std::str::from_utf8(playlist_bytes) else {
                self.refused = true;
                return;
            };
            if plan.len() > 8192 || !playlist.ends_with("#EXT-X-ENDLIST\n") {
                self.refused = true;
                return;
            }
            let durations: Option<Vec<_>> = playlist
                .lines()
                .filter_map(|line| line.strip_prefix("#EXTINF:"))
                .map(|duration| WireDuration::parse(duration.strip_suffix(',')?))
                .collect();
            let Some(durations) = durations.filter(|d| d.len() == plan.len()) else {
                self.refused = true;
                return;
            };
            self.observation = Some(OutputObservation {
                preimage: OutputOrigin {
                    nonce: *self.nonce.as_bytes(),
                    epoch,
                    source_version: source_version.to_owned(),
                    recipe_key: recipe_key.to_owned(),
                    playlist: playlist_bytes.to_vec(),
                    muxer_init: init.muxer_init.clone(),
                    served_init: init.served_init.clone(),
                },
                origin,
                epoch,
                muxer_init: init.muxer_init.clone(),
                served_init: init.served_init.clone(),
                reducer: FullOutputMeasurement::new(
                    origin,
                    u64::from(plan.target_duration.max(1)),
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
