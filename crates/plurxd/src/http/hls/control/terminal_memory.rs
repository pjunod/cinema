//! Requested capacities of the concrete terminal continuation retained after
//! a finite VOD End. This walks owned application data, not JSON wire length.
use super::*;

fn add(a: usize, b: usize) -> Option<usize> {
    a.checked_add(b)
}
fn vec_bytes<T>(v: &Vec<T>) -> Option<usize> {
    v.capacity().checked_mul(std::mem::size_of::<T>())
}
fn strings(v: &Vec<String>) -> Option<usize> {
    let mut n = vec_bytes(v)?;
    for s in v {
        n = add(n, s.capacity())?;
    }
    Some(n)
}
fn opt_string(v: &Option<String>) -> usize {
    v.as_ref().map_or(0, String::capacity)
}

// serde_json::Map uses BTreeMap in this build (no preserve_order feature).
// The pinned Rust 1.97.1 alloc node layout has B=6: at most 11 key/value
// slots and 12 child pointers per node. An occupied tree with n entries has
// at most n+1 nodes, including a possibly allocated empty root. Charge each
// as the larger internal node, with one pointer for the parent, two u16
// fields, and padding for every field boundary/final alignment. This is a
// source-derived requested-layout bound, not an allocator/RSS guarantee.
// Source: rust-lang/rust 8bab26f4f68e0e26f0bb7960be334d5b520ea452,
// library/alloc/src/collections/btree/node.rs, lines 40-62 and 94-126.
fn json_object_nodes(entries: usize) -> Option<usize> {
    use std::mem::{align_of, size_of};
    let pair = size_of::<String>().checked_add(size_of::<serde_json::Value>())?;
    let slots = pair.checked_mul(11)?;
    let pointers = size_of::<usize>().checked_mul(13)?;
    let fields = pointers
        .checked_add(2 * size_of::<u16>())?
        .checked_add(slots)?;
    let alignment = align_of::<String>()
        .max(align_of::<serde_json::Value>())
        .max(align_of::<usize>());
    let per_node = fields.checked_add(alignment.checked_mul(7)?)?;
    entries.checked_add(1)?.checked_mul(per_node)
}

fn json_value(v: &serde_json::Value) -> Option<usize> {
    match v {
        serde_json::Value::String(s) => Some(s.capacity()),
        serde_json::Value::Array(items) => {
            let mut n = vec_bytes(items)?;
            for item in items {
                n = add(n, json_value(item)?)?;
            }
            Some(n)
        }
        serde_json::Value::Object(entries) => {
            let mut n = json_object_nodes(entries.len())?;
            for (key, value) in entries {
                n = add(n, key.capacity())?;
                n = add(n, json_value(value)?)?;
            }
            Some(n)
        }
        _ => Some(0),
    }
}

fn quality_candidate(v: &plurx_core::playback::candidate::QualityCandidate) -> usize {
    // Exhaustive inventory: an added field must force accounting review.
    let plurx_core::playback::candidate::QualityCandidate {
        id: _,
        recipe_digest: _,
        route: _,
        planned_codec: _,
        processed_dv: _,
        normalized_geometry: _,
        width: _,
        height: _,
        target_height: _,
        average_bps: _,
        peak_bps: _,
        grade: _,
        decoder_compatible: _,
        complete_cache: _,
        sustainable: _,
    } = v;
    opt_string(&v.planned_codec)
}

