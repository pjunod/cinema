//! Process-local attribution only; diagnostic counts never authorize a release.

pub(super) fn prometheus() -> String {
    let launches = plurx_core::process::source_lifetime::launch_snapshot();
    format!(
        "# HELP plurx_source_launch_total Observed launcher attempts since process start, including refused attempts.\n\
         # TYPE plurx_source_launch_total counter\n\
         plurx_source_launch_total{{access=\"no_source\"}} {}\n\
         plurx_source_launch_total{{access=\"metadata_only\"}} {}\n\
         plurx_source_launch_total{{access=\"brokered\"}} {}\n\
         plurx_source_launch_total{{access=\"unverified\"}} {}\n\
         plurx_source_launch_total{{access=\"unknown\"}} {}\n",
        launches.no_source,
        launches.metadata_only,
        launches.brokered,
        launches.unverified,
        launches.unknown.saturating_sub(launches.unverified),
    )
}
