//! Selected-stream mastering metadata parsing shared by all platform policies.

/// SDR consumes only the selected stream's mastering minimum. Missing HDR
/// primaries do not invalidate this independent floor fact or invent BT.2020.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn source_mastering_minimum(stream: &serde_json::Value) -> Result<Option<u32>, String> {
    let invalid = || "targeted SDR source MDCV minimum malformed or conflicting".to_owned();
    let Some(records) = stream.get("side_data_list") else {
        return Ok(None);
    };
    let records = records.as_array().ok_or_else(invalid)?;
    let mut result = None;
    for record in records.iter().filter(|record| {
        record
            .get("side_data_type")
            .and_then(serde_json::Value::as_str)
            == Some("Mastering display metadata")
    }) {
        let Some(value) = record.get("min_luminance") else {
            continue;
        };
        let text = value.as_str().ok_or_else(invalid)?;
        let (numerator, denominator) = text.split_once('/').ok_or_else(invalid)?;
        let numerator = numerator.parse::<u64>().map_err(|_| invalid())?;
        let denominator = denominator.parse::<u64>().map_err(|_| invalid())?;
        if denominator == 0 {
            return Err(invalid());
        }
        let scaled = (u128::from(numerator) * 10_000 + u128::from(denominator / 2))
            / u128::from(denominator);
        let minimum = u32::try_from(scaled).map_err(|_| invalid())?;
        if minimum > 10_000 || result.is_some_and(|old| old != minimum) {
            return Err(invalid());
        }
        result = Some(minimum);
    }
    Ok(result)
}

