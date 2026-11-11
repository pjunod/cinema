//! One-packet head census from the same copy recipe and triple-output engine.
//! Its artificial fragment supplies promotion/clocks, never coverage rows.
use super::pipes::{spawn_source_generation, CopyPipeEvent, CopyPipeKind};
use crate::{
    prodrun::ProducerSlot,
    source_generation::{SourceGenerationDispatch, SourceRenditionOwners},
    source_input::SourceInput,
    transcode::source_actor::SourceGenerationAuthority,
};
use plurx_core::{
    domain::MediaFile,
    fmp4::{Fragment, FragmentReader, Init, PromotionInputs, Unit},
    segplan::{
        packet_clock::{PacketClockParser, SourcePacketObservation},
        ranges::SourceTimestamp,
    },
    transcode::CopyVideoOptions,
};
use std::{path::Path, sync::Arc, time::Instant};
use tokio_util::sync::CancellationToken;
pub(super) const MAX_CENSUS_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_HEAD_MP4: usize = 4 * 1024 * 1024;
const MAX_HEAD_CLOCK: usize = 64 * 1024;

pub(crate) struct CopyClockCensus {
    pub(crate) source_object_version: String,
    pub(crate) first_dts: SourceTimestamp,
    pub(crate) first_pts: i64,
    pub(crate) format_start: Option<SourceTimestamp>,
    pub(crate) raw_canonical_init: Init,
    pub(crate) raw_canonical_fragment: Fragment,
    pub(crate) raw_av_init: Init,
    pub(crate) promotion: PromotionInputs,
}
impl CopyClockCensus {
    pub(crate) fn verify_seek_landing(&self,packet:SourcePacketObservation,wanted_ms:i64)->Result<(),String>{
        verify_seek_landing(self.first_dts,self.first_pts,self.format_start,packet,wanted_ms)
    }
    /// The canonical row is clock-associated, not a timestamp read from a
    /// rebased output. Keep exact rational arithmetic until the wire's final
    /// millisecond representation. A B-frame head can produce origin -83ms.
    pub(crate) fn media_origin_ms(
        &self,
        canonical_dts: u64,
        canonical_title_start: u64,
        av_dts: u64,
        timescale: u32,
    ) -> Result<i64, String> {
        project_media_origin_ms(self.first_dts,self.format_start,canonical_dts,canonical_title_start,av_dts,timescale)
    }
}
fn project_media_origin_ms(first_dts:SourceTimestamp,format_start:Option<SourceTimestamp>,canonical_dts:u64,canonical_title_start:u64,av_dts:u64,timescale:u32)->Result<i64,String>{
        if timescale == 0 {
            return Err("copy_clock_invalid".into());
        }
        let format = format_start.ok_or("copy_format_start_unknown")?;
        let source = i128::from(first_dts.dts)
            .checked_mul(i128::from(first_dts.time_base_num))
            .and_then(|value| value.checked_mul(i128::from(timescale)))
            .and_then(|value| value.checked_mul(i128::from(format.time_base_den)))
            .ok_or("copy_clock_overflow")?;
        let delta = i128::from(canonical_dts)
            .checked_sub(i128::from(canonical_title_start))
            .and_then(|value| value.checked_sub(i128::from(av_dts)))
            .and_then(|value| value.checked_mul(i128::from(first_dts.time_base_den)))
            .and_then(|value| value.checked_mul(i128::from(format.time_base_den)))
            .ok_or("copy_clock_overflow")?;
        let zero = i128::from(format.dts)
            .checked_mul(i128::from(format.time_base_num))
            .and_then(|value| value.checked_mul(i128::from(first_dts.time_base_den)))
            .and_then(|value| value.checked_mul(i128::from(timescale)))
            .ok_or("copy_clock_overflow")?;
        let numerator = source
            .checked_add(delta)
            .and_then(|value| value.checked_sub(zero))
            .and_then(|value| value.checked_mul(1000))
            .ok_or("copy_clock_overflow")?;
        let denominator = i128::from(first_dts.time_base_den)
            .checked_mul(i128::from(timescale))
            .and_then(|value| value.checked_mul(i128::from(format.time_base_den)))
            .ok_or("copy_clock_overflow")?;
        let millis = numerator
            .checked_div(denominator)
            .ok_or("copy_clock_overflow")?;
        if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&millis) {
            return Err("copy_clock_overflow".into());
        }
        i64::try_from(millis).map_err(|_| "copy_clock_overflow".into())

}

