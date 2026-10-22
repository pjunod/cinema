{
    match force {
        Force::Auto => decide(file, profile, node),
        Force::Transcode => {
            let (_, mut reasons) = evaluate(file, profile);
            reasons.insert(0, "forced transcode (manual quality)".to_owned());
            // The quality menu's Transcode used to be pinned to SDR, on the
            // reasoning that widening it would be a grade change nobody asked
            // for on a control whose purpose is "make this smaller". M4 turns
            // that around: the control asks for a smaller *stream*, not a
            // worse *picture*, and the rung that keeps the picture is the one
            // every other transcode now gets. The negotiation still refuses
            // wherever any of the three parties does.
            let (grade, grade_reason) = target_grade(file, profile, node);
            if !grade_reason.is_empty() {
                reasons.push(grade_reason.to_owned());
            }
            let audio = resolve_audio_for_method(file, profile, PlaybackMethod::Transcode);
            Decision {
                method: PlaybackMethod::Transcode,
                reasons,
                transcode_audio: true,
                delivered_audio: audio,
                preserve_dolby_vision: false,
                // A forced transcode re-encodes the picture: there is no copy
                // pipe left for a conversion to sit inside.
                convert_dolby_vision: false,
                container: "mp4",
                delivered_dynamic_range: delivered_dynamic_range(
                    file,
                    PlaybackMethod::Transcode,
                    false,
                    grade,
                ),
                delivered_dolby_vision_profile: None,
                transcode_grade: grade,
            }
        }
        Force::Original => {
            // Never re-encode video: direct-play when the browser can take the
            // container + audio, else a copy-video remux. If the pick turns out
            // undecodable, the client's error path falls back to transcode.
            let (c, _) = evaluate(file, profile);
            let has_av_offset = file.audio_offset_ms != 0;
            // A DV source this client can't decode still may not be handed
            // over raw — Original means "no video re-encode", which a strip
            // remux honours (the base layer is untouched). When even that is
            // unavailable the remux is the client's error path to rescue, as
            // it always was.
            let dv = dv_handling(file, profile, node, OutputGrade::Sdr);
            let method = if c.container_ok && c.audio_ok && !has_av_offset && dv == DvHandling::None
            {
                PlaybackMethod::DirectPlay
            } else {
                PlaybackMethod::Remux
            };
            let mut reasons = vec!["forced original quality (no video transcode)".to_owned()];
            if c.packaging_normalization {
                reasons.push(progressive_hevc_normalization_reason(file));
            }
            if dv == DvHandling::Strip {
                reasons.push(
                    "Dolby Vision metadata removed for this device; compatible HDR base kept"
                        .to_owned(),
                );
            }
            if dv == DvHandling::Convert {
                reasons.push(
                    "Dolby Vision Profile 7 converted to Profile 8.1 for this device; the \
                     HDR10-compatible base layer is copied untouched and the dual-layer \
                     enhancement layer, which no consumer decoder takes, is dropped"
                        .to_owned(),
                );
            }
            // Original means "no video re-encode", which the conversion
            // honours: it copies every picture byte and rewrites only the
            // per-frame metadata. So a forced-Original session on a client
            // that takes Profile 8 gets the conversion too, and `dv_handling`
            // above has already answered `Convert` for it.
            let convert_dolby_vision = dv == DvHandling::Convert;
            let preserve_dolby_vision = convert_dolby_vision
                || (is_dolby_vision(file) && profile.decodes_dolby_vision_as_copied(file));
            let audio = resolve_audio_for_method(file, profile, method);
            Decision {
                method,
                reasons,
                transcode_audio: !c.audio_ok,
                delivered_audio: audio,
                preserve_dolby_vision,
                convert_dolby_vision,
                container: "mp4",
                // Original never re-encodes video, so there is no grade: the
                // method is DirectPlay or Remux and this argument is inert.
                delivered_dynamic_range: delivered_dynamic_range(
                    file,
                    method,
                    preserve_dolby_vision,
                    OutputGrade::Sdr,
                ),
                delivered_dolby_vision_profile: delivered_dolby_vision_profile(
                    file,
                    method,
                    preserve_dolby_vision,
                    convert_dolby_vision,
                ),
                transcode_grade: OutputGrade::Sdr,
            }
        }
    }
}