/// Parse only the already-attested selected video stream, never a container or
/// another track. FFprobe's rational values are converted with integer rounding
/// to ST 2086 units. A missing record differs from malformed/conflicting data.
fn source_mastering_metadata_details(
    stream: &serde_json::Value,
) -> Result<Option<ValidatedSourceMastering>, String> {
    use plurx_core::transcode::dv_processing::DvSourceMastering;
    let invalid = || "targeted HDR10 source MDCV malformed or conflicting".to_owned();
    let Some(records) = stream.get("side_data_list") else {
        return Ok(None);
    };
    let records = records.as_array().ok_or_else(invalid)?;
    fn scaled(record: &serde_json::Value, field: &str, scale: u64) -> Option<u64> {
        let text = record.get(field)?.as_str()?;
        let (numerator, denominator) = text.split_once('/')?;
        let numerator = numerator.parse::<u64>().ok()?;
        let denominator = denominator.parse::<u64>().ok()?;
        if denominator == 0 {
            return None;
        }
        let value = u128::from(numerator) * u128::from(scale);
        u64::try_from((value + u128::from(denominator / 2)) / u128::from(denominator)).ok()
    }
    let mut result = None;
    for record in records.iter().filter(|record| {
        record
            .get("side_data_type")
            .and_then(serde_json::Value::as_str)
            == Some("Mastering display metadata")
    }) {
        let mut xy = [0; 8];
        for (at, name) in [
            "red_x",
            "red_y",
            "green_x",
            "green_y",
            "blue_x",
            "blue_y",
            "white_point_x",
            "white_point_y",
        ]
        .into_iter()
        .enumerate()
        {
            xy[at] = scaled(record, name, 50_000)
                .and_then(|v| u16::try_from(v).ok())
                .ok_or_else(invalid)?;
        }
        let minimum = scaled(record, "min_luminance", 10_000)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(invalid)?;
        let maximum = scaled(record, "max_luminance", 10_000).ok_or_else(invalid)?;
        if maximum == 0 || maximum > 100_000_000 || u64::from(minimum) >= maximum {
            return Err(invalid());
        }
        plurx_core::fmp4::Hdr10OutputMetadata::new(xy, 10_000, minimum, 0)
            .map_err(|_| invalid())?;
        let next = DvSourceMastering {
            primaries_rgbw: xy,
            min_1e4_nits: minimum,
        };
        let next = ValidatedSourceMastering {
            mastering: next,
            max_1e4_nits: maximum,
        };
        if result.is_some_and(|old| old != next) {
            return Err(invalid());
        }
        result = Some(next);
    }
    Ok(result)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ValidatedSourceMastering {
    mastering: plurx_core::transcode::dv_processing::DvSourceMastering,
    max_1e4_nits: u64,
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn source_mastering_metadata(
    stream: &serde_json::Value,
) -> Result<Option<plurx_core::transcode::dv_processing::DvSourceMastering>, String> {
    source_mastering_metadata_details(stream).map(|value| value.map(|value| value.mastering))
}

/// The selected stream's exact ST 2086 primary coordinates and luminance.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn source_st2086(
    stream: &serde_json::Value,
) -> Result<Option<([u16; 8], u32, u32)>, String> {
    source_mastering_metadata_details(stream)?
        .map(st2086_triple)
        .transpose()
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn st2086_triple(source: ValidatedSourceMastering) -> Result<([u16; 8], u32, u32), String> {
    Ok((
        source.mastering.primaries_rgbw,
        u32::try_from(source.max_1e4_nits)
            .map_err(|_| "source mastering maximum overflow".to_owned())?,
        source.mastering.min_1e4_nits,
    ))
}

/// The frame projection is a separate observation of the same held object.
/// Require its absolute stream index on every frame and compare complete MDCV,
/// including maximum luminance, before projecting the fields mapping consumes.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn source_mastering_from_frames(
    stream: &serde_json::Value,
    document: &serde_json::Value,
    absolute_stream: u32,
) -> Result<Option<plurx_core::transcode::dv_processing::DvSourceMastering>, String> {
    source_mastering_details_from_frames(stream, document, absolute_stream, "HDR10")
        .map(|value| value.map(|value| value.mastering))
}

/// The complete ST 2086 triple under the rule of
/// [`source_mastering_from_frames`]: the selected stream's own declaration,
/// then at most three frames, every one on the selected absolute stream and
/// every complete record (maximum luminance included) equal to the others. A
/// projection with no record anywhere is `Ok(None)`; malformed or conflicting
/// records refuse rather than pick one.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
pub(crate) fn source_st2086_from_frames(
    stream: &serde_json::Value,
    document: &serde_json::Value,
    absolute_stream: u32,
) -> Result<Option<([u16; 8], u32, u32)>, String> {
    source_mastering_details_from_frames(stream, document, absolute_stream, "M7 ST 2086")?
        .map(st2086_triple)
        .transpose()
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn source_mastering_details_from_frames(
    stream: &serde_json::Value,
    document: &serde_json::Value,
    absolute_stream: u32,
    destination: &str,
) -> Result<Option<ValidatedSourceMastering>, String> {
    let invalid = || format!("targeted {destination} source frame MDCV malformed or conflicting");
    let frames = source_frame_records(document, absolute_stream, destination)?;
    let mut observed = source_mastering_metadata_details(stream)?;
    for frame in frames {
        if let Some(next) = source_mastering_metadata_details(frame).map_err(|_| invalid())? {
            if observed.is_some_and(|old| old != next) {
                return Err(invalid());
            }
            observed = Some(next);
        }
    }
    Ok(observed)
}

/// Both mastering projections require the same bounded absolute-stream fence.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn source_frame_records<'a>(
    document: &'a serde_json::Value,
    absolute_stream: u32,
    destination: &str,
) -> Result<&'a [serde_json::Value], String> {
    let invalid = || format!("targeted {destination} source frame MDCV malformed or conflicting");
    let frames = document
        .get("frames")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(invalid)?;
    if frames.len() > 3 {
        return Err(invalid());
    }
    for frame in frames {
        if frame
            .get("stream_index")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(absolute_stream))
        {
            return Err(format!(
                "targeted {destination} source frame MDCV selected stream differs"
            ));
        }
    }
    Ok(frames)
}