fn device_caps(v: &plurx_core::playback::DeviceCaps) -> Option<usize> {
    // Exhaustive inventory: an added field must force accounting review.
    let plurx_core::playback::DeviceCaps {
        decoder_compaction: _,
        v: _,
        client: _,
        video: _,
        audio: _,
        audio_sinks: _,
        containers: _,
        transports: _,
        progressive_hevc_sample_entries: _,
        hls_hevc_sample_entries: _,
        subtitle_overlays: _,
        dv_transport: _,
        display: _,
        learned_limits: _,
        legacy_blanket_dolby_vision: _,
        max_height: _,
    } = v;
    let mut n = opt_string(&v.decoder_compaction);
    if let Some(client) = &v.client {
        // Exhaustive inventory: an added field must force accounting review.
        let plurx_core::playback::caps::ClientInfo {
            kind: _,
            build: _,
            ua: _,
        } = client;
        n = add(n, client.kind.capacity())?;
        n = add(n, client.build.capacity())?;
        n = add(n, client.ua.capacity())?;
    }
    n = add(n, vec_bytes(&v.video)?)?;
    for entry in &v.video {
        // Exhaustive inventory: an added field must force accounting review.
        let plurx_core::playback::caps::VideoCaps {
            codec: _,
            profiles: _,
            max_height: _,
            max_width: _,
            max_frame_rate: _,
            max_bitrate_bps: _,
            present: _,
            dv_profiles: _,
        } = entry;
        n = add(n, entry.codec.capacity())?;
        n = add(n, strings(&entry.profiles)?)?;
        n = add(n, vec_bytes(&entry.present)?)?;
        n = add(n, vec_bytes(&entry.dv_profiles)?)?;
    }
    n = add(n, strings(&v.audio)?)?;
    n = add(n, vec_bytes(&v.audio_sinks)?)?;
    for sink in &v.audio_sinks {
        n = add(n, sink.codec.capacity())?;
        n = add(n, vec_bytes(&sink.sample_rates_hz)?)?;
    }
    for values in [&v.containers, &v.transports, &v.subtitle_overlays] {
        n = add(n, strings(values)?)?;
    }
    for values in [
        &v.progressive_hevc_sample_entries,
        &v.hls_hevc_sample_entries,
    ]
    .into_iter()
    .flatten()
    {
        n = add(n, strings(values)?)?;
    }
    n = add(n, opt_string(&v.dv_transport))?;
    n = add(n, vec_bytes(&v.learned_limits)?)?;
    for limit in &v.learned_limits {
        // Exhaustive inventory: an added field must force accounting review.
        let plurx_core::playback::caps::LearnedLimit {
            identity: _,
            label: _,
            lost: _,
            secs: _,
            rate: _,
            at_ms: _,
        } = limit;
        n = add(n, limit.identity.capacity())?;
        n = add(n, limit.label.capacity())?;
    }
    Some(n)
}

fn start_response(v: &StartResponse) -> Option<usize> {
    // Exhaustive inventory: an added field must force accounting review.
    let StartResponse {
        delivered_audio: _,
        quality_catalog_status: _,
        display_aware_auto_protocol: _,
        quality_candidate_id: _,
        quality_candidates: _,
        measured_candidate_outputs: _,
        enhancement: _,
        effective_processing: _,
        session_id: _,
        playlist_url: _,
        duration_ms: _,
        start_seconds: _,
        media_origin_ms: _,
        height: _,
        encoder: _,
        vod: _,
        ladder: _,
        prior_kbps: _,
        delivered_dynamic_range: _,
        delivered_dolby_vision_profile: _,
        control: _,
        plan_notes: _,
    } = v;
    // These are process-only sidecars skipped by durable response decoding.
    // The selected finite control handler parsed its StartResponse from the
    // route, so a future path retaining one needs a new explicit accounting.
    if v.measured_candidate_outputs.is_some()
        || v.enhancement.is_some()
        || v.effective_processing.is_some()
    {
        return None;
    }
    let mut n = 0usize;
    if let Some(audio) = &v.delivered_audio {
        n = add(n, audio.requested_heap_bytes()?)?;
    }
    if let Some(value) = &v.quality_catalog_status {
        n = add(n, json_value(value)?)?;
    }
    for value in [&v.display_aware_auto_protocol, &v.delivered_dynamic_range] {
        n = add(n, opt_string(value))?;
    }
    if let Some(candidates) = &v.quality_candidates {
        n = add(n, vec_bytes(candidates)?)?;
        for candidate in candidates {
            n = add(n, quality_candidate(candidate))?;
        }
    }
    for value in [&v.session_id, &v.playlist_url, &v.encoder] {
        n = add(n, value.capacity())?;
    }
    n = add(n, vec_bytes(&v.ladder)?)?;
    n = add(n, strings(&v.plan_notes)?)?;
    if let Some(control) = &v.control {
        n = add(n, control.requested_heap_bytes()?)?;
    }
    Some(n)
}

