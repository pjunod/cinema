use super::*;

/// A split family proves one source: its shared video and every soundtrack
/// were prepared from the same source object, or the family is refused.
pub(super) fn split_family_shares_source<'a>(
    video: Option<&str>,
    soundtracks: impl IntoIterator<Item = &'a str>,
) -> bool {
    video.is_some_and(|video| soundtracks.into_iter().all(|soundtrack| soundtrack == video))
}

/// One verified child of a split copy presentation.
struct SplitChild {
    rendition: Arc<Rendition>,
    init: plurx_core::fmp4::Init,
}

impl VodServe {
    /// The session's live rendition is the shared video of a split copy.
    pub(super) async fn split_copy_session(&self, session_id: &str) -> bool {
        let sessions = self.shared.sessions.lock().await;
        sessions
            .get(session_id)
            .and_then(|session| session.live_rendition())
            .is_some_and(|rendition| {
                rendition.recipe.audio_excluded && rendition.recipe.encoding.is_none()
            })
    }

    /// Verify the shared copy video and every soundtrack from their actual
    /// served inits, then render the master. None when the session is not a
    /// split copy; Ok(None) when the session changed underneath the request.
    pub(super) async fn split_copy_master_before(
        &self,
        session_id: &str,
        deadline: Instant,
    ) -> Option<VodPublication<Option<Vec<u8>>>> {
        if !self.split_copy_session(session_id).await {
            return None;
        }
        let publication = self.session_rendition(session_id).await?;
        let owner = publication.owner;
        let result = async {
            let (_, _, _) = publication.result?;
            let (primary, children, requested_audio) = {
                let sessions = self.shared.sessions.lock().await;
                let Some(session) = sessions.get(session_id) else {
                    return Ok(None);
                };
                if !Arc::ptr_eq(&session.incarnation, &owner.incarnation)
                    || !session.owns_response_media(&owner)
                {
                    return Ok(None);
                }
                let Some(primary) = session.live_rendition() else {
                    return Ok(None);
                };
                (
                    Arc::clone(primary),
                    session
                        .children
                        .iter()
                        .map(|child| Arc::clone(&child.rendition))
                        .collect::<Vec<_>>(),
                    session.requested_audio_index,
                )
            };
            // The shared video first: it bounds the request for every soundtrack.
            let mut ordered = children
                .iter()
                .filter(|rendition| rendition.recipe.encoding.is_none())
                .collect::<Vec<_>>();
            if ordered.len() != 1 || !Arc::ptr_eq(ordered[0], &primary) {
                return Ok(None);
            }
            ordered.extend(
                children
                    .iter()
                    .filter(|rendition| rendition.recipe.encoding.is_some()),
            );
            let mut verified = Vec::with_capacity(ordered.len());
            for rendition in ordered {
                match self
                    .verified_split_child(session_id, &owner, rendition, deadline)
                    .await?
                {
                    Some(child) => verified.push(child),
                    None => return Ok(None),
                }
            }
            let fail =
                |error: plurx_core::fmp4::Fmp4Error| VodError::ProducerFailed(error.to_string());
            let mut verified = verified.into_iter();
            let video = verified.next().expect("verified shared video");
            let file = &video.rendition.recipe.file;
            let version = video
                .rendition
                .recipe
                .source_object_version
                .as_deref()
                .ok_or_else(|| {
                    VodError::ProducerFailed("split video has no source revision".into())
                })?;
            let copy = plurx_core::transcode::VodCopyVideoRendition::from_verified_init(
                &video.init,
                &video.rendition.key,
                version,
                None,
            )
            .map_err(fail)?;
            let video_budget = copy_video_budget(&video.rendition)?;
            let mut soundtracks = Vec::new();
            let mut audio_budgets = Vec::new();
            for child in verified {
                let encoding = child
                    .rendition
                    .recipe
                    .encoding
                    .as_ref()
                    .expect("encoded soundtrack");
                let recipe = encoding.shared_audio.as_ref().ok_or_else(|| {
                    VodError::ProducerFailed("split child is not a soundtrack".into())
                })?;
                let source_index = child.rendition.recipe.audio_index.ok_or_else(|| {
                    VodError::ProducerFailed("soundtrack names no source stream".into())
                })?;
                let stream = file
                    .audio_streams
                    .iter()
                    .find(|stream| stream.index == source_index)
                    .ok_or_else(|| {
                        VodError::ProducerFailed("soundtrack stream disappeared".into())
                    })?;
                let audio = plurx_core::transcode::VodSharedAudioRendition::from_verified_init(
                    &encoding.plan,
                    recipe,
                    &child.init,
                    &child.rendition.key,
                    &encoding.source_object_version,
                )
                .map_err(fail)?;
                audio_budgets.push(plurx_core::transcode::VodRenditionBandwidth {
                    rendition_id: child.rendition.key.clone(),
                    average_bps: None,
                    peak_bps: encoding
                        .continuous_peak_bps(&child.rendition.plan)
                        .ok_or_else(|| {
                            VodError::ProducerFailed("audio delivery budget missing".into())
                        })?,
                    sustained_bps: encoding.continuous_sustained_bps(),
                });
                soundtracks.push(plurx_core::transcode::VodSplitSoundtrack {
                    audio,
                    source_index,
                    language: stream.language.clone(),
                    name: stream
                        .title
                        .clone()
                        .filter(|title| !title.trim().is_empty())
                        .or_else(|| stream.language.clone())
                        .unwrap_or_else(|| format!("Track {}", soundtracks.len() + 1)),
                });
            }
            let default = requested_audio
                .filter(|index| soundtracks.iter().any(|track| track.source_index == *index))
                .or_else(|| soundtracks.first().map(|track| track.source_index))
                .ok_or_else(|| VodError::ProducerFailed("split copy has no soundtrack".into()))?;
            let family = plurx_core::transcode::VodSplitCopyFamily::new(copy, soundtracks, default)
                .map_err(fail)?;
            // Recheck every object after rendering: a source change or a new
            // producer generation must not be advertised under old identities.
            for rendition in std::iter::once(&primary).chain(children.iter()) {
                if rendition.closed.load(Relaxed)
                    || rendition.failure().is_some()
                    || self.source_changed(rendition)
                {
                    return Err(VodError::ProducerFailed(
                        "split copy source or rendition changed during verification".into(),
                    ));
                }
            }
            family
                .master_playlist(&video_budget, &audio_budgets, default)
                .map(String::into_bytes)
                .map(Some)
                .map_err(fail)
        }
        .await;
        Some(VodPublication { result, owner })
    }

