use super::*;

#[test]
fn request_decoder_planar_download_feeds_software_frame_vulkan_renderer() {
    let facts = DecodeFacts::from_ffprobe_json(
            &serde_json::json!({"streams": [{"index": 0, "codec_type": "video", "codec_name": "hevc",
                "profile": "Main 10", "pix_fmt": "yuv420p10le", "width": 3840, "height": 2160,
                "color_transfer": "smpte2084", "color_primaries": "bt2020", "color_space": "bt2020nc"}]}),
            DecodeSourceIdentity::from_sha256("a".repeat(64)).expect("fixture source digest is valid"),
        ).expect("Main10 fixture produces valid decode facts");
    assert!(pipeline_accepts_decode(
        Pipeline::LibplaceboSoftware,
        DecodeBackend::V4l2Request
    ));
    let surface = surface_contract(
        DecodeBackend::V4l2Request,
        Pipeline::LibplaceboSoftware,
        &facts,
        Encoder::Software,
        SubtitleRendering::None,
    );
    assert_eq!(surface.decode_domain(), FrameDomain::DrmPrime);
    assert_eq!(surface.decoder_download_format(), Some("yuv420p10le"));
    assert_eq!(surface.renderer_domain(), FrameDomain::SystemMemory);
    assert_eq!(surface.renderer_upload_format(), None);
    assert_eq!(surface.renderer_download_format(), None);
    assert_eq!(surface.encoder_upload_domain(), None);
}