fn verify_seek_landing(first_dts:SourceTimestamp,first_pts:i64,format:Option<SourceTimestamp>,packet:SourcePacketObservation,wanted_ms:i64)->Result<(),String>{
    let failed=||"copy_sync_unverified".to_owned();
    let format=format.ok_or_else(failed)?;
    if wanted_ms<0 || packet.flags&1==0 || packet.time_base_num!=first_dts.time_base_num || packet.time_base_den!=first_dts.time_base_den || packet.time_base_num==0 || packet.time_base_den==0 || format.time_base_num==0 || format.time_base_den==0{return Err(failed());}
    let denominator=i128::from(packet.time_base_den).checked_mul(i128::from(format.time_base_den)).ok_or_else(failed)?;
    let film_pts=|pts:i64|->Result<i128,String>{
        i128::from(pts).checked_mul(i128::from(packet.time_base_num)).and_then(|value|value.checked_mul(i128::from(format.time_base_den)))
            .and_then(|value|i128::from(format.dts).checked_mul(i128::from(format.time_base_num)).and_then(|zero|zero.checked_mul(i128::from(packet.time_base_den))).and_then(|zero|value.checked_sub(zero)))
            .and_then(|value|value.checked_mul(1000)).ok_or_else(failed)
    };
    let wanted=i128::from(wanted_ms).checked_mul(denominator).ok_or_else(failed)?;
    if wanted<film_pts(first_pts)? {
        if packet.dts==first_dts.dts && packet.pts==first_pts{return Ok(());}
        return Err(failed());
    }
    let difference=wanted.checked_sub(film_pts(packet.pts)?).ok_or_else(failed)?;
    let maximum=denominator.checked_mul(i128::from(plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS)*1000).ok_or_else(failed)?;
    if difference<0 || difference>maximum{return Err(failed());}
    Ok(())
}