fn session_request(v: &crate::transcode::SessionRequest) -> Option<usize> {
    // Exhaustive inventory: an added field must force accounting review.
    let crate::transcode::SessionRequest {
        continuous_media: _,
        quality_catalog: _,
        candidate_context: _,
        sdr_master_codecs: _,
        vod_only: _,
        passive_vod: _,
        finite_bitrate_limit_bps: _,
        file_id: _,
        playback_id: _,
        request_id: _,
        control_sequence: _,
        automatic: _,
        previous_session_id: _,
        reopen_reason: _,
        kind: _,
        start_seconds: _,
        audio_index: _,
        audio_delivery: _,
        audio_claim: _,
        subtitle_burn: _,
        audio_offset_ms: _,
        hdr10: _,
        dv_processing_destination: _,
        presentation: _,
        block_budget_secs: _,
        transport: _,
    } = v;
    // The fixed ordinary finite profile excludes continuous families. The
    // route's durable decode also has no skipped process-local candidates.
    if v.continuous_media.is_some() || v.quality_catalog.is_some() || v.candidate_context.is_some()
    {
        return None;
    }
    let mut n = 0usize;
    n = add(n, v.playback_id.capacity())?;
    for value in [&v.request_id, &v.previous_session_id, &v.transport] {
        n = add(n, opt_string(value))?;
    }
    if let Some(audio) = &v.audio_delivery {
        n = add(n, audio.requested_heap_bytes()?)?;
    }
    if let Some(claim) = &v.audio_claim {
        n = add(n, claim.requested_heap_bytes()?)?;
    }
    Some(n)
}

fn recipe(v: &RemoteStartRequest) -> Option<usize> {
    // Exhaustive inventory: an added field must force accounting review.
    let RemoteStartRequest {
        retained_output_receiver: _,
        retained_output: _,
        candidate_catalog: _,
        candidate_id: _,
        presentation_target: _,
        decoder_caps: _,
        protocol_version: _,
        incarnation_id: _,
        principal: _,
        source_size: _,
        source_mtime: _,
        typeless_playlist: _,
        library_channel: _,
        request: _,
    } = v;
    let mut n = 0usize;
    n = add(n, v.incarnation_id.capacity())?;
    if let plurx_core::playback_principal::PlaybackPrincipal::Sharing { viewer_key, .. } =
        &v.principal
    {
        n = add(n, viewer_key.as_str().len())?;
    }
    if let Some(output) = &v.retained_output {
        n = add(n, output.artifact_id.capacity())?;
        n = add(n, output.output_identity.capacity())?;
    }
    if let Some(catalog) = &v.candidate_catalog {
        // Exhaustive inventory: an added field must force accounting review.
        let crate::media_sessions::CandidateCatalogContext {
            caps: _,
            candidate: _,
            binding: _,
        } = catalog;
        n = add(n, device_caps(&catalog.caps)?)?;
        n = add(n, quality_candidate(&catalog.candidate))?;
        n = add(n, catalog.binding.source_digest.capacity())?;
    }
    if let Some(decoder) = &v.decoder_caps {
        n = add(n, decoder.requested_heap_bytes()?)?;
    }
    if let Some(channel) = &v.library_channel {
        n = add(n, json_value(channel)?)?;
    }
    add(n, session_request(&v.request)?)
}

