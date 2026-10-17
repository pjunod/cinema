use super::session_delivered_processing_shape;
use crate::transcode::SessionKind;
use plurx_core::transcode::OutputGrade;

#[test]
fn prepared_p81_marker_projects_dv_without_changing_physical_transcode() {
    let kind = SessionKind::Transcode { height: 1080 };
    assert_eq!(
        session_delivered_processing_shape(None, &kind, OutputGrade::Hdr10, Some(8)),
        (Some("dolby_vision"), Some(8))
    );
    assert_eq!(
        session_delivered_processing_shape(None, &kind, OutputGrade::Hdr10, None),
        (None, None)
    );
    assert_eq!(
        session_delivered_processing_shape(None, &kind, OutputGrade::Sdr, Some(8)),
        (None, None)
    );
    assert_eq!(
        session_delivered_processing_shape(None, &kind, OutputGrade::Hdr10, Some(7)),
        (None, None)
    );
}
