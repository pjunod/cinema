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
    opt_string(&v.planned_codec)
}

fn device_caps(v: &plurx_core::playback::DeviceCaps) -> Option<usize> {
    let mut n = opt_string(&v.decoder_compaction);
    if let Some(client) = &v.client {
        n = add(n, client.kind.capacity())?;
        n = add(n, client.build.capacity())?;
        n = add(n, client.ua.capacity())?;
    }
    n = add(n, vec_bytes(&v.video)?)?;
    for entry in &v.video {
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
        n = add(n, limit.identity.capacity())?;
        n = add(n, limit.label.capacity())?;
    }
    Some(n)
}

fn start_response(v: &StartResponse) -> Option<usize> {
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

pub(super) fn committer(v: &DurableTerminalCommitter) -> Option<usize> {
    // Inline fields live inside this allocation; walkers below add only their
    // separately allocated payload capacities. Shared Store/Arc internals are
    // not charged to this finite continuation.
    let mut n = std::mem::size_of::<DurableTerminalCommitter>();
    n = add(n, route(&v.route)?)?;
    n = add(n, start_response(&v.start)?)?;
    n = add(n, recipe(&v.recipe)?)?;
    add(n, request(&v.request)?)
}