    /// Open one child's init through the session's own reader and parse it.
    async fn verified_split_child(
        &self,
        session_id: &str,
        owner: &ResponseOwner,
        rendition: &Arc<Rendition>,
        deadline: Instant,
    ) -> Result<Option<SplitChild>, VodError> {
        let Some(lookup) = self
            .session_media_rendition(session_id, Some(&rendition.key))
            .await
        else {
            return Ok(None);
        };
        let Some(found) = lookup.result? else {
            return Ok(None);
        };
        if !Arc::ptr_eq(&lookup.owner.incarnation, &owner.incarnation)
            || !Arc::ptr_eq(&found.rendition, rendition)
        {
            return Ok(None);
        }
        let budget = found
            .block_budget
            .min(deadline.saturating_duration_since(Instant::now()));
        let mut ready = self.serve_init(rendition, budget, found.delivery).await?;
        let expected = rendition
            .identity
            .lock()
            .await
            .identity
            .as_ref()
            .map(|identity| identity.served_init.clone());
        let Some(expected) = expected else {
            return Ok(None);
        };
        let Some(bytes) = super::vod_serve_delivery::read_child_init(&mut ready, &expected).await?
        else {
            return Ok(None);
        };
        let mut reader = FragmentReader::new();
        reader.push(&bytes);
        match reader.next_unit() {
            Ok(Some(Unit::Init(init))) if reader.buffered() == 0 => Ok(Some(SplitChild {
                rendition: Arc::clone(rendition),
                init,
            })),
            _ => Err(VodError::ProducerFailed(
                "split copy init is not one verified record".into(),
            )),
        }
    }

