//! Qualification adapter for the real pure policy decision (no playback routing).
use plurx_core::{scan::dv_title::DvTitleStats, transcode::dv_processing::DvExportPolicy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("usage: dv_target_policy SCAN_JSON BUCKET".into());
    }
    let stats: DvTitleStats = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    let (policy, reason) =
        DvExportPolicy::for_display_target((64, 64), args[2].parse()?, Some(stats))?;
    println!(
        "{}",
        serde_json::json!({
            "output_policy": policy.output_policy(),
            "endpoints": policy.display_target().map(|target| target.endpoint_args()),

            "decline": reason.map(|reason| reason.reason()),
        })
    );
    Ok(())
}