fn request(v: &crate::playback_control::ControlRequestV1) -> Option<usize> {
    // Exhaustive inventory: an added field must force accounting review.
    let crate::playback_control::ControlRequestV1 {
        protocol: _,
        generation: _,
        control_epoch: _,
        client_instance_id: _,
        sequence: _,
        demand: _,
        position_ms: _,
        buffered_from_ms: _,
        buffered_through_ms: _,
        playback_rate: _,
        render_state: _,
        seek_target_ms: _,
        observed_download_bps: _,
        selection: _,
        capabilities: _,
        observation: _,
        acknowledgement: _,
        supported_actions: _,
        intent: _,
    } = v;
    let mut n = 0usize;
    for value in [&v.protocol, &v.generation, &v.client_instance_id] {
        n = add(n, value.capacity())?;
    }
    if let Some(caps) = &v.capabilities {
        n = add(n, caps.requested_heap_bytes()?)?;
    }
    if let Some(observation) = &v.observation {
        n = add(n, opt_string(&observation.error_detail))?;
    }
    if let Some(ack) = &v.acknowledgement {
        n = add(n, ack.action_id.capacity())?;
    }
    if let Some(actions) = &v.supported_actions {
        n = add(n, strings(actions)?)?;
    }
    if let Some(intent) = &v.intent {
        n = add(n, intent.lifetime_id.capacity())?;
    }
    Some(n)
}

fn route(v: &MediaSessionRoute) -> Option<usize> {
    // Exhaustive inventory: an added field must force accounting review.
    let MediaSessionRoute {
        incarnation_id: _,
        session_id: _,
        principal: _,
        playback_id: _,
        recovery_epoch: _,
        request_fingerprint: _,
        owner_node_id: _,
        owner_epoch: _,
        lease_expires_at_ms: _,
        state: _,
        terminal_reason: _,
        publication_ready_at_ms: _,
        recipe_json: _,
        response_json: _,
        produced_playable_through_ms: _,
        fetched_through_ms: _,
        media_origin_ms: _,
        media_sequence: _,
        discontinuity_sequence: _,
        updated_at_ms: _,
        drain_deadline_ms: _,
    } = v;
    let mut n = 0usize;
    for value in [
        &v.incarnation_id,
        &v.session_id,
        &v.playback_id,
        &v.recovery_epoch,
        &v.request_fingerprint,
        &v.owner_node_id,
        &v.state,
        &v.recipe_json,
        &v.response_json,
    ] {
        n = add(n, value.capacity())?;
    }
    n = add(n, opt_string(&v.terminal_reason))?;
    if let plurx_core::playback_principal::PlaybackPrincipal::Sharing { viewer_key, .. } =
        &v.principal
    {
        n = add(n, viewer_key.as_str().len())?;
    }
    Some(n)
}

pub(super) fn committer_parts(
    route_value: &MediaSessionRoute,
    start: &StartResponse,
    recipe_value: &RemoteStartRequest,
    request_value: &crate::playback_control::ControlRequestV1,
) -> Option<usize> {
    let mut n = crate::playback_control::arc_requested_layout(std::alloc::Layout::new::<
        DurableTerminalCommitter,
    >())?;
    n = add(n, route(route_value)?)?;
    n = add(n, start_response(start)?)?;
    n = add(n, recipe(recipe_value)?)?;
    add(n, request(request_value)?)
}

pub(super) fn committer(v: &DurableTerminalCommitter) -> Option<usize> {
    let DurableTerminalCommitter {
        store: _,
        route,
        start,
        recipe,
        request,
        faults: _,
        _allocation: _,
    } = v;
    // Shared Store/fault owners are not deep copied. The concrete context Arc
    // and each owned DTO field above are separately inventoried.
    committer_parts(route, start, recipe, request)
}

pub(super) fn preparation_services(node_id: &String) -> Option<usize> {
    add(
        crate::playback_control::arc_requested_layout(std::alloc::Layout::new::<
            PreparationSettlementServices,
        >())?,
        node_id.capacity(),
    )
}

