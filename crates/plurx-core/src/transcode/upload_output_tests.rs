use super::*;

/// A directory keeps FFmpeg's own rename; an upload endpoint swaps it
/// for `-method PUT` and changes no other muxer flag, which is what
/// keeps the objects byte-identical between the two.
#[test]
fn scratch_charge_upload_output_swaps_temp_file_for_put_and_nothing_else() {
    assert_eq!(
        hls_write_args("/tmp/s", &["independent_segments"]),
        ["-hls_flags", "independent_segments+temp_file"]
    );
    assert_eq!(hls_write_args("/tmp/s", &[]), ["-hls_flags", "temp_file"]);
    let upload = "http://127.0.0.1:40001/0123456789abcdef0123456789abcdef/0";
    assert!(hls_output_is_upload(upload));
    assert_eq!(
        hls_write_args(upload, &["independent_segments"]),
        ["-hls_flags", "independent_segments", "-method", "PUT"]
    );
    assert_eq!(hls_write_args(upload, &[]), ["-method", "PUT"]);
}
