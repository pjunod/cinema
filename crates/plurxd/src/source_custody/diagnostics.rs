//! Process-local attribution only; diagnostic counts never authorize a release.
use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex};

const MAX_PURPOSES: usize = 256;
static POISONED: LazyLock<Mutex<BTreeMap<&'static str, u64>>> = LazyLock::new(Default::default);

pub(super) fn record_poisoned(purpose: &'static str) {
    let mut counts = POISONED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = if counts.contains_key(purpose) || counts.len() < MAX_PURPOSES - 1 {
        purpose
    } else {
        "other"
    };
    let count = counts.entry(key).or_default();
    *count = count.saturating_add(1);
}

fn label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

pub(super) fn prometheus() -> String {
    use std::fmt::Write;
    let launches = plurx_core::process::source_lifetime::launch_snapshot();
    let mut text = format!(
        "# HELP plurx_source_launch_total Observed launcher attempts since process start, including refused attempts.\n\
         # TYPE plurx_source_launch_total counter\n\
         plurx_source_launch_total{{access=\"no_source\"}} {}\n\
         plurx_source_launch_total{{access=\"metadata_only\"}} {}\n\
         plurx_source_launch_total{{access=\"brokered\"}} {}\n\
         plurx_source_launch_total{{access=\"unverified\"}} {}\n\
         plurx_source_launch_total{{access=\"unknown\"}} {}\n\
         # HELP plurx_source_witness_poisoned_total Reader witnesses first invalidated by a named launch purpose in this process.\n\
         # TYPE plurx_source_witness_poisoned_total counter\n",
        launches.no_source, launches.metadata_only, launches.brokered,
        launches.unverified, launches.unknown.saturating_sub(launches.unverified),
    );
    let counts = POISONED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (purpose, count) in counts.iter() {
        let _ = writeln!(
            text,
            "plurx_source_witness_poisoned_total{{purpose=\"{}\"}} {count}",
            label(purpose)
        );
    }
    text
}