pub(super) fn preparation_services_value(value: &PreparationSettlementServices) -> Option<usize> {
    let PreparationSettlementServices {
        store: _,
        transcode: _,
        media_sessions: _,
        media_pool: _,
        membership: _,
        link_receipts: _,
        hls_route_hooks: _,
        serving: _,
        node_id,
        _allocation: _,
        _lifetime: _,
    } = value;
    add(
        crate::playback_control::arc_requested_layout(std::alloc::Layout::new::<
            PreparationSettlementServices,
        >())?,
        node_id.capacity(),
    )
}

fn preparation_status_bytes(status: Option<&crate::transcode::HlsSessionInfo>) -> Option<usize> {
    match status {
        None => Some(0),
        Some(crate::transcode::HlsSessionInfo::Live(status)) => add(
            std::mem::size_of::<crate::transcode::SessionInfo>(),
            status.requested_heap_bytes()?,
        ),
        Some(crate::transcode::HlsSessionInfo::Vod(status)) => add(
            std::mem::size_of::<crate::vodserve::VodSessionInfo>(),
            status.requested_heap_bytes()?,
        ),
    }
}

fn preparation_controller_bytes() -> Option<usize> {
    fn frame<A, F: std::future::Future>(_: impl Fn(A) -> F) -> usize {
        std::mem::size_of::<F>()
    }
    let owner = crate::playback_control::arc_requested_layout(std::alloc::Layout::new::<
        OriginalPreparationController,
    >())?;
    let answer = crate::playback_control::arc_requested_layout(std::alloc::Layout::new::<
        PreparationCommittedAnswer,
    >())?;
    // The unpolled compiler-generated frame is measured by its actual type.
    // This does not include private Tokio task/watch/latch allocator layouts,
    // HashMap bucket growth or nested response/JSON payloads: those remain
    // explicit Unknown components until their own preflight is established.
    let capacity = crate::playback_control::arc_requested_layout(std::alloc::Layout::new::<
        PreparationSettlementCapacity,
    >())?;
    add(
        add(add(owner, answer)?, capacity)?,
        frame(run_original_preparation),
    )
}

pub(super) fn preparation_context_parts(
    route_value: &MediaSessionRoute,
    start: &StartResponse,
    recipe_value: &RemoteStartRequest,
    request_value: &crate::playback_control::ControlRequestV1,
    status: Option<&crate::transcode::HlsSessionInfo>,
) -> Option<usize> {
    let mut bytes = crate::playback_control::arc_requested_layout(std::alloc::Layout::new::<
        PreparationSettlementContext,
    >())?;
    bytes = add(
        bytes,
        crate::playback_control::arc_requested_layout(std::alloc::Layout::new::<
            DurablePreparationSettlementAdmission,
        >())?,
    )?;
    bytes = add(bytes, route(route_value)?)?;
    bytes = add(bytes, start_response(start)?)?;
    bytes = add(bytes, recipe(recipe_value)?)?;
    bytes = add(bytes, request(request_value)?)?;
    bytes = add(bytes, preparation_controller_bytes()?)?;
    // One executor owns these source-backed strings inside that original
    // frame. Hashing its operation at acceptance uses borrowed values only.
    bytes = add(bytes, route_value.playback_id.capacity())?;
    bytes = add(bytes, route_value.owner_node_id.capacity())?;
    if let plurx_core::playback_principal::PlaybackPrincipal::Sharing { viewer_key, .. } =
        &route_value.principal
    {
        bytes = add(bytes, viewer_key.as_str().len())?;
    }
    // One status moves into the shared context; the commit path makes exactly
    // one independent acknowledgement status copy while this context lives.
    add(bytes, preparation_status_bytes(status)?.checked_mul(2)?)
}

pub(super) fn preparation_context_value(value: &PreparationSettlementContext) -> Option<usize> {
    let PreparationSettlementContext {
        gate: _,
        route,
        start,
        recipe,
        request,
        status,
        _allocation: _,
        state: _,
    } = value;
    preparation_context_parts(route, start, recipe, request, status.as_ref())
}