    /// Child playlist for the shared video, from its actual served init.
    pub(super) async fn split_copy_child_playlist(
        &self,
        found: &ResolvedMediaReader,
    ) -> Result<Option<Vec<u8>>, VodError> {
        let expected = found
            .rendition
            .identity
            .lock()
            .await
            .identity
            .as_ref()
            .map(|identity| identity.served_init.clone());
        let Some(expected) = expected else {
            return Ok(None);
        };
        let mut ready = self
            .serve_init(
                &found.rendition,
                found.block_budget,
                Arc::clone(&found.delivery),
            )
            .await?;
        let Some(bytes) = super::vod_serve_delivery::read_child_init(&mut ready, &expected).await?
        else {
            return Ok(None);
        };
        let mut reader = FragmentReader::new();
        reader.push(&bytes);
        let init = match reader.next_unit() {
            Ok(Some(Unit::Init(init))) if reader.buffered() == 0 => init,
            _ => {
                return Err(VodError::ProducerFailed(
                    "split copy init is not one verified record".into(),
                ))
            }
        };
        let playlist = self
            .split_copy_video_playlist(&found.rendition, &init)
            .await?;
        if self.source_changed(&found.rendition)
            || found
                .rendition
                .identity
                .lock()
                .await
                .identity
                .as_ref()
                .is_none_or(|identity| identity.served_init != expected)
        {
            return Err(VodError::ProducerFailed(
                "split copy source or init changed during playlist verification".into(),
            ));
        }
        Ok(Some(playlist.into_bytes()))
    }

    /// The video-only copy's media playlist over its immutable keyframe plan.
    pub(super) async fn split_copy_video_playlist(
        &self,
        rendition: &Arc<Rendition>,
        init: &plurx_core::fmp4::Init,
    ) -> Result<String, VodError> {
        let version = rendition
            .recipe
            .source_object_version
            .as_deref()
            .ok_or_else(|| VodError::ProducerFailed("split video has no source revision".into()))?;
        plurx_core::transcode::VodCopyVideoRendition::from_verified_init(
            init,
            &rendition.key,
            version,
            None,
        )
        .and_then(|video| video.media_playlist(&rendition.plan))
        .map_err(|error| VodError::ProducerFailed(error.to_string()))
    }
}

/// Peak and average from the copy's own planned bytes. The plan's estimates
/// are the generous admission numbers the muxed copy master already uses.
fn copy_video_budget(
    rendition: &Arc<Rendition>,
) -> Result<plurx_core::transcode::VodRenditionBandwidth, VodError> {
    let plan = &rendition.plan;
    let mut peak = 0u64;
    let (mut bytes, mut ticks) = (0u128, 0u128);
    for entry in &plan.entries {
        if entry.duration_ticks == 0 {
            continue;
        }
        let rate = u128::from(entry.est_bytes) * 8 * u128::from(plan.timescale)
            / u128::from(entry.duration_ticks);
        peak = peak.max(u64::try_from(rate).unwrap_or(u64::MAX));
        bytes += u128::from(entry.est_bytes);
        ticks += u128::from(entry.duration_ticks);
    }
    let average = (ticks > 0)
        .then(|| bytes * 8 * u128::from(plan.timescale) / ticks)
        .and_then(|average| u64::try_from(average).ok())
        .filter(|average| *average > 0 && *average <= peak);
    if peak == 0 {
        return Err(VodError::ProducerFailed(
            "split video plan has no bytes".into(),
        ));
    }
    Ok(plurx_core::transcode::VodRenditionBandwidth {
        rendition_id: rendition.key.clone(),
        peak_bps: peak,
        average_bps: average,
        sustained_bps: None,
    })
}
