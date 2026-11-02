use super::*;

#[cfg(target_os = "linux")]
struct SelectedCopyPlanning {
    owner: plurx_core::adaptive_resources::runtime::planning_probe::FixedPlanningProbeOwner,
    application: plurx_core::adaptive_resources::runtime::finite_vod::FiniteApplicationLease,
    report: plurx_core::adaptive_resources::runtime::planning_probe::FixedProbeMetadataLease,
    recipe_binding: String,
}

#[cfg(target_os = "linux")]
fn selected_copy_session_dynamic_bytes(session: &Session) -> Option<usize> {
    session
        .frozen_presentation
        .as_ref()?
        .requested_heap_bytes()?
        .checked_add(session.dir.capacity())?
        .checked_add(session.item_title.capacity())?
        .checked_add(session.user_name.capacity())?
        .checked_add(session.supersession_user.capacity())?
        .checked_add(session.playback_id.capacity())?
        .checked_add(session.audio_delivery.as_ref().map_or(
            Some(0),
            plurx_core::playback::audio::AudioDelivery::requested_heap_bytes,
        )?)?
        .checked_add(
            session
                .recovery
                .as_ref()
                .map_or(Some(0), SessionRecoveryIdentity::requested_heap_bytes)?,
        )
}

#[cfg(target_os = "linux")]
fn selected_copy_session_fixed_heap_bytes() -> Option<usize> {
    // These are the actual Arc::new payloads in the selected Session literal.
    // Its inline fields and the Arc<Session> header are counted separately.
    [
        std::alloc::Layout::new::<AtomicBool>(), // actor_prepublication_producer
        std::alloc::Layout::new::<AtomicBool>(), // terminal_response_pending
        std::alloc::Layout::new::<AtomicBool>(), // failed
        std::alloc::Layout::new::<AtomicBool>(), // retention_cleanup_active
        std::alloc::Layout::new::<AtomicI64>(),  // high_segment
        std::alloc::Layout::new::<AtomicI64>(),  // fetched_end_ms
        std::alloc::Layout::new::<AtomicI64>(),  // live_bytes
        std::alloc::Layout::new::<AtomicI64>(),  // retention_garbage_bytes
        std::alloc::Layout::new::<Progress>(),
        std::alloc::Layout::new::<RetiredRelease>(),
        std::alloc::Layout::new::<std::sync::Mutex<Vec<(std::path::PathBuf, i64)>>>(),
        std::alloc::Layout::new::<std::sync::Mutex<u64>>(), // compatibility_attempt
    ]
    .into_iter()
    .try_fold(0usize, |total, payload| {
        total.checked_add(crate::playback_control::arc_requested_layout(payload)?)
    })
}

// Selected copy has no daemon-created scratch to clean up. Keep the legacy
// start guard's call sites identical while its ownership is optional here.
trait OptionalCopyStartSettlement {
    fn attach(&mut self, session: &Arc<Session>);
    fn disarm(&mut self);
    fn settle(self);
}
impl OptionalCopyStartSettlement for Option<PrepublicationStartSettlement> {
    fn attach(&mut self, session: &Arc<Session>) {
        if let Some(owner) = self.as_mut() {
            owner.attach(session);
        }
    }
    fn disarm(&mut self) {
        if let Some(owner) = self.as_mut() {
            owner.disarm();
        }
    }
    fn settle(self) {
        if let Some(owner) = self {
            owner.settle();
        }
    }
}

/// Construct the entire selected R demand while the original P still owns its
/// native planning work. This is pure: no source, output directory or root is
/// opened here. The resident window is a policy ceiling, independent of the
/// lifetime segment-name range; a full window backpressures the original
/// pipe until the rolling actor approves a typed prefix removal.
#[cfg(target_os = "linux")]
#[expect(
    clippy::too_many_arguments,
    reason = "bind the selected copy graph to its original planning owner, exact recipe inputs, application charge, and authoritative epoch/capacity in one pure preparation step"
)]
fn selected_copy_request(
    runtime: &Arc<plurx_core::adaptive_resources::runtime::NodeResourceRuntime>,
    planning: &SelectedCopyPlanning,
    source: &plurx_core::domain::MediaFile,
    rendition_id: String,
    video: plurx_core::transcode::CopyVideoOptions,
    audio_index: Option<i64>,
    transcode_audio: bool,
    pacing: plurx_core::transcode::Pacing,
    audio_delivery: Option<&plurx_core::playback::audio::AudioDelivery>,
    runtime_cache: std::path::PathBuf,
    application_metadata_bytes: usize,
    limit_epoch: u64,
    capacity: plurx_core::adaptive_resources::ResourceCapacity,
) -> Result<plurx_core::adaptive_resources::runtime::copy_vod::CopyVodRequest, String> {
    use plurx_core::adaptive_resources::runtime::copy_vod::{CopyPipeCommand, CopyVodRequest};
    use plurx_core::adaptive_resources::runtime::finite_vod::{
        FiniteOutputLimits, FiniteResponseBuffer, SoftwareAvcExecutable,
    };
    use plurx_core::adaptive_resources::{ResourceVector, WorkloadDemand};
    use plurx_core::process::observation::Metric;
    let max_segments = CopyVodRequest::maximum_segment_names();
    let parser_limits = plurx_core::copy_segmenter::Limits {
        max_bytes: plurx_core::adaptive_resources::runtime::copy_vod::COPY_VOD_MAX_SEGMENT_BYTES,
        ..Default::default()
    };
    if max_segments == 0 || parser_limits.max_bytes == 0 {
        return Err("selected copy has no bounded segment inventory".into());
    }
    let one_file =
        u64::try_from(parser_limits.max_bytes).map_err(|_| "selected copy file bound overflows")?;
    let resident_files = 128u32;
    let resident_logical_bytes = u64::from(resident_files)
        .checked_mul(one_file)
        .ok_or("selected copy resident window overflows")?;
    let reserved_storage_bytes = resident_logical_bytes
        .checked_add(u64::from(resident_files) * 16 * 1024)
        .and_then(|n| n.checked_add(128 * 1024))
        .ok_or("selected copy storage bound overflows")?;
    let playlist_bytes = usize::try_from(max_segments)
        .ok()
        .and_then(|n| n.checked_mul(64))
        .and_then(|n| {
            n.checked_add(plurx_core::fmp4::playlist_header(parser_limits.target_seconds).len())
        })
        .and_then(|n| n.checked_add("#EXT-X-ENDLIST\n".len()))
        .ok_or("selected copy playlist bound overflows")?;
    let app_entries = usize::try_from(max_segments)
        .ok()
        .and_then(|n| n.checked_mul(std::mem::size_of::<SegmentMeta>() + 12))
        .and_then(|n| n.checked_mul(3))
        .ok_or("selected copy rolling index bound overflows")?;
    // A terminal Session is replaced by one compact same-operation selector
    // in the manager's BTree book. Charge a sparse leaf plus internal node
    // for that entry until the original known close refunds this role.
    let compact_entry = std::mem::size_of::<uuid::Uuid>()
        .checked_add(std::mem::size_of::<SelectedCopyTerminalTicket>())
        .and_then(|n| n.checked_mul(11))
        .and_then(|n| n.checked_add(16 * std::mem::size_of::<usize>()))
        .and_then(|n| n.checked_mul(2))
        .ok_or("selected copy terminal selector bound overflows")?;
    let retired_entry = std::mem::size_of::<String>()
        .checked_add(std::mem::size_of::<RetiredPresentation>())
        .and_then(|n| n.checked_mul(11))
        .and_then(|n| n.checked_add(16 * std::mem::size_of::<usize>()))
        .and_then(|n| n.checked_mul(2))
        .and_then(|n| n.checked_add(36))
        .and_then(|n| n.checked_add(retired_presentation_cleanup_frame_bytes()))
        .and_then(|n| n.checked_add(36))
        .ok_or("selected copy retired presentation bound overflows")?;
    let application_owner_bytes = app_entries
        .checked_add(
            playlist_bytes
                .checked_mul(2)
                .ok_or("selected copy playlist overlap overflows")?,
        )
        .and_then(|n| n.checked_add(compact_entry))
        .and_then(|n| n.checked_add(retired_entry))
        .and_then(|n| {
            n.checked_add(std::mem::size_of::<Session>() + 2 * std::mem::size_of::<usize>())
        })
        .and_then(|n| n.checked_add(selected_copy_session_fixed_heap_bytes()?))
        .and_then(|n| {
            n.checked_add(
                crate::playback_control::RollingControlHandle::original_actor_frame_bytes()?,
            )
        })
        .and_then(|n| n.checked_add(source.requested_heap_bytes()?))
        .and_then(|n| n.checked_add(application_metadata_bytes))
        .ok_or("selected copy application owner bound overflows")?;
    let response_memory_bytes =
        FiniteResponseBuffer::required_partition_bytes(parser_limits.max_bytes, 2)
            .ok_or("selected copy response partition overflows")?;
    let output = FiniteOutputLimits {
        response_memory_bytes,
        retained_metadata_bytes: 0,
        client_state_bytes: 16 * 1024 * 1024,
        resident_file_objects: resident_files,
        resident_logical_bytes,
        reserved_storage_bytes,
        one_file_logical_bytes: one_file,
        one_io_payload_bytes: one_file,
        live_handles: 24,
        reader_leases: 8,
        resident_memory_bytes: 128 * 1024 * 1024,
        allocation_unit_bytes: 4096,
        per_object_reserve_bytes: 16 * 1024,
        child_directory_reserve_bytes: 64 * 1024,
        parent_entry_reserve_bytes: 64 * 1024,
        bootstrap_memory_bytes: 1024 * 1024,
    };
    let producer = planning.report.engine().producer();
    let executable = SoftwareAvcExecutable::new(
        producer.path().to_owned(),
        producer.source_identity(),
        producer.sha256(),
    )
    .map_err(|error| format!("selected copy executable plan: {error}"))?;
    let command = CopyPipeCommand::compile(
        source.clone(),
        video,
        executable,
        runtime_cache,
        audio_index,
        transcode_audio,
        pacing,
        audio_delivery,
    )
    .map_err(|error| format!("selected copy command: {error}"))?;
    let Metric::Known(cpu_ns_per_second) = capacity.cpu_ns_per_second else {
        return Err("selected copy CPU capacity is unknown".into());
    };
    if cpu_ns_per_second == 0 {
        return Err("selected copy CPU capacity is zero".into());
    }
    let mut request = CopyVodRequest {
        rendition_id,
        generation_epoch: CopyVodRequest::INITIAL_GENERATION_EPOCH,
        recipe_source_binding: planning.recipe_binding.clone(),
        source: planning.report.finite_source_request(),
        command,
        max_segments,
        parser_limits,
        reader_bounds: plurx_core::fmp4::FragmentReaderBounds {
            buffer_bytes: parser_limits.max_bytes,
            unit_bytes: parser_limits.max_bytes,
            max_tracks: 8,
            max_runs: 262_144,
            max_samples: 262_144,
        },
        max_pending_fragments: 8,
        operation_timeout: Duration::from_secs(60),
        demand: WorkloadDemand {
            resources: Metric::Known(ResourceVector {
                cpu_ns_per_second,
                peak_memory_bytes: 0,
                handles: u64::from(output.live_handles) + 9,
                processes: 1,
                hardware_slots: 0,
                tuner_slots: 0,
                scratch_bytes: reserved_storage_bytes,
            }),
            limit_epoch,
            trial: false,
        },
        application_owner_bytes: u64::try_from(application_owner_bytes)
            .map_err(|_| "selected copy application charge overflows")?,
        output,
    };
    let known_memory = runtime
        .copy_vod_known_memory_bytes(&request)
        .map_err(|error| format!("sizing selected copy role: {error}"))?;
    let Metric::Known(resources) = &mut request.demand.resources else {
        return Err("selected copy known component disappeared".into());
    };
    resources.peak_memory_bytes = known_memory;
    Ok(request)
}

#[cfg(all(test, target_os = "linux", feature = "cluster-integration-tests"))]
impl TranscodeManager {
    async fn selected_copy_quota_request_fixture(
        &self,
        file_id: i64,
    ) -> plurx_core::adaptive_resources::runtime::copy_vod::CopyVodRequest {
        let runtime = self
            .resource_runtime
            .as_ref()
            .expect("actual selected runtime");
        let inputs = self
            .rolling_start_inputs(file_id, None)
            .await
            .expect("actual source snapshot");
        let options = CopySessionOptions {
            transcode_audio: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
            dual_layer_as_copied: false,
        };
        let mut planning = self
            .collect_selected_copy_planning(
                &inputs.file,
                inputs.probe_json.as_deref(),
                None,
                &options,
            )
            .await
            .expect("same real selected P");
        let capacity = runtime.accounting().expect("actual observed capacity");
        let request = selected_copy_request(
            runtime,
            &planning,
            &inputs.file,
            format!("copy-quota-{}", uuid::Uuid::new_v4()),
            plurx_core::transcode::CopyVideoOptions::new(false, false),
            None,
            false,
            plurx_core::transcode::Pacing::unpaced(),
            None,
            self.runtime_cache.clone(),
            0,
            capacity.limit_epoch,
            capacity.capacity,
        )
        .expect("real finalized selected request");
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        planning
            .owner
            .finish_native(deadline)
            .await
            .expect("same original P native close");
        let mut cleanup = planning.owner.into_cleanup();
        drop(planning.report);
        drop(planning.application);
        cleanup
            .finish_retirement(deadline)
            .await
            .expect("actual P payload/journal close");
        request
    }