fn one_fragment(bytes: &[u8]) -> Result<(Init, Fragment), String> {
    let mut reader = FragmentReader::new();
    reader.reject_reinitialization();
    reader.push(bytes);
    let mut init = None;
    let mut fragment = None;
    while let Some(unit) = reader
        .next_unit()
        .map_err(|_| "copy_head_mp4_invalid".to_owned())?
    {
        match unit {
            Unit::Init(value) => {
                if init.replace(value).is_some() {
                    return Err("copy_head_init_changed".into());
                }
            }
            Unit::Fragment(value) => {
                if fragment.replace(value).is_some() {
                    return Err("copy_head_packet_limit".into());
                }
            }
            Unit::Trailer => {}
        }
    }
    if reader.buffered() != 0 || !reader.saw_trailer() {
        return Err("copy_head_truncated".into());
    }
    let init = init.ok_or("copy_head_missing_init")?;
    let fragment = fragment.ok_or("copy_head_missing_fragment")?;
    let video = init.video().ok_or("copy_head_missing_video")?;
    if init.tracks.len() != 1
        || fragment.tracks.len() != 1
        || fragment
            .track(video.id)
            .is_none_or(|track| track.sample_count() != 1)
    {
        return Err("copy_head_packet_limit".into());
    }
    Ok((init, fragment))
}
fn one_clock(bytes: &[u8]) -> Result<SourcePacketObservation, String> {
    if !bytes.ends_with(b"\n") {
        return Err("copy_head_clock_truncated".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "copy_head_clock_encoding".to_owned())?;
    let mut parser = PacketClockParser::new();
    let mut packet = None;
    for line in text.lines() {
        if let Some(value) = parser
            .push_line(line)
            .map_err(|_| "copy_head_clock_invalid".to_owned())?
        {
            if packet.replace(value).is_some() {
                return Err("copy_head_packet_limit".into());
            }
        }
    }
    parser
        .finish()
        .map_err(|_| "copy_head_clock_truncated".to_owned())?;
    let packet = packet.ok_or("copy_head_clock_missing")?;
    if packet.sequence != 0 || packet.flags & 1 == 0 || packet.duration == 0 {
        return Err("copy_head_not_sync".into());
    }
    Ok(packet)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_source_census(
    input: SourceInput,
    object: String,
    file: &MediaFile,
    video: CopyVideoOptions,
    owners: &SourceRenditionOwners,
    dispatch: SourceGenerationDispatch,
    authority: SourceGenerationAuthority,
    slot: Arc<ProducerSlot>,
    runtime_cache: &Path,
    deadline: Instant,
    stop: &CancellationToken,
) -> Result<CopyClockCensus, String> {
    if let Err(error) = input.seal_read_ceiling(MAX_CENSUS_SOURCE_BYTES) {
        let _ = input.close_and_wait().await;
        return Err(error);
    }
    let mut args =
        match plurx_core::transcode::copy_head_census_args(file, video, "pipe:6", "pipe:7") {
            Ok(args) => args,
            Err(error) => {
                let _ = input.close_and_wait().await;
                return Err(error.into());
            }
        };
    // This head-only observation does not change a canonical row recipe.
    // AVFormatContext.start_time is physically printed only at info level.
    if let Some(level) = args.iter().position(|arg| arg == "-loglevel") {
        if let Some(value) = args.get_mut(level + 1) {
            *value = "info".into();
        }
    }
    let mut format = super::census_format::InputFormatStart::new(input.url());
    let mut running = spawn_source_generation(
        input,
        &file.path,
        &args,
        runtime_cache,
        owners,
        dispatch,
        authority,
        slot,
    )
    .await?;
    let mut av = Vec::new();
    let mut canonical = Vec::new();
    let mut clock = Vec::new();
    let mut result = {
        let observed = async {
            loop {
                let pipes = running.pipes.as_mut().ok_or("copy_head_missing_readers")?;
                let event = pipes.next().await.ok_or("copy_head_readers_lost")?;
                match event {
                    CopyPipeEvent::Data(kind, bytes) => {
                        let target = match kind {
                            CopyPipeKind::Av => Some((&mut av, MAX_HEAD_MP4)),
                            CopyPipeKind::Canonical => Some((&mut canonical, MAX_HEAD_MP4)),
                            CopyPipeKind::Clock => Some((&mut clock, MAX_HEAD_CLOCK)),
                            CopyPipeKind::Diagnostic => {
                                format.feed(&bytes);
                                None
                            }
                        };
                        if let Some((target, max)) = target {
                            if target.len().saturating_add(bytes.len()) > max {
                                return Err("copy_head_output_limit".to_owned());
                            }
                            target.extend_from_slice(&bytes);
                        }
                    }
                    CopyPipeEvent::Failed(_, error) => return Err(error),
                    CopyPipeEvent::Eof(_) => {
                        if pipes.all_eof() {
                            return Ok::<_, String>(());
                        }
                    }
                }
            }
        };
        tokio::pin!(observed);
        tokio::select! {biased;()=stop.cancelled()=>Err("copy_range_cancelled".to_owned()),()=tokio::time::sleep_until(tokio::time::Instant::from_std(deadline))=>Err("copy_head_deadline".to_owned()),result=&mut observed=>result}
    };
    if result.is_ok() {
        let _ = running
            .slot
            .request_registered_completion(&running.registration)
            .await;
        let status = tokio::select! {biased;()=stop.cancelled()=>{result=Err("copy_range_cancelled".into());None},()=tokio::time::sleep_until(tokio::time::Instant::from_std(deadline))=>{result=Err("copy_head_deadline".into());None},status=running.registration.wait_child_exit()=>Some(status)};
        if status.is_some_and(|status| !status.success()) {
            result = Err("copy_head_child_failed".into());
        }
    }
    let receipt = running.settle(result.is_ok()).await?;
    result?;
    if !receipt.exit_success() {
        return Err("copy_head_child_failed".into());
    }
    let (raw_canonical_init, raw_canonical_fragment) = one_fragment(&canonical)?;
    let (raw_av_init, _) = one_fragment(&av)?;
    let packet = one_clock(&clock)?;
    let track = raw_canonical_fragment
        .track(
            raw_canonical_init
                .video()
                .ok_or("copy_head_missing_video")?
                .id,
        )
        .ok_or("copy_head_missing_video")?;
    if track
        .samples()
        .next()
        .is_none_or(|sample| u64::from(sample.size) != packet.size)
    {
        return Err("copy_head_clock_sample_mismatch".into());
    }
    let promotion = PromotionInputs::from_fragment(&raw_canonical_fragment, &raw_canonical_init);
    Ok(CopyClockCensus {
        source_object_version: object,
        first_dts: SourceTimestamp {
            dts: packet.dts,
            time_base_num: packet.time_base_num,
            time_base_den: packet.time_base_den,
        },
        first_pts: packet.pts,
        format_start: format.finish(),
        raw_canonical_init,
        raw_canonical_fragment,
        raw_av_init,
        promotion,
    })
}

#[cfg(test)]
mod clock_projection_tests {
    use super::*;
    fn stamp(dts:i64,num:u32,den:u32)->SourceTimestamp{SourceTimestamp{dts,time_base_num:num,time_base_den:den}}
    #[test]
    fn signed_origin_uses_container_zero_and_canonical_dts_not_video_pts(){
        assert_eq!(project_media_origin_ms(stamp(-7500,1,90000),Some(stamp(0,1,1000000)),0,0,0,90000).unwrap(),-83);
        assert_eq!(project_media_origin_ms(stamp(-7500,1,90000),Some(stamp(-21333,1,1000000)),0,0,0,90000).unwrap(),-62);
        assert_eq!(project_media_origin_ms(stamp(-7500,1,90000),None,0,0,0,90000).unwrap_err(),"copy_format_start_unknown");
    }
    #[test]
    fn nonzero_seek_and_av_tfdt_are_independent_rational_coordinates(){
        let source=stamp(-7500,1,90000);let format=Some(stamp(0,1,1000000));
        assert_eq!(project_media_origin_ms(source,format,486000000,0,0,90000).unwrap(),5399916);
        assert_eq!(project_media_origin_ms(source,format,486000000,0,486000000,90000).unwrap(),-83);
        assert!(project_media_origin_ms(source,format,0,0,0,0).is_err());
        assert!(project_media_origin_ms(stamp(i64::MAX,u32::MAX,u32::MAX),Some(stamp(0,1,u32::MAX)),u64::MAX,0,0,u32::MAX).is_err());
    }
    #[test]
    fn seek_landing_is_bounded_in_format_film_pts_and_delayed_video_needs_exact_head(){
        let first=stamp(-80,1,1000);let format=Some(stamp(0,1,1000000));
        let mut packet=SourcePacketObservation{sequence:0,dts:5399920,pts:5400000,duration:40,size:100,checksum:0,side_data_count:0,time_base_num:1,time_base_den:1000,flags:1};
        assert!(verify_seek_landing(first,0,format,packet,5400000).is_ok());
        packet.pts=5384000;assert!(verify_seek_landing(first,0,format,packet,5400000).is_ok());
        packet.pts=5383999;assert!(verify_seek_landing(first,0,format,packet,5400000).is_err());
        packet.pts=5400001;assert!(verify_seek_landing(first,0,format,packet,5400000).is_err());
        packet.dts=9920;packet.pts=10000;let delayed=stamp(9920,1,1000);
        assert!(verify_seek_landing(delayed,10000,format,packet,0).is_ok());
        packet.dts+=1;assert!(verify_seek_landing(delayed,10000,format,packet,0).is_err());
        packet.dts=9920;packet.flags=0;assert!(verify_seek_landing(delayed,10000,format,packet,0).is_err());
    }

}
