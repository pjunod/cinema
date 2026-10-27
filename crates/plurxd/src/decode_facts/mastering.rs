//! Selected-stream mastering metadata parsing shared by all platform policies.

/// SDR consumes only the selected stream's mastering minimum. Missing HDR
/// primaries do not invalidate this independent floor fact or invent BT.2020.
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

pub(crate) fn source_mastering_metadata(
    stream: &serde_json::Value,
) -> Result<Option<plurx_core::transcode::dv_processing::DvSourceMastering>, String> {
    source_mastering_metadata_details(stream).map(|value| value.map(|value| value.mastering))
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
    let invalid = || "targeted HDR10 source frame MDCV malformed or conflicting".to_owned();
    let frames = source_frame_records(document, absolute_stream, "HDR10")?;
    let mut observed = source_mastering_metadata_details(stream)?;
    for frame in frames {
        if let Some(next) = source_mastering_metadata_details(frame).map_err(|_| invalid())? {
            if observed.is_some_and(|old| old != next) {
                return Err(invalid());
            }
            observed = Some(next);
        }
    }
    Ok(observed.map(|value| value.mastering))
}

/// Both mastering projections require the same bounded absolute-stream fence.
#[cfg(any(target_os = "linux", test))]
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