/// SDR consumes only actual minima; unrelated/absent coordinates and maxima
/// cannot invalidate that floor or become invented HDR mastering authority.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn source_mastering_minimum_from_frames(
    stream: &serde_json::Value,
    document: &serde_json::Value,
    absolute_stream: u32,
) -> Result<Option<u32>, String> {
    let mut observed = source_mastering_minimum(stream)?;
    for frame in source_frame_records(document, absolute_stream, "SDR")? {
        if let Some(next) = source_mastering_minimum(frame)? {
            if observed.is_some_and(|old| old != next) {
                return Err(
                    "targeted SDR source frame MDCV minimum malformed or conflicting".into(),
                );
            }
            observed = Some(next);
        }
    }
    Ok(observed)
}

/// What a master-grade HDR10 render re-declares: the selected stream's own
/// mastering display and, when it states them, its content light levels. A
/// stream with no mastering record (a Profile 5 source carries none) declares
/// nothing, and malformed or conflicting records are not repaired or guessed.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn source_master_metadata(
    stream: &serde_json::Value,
) -> Option<plurx_core::transcode::dv_processing::DvMasterMetadata> {
    let source = source_mastering_metadata_details(stream).ok()??;
    let maximum = u32::try_from(source.max_1e4_nits).ok()?;
    let mut light = None;
    for record in stream
        .get("side_data_list")?
        .as_array()?
        .iter()
        .filter(|record| {
            record
                .get("side_data_type")
                .and_then(serde_json::Value::as_str)
                == Some("Content light level metadata")
        })
    {
        let level = |field| {
            record
                .get(field)
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u16::try_from(value).ok())
        };
        let next = [level("max_content")?, level("max_average")?];
        if light.is_some_and(|old| old != next) {
            return None;
        }
        light = Some(next);
    }
    plurx_core::transcode::dv_processing::DvMasterMetadata::new(
        source.mastering.primaries_rgbw,
        maximum,
        source.mastering.min_1e4_nits,
        light,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(extra: serde_json::Value) -> serde_json::Value {
        let mut records = vec![serde_json::json!({
            "side_data_type":"Mastering display metadata",
            "red_x":"34000/50000","red_y":"16000/50000","green_x":"13250/50000","green_y":"34500/50000",
            "blue_x":"7500/50000","blue_y":"3000/50000","white_point_x":"15635/50000","white_point_y":"16450/50000",
            "min_luminance":"1/10000","max_luminance":"10000000/10000"})];
        records.extend(extra.as_array().expect("records").iter().cloned());
        serde_json::json!({"index":0,"side_data_list":records})
    }

    fn mastering_record() -> serde_json::Value {
        stream(serde_json::json!([]))["side_data_list"][0].clone()
    }

    fn frames(index: u64, count: usize, record: Option<serde_json::Value>) -> serde_json::Value {
        let frame = match record {
            Some(record) => serde_json::json!({"stream_index":index,"side_data_list":[record]}),
            None => serde_json::json!({"stream_index":index}),
        };
        serde_json::json!({"frames": vec![frame; count]})
    }

    const P3: [u16; 8] = [34000, 16000, 13250, 34500, 7500, 3000, 15635, 16450];

    #[test]
    fn st2086_frames_supply_the_triple_only_when_the_stream_declares_none() {
        let declared = stream(serde_json::json!([]));
        let silent = serde_json::json!({"index":7,"color_primaries":"bt2020"});
        // The stream's own declaration wins and agrees with its frames.
        assert_eq!(
            source_st2086_from_frames(&declared, &frames(7, 3, Some(mastering_record())), 7),
            Ok(Some((P3, 10_000_000, 1)))
        );
        assert_eq!(
            source_st2086_from_frames(&declared, &frames(7, 1, None), 7),
            Ok(Some((P3, 10_000_000, 1)))
        );
        // Frames alone supply the exact triple (the Eclipse shape).
        assert_eq!(
            source_st2086_from_frames(&silent, &frames(7, 3, Some(mastering_record())), 7),
            Ok(Some((P3, 10_000_000, 1)))
        );
        // Nothing declared anywhere is absence, not a refusal.
        assert_eq!(
            source_st2086_from_frames(&silent, &frames(7, 3, None), 7),
            Ok(None)
        );
        assert_eq!(
            source_st2086_from_frames(&silent, &serde_json::json!({"frames":[]}), 7),
            Ok(None)
        );
    }

    #[test]
    fn st2086_frames_refuse_conflicts_foreign_streams_and_unbounded_projections() {
        let silent = serde_json::json!({"index":7});
        let record = mastering_record();
        for (field, value) in [
            ("red_x", "33000/50000"),
            ("min_luminance", "50/10000"),
            ("max_luminance", "40000000/10000"),
        ] {
            let mut changed = record.clone();
            changed[field] = value.into();
            let disagreement = serde_json::json!({"frames":[
                {"stream_index":7,"side_data_list":[record.clone()]},
                {"stream_index":7,"side_data_list":[changed.clone()]}]});
            let error = source_st2086_from_frames(&silent, &disagreement, 7)
                .expect_err("conflicting frames");
            assert!(
                error.contains("malformed or conflicting"),
                "{field}: {error}"
            );
            // A stream declaration that disagrees with its own frames refuses.
            assert!(
                source_st2086_from_frames(
                    &stream(serde_json::json!([])),
                    &frames(7, 1, Some(changed)),
                    7
                )
                .is_err(),
                "stream/frame conflict {field}"
            );
        }
        let wrong = source_st2086_from_frames(&silent, &frames(8, 1, Some(record.clone())), 7)
            .expect_err("foreign stream");
        assert!(wrong.contains("selected stream differs"), "{wrong}");
        let mixed = serde_json::json!({"frames":[
            {"stream_index":7,"side_data_list":[record.clone()]},{"stream_index":8}]});
        assert!(source_st2086_from_frames(&silent, &mixed, 7).is_err());
        assert!(
            source_st2086_from_frames(&silent, &frames(7, 4, Some(record.clone())), 7).is_err()
        );
        for malformed in [serde_json::json!({}), serde_json::json!({"frames":{}})] {
            assert!(source_st2086_from_frames(&silent, &malformed, 7).is_err());
        }
        let mut incomplete = record;
        incomplete
            .as_object_mut()
            .expect("record")
            .remove("max_luminance");
        assert!(source_st2086_from_frames(&silent, &frames(7, 1, Some(incomplete)), 7).is_err());
    }

    #[test]
    fn master_grade_metadata_is_the_selected_streams_own_declaration() {
        use plurx_core::transcode::dv_processing::DvMasterMetadata;
        let p3 = [34000, 16000, 13250, 34500, 7500, 3000, 15635, 16450];
        let light = |content: u64, average: u64| {
            serde_json::json!([{"side_data_type":"Content light level metadata",
                "max_content":content,"max_average":average}])
        };
        assert_eq!(
            source_master_metadata(&stream(serde_json::json!([]))),
            Some(DvMasterMetadata::new(p3, 10_000_000, 1, None).expect("mastering only"))
        );
        assert_eq!(
            source_master_metadata(&stream(light(450, 120))),
            Some(DvMasterMetadata::new(p3, 10_000_000, 1, Some([450, 120])).expect("stated light"))
        );
        // An unstated (zero) MaxCLL stays unstated.
        assert_eq!(
            source_master_metadata(&stream(light(0, 120))),
            source_master_metadata(&stream(serde_json::json!([])))
        );
        // No mastering record (a Profile 5 source), a conflicting pair, an
        // unrepresentable level and a malformed record all declare nothing.
        for silent in [
            serde_json::json!({"index":0}),
            serde_json::json!({"index":0,"side_data_list":light(450, 120)}),
            stream(serde_json::json!([light(450, 120)[0], light(451, 120)[0]])),
            stream(light(70_000, 120)),
            stream(
                serde_json::json!([{"side_data_type":"Content light level metadata","max_content":"450"}]),
            ),
            stream(
                serde_json::json!([{"side_data_type":"Mastering display metadata","red_x":"unknown"}]),
            ),
        ] {
            assert_eq!(source_master_metadata(&silent), None, "{silent}");
        }
    }
}