    pub(crate) async fn selected_copy_quota_terminal_regression(&self, file_id: i64) {
        let runtime = self.resource_runtime.as_ref().expect("actual runtime");
        let baseline = runtime.accounting().expect("original baseline");
        let request = self.selected_copy_quota_request_fixture(file_id).await;
        let path = runtime
            .copy_vod_output_path(&request.rendition_id)
            .expect("configured same copy root");
        let application_bytes =
            usize::try_from(request.application_owner_bytes).expect("actual application bytes");
        let client_bytes =
            usize::try_from(request.output.client_state_bytes).expect("actual client partition");
        let quota =
            crate::playback_cache::reserve_finite(&path, request.output.reserved_storage_bytes)
                .await
                .expect("authentic global quota");
        let quota = crate::playback_cache::FiniteCacheReservation::enter(quota);
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        // Match production after the original P retirement and cache wait:
        // obtain a real current observation without rebinding the finalized
        // request's demand or allocation epoch.
        runtime
            .refresh_admission_capacity(deadline)
            .await
            .expect("actual current capacity before original copy R");
        let mut original = runtime
            .prepare_copy_vod(request, deadline)
            .await
            .expect("actual original copy R");
        assert!(crate::playback_cache::is_pinned(&path));
        let application = original
            .take_application_lease(application_bytes)
            .expect("actual original app lease");
        let output = original.output();
        let actor_bytes = crate::playback_control::RollingActorAccounting::owner_bytes()
            .expect("actual actor layout");
        let (metadata, client) = tokio::time::timeout_at(
            deadline.into(),
            output.reserve_metadata_partitions(
                actor_bytes,
                client_bytes
                    .checked_sub(actor_bytes)
                    .expect("actor fits actual partition"),
            ),
        )
        .await
        .expect("real borrowed metadata deadline")
        .expect("same original budget slices");
        let accounting = crate::playback_control::RollingActorAccounting::new(metadata, client)
            .expect("actual actor accounting");
        let (control, registration) = crate::playback_control::RollingControlHandle::spawn_prepublication_producer_with_application(
            "copy-quota-original-actor", application, accounting);
        let snapshot = control.snapshot().await.expect("actual actor snapshot");
        let last_alias = snapshot.clone();
        let actor = control.original_actor_join();
        let native = original
            .prepare_retirement(true, deadline)
            .await
            .expect("same original copy no-entry ticket");
        drop(output);
        drop(original);
        drop(registration);
        drop(control);
        let identity = uuid::Uuid::new_v4();
        self.retired_copy_tickets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                identity,
                SelectedCopyTerminalTicket {
                    native,
                    quota: Some(quota),
                    actor: Some(actor),
                },
            );
        for _ in 0..16 {
            self.reconcile_selected_copy_tickets_for_test().await;
            let joined = self
                .retired_copy_tickets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&identity)
                .is_some_and(|ticket| ticket.actor.is_none());
            if joined {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            self.retired_copy_tickets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&identity)
                .is_some_and(|ticket| ticket.actor.is_none() && ticket.quota.is_some()),
            "actual actor joined but same original native/app close still owns quota"
        );
        assert!(crate::playback_cache::is_pinned(&path));
        drop(snapshot);
        self.reconcile_selected_copy_tickets_for_test().await;
        assert!(
            crate::playback_cache::is_pinned(&path),
            "last actual application alias still holds original native retirement"
        );
        drop(last_alias);
        tokio::time::timeout_at(deadline.into(), async {
            loop {
                self.reconcile_selected_copy_tickets_for_test().await;
                if !self
                    .retired_copy_tickets
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .contains_key(&identity)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("original native/app close completes through existing book");
        assert!(!crate::playback_cache::is_pinned(&path));
        assert_eq!(
            runtime.accounting().expect("actual final refund").used,
            baseline.used
        );
    }

    pub(crate) async fn selected_copy_quota_refusal_regression(&self, file_id: i64) {
        let runtime = self.resource_runtime.as_ref().expect("actual runtime");
        let mut request = self.selected_copy_quota_request_fixture(file_id).await;
        let baseline = runtime.accounting().expect("actual before-R baseline");
        let path = runtime
            .copy_vod_output_path(&request.rendition_id)
            .expect("same configured output");
        let quota =
            crate::playback_cache::reserve_finite(&path, request.output.reserved_storage_bytes)
                .await
                .expect("same authentic grant before invalid request");
        let quota = crate::playback_cache::FiniteCacheReservation::enter(quota);
        request.parser_limits.max_bytes =
            plurx_core::adaptive_resources::runtime::copy_vod::COPY_VOD_MAX_SEGMENT_BYTES + 1;
        let error = runtime
            .prepare_copy_vod(request, std::time::Instant::now() + Duration::from_secs(90))
            .await
            .err()
            .expect("actual pure invalid request refusal");
        assert!(error.definitely_before_reservation());
        quota.known_closed(); // Exact private core provenance; never generic Err.
        assert!(!crate::playback_cache::is_pinned(&path));
        assert_eq!(
            runtime.accounting().expect("actual unchanged ledger").used,
            baseline.used
        );
        assert!(!path.exists());

        let mut request = self.selected_copy_quota_request_fixture(file_id).await;
        let path = runtime
            .copy_vod_output_path(&request.rendition_id)
            .expect("same configured second output");
        let quota =
            crate::playback_cache::reserve_finite(&path, request.output.reserved_storage_bytes)
                .await
                .expect("authentic grant before actual constructor error");
        let quota = crate::playback_cache::FiniteCacheReservation::enter(quota);
        // Real CopySink constructor refuses this only after R was queued.
        request.operation_timeout = Duration::from_secs(121);
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        // P and quota waits consume the earlier observation. Reobserve the
        // actual node before testing the post-R constructor boundary.
        runtime
            .refresh_admission_capacity(deadline)
            .await
            .expect("actual current capacity before the constructor refusal");
        let error = runtime
            .prepare_copy_vod(request, deadline)
            .await
            .err()
            .expect("actual generic post-R constructor error");
        assert!(
            !error.definitely_before_reservation(),
            "the real constructor refusal must occur after R: {error:?}"
        );
        drop(quota);
        assert!(
            crate::playback_cache::is_pinned(&path),
            "generic error/default Drop keeps SAME pending grant"
        );
    }

    pub(crate) async fn selected_copy_zero_quota_regression(&self, file_id: i64) {
        let runtime = self.resource_runtime.as_ref().expect("actual runtime");
        let baseline = runtime.accounting().expect("actual baseline");
        self.store
            .put_setting(keys::CACHE_MAX_GB, "0")
            .await
            .expect("actual operator setting");
        let result = self
            .start_copy(
                file_id,
                0.0,
                None,
                CopySessionOptions {
                    transcode_audio: false,
                    preserve_dolby_vision: false,
                    convert_dolby_vision: false,
                    dual_layer_as_copied: false,
                },
                "actual-copy-quota-test",
                &uuid::Uuid::new_v4().to_string(),
            )
            .await;
        assert!(result
            .err()
            .expect("actual selected start refuses zero quota")
            .contains("operator quota"));
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        tokio::time::timeout_at(deadline.into(), async {
            loop {
                runtime.maintain_planning_cleanup(deadline).await;
                if runtime
                    .accounting()
                    .expect("actual original P cleanup")
                    .used
                    == baseline.used
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("same original P cleanup after before-R quota refusal");
        assert!(self
            .retired_copy_tickets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty());
        assert!(self.sessions.lock().await.is_empty());
    }

    pub(crate) async fn selected_copy_profile_admission_regression(&self, file_id: i64) {
        use plurx_core::adaptive_resources::runtime::copy_vod::COPY_VOD_MAX_SEGMENT_BYTES;
        let runtime = self
            .resource_runtime
            .as_ref()
            .expect("actual selected runtime");
        let baseline = runtime.accounting().expect("real initial accounting");
        let inputs = self
            .rolling_start_inputs(file_id, None)
            .await
            .expect("actual source/probe/settings snapshot");
        let options = CopySessionOptions {
            transcode_audio: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
            dual_layer_as_copied: false,
        };
        let mut planning = self
            .collect_selected_copy_planning(
                &inputs.file,
                inputs.probe_json.as_deref(),
                None,
                &options,
            )
            .await
            .expect("actual original selected P and frozen inputs");
        let accounting = runtime.accounting().expect("same observed capacity");
        let request = selected_copy_request(
            runtime,
            &planning,
            &inputs.file,
            format!("copy-profile-{}", uuid::Uuid::new_v4()),
            plurx_core::transcode::CopyVideoOptions::new(false, false),
            None,
            false,
            plurx_core::transcode::Pacing::unpaced(),
            None,
            self.runtime_cache.clone(),
            // No Session or external dynamic presentation payload is built by
            // this boundary fixture. The production helper still projects its
            // actual Session/control/core layouts and source/recipe capacity.
            0,
            accounting.limit_epoch,
            accounting.capacity,
        )
        .expect("actual selected request");
        assert_eq!(request.parser_limits.max_bytes, COPY_VOD_MAX_SEGMENT_BYTES);
        assert!(
            plurx_core::copy_segmenter::Limits::default().max_bytes
                > request.parser_limits.max_bytes,
            "legacy global default stays larger than the selected typed ceiling"
        );
        assert_eq!(
            request.output.one_file_logical_bytes,
            COPY_VOD_MAX_SEGMENT_BYTES as u64
        );
        assert_eq!(
            request.output.one_io_payload_bytes,
            COPY_VOD_MAX_SEGMENT_BYTES as u64
        );
        assert_eq!(
            request.output.resident_logical_bytes,
            u64::from(request.output.resident_file_objects) * COPY_VOD_MAX_SEGMENT_BYTES as u64
        );
        assert_eq!(
            request.output.reserved_storage_bytes,
            request.output.resident_logical_bytes
                + u64::from(request.output.resident_file_objects) * 16 * 1024
                + 128 * 1024
        );
        assert!(
            runtime
                .copy_vod_known_memory_bytes(&request)
                .expect("actual fixed graph memory projection")
                > 0
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        planning
            .owner
            .finish_native(deadline)
            .await
            .expect("positive original P native close");
        let mut cleanup = planning.owner.into_cleanup();
        drop(planning.report);
        drop(planning.application);
        cleanup
            .finish_retirement(deadline)
            .await
            .expect("same original P application/journal retirement");
        let path_id = request.rendition_id.clone();
        // The actual P/journal close can outlive its earlier observation.
        // Production reobserves here while keeping this exact finalized
        // demand and allocation epoch; exercise that same boundary.
        runtime
            .refresh_admission_capacity(deadline)
            .await
            .expect("actual current capacity after original copy planning close");
        let mut original = runtime
            .prepare_copy_vod(request, deadline)
            .await
            .expect("selected request crosses the actual core pre-R cap validation");
        assert_eq!(
            runtime
                .accounting()
                .expect("actual R charged")
                .unknown_peak_producers,
            baseline.unknown_peak_producers + 1
        );
        assert!(original.directory_path().ends_with(&path_id));
        assert!(
            !original.directory_path().exists(),
            "no output/native entry was requested"
        );
        let mut ticket = original
            .prepare_retirement(true, deadline)
            .await
            .expect("same original positive no-entry copy closure");
        drop(original);
        ticket
            .finish_retirement(deadline)
            .await
            .expect("same original typed native/app terminal retirement");
        let settled = runtime.accounting().expect("actual known terminal refund");
        assert_eq!(settled.used, baseline.used);
        assert_eq!(
            settled.unknown_peak_producers,
            baseline.unknown_peak_producers
        );
    }
}

/// Everything a rolling start reads from the Store — the file, its stored
/// probe and every setting on the path — from one statement.
///
/// The start path used to read settings one key at a time as each step needed
/// them: thirteen serial linearizable reads on a rolling transcode start at
/// position 0 by the week of 2026-09-27 (eight after #627), two of them inside
/// an optional lookup with `?`, so a store error there failed Play.
pub(super) struct RollingStartInputs {
    pub(super) file: plurx_core::domain::MediaFile,
    pub(super) probe_json: Option<String>,
    pub(super) settings: std::collections::BTreeMap<String, String>,
    pub(super) capacity: plurx_core::store::PlanningCapacity,
    /// The file's item title from the same statement, for attribution only.
    pub(super) item_title: Option<String>,
}

impl RollingStartInputs {
    pub(super) fn capacity_limits(&self) -> (usize, usize) {
        let parse = |key, default| {
            TranscodeManager::parse_num_setting(
                self.capacity.admission_value(&self.settings, key),
                default,
            )
        };
        (
            parse(keys::MAX_HW_SESSIONS, DEFAULT_MAX_HW_SESSIONS),
            parse(keys::SW_POOL_THREADS, crate::admission::software_budget()),
        )
    }

    /// The stored encoder preference; empty is auto.
    pub(super) fn encoder_preference(&self) -> String {
        self.settings
            .get(keys::HWACCEL)
            .cloned()
            .unwrap_or_default()
    }

    pub(super) fn content_aware_encoding(&self) -> bool {
        self.settings
            .get(keys::CONTENT_AWARE_ENCODING)
            .map(String::as_str)
            == Some("1")
    }
}

impl TranscodeManager {
    /// The one Store read of a rolling start. A start bound to a candidate
    /// reuses the snapshot its plan was accepted from, which was fetched with
    /// the same key list; anything else reads one now. Its error is fatal: it
    /// carries the file.
    pub(super) async fn rolling_start_inputs(
        &self,
        file_id: i64,
        candidate_context: Option<&super::CandidateExecutionContext>,
    ) -> Result<RollingStartInputs, String> {
        // The same guard the catalog restore path keeps: a processed candidate
        // exists only as the prepared VOD graph it was qualified with, and the
        // rolling engine would serve its recipe identity unprocessed.
        if candidate_context.is_some_and(|context| context.selected_candidate.processed_dv) {
            return Err(vod_refusal_error(
                "candidate_recipe_unavailable",
                "processed candidate requires fresh admitted owner qualification",
            ));
        }
        if let Some(snapshot) = candidate_context
            .and_then(|context| context.planning_snapshot.as_ref())
            .filter(|snapshot| snapshot.file.id == file_id)
        {
            return Ok(RollingStartInputs {
                file: snapshot.file.clone(),
                probe_json: snapshot.probe_json.clone(),
                settings: snapshot.settings.clone(),
                capacity: snapshot.capacity.clone(),
                item_title: snapshot.item_title.clone(),
            });
        }
        let snapshot = self
            .store
            .playback_planning_snapshot(file_id, &super::QUALITY_PLANNING_KEYS)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "file not found".to_owned())?;
        Ok(RollingStartInputs {
            file: snapshot.file,
            probe_json: snapshot.probe_json,
            settings: snapshot.settings,
            capacity: snapshot.capacity,
            item_title: snapshot.item_title,
        })
    }

    /// The rolling-retention budget from settings already read: a pure
    /// parse, never a Store read. `None` is off. Retention is enabled by its
    /// Developer switch (`vod.rolling_retention`, absent = off), not by the
    /// cache key's presence; its budget is `cache.max_gb` under the one
    /// unset rule ([`crate::cachekeep::cache_budget`]).
    pub(super) fn rolling_retained_budget_from(
        settings: &std::collections::BTreeMap<String, String>,
    ) -> Option<u64> {
        let enabled = plurx_core::store::stored_switch(
            settings
                .get(plurx_core::store::keys::VOD_ROLLING_RETENTION)
                .map(String::as_str),
            false,
        );
        if !enabled {
            return None;
        }
        crate::cachekeep::cache_budget(
            settings
                .get(plurx_core::store::keys::CACHE_MAX_GB)
                .map(String::as_str),
        )
    }

    /// Input pacing from settings already read.
    pub(super) async fn pacing_from_settings(
        settings: &std::collections::BTreeMap<String, String>,
        for_copy: bool,
    ) -> Pacing {
        Self::pacing_from(
            Self::num_from(settings, keys::HLS_READRATE, HLS_READRATE_DEFAULT),
            Self::num_from(settings, keys::HLS_BURST_SECS, HLS_BURST_SECS_DEFAULT),
            for_copy,
        )
        .await
    }

    /// The scratch and ahead limits from settings already read, published as
    /// the current snapshot so the 2 s refresh and this start agree.
    pub(super) fn ahead_limits_for_start(
        &self,
        settings: &std::collections::BTreeMap<String, String>,
    ) -> AheadLimits {
        let limits = Self::ahead_limits_from(settings);
        self.publish_ahead_limits(limits);
        limits
    }

    /// The rolling producer owns route-specific initial negotiation. A retained
    /// producer answer is already authoritative, even if catalog facts changed.
    pub(super) fn rolling_start_audio_options(
        &self,
        file: &plurx_core::domain::MediaFile,
        mut options: TranscodeOptions,
        claim: Option<&plurx_core::playback::audio::AudioClaim>,
        retained: Option<&plurx_core::playback::audio::AudioDelivery>,
    ) -> TranscodeOptions {
        let audio = retained.cloned().or_else(|| {
            claim.map(|claim| {
                plurx_core::playback::audio::resolve_audio(
                    options
                        .audio_index
                        .and_then(|index| {
                            file.audio_streams
                                .iter()
                                .find(|stream| stream.index == index)
                        })
                        .or_else(|| file.audio_streams.first()),
                    &claim.profile(),
                    plurx_core::playback::audio::AudioRoute::RollingHls,
                    file.audio_offset_ms,
                )
            })
        });
        if let Some(audio) = audio {
            options.set_audio_delivery(audio);
        }
        options
    }

    /// The original P owner enters the core book synchronously before its
    /// first native await. Its real probe and executable facts are compared
    /// to the catalog row before any rolling R can acquire a source or root.
    #[cfg(target_os = "linux")]
    async fn collect_selected_copy_planning(
        &self,
        file: &plurx_core::domain::MediaFile,
        stored_probe: Option<&str>,
        audio_override: Option<i64>,
        options: &CopySessionOptions,
    ) -> Result<SelectedCopyPlanning, String> {
        use sha2::Digest as _;
        let runtime = self
            .resource_runtime
            .as_ref()
            .ok_or("rolling copy resource runtime is unavailable")?;
        runtime
            .refresh_admission_capacity(std::time::Instant::now() + Duration::from_secs(15))
            .await
            .map_err(|error| format!("observing rolling copy admission capacity: {error:?}"))?;
        let snapshot = self
            .store
            .playback_planning_snapshot(file.id, &super::QUALITY_PLANNING_KEYS)
            .await
            .map_err(|error| format!("reading copy planning snapshot: {error}"))?
            .ok_or("rolling copy source is missing")?;
        if snapshot.file.id != file.id
            || snapshot.file.path != file.path
            || snapshot.file.size != file.size
            || snapshot.file.mtime != file.mtime
            || snapshot.probe_json.as_deref() != stored_probe
        {
            return Err("rolling copy source changed between catalog and admission".into());
        }
        let stored =
            stored_probe.ok_or("rolling copy requires a stored probe; rescan the source")?;
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"rolling-copy-one-input-v1\0");
        hasher.update(file.id.to_le_bytes());
        hasher.update(file.size.to_le_bytes());
        hasher.update(file.mtime.to_le_bytes());
        hasher.update(audio_override.unwrap_or(-1).to_le_bytes());
        hasher.update([
            u8::from(options.transcode_audio),
            u8::from(options.preserve_dolby_vision),
            u8::from(options.convert_dolby_vision),
        ]);
        let recipe_binding = hex::encode(hasher.finalize());
        let snapshot_bytes =
            crate::transcode::adaptive_attempts::planning_snapshot_requested_heap_bytes(&snapshot)
                .ok_or("rolling copy planning snapshot heap overflows")?;
        let application_bytes = snapshot_bytes
            .checked_add(crate::transcode::adaptive_attempts::PLANNING_TO_R_HANDOFF_BYTES)
            .ok_or("rolling copy planning application charge overflows")?;
        let request = crate::transcode::adaptive_attempts::fixed_planning_request(
            self,
            &snapshot,
            &recipe_binding,
            application_bytes,
        )?;
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        let mut planning = runtime
            .prepare_fixed_planning_probe(request, deadline)
            .map_err(|error| format!("admitting rolling copy planning: {error}"))?;
        let application = planning
            .take_application_lease(application_bytes)
            .map_err(|error| format!("retaining rolling copy planning owner: {error}"))?;
        planning
            .prepare_inputs(deadline)
            .await
            .map_err(|error| format!("acquiring rolling copy planning inputs: {error}"))?;
        let report = planning
            .collect_and_refine(deadline)
            .await
            .map_err(|error| format!("collecting rolling copy planning facts: {error}"))?;
        if report.source_file_id() != file.id.to_string()
            || report.source_path() != file.path
            || report.scanner_size() != file.size
            || report.scanner_mtime() != file.mtime
        {
            return Err("rolling copy admitted source differs from catalog".into());
        }
        let comparison = crate::ffmpeg::compare_probe_documents(stored, report.document())
            .map_err(|error| format!("rolling copy stored probe differs: {error}"))?;
        if !comparison.same {
            return Err(format!(
                "rolling copy source requires rescan at {}",
                comparison.rendered_differences()
            ));
        }
        Ok(SelectedCopyPlanning {
            owner: planning,
            application,
            report,
            recipe_binding,
        })
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)] // one stream's worth of knobs
    pub async fn start(
        &self,
        file_id: i64,
        target_height: i64,
        start_seconds: f64,
        audio_override: Option<i64>,
        subtitle_override: Option<i64>,
        user_name: &str,
        playback_id: &str,
    ) -> Result<StartInfo, String> {
        let supersession_user = serde_json::json!(["username", user_name]).to_string();
        // A process-local start has no durable recovery identity.
        self.start_with_audio_offset(
            file_id,
            target_height,
            start_seconds,
            audio_override,
            subtitle_override,
            0,
            user_name,
            &supersession_user,
            None,
            None,
            None,
            playback_id,
            false,
            false,
            None,
            Priority::Live,
            None,
            None,
            None,
        )
        .await
    }

    /// [`Self::start`] for a client that sends audio sinks, as every current
    /// client does.
    #[cfg(test)]
    pub async fn start_claiming(
        &self,
        file_id: i64,
        target_height: i64,
        playback_id: &str,
        claim: &plurx_core::playback::audio::AudioClaim,
    ) -> Result<StartInfo, String> {
        // Match the ordinary fixture start: this unclaimed manager launch has
        // no durable recovery identity, rather than a synthetic Local user0.
        self.start_with_audio_offset(
            file_id,
            target_height,
            0.0,
            None,
            None,
            0,
            "paul",
            &serde_json::json!(["username", "paul"]).to_string(),
            None,
            None,
            None,
            playback_id,
            false,
            false,
            None,
            Priority::Live,
            Some(claim),
            None,
            None,
        )
        .await
    }

    /// Acquire the foreground encoder's durable permit after every background
    /// encoder has actually yielded.
    ///
    /// The waiter remains registered until the returned hardware/software
    /// permit exists, so there is no handoff gap in which a producer can take
    /// capacity back. The permit then carries live ownership for the session's
    /// whole lifetime, keeping every background pool parked after this method
    /// drops the short-lived waiter.
    pub(super) async fn admit_live(
        &self,
        preferred: Encoder,
        plan: Option<&ResolvedTranscode>,
        work: Workload<'_>,
        max_wait: Duration,
        priority: Priority,
    ) -> Result<LiveAdmission, String> {
        let sw_budget = self.software_budget().await;
        let max_hw = if preferred != Encoder::Software {
            self.max_hw_sessions().await
        } else {
            0
        };
        self.admit_live_with(preferred, plan, work, max_wait, priority, sw_budget, max_hw)
            .await
    }

    /// [`Self::admit_live`] under pool sizes the caller already read.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn admit_live_with(
        &self,
        preferred: Encoder,
        plan: Option<&ResolvedTranscode>,
        work: Workload<'_>,
        max_wait: Duration,
        priority: Priority,
        sw_budget: usize,
        max_hw: usize,
    ) -> Result<LiveAdmission, String> {
        let mut queued = (priority == Priority::Live).then(|| self.admissions.queue_for_slot());
        let deadline = Instant::now() + max_wait;

        if preferred != Encoder::Software {
            let max = max_hw;
            // What this pipeline will actually spend, read off the plan rather
            // than off the encoder's name. A hardware encoder fed by a software
            // decode spends most of a box's cores on the decode, and admitting
            // it as "hardware, therefore no CPU" is how several of them end up
            // on one node with every counter reading healthy.
            let estimate = plan.map(|plan| TranscodeResourceEstimate::of(plan, &work));
            let mixed =
                estimate.is_some_and(|estimate| estimate.hardware_slot && estimate.cpu_threads > 0);
            loop {
                let decision = self.admissions.admit_with_priority(max, work, priority);
                if let Admission::Hardware(slot) = decision {
                    if !mixed {
                        return Ok(LiveAdmission {
                            encoder: preferred,
                            hw_slot: Some(slot),
                            sw_permit: None,
                        });
                    }
                    // Release the slot before re-taking both together. The two
                    // pools are one mutex, so the bundle is granted whole or
                    // not at all; holding this slot while waiting for CPU is
                    // the shape that deadlocks against a start doing the same
                    // in the other order.
                    drop(slot);
                    let estimate = estimate.expect("a mixed pipeline has an estimate");
                    if let Some(bundle) = self
                        .admissions
                        .try_admit_bundle(max, sw_budget, &estimate, priority)
                    {
                        let (hw_slot, sw_permit) = bundle.into_parts();
                        tracing::info!(
                            target: "plurxd::transcode",
                            threads = estimate.cpu_threads,
                            "software decode into a hardware encoder; reserving the CPU it will spend"
                        );
                        return Ok(LiveAdmission {
                            encoder: preferred,
                            hw_slot,
                            sw_permit,
                        });
                    }
                    // A slot exists but the CPU the decode needs does not.
                    // Bounded, and answered honestly: the old accounting would
                    // have started here and simply not reserved the cores.
                    if let Some(waiting) = queued.as_mut() {
                        waiting.mark_capacity_blocked();
                    }
                    let now = Instant::now();
                    if now < deadline {
                        tokio::time::sleep(ADMISSION_POLL.min(deadline - now)).await;
                        continue;
                    }
                    let why = format!(
                        "a hardware slot is free but this title decodes in software and the CPU pool is spent ({} of {sw_budget} threads reserved); try again in a moment",
                        self.admissions.software_in_use()
                    );
                    tracing::warn!(
                        target: "plurxd::transcode",
                        class = %work.software_class(), "{why}"
                    );
                    return Err(capacity_error(why));
                }

                if let Some(waiting) = queued.as_mut() {
                    waiting.mark_capacity_blocked();
                }
                let now = Instant::now();
                if now < deadline {
                    tokio::time::sleep(ADMISSION_POLL.min(deadline - now)).await;
                    continue;
                }

                return match decision {
                    // Background work owned a pool for the whole cooperative
                    // window and never yielded. That is a stuck worker, not a
                    // busy encoder, and the viewer does not pay for it: take
                    // the slot the cap allows (background holds still count),
                    // or the CPU forced, and let the holder find the pool
                    // owned at its next check. Only a live viewer earns this;
                    // a speculative start has nobody waiting on it.
                    Admission::WaitingForBackground if priority == Priority::Live => self
                        .admit_over_background(
                            preferred,
                            max,
                            sw_budget,
                            estimate.as_ref(),
                            work,
                            max_wait,
                        ),
                    Admission::WaitingForBackground => Err(capacity_error(format!(
                        "background encoding did not yield within {:.1}s; try again in a moment",
                        max_wait.as_secs_f64()
                    ))),
                    Admission::Software => match self.admissions.try_admit_software(
                        sw_budget,
                        work.software_threads(),
                        priority,
                    ) {
                        Some(permit) => {
                            tracing::info!(
                                target: "plurxd::transcode",
                                class = %work.software_class(),
                                threads = permit.threads(),
                                "hardware transcode slots full; this class runs comfortably in software here, so starting it there"
                            );
                            Ok(LiveAdmission {
                                encoder: Encoder::Software,
                                hw_slot: None,
                                sw_permit: Some(permit),
                            })
                        }
                        None => {
                            let why = format!(
                                "all {max} hardware transcode slots are in use and the software CPU pool is spent ({} of {sw_budget} threads reserved); try again in a moment",
                                self.admissions.software_in_use()
                            );
                            tracing::warn!(
                                target: "plurxd::transcode",
                                class = %work.software_class(), "{why}"
                            );
                            Err(capacity_error(why))
                        }
                    },
                    Admission::Refused(why) => {
                        tracing::warn!(
                            target: "plurxd::transcode",
                            class = %work.software_class(), "{why}"
                        );
                        Err(capacity_error(why))
                    }
                    Admission::Hardware(_) => unreachable!("hardware returned above"),
                };
            }
        }

        loop {
            if let Some(permit) =
                self.admissions
                    .try_admit_software(sw_budget, work.software_threads(), priority)
            {
                return Ok(LiveAdmission {
                    encoder: Encoder::Software,
                    hw_slot: None,
                    sw_permit: Some(permit),
                });
            }
            if let Some(waiting) = queued.as_mut() {
                waiting.mark_capacity_blocked();
            }
            let now = Instant::now();
            if now < deadline {
                tokio::time::sleep(ADMISSION_POLL.min(deadline - now)).await;
                continue;
            }
            if self.admissions.background_is_active() && priority == Priority::Live {
                // Same ruling as the hardware branch: the window is the
                // background worker's chance to checkpoint, not the viewer's
                // deadline. The take discounts the stuck background
                // reservation only; live usage still bounds it, so a pool
                // spent by other viewers is refused exactly as it would be
                // with no background worker in the picture.
                let weight = work.software_threads();
                if let Some(permit) = self
                    .admissions
                    .software_pool()
                    .take_over_background(sw_budget, weight)
                {
                    self.note_background_overrun("software", weight, max_wait);
                    return Ok(LiveAdmission {
                        encoder: Encoder::Software,
                        hw_slot: None,
                        sw_permit: Some(permit),
                    });
                }
                let why = format!(
                    "the software CPU pool is spent by live sessions ({} of {sw_budget} threads reserved) and no slot freed within {:.1}s; try again in a moment",
                    self.admissions.software_in_use(),
                    max_wait.as_secs_f64()
                );
                tracing::warn!(target: "plurxd::transcode", class = %work.software_class(), "{why}");
                return Err(capacity_error(why));
            }
            let why = if self.admissions.background_is_active() {
                format!(
                    "background encoding did not yield within {:.1}s; try again in a moment",
                    max_wait.as_secs_f64()
                )
            } else {
                format!(
                    "the software CPU pool is spent ({} of {sw_budget} threads reserved) and no slot freed within {:.1}s; try again in a moment",
                    self.admissions.software_in_use(),
                    max_wait.as_secs_f64()
                )
            };
            tracing::warn!(target: "plurxd::transcode", class = %work.software_class(), "{why}");
            return Err(capacity_error(why));
        }
    }

    /// The live admission a viewer gets when background ownership outlived
    /// the cooperative window. Hardware within the cap first (a background
    /// hardware hold still counts against `max`, so this never oversubscribes
    /// the GPU); otherwise the ordinary software decision, with the CPU taken
    /// over the background reservation and bounded by live usage. A class
    /// software cannot carry is still refused, honestly — that refusal is
    /// about the stream, not about the stuck worker — and so is a pool that
    /// other viewers have spent.
    fn admit_over_background(
        &self,
        preferred: Encoder,
        max: usize,
        sw_budget: usize,
        estimate: Option<&TranscodeResourceEstimate>,
        work: Workload<'_>,
        waited: Duration,
    ) -> Result<LiveAdmission, String> {
        match self.admissions.admit_over_background(max, work) {
            Admission::Hardware(slot) => {
                // A software decode into this hardware encoder still spends
                // the cores the estimate names; reserve them the same bounded
                // way, or give the slot back — the ordinary path takes the
                // bundle whole or not at all, and so does this one.
                let cpu_threads = estimate
                    .filter(|estimate| estimate.hardware_slot && estimate.cpu_threads > 0)
                    .map(|estimate| estimate.cpu_threads);
                let sw_permit = match cpu_threads {
                    None => None,
                    Some(threads) => match self
                        .admissions
                        .software_pool()
                        .take_over_background(sw_budget, threads)
                    {
                        Some(permit) => Some(permit),
                        None => {
                            drop(slot);
                            let why = format!(
                                "a hardware slot is free but this title decodes in software and the CPU pool is spent by live sessions ({} of {sw_budget} threads reserved); try again in a moment",
                                self.admissions.software_in_use()
                            );
                            tracing::warn!(
                                target: "plurxd::transcode",
                                class = %work.software_class(), "{why}"
                            );
                            return Err(capacity_error(why));
                        }
                    },
                };
                self.note_background_overrun(
                    "hardware",
                    sw_permit
                        .as_ref()
                        .map_or(0, crate::admission::SwPermit::threads),
                    waited,
                );
                Ok(LiveAdmission {
                    encoder: preferred,
                    hw_slot: Some(slot),
                    sw_permit,
                })
            }
            Admission::Software => {
                let weight = work.software_threads();
                let Some(permit) = self
                    .admissions
                    .software_pool()
                    .take_over_background(sw_budget, weight)
                else {
                    let why = format!(
                        "all {max} hardware transcode slots are in use and the software CPU pool is spent by live sessions ({} of {sw_budget} threads reserved); try again in a moment",
                        self.admissions.software_in_use()
                    );
                    tracing::warn!(
                        target: "plurxd::transcode",
                        class = %work.software_class(), "{why}"
                    );
                    return Err(capacity_error(why));
                };
                self.note_background_overrun("software", weight, waited);
                tracing::info!(
                    target: "plurxd::transcode",
                    class = %work.software_class(),
                    threads = weight,
                    "hardware transcode slots full; this class runs comfortably in software here, so starting it there"
                );
                Ok(LiveAdmission {
                    encoder: Encoder::Software,
                    hw_slot: None,
                    sw_permit: Some(permit),
                })
            }
            Admission::Refused(why) => {
                tracing::warn!(
                    target: "plurxd::transcode",
                    class = %work.software_class(),
                    software_budget = sw_budget,
                    "{why}"
                );
                Err(capacity_error(why))
            }
            Admission::WaitingForBackground => {
                unreachable!("admit_over_background never waits for background")
            }
        }
    }

    /// One log line and one counter per viewer started over a background
    /// hold. The counter is the signal that some background worker is holding
    /// a permit through a phase that never looks at the pool; the log line
    /// names the pool so the worker can be found.
    fn note_background_overrun(&self, pool: &'static str, threads: usize, waited: Duration) {
        let snapshot = self.admissions.snapshot();
        tracing::warn!(
            target: "plurxd::transcode",
            pool,
            threads,
            waited_s = waited.as_secs_f64(),
            hardware_used = snapshot.hardware_used,
            software_used = snapshot.software_used,
            "background work did not yield within the cooperative window; starting the viewer over it"
        );
        crate::telemetry::record_background_overrun(pool);
    }

    /// Reserve the foreground encoder pool for the measured live workload.
    /// Copy and audio-only routes never call this method, so an unavailable or
    /// saturated video encoder cannot block source-compatible playback.
    pub(crate) async fn admit_live_tv(
        &self,
        source_height: u16,
        codec: &str,
        hdr: Option<&str>,
        target_height: u16,
        max_wait: Duration,
    ) -> Result<LiveAdmission, String> {
        let preferred = self.encoder().await;
        let work = Workload {
            source_height: i64::from(source_height),
            codec,
            hdr,
            target_height: i64::from(target_height),
        };
        // Live TV plans its own command and is never served from the movie
        // cache, so there is no resolved movie plan to read an estimate from.
        self.admit_live(preferred, None, work, max_wait, Priority::Live)
            .await
    }

    #[allow(clippy::too_many_arguments)] // one stream's worth of knobs
    pub(super) async fn start_with_audio_offset(
        &self,
        file_id: i64,
        target_height: i64,
        start_seconds: f64,
        audio_override: Option<i64>,
        subtitle_override: Option<i64>,
        audio_offset_ms: i64,
        user_name: &str,
        supersession_user: &str,
        recovery: Option<&SessionRecoveryIdentity>,
        replacement_deadline: Option<tokio::time::Instant>,
        takeover: Option<SessionTakeoverStart>,
        playback_id: &str,
        automatic: bool,
        hdr10: bool,
        candidate_context: Option<&super::CandidateExecutionContext>,
        priority: Priority,
        audio_claim: Option<&plurx_core::playback::audio::AudioClaim>,
        retained_audio: Option<&plurx_core::playback::audio::AudioDelivery>,
        sdr_master_codecs: Option<bool>,
    ) -> Result<StartInfo, String> {
        let rate_control = self.rate_control_snapshot();
        // Cluster replacements are provisional until their durable pointer CAS
        // wins. Killing the old process here would turn an admission/Store
        // failure into an avoidable playback outage. Takeovers likewise
        // continue an existing incarnation and supersede nothing.
        if replacement_deadline.is_none() && takeover.is_none() {
            self.reap_superseded_before(None, supersession_user, playback_id)
                .await?;
        }

        // The file, its probe and every setting this start reads, in one
        // statement (D4). Nothing below reads settings from the Store.
        let inputs = self
            .rolling_start_inputs(file_id, candidate_context)
            .await?;
        let preference = inputs.encoder_preference();
        let content_aware = inputs.content_aware_encoding();
        let (max_hw_sessions, sw_budget) = inputs.capacity_limits();
        let RollingStartInputs {
            mut file,
            probe_json,
            settings,
            item_title,
            capacity: _,
        } = inputs;
        let retained_budget = Self::rolling_retained_budget_from(&settings);
        let pacing = Self::pacing_from_settings(&settings, false).await;

        file.audio_offset_ms = if file.audio_streams.is_empty() {
            0
        } else {
            audio_offset_ms.clamp(-15_000, 15_000)
        };
        #[cfg(windows)]
        let source_handle = bind_windows_session_source(&mut file).await?;
        let item_title = item_title.unwrap_or_else(|| "(unknown)".to_owned());

        // Which tracks this session carries. Before the hardware slot, because
        // the cache lookup below needs the answer — two sessions differing only
        // in audio track are different bytes, and a cache that ignored that
        // would serve the wrong language.
        // The grade with no burn. Read before the tracks, because whether the
        // implicit pick may be a burn at all depends on what this session
        // would otherwise be delivering.
        let base_grade = self
            .grade_preview_with_preference(&file, hdr10, target_height, None, &preference)
            .await;
        let Tracks {
            audio_index,
            subtitle_burn,
        } = Self::select_tracks_with_prefs(
            &file,
            audio_override,
            subtitle_override,
            &Self::lang_prefs_from(&settings),
            base_grade == OutputGrade::Hdr10,
        );

        // The cache, before anything is claimed. A hit needs no encoder, no
        // hardware slot and no place in the queue — the work is already done,
        // and making a viewer wait behind a busy GPU for bytes that exist is
        // the one thing this cache exists to prevent.
        // Resolved together, once, before the cache lookup: the grade and
        // encoder both change the bytes and therefore the recipe identity.
        let (mut encoder, grade) = self
            .encoder_and_grade_for_with_preference(
                &file,
                hdr10,
                target_height,
                subtitle_burn.is_some(),
                &preference,
            )
            .await?;
        let mut opts = self.live_lookup_options(
            rate_control,
            encoder,
            &file,
            target_height,
            start_seconds,
            audio_index,
            subtitle_burn.clone(),
            None,
            grade,
        );
        if let Some(context) = candidate_context {
            opts.normalized_geometry = context.normalized_geometry;
            if let Some(profile) = context.profile {
                opts.auto_quality_rate_profile = Some(profile);
                opts.video_bitrate_kbps = profile.video_bitrate_kbps();
                opts.effective_rate_control = plurx_core::transcode::EffectiveRateControl::Vbr;
            }
        }
        opts = self.rolling_start_audio_options(&file, opts, audio_claim, retained_audio);
        let audio_delivery = opts.audio.clone();
        if let Some(takeover) = takeover.as_ref() {
            opts.start_number = takeover.media_sequence;
        }
        let plan =
            self.resolve_movie_plan_from_probe(&file, &opts, encoder, probe_json.as_deref())?;
        opts.pipeline = plan.options().pipeline;
        opts.strict_dolby = plan.options().strict_dolby.clone();
        // Freeze the master shape from the same settings read as the plan.
        let sdr_master_codecs = sdr_master_codecs.unwrap_or_else(|| {
            plurx_core::store::stored_switch(
                settings
                    .get(keys::PLAYBACK_SDR_MASTER_CODECS)
                    .map(String::as_str),
                false,
            )
        });
        if takeover.is_none() {
            if let Some(info) = self
                .serve_cached(
                    &file,
                    &opts,
                    &plan,
                    &item_title,
                    SessionOwner {
                        user_name,
                        supersession_user,
                        playback_id,
                        automatic,
                        sdr_master_codecs,
                    },
                )
                .await
            {
                return Ok(info);
            }
        }
        // The retained lookup's captured production, kept for the producer
        // below when its logical graph is byte-identical: one source fence
        // and engine attestation per start, not one per consumer.
        let mut retained_lookup: Option<(
            Vec<u8>,
            Arc<crate::rolling_provenance::RollingProduction>,
        )> = None;
        // A complete retained answer needs no producer resources. Derive the
        // exact preferred graph's thread weight from the same admission policy,
        // without taking a permit or installing a waiter. A later demotion is
        // a different graph and deliberately cannot broaden this lookup.
        if start_seconds == 0.0
            && takeover.is_none()
            && opts.subtitle_burn.is_none()
            && retained_budget.is_some()
        {
            let work = Workload::of(&file, target_height);
            let estimate = TranscodeResourceEstimate::of(&plan, &work);
            let mut retained_opts = opts.clone();
            retained_opts.software_threads = if encoder == Encoder::Software {
                Some(work.software_threads() as u32)
            } else if estimate.hardware_slot && estimate.cpu_threads > 0 {
                Some(estimate.cpu_threads as u32)
            } else {
                None
            };
            // The lookup is optional. A plan it cannot resolve is counted
            // and skipped — a miss starts a producer — never a failed Play.
            let retained_plan = match self.resolve_movie_plan_from_probe(
                &file,
                &retained_opts,
                encoder,
                probe_json.as_deref(),
            ) {
                Ok(plan) => Some(plan),
                Err(error) => {
                    tracing::info!(
                        target: "plurxd::transcode",
                        file = file_id,
                        %error,
                        "skipping the retained-output lookup: its plan did not resolve"
                    );
                    crate::telemetry::record_rolling_retained_lookup_skipped("plan_error");
                    None
                }
            };
            if let Some((retained_plan, Ok(execution))) = retained_plan.map(|plan| {
                retained_opts.pipeline = plan.options().pipeline;
                retained_opts.strict_dolby = plan.options().strict_dolby.clone();
                let execution = TranscodeExecution::from_options(
                    &file,
                    &retained_opts,
                    pacing,
                    "retained-output",
                );
                (plan, execution)
            }) {
                let observation = DiagnosticObservation::for_plan(
                    &retained_plan,
                    &self.measured_decoders,
                    self.automatic_decoder_recovery_enabled(),
                );
                let execution =
                    execution.observing_qualified_grammar(observation.qualified_logging());
                let logical = serde_json::to_vec(&serde_json::json!({
                    "file": &file,
                    "kind": SessionKind::Transcode { height: target_height },
                    "audio": &retained_opts.audio,
                    "plan": retained_plan.plan_digest(),
                    "args": transcode::hls_args(&retained_plan, &execution),
                }))
                .ok();
                if let Some(logical) = logical {
                    if let Some(production) = crate::rolling_provenance::RollingProduction::capture(
                        &file,
                        &logical,
                        false,
                        &producer_ffmpeg_bin(),
                    )
                    .await
                    .filter(|production| production.executable_matches_plan(&retained_plan))
                    {
                        let kind = SessionKind::Transcode {
                            height: target_height,
                        };
                        let frozen = FrozenHlsPresentation::from_contract(
                            file.clone(),
                            HlsContext {
                                codec_facts: Some(
                                    FrozenHlsCodecFacts::encoded(&retained_plan)
                                        .with_sdr_master_codecs(sdr_master_codecs),
                                ),
                                bandwidth: None,
                                file_id,
                                start_seconds,
                                media_origin_seconds: start_seconds,
                                codecs: audio_delivery_hls_codecs(
                                    transcoded_hls_codecs_for_plan(&retained_plan),
                                    retained_opts.audio.as_ref(),
                                ),
                                supplemental_codecs: None,
                                frame_rate: frozen_video_frame_rate(probe_json.as_deref()),
                            },
                            &kind,
                            Some(retained_plan.output_contract()),
                        );
                        if let Some(info) = self
                            .attach_rolling_retained(
                                &production,
                                frozen,
                                kind,
                                Some(retained_plan.output_contract()),
                                retained_opts.audio.clone(),
                                retained_opts.pipeline.output_grade(),
                                target_height,
                                user_name,
                                supersession_user,
                                playback_id,
                                &item_title,
                                automatic,
                            )
                            .await
                        {
                            return Ok(info);
                        }
                        retained_lookup = Some((logical, production));
                    }
                }
            }
        }
        // A measured speculative producer has a distinct recipe. Try only
        // its completed artifact: neither analysis nor a measured live encode
        // belongs on Play. A miss leaves the original plan/options untouched.
        // Explicit candidate bindings and takeovers already name their exact
        // recipe and must never be redirected to a different one.
        if takeover.is_none() && candidate_context.is_none() {
            if let Some(mut cached_options) = self
                .measured_content_cache_options_from(
                    &file,
                    &opts,
                    encoder,
                    content_aware,
                    probe_json.as_deref(),
                )
                .await
            {
                // Resolution reads stored decoder facts; it does not probe or
                // score media. serve_cached retains all manifest/source checks,
                // shared generation pins and normal session ownership.
                if let Ok(cached_plan) = self.resolve_movie_plan_from_probe(
                    &file,
                    &cached_options,
                    encoder,
                    probe_json.as_deref(),
                ) {
                    cached_options.pipeline = cached_plan.options().pipeline;
                    cached_options.strict_dolby = cached_plan.options().strict_dolby.clone();
                    if let Some(info) = self
                        .serve_cached(
                            &file,
                            &cached_options,
                            &cached_plan,
                            &item_title,
                            SessionOwner {
                                user_name,
                                supersession_user,
                                playback_id,
                                automatic,
                                sdr_master_codecs,
                            },
                        )
                        .await
                    {
                        return Ok(info);
                    }
                }
            }
        }
        // Can this ffmpeg build actually burn? Asked here — after the cache
        // lookup, which needs no ffmpeg at all, and before any slot, process
        // or session exists — because the alternative is spawning a graph that
        // dies at once with `No such filter` and reporting that to the viewer
        // as an anonymous stream failure a minute later. A build without the
        // filter will never grow one mid-session; refusing at the door names
        // the reason while the request is still in front of the person who
        // made it.
        if let Some(burn) = subtitle_burn.as_ref() {
            if let Some(reason) = crate::pipeprobe::burn_filters().await.refusal(burn.bitmap) {
                tracing::warn!(
                    target: "plurxd::transcode",
                    file = file_id,
                    subtitle = burn.subtitle_index,
                    bitmap = burn.bitmap,
                    "refusing a subtitle burn this ffmpeg build cannot run: {reason}"
                );
                return Err(unsupported_build_error(reason));
            }
        }
        let subtitle_handle = self
            .ensure_text_subtitle(
                &file,
                subtitle_burn.as_ref(),
                crate::process_control::ChildWork::realtime("text subtitle for a session start"),
            )
            .await?;

        // Claim a hardware slot before spawning anything. An iGPU has one
        // video-processing block, and a third 4K session on it does not run a
        // third as fast — it drags the other two under realtime with it, so one
        // person pressing play becomes three people stuttering.
        //
        // The wait is short and deliberate: a slot usually frees within seconds
        // (a superseded session, a closed tab), and someone who has pressed
        // play will forgive five seconds far sooner than a hang.
        let work = Workload::of(&file, target_height);
        let admission = self
            .admit_live_with(
                encoder,
                Some(&plan),
                work,
                if priority == Priority::Speculative {
                    Duration::ZERO
                } else {
                    QUEUE_WAIT
                },
                priority,
                sw_budget,
                max_hw_sessions,
            )
            .await?;
        encoder = admission.encoder;
        if grade == OutputGrade::Hdr10
            && target_height == HDR10_4K_HEIGHT
            && encoder != Encoder::Qsv
        {
            return Err(capacity_error(
                "the proved 4K HDR10 QuickSync slot is busy; retry at 1080p",
            ));
        }
        let hw_slot = admission.hw_slot;
        let sw_permit = admission.sw_permit;
        let session_id = takeover
            .as_ref()
            .map(|takeover| takeover.provisional_session_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        // The public UUID is a bearer capability. Keep it out of ffmpeg's
        // argv and stderr entirely by giving the scratch directory an
        // independent, process-private name.
        let dir = self.work_dir.join(format!("w-{}", uuid::Uuid::new_v4()));
        #[cfg(windows)]
        let dir = std::path::absolute(dir)
            .map_err(|error| format!("resolving Windows session directory: {error}"))?;
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| format!("creating session dir: {e}"))?;
        #[cfg(windows)]
        let output_handle = plurx_core::fs_secure::SecureDirectory::open(&dir)
            .await
            .map_err(|error| format!("holding Windows session directory: {error}"))?;
        let mut start_settlement = PrepublicationStartSettlement::new(dir.clone());

        // Admission may have moved this session to software, which changes the
        // pipeline it is entitled to — so the options are rebuilt rather than
        // patched. Same builder, so the two cannot describe different sessions.
        // The software permit's thread budget rides in the same rebuild: what
        // admission reserved is exactly what x264 is told to spend.
        let mut opts = self.live_lookup_options(
            rate_control,
            encoder,
            &file,
            target_height,
            start_seconds,
            audio_index,
            subtitle_burn,
            sw_permit.as_ref().map(|p| p.threads() as u32),
            grade,
        );
        if let Some(audio) = &audio_delivery {
            opts.set_audio_delivery(audio.clone());
        }
        if let Some(context) = candidate_context {
            opts.normalized_geometry = context.normalized_geometry;
            if let Some(profile) = context.profile {
                opts.auto_quality_rate_profile = Some(profile);
                opts.video_bitrate_kbps = profile.video_bitrate_kbps();
                opts.effective_rate_control = plurx_core::transcode::EffectiveRateControl::Vbr;
            }
        }
        if let Some(takeover) = takeover.as_ref() {
            opts.start_number = takeover.media_sequence;
        }
        #[cfg(windows)]
        if let Some(subtitle) = subtitle_handle.as_ref() {
            opts.subtitle_file = Some(
                plurx_core::fs_secure::std_file_path(subtitle)
                    .map_err(|error| format!("resolving held Windows subtitle path: {error}"))?,
            );
        }
        // Admission is allowed to demote the encoder. That is a different
        // byte-producing decision, so it receives a fresh complete plan;
        // neither the old encoder nor its decode surface is patched in place.
        let plan =
            self.resolve_movie_plan_from_probe(&file, &opts, encoder, probe_json.as_deref())?;
        // Retries, descriptors and attribution carry the graph resolution
        // selected, including its verdict-aware software downgrade.
        opts.pipeline = plan.options().pipeline;
        opts.strict_dolby = plan.options().strict_dolby.clone();
        let macos_executable = capture_macos_plan_executable(&plan, &producer_ffmpeg_bin()).await?;
        // Every object FFmpeg's muxer writes for this session passes a
        // scratch grant before it reaches the disk, so the session starts on
        // its startup allowance and grows, instead of reserving the whole
        // per-session ceiling that an unbounded writer had to.
        let output_bitrate = transcode_output_bitrate(&opts);
        let scratch_envelope = rolling_scratch_envelope(output_bitrate, 1.0);
        let scratch_reservation = self.reserve_rolling_scratch_with(
            RollingScratchSizing::Startup(rolling_startup_bytes(output_bitrate, 1.0)),
            self.ahead_limits_for_start(&settings),
        )?;
        let upload = self.bind_scratch_upload(&dir, &scratch_reservation, scratch_envelope)?;
        let automatic_decoder_recovery = self.automatic_decoder_recovery_enabled();
        let observation = DiagnosticObservation::for_plan(
            &plan,
            &self.measured_decoders,
            automatic_decoder_recovery,
        );
        let mut execution =
            TranscodeExecution::from_options(&file, &opts, pacing, &upload.base_url(0))
                .map_err(|error| error.to_string())?
                .observing_qualified_grammar(observation.qualified_logging());
        let mut canonical_execution = execution.clone();
        canonical_execution.out_dir = "retained-output".to_owned();
        let logical = serde_json::to_vec(&serde_json::json!({
            "file": &file,
            "kind": SessionKind::Transcode { height: target_height },
            "audio": &opts.audio,
            "plan": plan.plan_digest(),
            "args": transcode::hls_args(&plan, &canonical_execution),
        }))
        .ok();
        let rolling_provenance = if start_seconds == 0.0
            && takeover.is_none()
            && opts.subtitle_burn.is_none()
            && retained_budget.is_some()
        {
            match logical {
                Some(logical) => match retained_lookup.take() {
                    Some((looked_up, production))
                        if looked_up == logical && production.unbound() =>
                    {
                        Some(production)
                    }
                    _ => {
                        crate::rolling_provenance::RollingProduction::capture(
                            &file,
                            &logical,
                            false,
                            &producer_ffmpeg_bin(),
                        )
                        .await
                    }
                },
                None => None,
            }
        } else {
            None
        };
        let rolling_provenance =
            rolling_provenance.filter(|production| production.executable_matches_plan(&plan));
        if let Some(provenance) = &rolling_provenance {
            execution.source_path = provenance.input_path();
        }
        let mut execution_file = file.clone();
        if let Some(provenance) = &rolling_provenance {
            execution_file.path = provenance.input_path();
        }
        let args = transcode::hls_args(&plan, &execution);
        if plan.input_is_hdr()
            && plan.options().pipeline == Pipeline::Cpu
            && plan.options().tone_map == ToneMap::Zscale
        {
            crate::telemetry::record_tone_map_peak(plan.options().tone_map_peak_source);
        }
        // Log the exact command — the single most useful diagnostic. It reveals
        // the decode/filter/encode pipeline actually used (e.g. whether heavy
        // HEVC is being hardware-decoded), and confirms which build is running.
        //
        // And, when this session did not get the graph the node proved, why
        // not. Without it `pipeline=cpu` on a 4K HDR title reads as the GPU
        // path being broken, when the usual answer is that the source is Dolby
        // Vision and the CPU chain is the *correct* choice.
        let declined = Pipeline::declined(
            self.pipeline,
            encoder,
            transcode::routing_hdr(&file),
            transcode::heavy_source(&file),
            opts.subtitle_burn.as_ref().is_some_and(|b| !b.bitmap),
        );
        tracing::info!(
            target: "plurxd::transcode",
            session = %session_log_id(&session_id), encoder = encoder.label(), pipeline = opts.pipeline.name(),
            proven = self.pipeline.name(), hdr = file.hdr.as_deref().unwrap_or("sdr"),
            peak_nits = plan.options().tone_map_peak_nits,
            peak_source = plan.options().tone_map_peak_source.name(),
            declined = declined.unwrap_or(""),
            deinterlace = plan.deinterlace().name(),
            build = crate::version::BUILD,
            "{}", ffmpeg_args_log_message("transcode ffmpeg args", &args, &session_id)
        );
        let session_kind = SessionKind::Transcode {
            height: target_height,
        };
        let hls_codecs =
            audio_delivery_hls_codecs(transcoded_hls_codecs_for_plan(&plan), opts.audio.as_ref());
        let frozen_presentation = FrozenHlsPresentation::from_contract(
            file.clone(),
            HlsContext {
                codec_facts: Some(
                    FrozenHlsCodecFacts::encoded(&plan).with_sdr_master_codecs(sdr_master_codecs),
                ),
                bandwidth: None,
                file_id,
                start_seconds,
                media_origin_seconds: start_seconds,
                codecs: hls_codecs.clone(),
                supplemental_codecs: None,
                frame_rate: frozen_video_frame_rate(probe_json.as_deref()),
            },
            &session_kind,
            Some(plan.output_contract()),
        );
        let presentation_contract_fingerprint = frozen_presentation.contract_fingerprint.clone();
        let rolling_collection = self
            .begin_rolling_retention(
                rolling_provenance.as_ref(),
                file.duration_ms,
                file.size,
                retained_budget,
                &dir,
                file.id,
                &item_title,
            )
            .await;
        if let Some(collection) = &rolling_collection {
            upload.bind_retained(Arc::clone(collection));
        }
        let retry = if !needs_startup_transcode_retry(&plan) {
            None
        } else {
            // One value for both recipes: they are frozen together, and a
            // budget that moved between them would put the pair on two
            // different pictures of the node. It is the one this start was
            // admitted under, from its planning snapshot.
            let software_budget = sw_budget;
            let prepared_plan = if encoder == Encoder::Software {
                // Keep the encoder, admitted CPU reservation and presentation;
                // only the hardware decoder moves. Resolve under an explicit
                // restriction so startup cannot reselect the failed backend.
                self.resolve_restricted_movie_plan(
                    &file,
                    &opts,
                    encoder,
                    &plurx_core::transcode::AttemptRestrictions::requiring(
                        plurx_core::transcode::DecodeBackend::Software,
                    ),
                )
                .await
                .and_then(|retry_plan| {
                    PrepublicationTranscodeRetry::prepare_decode_restricted(
                        &plan,
                        &retry_plan,
                        &opts,
                        encoder,
                    )?
                    .ok_or_else(|| {
                        "hardware decode has no distinct software startup retry".to_owned()
                    })
                    .map(|prepared| (prepared, retry_plan))
                })
            } else {
                match PrepublicationTranscodeRetry::prepare(
                    &file,
                    &opts,
                    encoder,
                    rate_control.effective_for(Encoder::Software),
                ) {
                    Ok(prepared) => self
                        .resolve_movie_processing_retry(
                            &file,
                            &prepared.opts,
                            prepared.encoder,
                            opts.pipeline,
                        )
                        .await
                        .map(|retry_plan| (prepared, retry_plan)),
                    Err(error) => Err(error),
                }
            };
            let retry = prepared_plan.and_then(|(prepared, retry_plan)| {
                PrepublicationTranscodeRetry::build(
                    &execution_file,
                    prepared,
                    &retry_plan,
                    pacing,
                    &upload.base_url(1),
                    &presentation_contract_fingerprint,
                    self.admissions.software_pool(),
                    software_budget,
                    self.runtime_cache.clone(),
                    &self.measured_decoders,
                    automatic_decoder_recovery,
                    if encoder == Encoder::Software {
                        "startup-software-decode"
                    } else {
                        "one-step-color-safe"
                    },
                )
            });
            match retry {
                Ok(retry) => {
                    // The software-decode alternate, frozen beside the
                    // colour-safe one and by the same rule: resolved now, from
                    // this plan, so that nothing re-runs policy in the middle
                    // of a recovery.
                    //
                    // A failure to build it is *not* a failure to start the
                    // session. The colour-safe retry is what this session has
                    // always had and it is unaffected; an absent alternate
                    // costs a decode fault its recovery and nothing else, and
                    // refusing to play a title because its hypothetical
                    // second recipe would not resolve is a worse trade than
                    // any recovery it buys.
                    let alternate = match self
                        .resolve_restricted_movie_plan(
                            &file,
                            &opts,
                            encoder,
                            &plurx_core::transcode::AttemptRestrictions::requiring(
                                plurx_core::transcode::DecodeBackend::Software,
                            ),
                        )
                        .await
                    {
                        Ok(alternate_plan) => {
                            PrepublicationTranscodeRetry::prepare_decode_restricted(
                                &plan,
                                &alternate_plan,
                                &opts,
                                encoder,
                            )
                            .and_then(|prepared| {
                                prepared
                                    .map(|prepared| {
                                        PrepublicationTranscodeRetry::build(
                                            &execution_file,
                                            prepared,
                                            &alternate_plan,
                                            pacing,
                                            &upload.base_url(2),
                                            &presentation_contract_fingerprint,
                                            self.admissions.software_pool(),
                                            software_budget,
                                            self.runtime_cache.clone(),
                                            &self.measured_decoders,
                                            automatic_decoder_recovery,
                                            "software-decode",
                                        )
                                    })
                                    .transpose()
                            })
                            .unwrap_or_else(|error| {
                                tracing::warn!(
                                    target: "plurxd::transcode",
                                    session = %session_log_id(&session_id),
                                    "no software-decode alternate for this session: {error}"
                                );
                                None
                            })
                        }
                        Err(error) => {
                            tracing::warn!(
                                target: "plurxd::transcode",
                                session = %session_log_id(&session_id),
                                "software-decode alternate did not resolve: {error}"
                            );
                            None
                        }
                    };
                    // The durable budget, consulted once, here — and only to
                    // withhold.
                    //
                    // The actor chooses between the two frozen recipes, and it
                    // cannot ask the store: it is synchronous and holds no
                    // handle. So "has this playback already spent its one
                    // automatic recovery" is answered where a store and the
                    // identity are both in hand, by not giving the actor an
                    // alternate to name. A reopen of a playback that already
                    // recovered then reaches M5c1's permanent verdict on its
                    // first qualified fault — the honest terminal answer this
                    // effort exists to produce — instead of being told to
                    // retry and then failing at install.
                    //
                    // A row in *any* state withholds. `Reserved` is a recovery
                    // in flight or one abandoned by a cancelled executor, and
                    // both terminal states are a budget already spent.
                    let alternate = match (
                        alternate,
                        recovery.and_then(|identity| {
                            crate::playback_control::ProducerRecoveryLedger::new(
                                Arc::clone(&self.store),
                                identity.principal.clone(),
                                playback_id,
                                &identity.recovery_epoch,
                                // The generation being started is the one that will
                                // fail, which is what a reservation records.
                                &identity.incarnation_id,
                            )
                        }),
                    ) {
                        (Some(alternate), Some(ledger)) => match ledger.existing().await {
                            Ok(None) => Some(alternate.with_recovery(DecodeRecoveryReservation {
                                ledger,
                                failed_plan_digest: plan.plan_digest(),
                            })),
                            Ok(Some(existing)) => {
                                tracing::info!(
                                    target: "plurxd::transcode",
                                    session = %session_log_id(&session_id),
                                    state = existing.state.as_str(),
                                    "this playback has already used its recovery budget; \
                                     no software-decode alternate for this session"
                                );
                                None
                            }
                            Err(error) => {
                                // Fail closed. An unreadable budget is not an
                                // unspent one, and the cost of being wrong the
                                // other way is a second automatic recovery on
                                // a source that has already proven it does not
                                // decode.
                                tracing::warn!(
                                    target: "plurxd::transcode",
                                    session = %session_log_id(&session_id),
                                    %error,
                                    "the recovery budget could not be read; withholding the \
                                     software-decode alternate"
                                );
                                None
                            }
                        },
                        // No durable identity: a legacy process-local start, a
                        // relayed worker start, or a session predating the
                        // epoch column. Those keep the in-process one-shot they
                        // have always had — one automatic recovery per session
                        // — because withholding recovery from them would be a
                        // regression rather than a fix, and the reopen loop
                        // they can still reach is the loop that existed before
                        // this effort, not one it introduced.
                        (alternate, None) => alternate,
                        (None, _) => None,
                    };
                    Some(retry.with_decode_alternate(alternate))
                }
                Err(error) => {
                    let _ = tokio::fs::remove_dir_all(&dir).await;
                    start_settlement.disarm();
                    return Err(error);
                }
            }
        };
        let expected_remaining_ms =
            file.duration_ms
                .filter(|duration_ms| *duration_ms > 0)
                .map(|duration_ms| {
                    duration_ms
                        .saturating_sub((start_seconds * 1_000.0).round() as i64)
                        .max(0)
                });
        let completion_tolerance_ms = (transcode::SEGMENT_SECONDS as i64).saturating_mul(1_000);
        let policy = if let Some(retry) = retry.as_ref() {
            crate::playback_control::InitialProducerPolicy::hardware_with_startup(
                presentation_contract_fingerprint,
                PROGRESS_STALL,
                retry.actor_recipe.clone(),
                transcode_startup_kind(&plan),
            )
            // The actor decides between the two; it can only do that if it can
            // see both. The executor holds the material either way, so an
            // alternate the policy did not name is one the actor will never
            // ask for and the executor will never install.
            .with_decode_alternate(
                retry
                    .decode_alternate
                    .as_deref()
                    .map(|alternate| alternate.actor_recipe.clone()),
            )
        } else {
            crate::playback_control::InitialProducerPolicy::software(
                presentation_contract_fingerprint,
                PROGRESS_STALL,
            )
        }
        .with_completion_expectation(expected_remaining_ms, completion_tolerance_ms);
        let progress = Arc::new(Progress::new());
        let (control, mut executor_registration) =
            crate::playback_control::RollingControlHandle::spawn_prepublication_transcode(
                "session-start",
            );

        let session = Arc::new(Session {
            dir: dir.clone(),
            response_incarnation: uuid::Uuid::new_v4(),
            frozen_presentation: Some(frozen_presentation),
            rolling_provenance,
            rolling_collection,
            rolling_artifact: None,
            copy_output_measurement: std::sync::Mutex::new(None),
            copy_reader_task: Mutex::new(CopyReaderTask::default()),
            #[cfg(target_os = "linux")]
            core_copy: Mutex::new(None),
            #[cfg(target_os = "linux")]
            core_copy_close: Mutex::new(None),
            #[cfg(target_os = "linux")]
            core_copy_quota: std::sync::Mutex::new(None),
            #[cfg(target_os = "linux")]
            core_copy_refresh_gate: Mutex::new(()),
            actor_managed_response_publication: true,
            actor_managed_prepublication_process: true,
            actor_prepublication_producer: Arc::new(AtomicBool::new(true)),
            response_publication_transition: Mutex::new(()),
            first_media_handoff_applied: AtomicBool::new(false),
            first_media_handoff_notify: tokio::sync::Notify::new(),
            prepublication_cleanup_active: AtomicBool::new(false),
            retirement_cleanup_started: AtomicBool::new(false),
            retirement_cleanup_finished: AtomicBool::new(false),
            retirement_settlement: std::sync::Mutex::new(None),
            scratch_cleanup_started: AtomicBool::new(false),
            retirement_context: Some(self.rolling_retirement_context()),
            cache_integrity_cleanup_started: AtomicBool::new(false),
            child: Mutex::new(None),
            child_transition: Mutex::new(()),
            replacing_child: AtomicBool::new(false),
            terminal_response_pending: Arc::new(AtomicBool::new(false)),
            terminal_control: std::sync::Mutex::new(None),
            hooks: crate::seam_hooks::HookSlot::new(&NoopSessionHooks),
            cached: false,
            _cache_reader: None,
            subtitle_handle,
            #[cfg(windows)]
            source_handle: Some(source_handle),
            #[cfg(windows)]
            output_handle: Some(output_handle),
            cache_manifest: None,
            cache_location: None,
            control,
            publication: Mutex::new(RollingPublicationClock::default()),
            publication_worker_started: AtomicBool::new(false),
            flow_worker_started: AtomicBool::new(false),
            file_id,
            item_id: file.item_id,
            item_title,
            user_name: user_name.to_owned(),
            supersession_user: supersession_user.to_owned(),
            playback_id: playback_id.to_owned(),
            recovery: recovery.cloned(),
            automatic,
            kind: session_kind,
            audio_delivery: opts.audio.clone(),
            method: crate::delivery::Method::Transcode,
            start_seconds,
            // A transcode seeks accurately, so its media begins exactly where
            // it was asked to: no probe, no discrepancy to resolve.
            media_origin_seconds: start_seconds,
            grade: opts.pipeline.output_grade(),
            target_height,
            tone_map_peak_nits: (plan.options().tone_map == ToneMap::Zscale)
                .then_some(plan.options().tone_map_peak_nits),
            tone_map_peak_source: (plan.options().tone_map == ToneMap::Zscale)
                .then_some(plan.options().tone_map_peak_source.name()),
            encoder_label: Mutex::new(encoder.label()),
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            failed: Arc::new(AtomicBool::new(false)),
            failure: std::sync::Mutex::new(None),
            playlist_published: AtomicBool::new(false),
            high_segment: Arc::new(AtomicI64::new(-1)),
            compatibility_attempt: Arc::new(std::sync::Mutex::new(0)),
            fetched_end_ms: Arc::new(AtomicI64::new(0)),
            segments: Mutex::new(SegmentIndex::default()),
            ahead_bytes: AtomicI64::new(0),
            live_bytes: Arc::new(AtomicI64::new(0)),
            scratch: Some(scratch_reservation.bound_to(&session_id, 0)),
            retired_release: Arc::new(RetiredRelease::new()),
            scratch_envelope,
            upload: Some(upload),
            retention_garbage_bytes: Arc::new(AtomicI64::new(0)),
            retention_cleanup_queue: Arc::new(std::sync::Mutex::new(Vec::new())),
            retention_cleanup_active: Arc::new(AtomicBool::new(false)),
            progress: Arc::clone(&progress),
            class: std::sync::Mutex::new(work.class(if encoder == Encoder::Software {
                crate::admission::SOFTWARE
            } else {
                encoder.label()
            })),
            hw_slot: std::sync::Mutex::new(hw_slot),
            sw_permit: std::sync::Mutex::new(sw_permit),
            sw_delta_permit: std::sync::Mutex::new(None),
            delivery: Meter::for_method(crate::delivery::Method::Transcode.metric_label()),
            http_waits: HttpWaitLedger::default(),
            readrate: pacing
                .readrate
                .unwrap_or(if pacing.legacy_re { 1.0 } else { 0.0 }),
            suspended: AtomicBool::new(false),
            suspended_at: Mutex::new(None),
            suspend_count: AtomicU64::new(0),
            takeover,
            first_slide_logged: AtomicBool::new(false),
        });
        start_settlement.attach(&session);
        if let Err(reason) = executor_registration.register().await {
            fail_prepublication_transaction(
                &session,
                format!("rolling control actor rejected executor registration: {reason:?}"),
            )
            .await;
            start_settlement.disarm();
            return Err(format!(
                "rolling control actor rejected executor registration: {reason:?}"
            ));
        }
        let executor_retry = retry
            .clone()
            .map(|retry| PrepublicationRetry::Transcode(Box::new(retry)));
        let (executor_activation, manager_publication) = tokio::sync::oneshot::channel();
        spawn_prepublication_executor_owner(
            &session,
            executor_registration,
            manager_publication,
            executor_retry,
            session_id.clone(),
        );
        let generation = match session.control.begin_initial_producer_attempt(policy).await {
            Ok(generation) => generation,
            Err(reason) => {
                fail_prepublication_transaction(
                    &session,
                    format!(
                        "rolling control actor rejected the initial producer policy: {reason:?}"
                    ),
                )
                .await;
                start_settlement.disarm();
                return Err(format!(
                    "rolling control actor rejected the initial producer policy: {reason:?}"
                ));
            }
        };
        if let Some(provenance) = &session.rolling_provenance {
            provenance.bind_initial_attempt(generation);
        }
        session.bind_retry_compatibility_attempt(generation).await;
        if let Some(executable) = &macos_executable {
            if !executable.is_current().await {
                let reason = "macos_processing_implementation_changed: captured encoder changed before launch".to_owned();
                fail_prepublication_transaction(&session, reason.clone()).await;
                start_settlement.disarm();
                return Err(reason);
            }
        }
        if let Err(reason) = session
            .spawn_and_install_prepublication_child(generation, || {
                spawn_ffmpeg_at_with_env(
                    macos_executable
                        .as_ref()
                        .map(|executable| executable.path.as_path())
                        .or_else(|| {
                            session
                                .rolling_provenance
                                .as_ref()
                                .map(|proof| proof.executable_path())
                        })
                        .unwrap_or(std::path::Path::new(&producer_ffmpeg_bin())),
                    &args,
                    crate::process_control::ChildWork::realtime("playback transcode"),
                    encoder.label(),
                    &session_id,
                    FfmpegProgressObserver::rolling(
                        Arc::clone(&progress),
                        generation,
                        session.control.clone(),
                    ),
                    &self.runtime_cache,
                    {
                        #[cfg(unix)]
                        let descriptors = FfmpegDescriptors::from_raw_fds(
                            session
                                .rolling_provenance
                                .as_ref()
                                .map(|proof| proof.source_fd()),
                            None,
                            session
                                .subtitle_handle
                                .as_ref()
                                .map(std::os::fd::AsRawFd::as_raw_fd),
                            false,
                        );
                        #[cfg(windows)]
                        let descriptors = windows_session_descriptors(&session)?;
                        descriptors
                    },
                    observation.clone(),
                    &macos_executable
                        .as_ref()
                        .map(|executable| executable.processing_child_env())
                        .unwrap_or_default(),
                )
            })
            .await
        {
            fail_prepublication_transaction(
                &session,
                format!("initial transcode producer could not be installed: {reason}"),
            )
            .await;
            start_settlement.disarm();
            return Err(reason);
        }
        tracing::info!(
            target: "plurxd::transcode",
            session = %session_log_id(&session_id), file_id, target_height, start_seconds,
            encoder = encoder.label(), "started actor-owned prepublication transcode session"
        );
        if let Err(reason) = self
            .register_session(&session_id, Arc::clone(&session), generation)
            .await
        {
            start_settlement.disarm();
            return Err(format!("rolling session registration rejected: {reason:?}"));
        }
        if executor_activation.send(()).is_err() {
            let reason = "prepublication executor ended before manager publication".to_owned();
            fail_prepublication_transaction(&session, reason.clone()).await;
            start_settlement.disarm();
            return Err(reason);
        }
        start_settlement.settle();
        self.emit_session_event(
            &session_id,
            &session,
            "session_start",
            SessionEventFields {
                extra: Some(serde_json::json!({ "cache": "miss" }).to_string()),
                ..SessionEventFields::default()
            },
        )
        .await;
        self.record_codec_qualification_session(
            encoder,
            opts.pipeline.output_grade(),
            Some(opts.pipeline),
        );

        Ok(StartInfo {
            processed_dv_profile: None,
            processed_fel: None,
            degraded_processing: None,
            processing_note: None,
            retained_output: None,
            audio_delivery: opts.audio.clone(),
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            session_id,
            duration_ms: file.duration_ms,
            start_seconds,
            media_origin_seconds: start_seconds,
            target_height,
            kind: SessionKind::Transcode {
                height: target_height,
            },
            encoder: encoder.label(),
            grade: opts.pipeline.output_grade(),
            vod: false,
            control_lease_timeout_ms: crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
        })
    }

    /// Start a **copy-video** HLS session: the source video is repackaged into
    /// HLS (fMP4 segments) untouched, and only the audio is transcoded when the
    /// client can't take it. This is the remux path for players whose `<video>`
    /// won't accept a progressive fragmented MP4 (Safari) but decode HEVC/HDR
    /// natively via HLS — so the original 4K stream is preserved instead of the
    /// error-fallback re-encoding it down to 720p. No hardware/software encoder
    /// ladder (nothing is encoded), just a fail-fast guard.
    #[cfg(test)]
    pub(super) async fn start_copy(
        &self,
        file_id: i64,
        start_seconds: f64,
        audio_override: Option<i64>,
        options: CopySessionOptions,
        user_name: &str,
        playback_id: &str,
    ) -> Result<StartInfo, String> {
        let supersession_user = serde_json::json!(["username", user_name]).to_string();
        // As above: no cluster identity, no budget.
        self.start_copy_with_audio_offset(
            file_id,
            start_seconds,
            audio_override,
            0,
            options,
            user_name,
            &supersession_user,
            None,
            None,
            None,
            playback_id,
            false,
            None,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // one stream's worth of knobs
    pub(super) async fn start_copy_with_audio_offset(
        &self,
        file_id: i64,
        start_seconds: f64,
        audio_override: Option<i64>,
        audio_offset_ms: i64,
        options: CopySessionOptions,
        user_name: &str,
        supersession_user: &str,
        recovery: Option<&SessionRecoveryIdentity>,
        replacement_deadline: Option<tokio::time::Instant>,
        takeover: Option<SessionTakeoverStart>,
        playback_id: &str,
        automatic: bool,
        audio_delivery: Option<&plurx_core::playback::audio::AudioDelivery>,
        sdr_master_codecs: Option<bool>,
    ) -> Result<StartInfo, String> {
        // One Store read for the file, its probe and its settings (D4).
        let RollingStartInputs {
            mut file,
            probe_json: mut stored_probe,
            settings,
            item_title,
            capacity: _,
        } = self.rolling_start_inputs(file_id, None).await?;
        let retained_budget = Self::rolling_retained_budget_from(&settings);
        // This exact one-input rolling profile is admitted through the core
        // P→R graph. Once selected, a planning or admission refusal is a
        // refusal of this start; it cannot fall through to the legacy native
        // probe, raw scratch directory, or unregistered FFmpeg spawn.
        #[cfg(target_os = "linux")]
        let selected_core_copy = self.resource_runtime.is_some()
            && start_seconds == 0.0
            && takeover.is_none()
            && audio_offset_ms == 0
            && copyseg::supports(file.video_codec.as_deref());
        #[cfg(not(target_os = "linux"))]
        let selected_core_copy = false;
        // Same make-before-break rule as the transcode path. A takeover
        // continues an existing incarnation and supersedes nothing.
        if replacement_deadline.is_none() && takeover.is_none() {
            self.reap_superseded_before(None, supersession_user, playback_id)
                .await?;
        }

        #[cfg(target_os = "linux")]
        let selected_planning = if selected_core_copy {
            Some(
                self.collect_selected_copy_planning(
                    &file,
                    stored_probe.as_deref(),
                    audio_override,
                    &options,
                )
                .await?,
            )
        } else {
            None
        };
        #[cfg(target_os = "linux")]
        let probe_json = if let Some(report) = selected_planning.as_ref() {
            Some(report.report.document().to_owned())
        } else {
            crate::hevc_census::probe_json_for_copy_from(
                self.store.as_ref(),
                &file,
                stored_probe.take(),
            )
            .await
            .ok()
            .flatten()
        };
        #[cfg(not(target_os = "linux"))]
        let probe_json = crate::hevc_census::probe_json_for_copy_from(
            self.store.as_ref(),
            &file,
            stored_probe.take(),
        )
        .await
        .ok()
        .flatten();
        file.audio_offset_ms = if file.audio_streams.is_empty() {
            0
        } else {
            audio_offset_ms.clamp(-15_000, 15_000)
        };
        #[cfg(windows)]
        let source_handle = bind_windows_session_source(&mut file).await?;
        // Copy-video sessions and transcodes must interpret an omitted audio
        // override identically. `/decision` marks the shared-policy pick as
        // default, so silently falling back to ffmpeg's first stream here can
        // make the player show English while the session carries (for example)
        // an Italian container default. An explicit viewer choice still wins.
        // A copy session burns nothing — only the audio index is read here —
        // but the fact is reported honestly rather than defaulted, so the one
        // predicate cannot be handed a lie at any call site. A copy delivers
        // the source's own grade, which is `Sdr`'s row in
        // `delivered_dynamic_range` for an SDR file and the source grade for
        // an HDR one.
        let copy_delivers_hdr = plurx_core::playback::burn_would_discard_hdr(
            plurx_core::playback::delivered_dynamic_range(
                &file,
                plurx_core::playback::PlaybackMethod::Remux,
                options.preserve_dolby_vision,
                OutputGrade::Sdr,
            ),
            true,
        );
        let audio_index = Self::select_tracks_with_prefs(
            &file,
            audio_override,
            None,
            &Self::lang_prefs_from(&settings),
            copy_delivers_hdr,
        )
        .audio_index;
        let item_title = item_title.unwrap_or_else(|| "(unknown)".to_owned());
        // Every object this session writes passes a Rust grant boundary
        // before it exists, so it may start small and grow. Sizing covers the
        // *effective* startup gate —
        // the rolling publication clock's runway, not just the copy writer's
        // own 12 s — plus one complete segment, plus the envelope, because a
        // session that cannot reach a published playlist has no client to
        // drain it and nothing to wait for.
        //
        // FFmpeg's own `-f hls` muxer, where this session uses it, is held
        // to the same rule through the upload endpoint bound below.

        // An ffmpeg capability, read from the daemon's own record of which
        // ffmpeg it runs. It used to be read off the CACHE config — which
        // carries a copy of the same string — so a node with no cache
        // configured silently answered "no dovi_rpu" whatever it was running,
        // left the DV configuration in every remux, and had Chrome refuse the
        // stream Safari played fine.
        let have_dovi = self.dv_strippable();
        // The GOP-aware segmenter owns an in-process RPU rewrite, so a fresh
        // HEVC copy can deliver the requested P7 -> P8.1 conversion. Takeover
        // and the legacy muxer cannot, and still narrow to the HDR10 base via
        // `served_copy_options`.
        //
        // `options` is **shadowed** rather than read alongside the served
        // value, because reading the wrong one is the bug this exists to
        // prevent and it shipped once already: the returned `SessionKind` was
        // built from the asked-for options, and that is what the create
        // response's badge is computed from, so a stream this path stripped to
        // HDR10 was badged Dolby Vision. Shadowing makes that unspellable —
        // every later `options.` in this function is the served answer. `asked`
        // survives only to log the difference.
        let asked = options;
        let segmenting = takeover.is_none() && copyseg::supports(file.video_codec.as_deref());
        let options = if segmenting && asked.convert_dolby_vision {
            asked
        } else {
            served_copy_options(&file, asked)
        };
        let served = options;
        let preserve = options.preserve_dolby_vision;
        // Logged whenever EITHER field was given up, not only when a conversion
        // was asked for. `convert && !preserve` is the pair split the other
        // way, and a path that silently discarded a conversion is exactly as
        // worth a line as one that silently discarded a preservation.
        if asked.preserve_dolby_vision != options.preserve_dolby_vision
            || asked.convert_dolby_vision != options.convert_dolby_vision
        {
            tracing::info!(
                target: "plurxd::transcode",
                file_id,
                asked_preserve = asked.preserve_dolby_vision,
                asked_convert = asked.convert_dolby_vision,
                dual_layer = plurx_core::playback::dolby_vision_is_dual_layer(&file),
                dual_layer_as_copied = asked.dual_layer_as_copied,
                served_preserve = preserve,
                "this copy cannot convert Dolby Vision, so it serves the source's base \
                 layer rather than a stream that names a profile it did not produce"
            );
        }
        let video_options = rolling_copy_video_options(
            &file,
            probe_json.as_deref(),
            have_dovi,
            preserve,
            options.convert_dolby_vision,
        );
        if video_options.promotes_parameter_sets() && takeover.is_some() {
            return Err(
                "this HEVC source requires GOP-aware init promotion and cannot use the legacy takeover muxer"
                    .to_owned(),
            );
        }
        let pacing = Self::pacing_from_settings(&settings, true).await;
        let rolling_provenance = if !selected_core_copy
            && start_seconds == 0.0
            && takeover.is_none()
            && retained_budget.is_some()
        {
            let canonical_args = if segmenting {
                transcode::copy_pipe_args_with_audio_delivery(
                    &file,
                    start_seconds,
                    audio_index,
                    options.transcode_audio,
                    pacing,
                    video_options,
                    audio_delivery,
                )
            } else {
                transcode::hls_copy_args_with_audio_delivery(
                    &file,
                    start_seconds,
                    audio_index,
                    options.transcode_audio,
                    pacing,
                    video_options,
                    0,
                    "init.mp4",
                    "retained-output",
                    audio_delivery,
                )
            };
            let mut logical = serde_json::json!({
                "file": &file,
                "kind": SessionKind::Copy {
                    aac: options.transcode_audio,
                    preserve_dolby_vision: options.preserve_dolby_vision,
                    convert_dolby_vision: options.convert_dolby_vision,
                    dual_layer_as_copied: options.dual_layer_as_copied,
                },
                "audio_index": audio_index,
                "audio": audio_delivery,
                "args": canonical_args,
            });
            // This transform runs after FFmpeg, so its version is not in argv.
            // Keep completed rolling output from aliasing the old retaining bytes.
            if segmenting
                && video_options.retains_hevc_parameter_sets()
                && transcode::hevc_copy_tag_for_source(
                    &file,
                    video_options.preserves_dolby_vision(),
                ) == "dvh1"
            {
                logical["copyseg_dvh1_guard"] = serde_json::json!(1);
            }
            let logical = serde_json::to_vec(&logical).ok();
            match logical {
                Some(logical) => {
                    crate::rolling_provenance::RollingProduction::capture(
                        &file,
                        &logical,
                        file.audio_offset_ms != 0
                            && !file.audio_streams.is_empty()
                            && audio_delivery
                                .map_or(!options.transcode_audio, |audio| !audio.transcodes()),
                        &if segmenting {
                            ffmpeg_bin()
                        } else {
                            producer_ffmpeg_bin()
                        },
                    )
                    .await
                }
                None => None,
            }
        } else {
            None
        };
        let mut execution_file = (!selected_core_copy).then(|| file.clone());
        if let (Some(execution_file), Some(provenance)) =
            (execution_file.as_mut(), rolling_provenance.as_ref())
        {
            execution_file.path = provenance.input_path();
        }
        let legacy_args = |output: &str| match takeover.as_ref() {
            Some(takeover) => transcode::hls_copy_args_with_audio_delivery(
                execution_file
                    .as_ref()
                    .expect("legacy copy has its execution source"),
                start_seconds,
                audio_index,
                options.transcode_audio,
                pacing,
                video_options,
                takeover.media_sequence,
                &init_object_name(Some(takeover.owner_epoch)),
                output,
                audio_delivery,
            ),
            None => transcode::hls_copy_args_with_audio_delivery(
                execution_file
                    .as_ref()
                    .expect("legacy copy has its execution source"),
                start_seconds,
                audio_index,
                options.transcode_audio,
                pacing,
                video_options,
                0,
                "init.mp4",
                output,
                audio_delivery,
            ),
        };
        // Take over the cutting when the source is one whose keyframes can be
        // read (docs/streaming/SEGMENTER-PLAN.md). ffmpeg then writes one continuous
        // fragmented stream down a pipe and `copyseg` decides where the
        // segments end — in front of a keyframe no player will discard a
        // leading picture at. A structural Unsupported result may ask the
        // actor for the one frozen legacy retry; the reader never performs the
        // replacement itself.

        // Freeze the achieved media origin before actor admission. The actor,
        // retry recipe, and response contract must describe one immutable
        // presentation even if the start future is later cancelled.
        let media_origin_seconds = if selected_core_copy {
            // The only selected seek is exactly zero. The keyframe probe's
            // source-backed implementation returns zero without native entry.
            0.0
        } else {
            probe_media_origin(&file.path, start_seconds).await
        };

        // `served`, not `options`, from here on: the playlist must advertise
        // the HDR10 base this path serves and the session record must not
        // claim a conversion that did not happen.
        let (hls_codecs, hls_supplemental_codecs) =
            copied_hls_codecs(&file, audio_index, served, probe_json.as_deref());
        let hls_codecs = audio_delivery_hls_codecs(hls_codecs, audio_delivery);
        let copy_kind = SessionKind::Copy {
            aac: served.transcode_audio,
            preserve_dolby_vision: served.preserve_dolby_vision,
            convert_dolby_vision: served.convert_dolby_vision,
            dual_layer_as_copied: served.dual_layer_as_copied,
        };
        // Frozen with the rest of the presentation: this session's master
        // keeps one shape whatever the setting does later.
        let sdr_master_codecs = sdr_master_codecs.unwrap_or_else(|| {
            plurx_core::store::stored_switch(
                settings
                    .get(keys::PLAYBACK_SDR_MASTER_CODECS)
                    .map(String::as_str),
                false,
            )
        });
        let frozen_presentation = FrozenHlsPresentation::new(
            file.clone(),
            HlsContext {
                codec_facts: Some(
                    FrozenHlsCodecFacts::audio(
                        audio_delivery,
                        !file.audio_streams.is_empty(),
                        served.transcode_audio,
                    )
                    .with_sdr_master_codecs(sdr_master_codecs),
                ),
                bandwidth: None,
                file_id,
                start_seconds,
                media_origin_seconds,
                codecs: hls_codecs.clone(),
                supplemental_codecs: hls_supplemental_codecs.clone(),
                frame_rate: frozen_video_frame_rate(probe_json.as_deref()),
            },
            &copy_kind,
        );
        let presentation_contract_fingerprint = frozen_presentation.contract_fingerprint.clone();
        if let Some(production) = &rolling_provenance {
            if let Some(attached) = self
                .attach_rolling_retained(
                    production,
                    frozen_presentation.clone(),
                    copy_kind,
                    None,
                    audio_delivery.cloned(),
                    OutputGrade::Sdr,
                    file.height.unwrap_or(0),
                    user_name,
                    supersession_user,
                    playback_id,
                    &item_title,
                    automatic,
                )
                .await
            {
                return Ok(attached);
            }
        }
        let copy_bitrate = file
            .bitrate
            .filter(|rate| *rate > 0)
            .map(|rate| rate as f64);
        let scratch_envelope = rolling_scratch_envelope(copy_bitrate, 1.0);
        // Only the legacy scratch grant still reads settings below. Release
        // selected planning settings before the distinct R charge begins.
        let settings = (!selected_core_copy).then_some(settings);
        let session_id = takeover
            .as_ref()
            .map(|takeover| takeover.provisional_session_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        #[cfg(target_os = "linux")]
        let selected_owner = if let Some(mut planning) = selected_planning {
            let runtime = self
                .resource_runtime
                .as_ref()
                .ok_or("selected rolling copy runtime disappeared")?;
            let accounting = runtime
                .accounting()
                .map_err(|error| format!("reading selected copy capacity: {error:?}"))?;
            let rendition_id = format!("copy-{session_id}");
            let path_capacity = runtime
                .copy_vod_application_path_capacity(&rendition_id)
                .map_err(|error| format!("sizing selected copy session path: {error}"))?;
            let selected_session_metadata_bytes = frozen_presentation
                .requested_heap_bytes()
                .and_then(|n| n.checked_add(path_capacity))
                .and_then(|n| n.checked_add(item_title.capacity()))
                .and_then(|n| n.checked_add(user_name.len()))
                .and_then(|n| n.checked_add(supersession_user.len()))
                .and_then(|n| n.checked_add(playback_id.len()))
                .and_then(|n| {
                    n.checked_add(audio_delivery.map_or(
                        Some(0),
                        plurx_core::playback::audio::AudioDelivery::requested_heap_bytes,
                    )?)
                })
                .and_then(|n| {
                    n.checked_add(
                        recovery.map_or(Some(0), SessionRecoveryIdentity::requested_heap_bytes)?,
                    )
                })
                .ok_or("selected copy session metadata bound overflows")?;
            // Four original UUID selectors can coexist across manager,
            // observer, retired map and original grace owner.
            let application_metadata_bytes = selected_session_metadata_bytes
                // The local start path and the Session-attached guard
                // created below retain the second and third daemon paths.
                // The core owner's directory Arc is charged separately.
                .checked_add(
                    path_capacity
                        .checked_mul(2)
                        .ok_or("selected copy application path overlap overflows")?,
                )
                .and_then(|n| n.checked_add(session_id.capacity().checked_mul(4)?))
                .ok_or("selected copy application metadata bound overflows")?;
            let request = selected_copy_request(
                runtime,
                &planning,
                &file,
                rendition_id,
                video_options,
                audio_index,
                options.transcode_audio,
                pacing,
                audio_delivery,
                self.runtime_cache.clone(),
                application_metadata_bytes,
                accounting.limit_epoch,
                accounting.capacity,
            )?;
            let client_partition_bytes = usize::try_from(request.output.client_state_bytes)
                .map_err(|_| "selected copy client partition overflows")?;
            let app_bytes = usize::try_from(request.application_owner_bytes)
                .map_err(|_| "selected copy application charge overflows")?;
            let deadline = std::time::Instant::now() + Duration::from_secs(90);
            let quota_path = runtime
                .copy_vod_output_path(&request.rendition_id)
                .map_err(|error| format!("projecting selected copy quota path: {error}"))?;
            let quota = tokio::time::timeout_at(
                deadline.into(),
                crate::playback_cache::reserve_finite(
                    &quota_path,
                    request.output.reserved_storage_bytes,
                ),
            )
            .await
            .map_err(|_| "selected copy operator quota deadline expired")?
            .map_err(|error| format!("reserving selected copy operator quota: {error}"))?;
            // All JSON interpretation, command construction and hard-vector
            // sizing above occur while the actual P owner remains charged.
            // P native closure is the last step before the distinct R admit.
            drop(probe_json);
            drop(stored_probe);
            planning
                .owner
                .finish_native(deadline)
                .await
                .map_err(|error| format!("closing selected copy planning native work: {error}"))?;
            let mut cleanup = planning.owner.into_cleanup();
            drop(planning.report);
            drop(planning.application);
            cleanup
                .finish_retirement(deadline)
                .await
                .map_err(|error| format!("settling selected copy planning metadata: {error}"))?;
            // P/native/journal and quota waits consume the earlier capacity
            // observation. Reobserve before R; keep the finalized resource
            // demand and allocation epoch unchanged: changed policy, lower
            // capacity or Unknown evidence still refuses it.
            runtime
                .refresh_admission_capacity(deadline)
                .await
                .map_err(|error| format!("observing final selected copy admission: {error:?}"))?;
            // Error cleanup may await after R. Convert the SAME grant before
            // polling this constructor; generic errors/default Drop hold it.
            let quota = crate::playback_cache::FiniteCacheReservation::enter(quota);
            let owner = match runtime.prepare_copy_vod(request, deadline).await {
                Ok(owner) => owner,
                Err(error) => {
                    if error.definitely_before_reservation() {
                        quota.known_closed();
                    }
                    return Err(format!("admitting selected rolling copy: {error}"));
                }
            };
            if owner.directory_path() != quota_path {
                return Err("selected copy output differs from its original quota path".into());
            }
            let app = owner
                .take_application_lease(app_bytes)
                .map_err(|error| format!("retaining selected copy application owner: {error}"))?;
            // Slice the already charged client partition; no new allowance.
            let owner_bytes = crate::playback_control::RollingActorAccounting::owner_bytes()
                .ok_or("selected copy actor budget layout overflows")?;
            let client_limit = client_partition_bytes
                .checked_sub(owner_bytes)
                .ok_or("selected copy actor budget exceeds client partition")?;
            // queue_copy_vod already retained this SAME preparation before
            // returning owner. Cancellation drops owner, setting its original
            // cleanup repair request; ordinary maintain_failed_copy_admissions
            // selects that book entry through retained_preparations(). No
            // detached registration race or lost local selector is introduced.
            let output = owner.output();
            let (metadata, client) = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                output.reserve_metadata_partitions(owner_bytes, client_limit),
            )
            .await
            .map_err(|_| "selected copy client partition deadline expired")?
            .map_err(|error| format!("reserving selected copy client ownership: {error}"))?;
            drop(output);
            let accounting = crate::playback_control::RollingActorAccounting::new(metadata, client)
                .map_err(|_| "reserving initial selected copy actor payload")?;
            Some((
                owner,
                (app, accounting),
                selected_session_metadata_bytes,
                quota,
            ))
        } else {
            None
        };
        let scratch_reservation = if selected_core_copy {
            None
        } else {
            Some(self.reserve_rolling_scratch_with(
                RollingScratchSizing::Startup(rolling_startup_bytes(copy_bitrate, 1.0)),
                self.ahead_limits_for_start(
                    settings.as_ref().ok_or("legacy copy settings are absent")?,
                ),
            )?)
        };
        let dir = {
            #[cfg(target_os = "linux")]
            if let Some((owner, _, _, _)) = selected_owner.as_ref() {
                owner.directory_path().to_owned()
            } else {
                self.work_dir.join(format!("w-{}", uuid::Uuid::new_v4()))
            }
            #[cfg(not(target_os = "linux"))]
            {
                self.work_dir.join(format!("w-{}", uuid::Uuid::new_v4()))
            }
        };
        #[cfg(windows)]
        let dir = std::path::absolute(dir)
            .map_err(|error| format!("resolving Windows session directory: {error}"))?;
        if !selected_core_copy {
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| format!("creating session dir: {e}"))?;
        }
        #[cfg(windows)]
        let output_handle = plurx_core::fs_secure::SecureDirectory::open(&dir)
            .await
            .map_err(|error| format!("holding Windows session directory: {error}"))?;
        let mut start_settlement =
            (!selected_core_copy).then(|| PrepublicationStartSettlement::new(dir.clone()));
        // FFmpeg's own HLS muxer -- the legacy writer, a takeover, and the
        // legacy retry a segmenter session keeps in reserve -- uploads
        // through this endpoint, so its objects are granted before they land
        // exactly as the segmenter's are. Lane 0 is the initial attempt and
        // lane 1 the retry.
        let upload = if let Some(reservation) = scratch_reservation.as_ref() {
            Some(self.bind_scratch_upload(&dir, reservation, scratch_envelope)?)
        } else {
            None
        };
        let initial_args = if selected_core_copy {
            None
        } else if segmenting {
            Some(transcode::copy_pipe_args_with_audio_delivery(
                execution_file
                    .as_ref()
                    .ok_or("legacy copy execution source is absent")?,
                start_seconds,
                audio_index,
                options.transcode_audio,
                pacing,
                video_options,
                audio_delivery,
            ))
        } else {
            Some(legacy_args(
                &upload
                    .as_ref()
                    .ok_or("legacy copy upload is absent")?
                    .base_url(0),
            ))
        };
        if let Some(initial_args) = initial_args.as_ref() {
            tracing::info!(
                target: "plurxd::transcode",
                session = %session_log_id(&session_id),
                file_id,
                start_seconds,
                mode = if segmenting { "segmenter" } else { "legacy" },
                build = crate::version::BUILD,
                "{}",
                ffmpeg_args_log_message("copy-video HLS ffmpeg args", initial_args, &session_id)
            );
        }
        let rolling_collection = if selected_core_copy {
            None
        } else {
            self.begin_rolling_retention(
                rolling_provenance.as_ref(),
                file.duration_ms,
                file.size,
                retained_budget,
                &dir,
                file.id,
                &item_title,
            )
            .await
        };
        if let Some(collection) = &rolling_collection {
            upload
                .as_ref()
                .ok_or("legacy copy upload is absent")?
                .bind_retained(Arc::clone(collection));
        }
        let retry = if selected_core_copy {
            None
        } else {
            build_prepublication_copy_retry(
                segmenting,
                video_options,
                legacy_args(
                    &upload
                        .as_ref()
                        .ok_or("legacy copy upload is absent")?
                        .base_url(1),
                ),
                &presentation_contract_fingerprint,
                self.runtime_cache.clone(),
            )
        };
        let expected_remaining_ms =
            file.duration_ms
                .filter(|duration_ms| *duration_ms > 0)
                .map(|duration_ms| {
                    duration_ms
                        .saturating_sub((media_origin_seconds * 1_000.0).round() as i64)
                        .max(0)
                });
        let completion_tolerance_ms = (transcode::SEGMENT_SECONDS as i64).saturating_mul(1_000);
        let policy = if segmenting {
            crate::playback_control::InitialProducerPolicy::copy(
                presentation_contract_fingerprint,
                PROGRESS_STALL,
                retry.as_ref().map(|retry| retry.actor_recipe.clone()),
            )
        } else {
            crate::playback_control::InitialProducerPolicy::copy_immediate(
                presentation_contract_fingerprint,
                PROGRESS_STALL,
            )
        }
        .with_completion_expectation(expected_remaining_ms, completion_tolerance_ms);
        let progress = Arc::new(Progress::new());
        #[cfg(target_os = "linux")]
        let (selected_core_owner, selected_core_app, selected_metadata_bound, selected_core_quota) =
            match selected_owner {
                Some((owner, app, bound, quota)) => {
                    (Some(owner), Some(app), Some(bound), Some(quota))
                }
                None => (None, None, None, None),
            };
        #[cfg(target_os = "linux")]
        let (control, mut executor_registration) = match selected_core_app {
            Some((application, accounting)) => crate::playback_control::RollingControlHandle::spawn_prepublication_producer_with_application(
                "copy-session-start", application, accounting,
            ),
            None => crate::playback_control::RollingControlHandle::spawn_prepublication_producer(
                "copy-session-start",
            ),
        };
        #[cfg(not(target_os = "linux"))]
        let (control, mut executor_registration) =
            crate::playback_control::RollingControlHandle::spawn_prepublication_producer(
                "copy-session-start",
            );
        let failed = Arc::new(AtomicBool::new(false));
        let session = Arc::new(Session {
            // A copy session encodes nothing; `session_delivered_dynamic_range`
            // reads its range off the source and `preserve_dolby_vision`.
            grade: OutputGrade::Sdr,
            dir: dir.clone(),
            response_incarnation: uuid::Uuid::new_v4(),
            frozen_presentation: Some(frozen_presentation),
            rolling_provenance,
            rolling_collection,
            rolling_artifact: None,
            copy_output_measurement: std::sync::Mutex::new(None),
            copy_reader_task: Mutex::new(CopyReaderTask::default()),
            #[cfg(target_os = "linux")]
            core_copy: Mutex::new(selected_core_owner),
            #[cfg(target_os = "linux")]
            core_copy_close: Mutex::new(None),
            #[cfg(target_os = "linux")]
            core_copy_quota: std::sync::Mutex::new(selected_core_quota),
            #[cfg(target_os = "linux")]
            core_copy_refresh_gate: Mutex::new(()),
            actor_managed_response_publication: true,
            actor_managed_prepublication_process: true,
            actor_prepublication_producer: Arc::new(AtomicBool::new(true)),
            response_publication_transition: Mutex::new(()),
            first_media_handoff_applied: AtomicBool::new(false),
            first_media_handoff_notify: tokio::sync::Notify::new(),
            prepublication_cleanup_active: AtomicBool::new(false),
            retirement_cleanup_started: AtomicBool::new(false),
            retirement_cleanup_finished: AtomicBool::new(false),
            retirement_settlement: std::sync::Mutex::new(None),
            scratch_cleanup_started: AtomicBool::new(false),
            retirement_context: Some(self.rolling_retirement_context()),
            cache_integrity_cleanup_started: AtomicBool::new(false),
            child: Mutex::new(None),
            child_transition: Mutex::new(()),
            replacing_child: AtomicBool::new(false),
            terminal_response_pending: Arc::new(AtomicBool::new(false)),
            terminal_control: std::sync::Mutex::new(None),
            hooks: crate::seam_hooks::HookSlot::new(&NoopSessionHooks),
            cached: false,
            _cache_reader: None,
            subtitle_handle: None,
            #[cfg(windows)]
            source_handle: Some(source_handle),
            #[cfg(windows)]
            output_handle: Some(output_handle),
            cache_manifest: None,
            cache_location: None,
            control,
            publication: Mutex::new(RollingPublicationClock::default()),
            publication_worker_started: AtomicBool::new(false),
            flow_worker_started: AtomicBool::new(false),
            file_id,
            item_id: file.item_id,
            item_title,
            user_name: user_name.to_owned(),
            supersession_user: supersession_user.to_owned(),
            playback_id: playback_id.to_owned(),
            recovery: recovery.cloned(),
            automatic,
            kind: copy_kind,
            audio_delivery: audio_delivery.cloned(),
            method: crate::delivery::Method::HlsCopy,
            start_seconds,
            media_origin_seconds,
            target_height: file.height.unwrap_or(0),
            tone_map_peak_nits: None,
            tone_map_peak_source: None,
            encoder_label: Mutex::new("copy"),
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            failed,
            failure: std::sync::Mutex::new(None),
            playlist_published: AtomicBool::new(false),
            high_segment: Arc::new(AtomicI64::new(-1)),
            compatibility_attempt: Arc::new(std::sync::Mutex::new(0)),
            fetched_end_ms: Arc::new(AtomicI64::new(0)),
            segments: Mutex::new(SegmentIndex::default()),
            ahead_bytes: AtomicI64::new(0),
            live_bytes: Arc::new(AtomicI64::new(0)),
            scratch: scratch_reservation.map(|reservation| reservation.bound_to(&session_id, 0)),
            retired_release: Arc::new(RetiredRelease::new()),
            scratch_envelope,
            upload,
            retention_garbage_bytes: Arc::new(AtomicI64::new(0)),
            retention_cleanup_queue: Arc::new(std::sync::Mutex::new(Vec::new())),
            retention_cleanup_active: Arc::new(AtomicBool::new(false)),
            progress: Arc::clone(&progress),
            class: std::sync::Mutex::new(String::new()),
            hw_slot: std::sync::Mutex::new(None),
            sw_permit: std::sync::Mutex::new(None),
            sw_delta_permit: std::sync::Mutex::new(None),
            delivery: Meter::for_method(crate::delivery::Method::HlsCopy.metric_label()),
            http_waits: HttpWaitLedger::default(),
            readrate: pacing
                .readrate
                .unwrap_or(if pacing.legacy_re { 1.0 } else { 0.0 }),
            suspended: AtomicBool::new(false),
            suspended_at: Mutex::new(None),
            suspend_count: AtomicU64::new(0),
            takeover,
            first_slide_logged: AtomicBool::new(false),
        });
        // Selected output is core-owned: construct this existing cancellation
        // owner only with a rich Session attached, so its raw-directory branch
        // is unreachable. It retains the original actor join and core owner
        // before the first awaited actor bind can be cancelled.
        if selected_core_copy {
            start_settlement = Some(PrepublicationStartSettlement::new(dir.clone()));
        }
        start_settlement.attach(&session);
        #[cfg(target_os = "linux")]
        if let Some(bound) = selected_metadata_bound {
            if selected_copy_session_dynamic_bytes(&session).is_none_or(|actual| actual > bound) {
                return Err("selected copy session metadata exceeded its admitted capacity".into());
            }
        }
        if let Err(reason) = session
            .control
            .bind_response_publication_contract(
                session
                    .frozen_presentation
                    .as_ref()
                    .expect("new copy session has a frozen presentation")
                    .contract_fingerprint
                    .clone(),
                Arc::clone(&session.failed),
            )
            .await
        {
            fail_prepublication_transaction(&session,
                format!("rolling control actor rejected copy response-publication failure fencing: {reason:?}"),
            ).await;
            start_settlement.disarm();
            return Err(format!(
                "rolling control actor rejected copy response-publication failure fencing: {reason:?}"
            ));
        }
        if let Err(reason) = executor_registration.register().await {
            fail_prepublication_transaction(
                &session,
                format!("rolling control actor rejected copy executor registration: {reason:?}"),
            )
            .await;
            start_settlement.disarm();
            return Err(format!(
                "rolling control actor rejected copy executor registration: {reason:?}"
            ));
        }
        let (executor_activation, manager_publication) = tokio::sync::oneshot::channel();
        spawn_prepublication_executor_owner(
            &session,
            executor_registration,
            manager_publication,
            retry.clone().map(PrepublicationRetry::Copy),
            session_id.clone(),
        );
        let generation = match session.control.begin_initial_producer_attempt(policy).await {
            Ok(generation) => generation,
            Err(reason) => {
                fail_prepublication_transaction(
                    &session,
                    format!(
                        "rolling control actor rejected the initial copy producer policy: {reason:?}"
                    ),
                )
                .await;
                start_settlement.disarm();
                return Err(format!(
                    "rolling control actor rejected the initial copy producer policy: {reason:?}"
                ));
            }
        };
        if let Some(provenance) = &session.rolling_provenance {
            provenance.bind_initial_attempt(generation);
        }
        session.bind_retry_compatibility_attempt(generation).await;
        let pipe_stdout = if selected_core_copy {
            #[cfg(target_os = "linux")]
            {
                let deadline = std::time::Instant::now() + Duration::from_secs(90);
                let started = {
                    let mut selected = session.core_copy.lock().await;
                    selected
                        .as_mut()
                        .ok_or("selected copy owner was lost before entry")?
                        .start(deadline)
                        .await
                };
                if let Err(error) = started {
                    fail_prepublication_transaction(
                        &session,
                        format!("selected copy root did not start: {error}"),
                    )
                    .await;
                    start_settlement.disarm();
                    return Err(format!("selected copy root did not start: {error}"));
                }
                if let Err(error) = spawn_core_copy_result_owner(
                    Arc::clone(&session),
                    session_id.clone(),
                    generation,
                )
                .await
                {
                    fail_prepublication_transaction(&session, error.clone()).await;
                    start_settlement.disarm();
                    return Err(error);
                }
                None
            }
            #[cfg(not(target_os = "linux"))]
            {
                unreachable!("selected copy requires the Linux owner")
            }
        } else if segmenting {
            match session
                .spawn_and_install_prepublication_pipe_child(generation, || {
                    spawn_ffmpeg_pipe_at(
                        session
                            .rolling_provenance
                            .as_ref()
                            .map(|proof| proof.executable_path())
                            .unwrap_or(std::path::Path::new(&ffmpeg_bin())),
                        initial_args
                            .as_ref()
                            .ok_or("legacy copy arguments are absent")?,
                        crate::process_control::ChildWork::realtime("playback transcode"),
                        &session_id,
                        FfmpegProgressObserver::rolling(
                            Arc::clone(&progress),
                            generation,
                            session.control.clone(),
                        ),
                        &self.runtime_cache,
                        {
                            #[cfg(unix)]
                            let descriptors = FfmpegDescriptors::from_raw_fds(
                                session
                                    .rolling_provenance
                                    .as_ref()
                                    .map(|proof| proof.source_fd()),
                                None,
                                None,
                                false,
                            );
                            #[cfg(windows)]
                            let descriptors = windows_session_descriptors(&session)?;
                            descriptors
                        },
                        DiagnosticObservation::copy(&session_id),
                    )
                })
                .await
            {
                Ok(stdout) => Some(stdout),
                Err(reason) => {
                    fail_prepublication_transaction(
                        &session,
                        format!("initial copy pipe producer could not be installed: {reason}"),
                    )
                    .await;
                    start_settlement.disarm();
                    return Err(reason);
                }
            }
        } else {
            if let Err(reason) = session
                .spawn_and_install_prepublication_child(generation, || {
                    spawn_ffmpeg_at(
                        session
                            .rolling_provenance
                            .as_ref()
                            .map(|proof| proof.executable_path())
                            .unwrap_or(std::path::Path::new(&producer_ffmpeg_bin())),
                        initial_args
                            .as_ref()
                            .ok_or("legacy copy arguments are absent")?,
                        crate::process_control::ChildWork::realtime("playback copy HLS"),
                        "copy",
                        &session_id,
                        FfmpegProgressObserver::rolling(
                            Arc::clone(&progress),
                            generation,
                            session.control.clone(),
                        ),
                        &self.runtime_cache,
                        {
                            #[cfg(unix)]
                            let descriptors = FfmpegDescriptors::from_raw_fds(
                                session
                                    .rolling_provenance
                                    .as_ref()
                                    .map(|proof| proof.source_fd()),
                                None,
                                None,
                                false,
                            );
                            #[cfg(windows)]
                            let descriptors = windows_session_descriptors(&session)?;
                            descriptors
                        },
                        DiagnosticObservation::copy(&session_id),
                    )
                })
                .await
            {
                fail_prepublication_transaction(
                    &session,
                    format!("initial direct copy producer could not be installed: {reason}"),
                )
                .await;
                start_settlement.disarm();
                return Err(reason);
            }
            None
        };
        if let Some(stdout) = pipe_stdout {
            // Every object this writer publishes is authorized before it
            // exists. That is what makes the smaller admission above safe: an
            // under-estimated envelope makes the writer wait for the budget
            // rather than overrun it.
            let grants = session.scratch.as_ref().map(|permit| {
                crate::copyseg::WriteGrants::new(
                    Arc::clone(permit.ledger()),
                    permit.key(),
                    Arc::clone(&self.scratch_cap),
                    scratch_envelope,
                )
                .with_starved_signal(Arc::clone(&self.scratch_starved))
            });
            if let Err(reason) = spawn_copy_reader_owner(
                Arc::clone(&session),
                stdout,
                dir.clone(),
                session_id.clone(),
                generation,
                file.clone(),
                video_options,
                grants,
            )
            .await
            {
                fail_prepublication_transaction(&session, reason.clone()).await;
                start_settlement.disarm();
                return Err(reason);
            }
        }
        tracing::info!(
            target: "plurxd::transcode",
            session = %session_log_id(&session_id),
            file_id,
            start_seconds,
            producer_attempt = generation,
            mode = if segmenting { "segmenter" } else { "legacy" },
            "started actor-owned prepublication copy session"
        );
        if let Err(reason) = self
            .register_session(&session_id, Arc::clone(&session), generation)
            .await
        {
            // Registration rejection owns synchronous/detached retirement;
            // do not race that exact settlement with this start guard.
            start_settlement.disarm();
            return Err(format!(
                "rolling copy session registration rejected: {reason:?}"
            ));
        }
        if executor_activation.send(()).is_err() {
            let reason = "copy producer executor ended before manager publication".to_owned();
            fail_prepublication_transaction(&session, reason.clone()).await;
            start_settlement.disarm();
            return Err(reason);
        }
        start_settlement.settle();
        self.emit_session_event(
            &session_id,
            &session,
            "session_start",
            SessionEventFields::default(),
        )
        .await;

        Ok(StartInfo {
            processed_dv_profile: None,
            processed_fel: None,
            degraded_processing: None,
            processing_note: None,
            retained_output: None,
            audio_delivery: audio_delivery.cloned(),
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            session_id,
            duration_ms: file.duration_ms,
            start_seconds,
            media_origin_seconds,
            target_height: file.height.unwrap_or(0),
            // `served`, not `options` — for the reason the strip-down above
            // gives. This is the value the create response's
            // `delivered_dynamic_range` is computed from, and it overrides
            // whatever `/decision` said, so `options`' `preserve` would badge
            // a stream this path stripped to HDR10 as Dolby Vision: the
            // session record, the playlist and the badge would disagree about
            // one delivery, and the badge is the one the viewer reads.
            kind: SessionKind::Copy {
                aac: served.transcode_audio,
                preserve_dolby_vision: served.preserve_dolby_vision,
                convert_dolby_vision: served.convert_dolby_vision,
                dual_layer_as_copied: served.dual_layer_as_copied,
            },
            encoder: "copy",
            // A copy session encodes nothing; its dynamic range is the
            // source's, read off `kind`/`preserve_dolby_vision`.
            grade: OutputGrade::Sdr,
            vod: false,
            control_lease_timeout_ms: crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
        })
    }
}

/// Rolling copies retain HEVC parameter sets by default, avoiding deletion
/// without a whole-film stability proof. An incomplete dvh1 muxer claim is the
/// narrow exception: copyseg proves every sample against the repaired init
/// before buffering it, then strips only identical definitions. A contradiction
/// terminates that stream before the unsafe fragment can be published.
/// Non-HEVC sources and VOD's whole-source admission are unaffected.
pub(crate) fn rolling_copy_video_options(
    file: &plurx_core::domain::MediaFile,
    probe_json: Option<&str>,
    have_dovi: bool,
    preserve_dolby_vision: bool,
    convert_dolby_vision: bool,
) -> transcode::CopyVideoOptions {
    let options =
        transcode::CopyVideoOptions::from_probe(file, probe_json, have_dovi, preserve_dolby_vision)
            .with_dolby_vision_conversion(convert_dolby_vision);
    if matches!(file.video_codec.as_deref(), Some("hevc" | "h265")) {
        options.with_parameter_set_retention(true)
    } else {
        options
    }
}
