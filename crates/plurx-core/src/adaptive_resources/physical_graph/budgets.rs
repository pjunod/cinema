//! Combined bounds for the concrete in-core adapters, before acquisition.
//!
//! Roots reserve all three pipe slots even when a particular command uses fewer.
//! Retained source read buffers and audited drain buffers share one allocation;
//! cancellation or task completion without consumption does not free that share.
//! This does not estimate arbitrary legacy futures or kernel containment costs,
//! and is not a production demand model or physical-close proof.
use super::*;

pub(super) const ROOT_PIPE_SLOTS: u64 = 3;

#[derive(Default)]
pub(super) struct BudgetDelta {
    pub roots: u64,
    #[cfg(target_os = "linux")]
    pub native_root: Option<crate::process::contained_launch::FiniteNativeAllowance>,
    pub sources: u64,
    pub read_bytes: u64,
    pub drains: u64,
    pub finite_memory_bytes: u64,
}

fn overflow() -> io::Error {
    io::Error::other("physical graph adapter budget overflow")
}

/// Called under the graph members lock. The short ledger read is released before
/// any native call, task creation or await. No other adapter can concurrently
/// allocate against the same graph between this check and retained acquisition.
pub(super) fn check(
    ledger: &ResourceLedger,
    custody: &GraphCustody,
    members: &Members,
    delta: BudgetDelta,
) -> io::Result<()> {
    let roots = (members.roots.len() as u64)
        .checked_add(delta.roots)
        .ok_or_else(overflow)?;
    let sources = (members.source_files.len() as u64)
        .checked_add(delta.sources)
        .ok_or_else(overflow)?;
    let root_handles = roots.checked_mul(ROOT_PIPE_SLOTS).ok_or_else(overflow)?;
    let mut drains = delta.drains;
    let finite_memory = delta.finite_memory_bytes;
    #[cfg(target_os = "linux")]
    let (root_handles, mut finite_memory) = {
        let allowance = delta
            .native_root
            .or_else(|| members.native_setup.as_ref().map(|setup| setup.allowance));
        match allowance {
            Some(allowance) => (
                root_handles
                    .checked_sub(ROOT_PIPE_SLOTS)
                    .and_then(|handles| handles.checked_add(allowance.handles))
                    .ok_or_else(overflow)?,
                finite_memory
                    .checked_add(allowance.requested_bytes)
                    .ok_or_else(overflow)?,
            ),
            None => (root_handles, finite_memory),
        }
    };
    #[cfg(not(target_os = "linux"))]
    let mut finite_memory = finite_memory;
    let handles = root_handles.checked_add(sources).ok_or_else(overflow)?;
    for writer in &members.writers {
        #[cfg(unix)]
        if let Some(finite) = &writer.finite {
            // Joined finite records may still retain diagnostics and pipe
            // state. Keep the entire declared partition charged here.
            finite_memory = finite_memory
                .checked_add(finite.minimum_memory_bytes())
                .ok_or_else(overflow)?;
            continue;
        }
        #[cfg(target_os = "linux")]
        if let Some(copy) = &writer.copy {
            finite_memory = finite_memory
                .checked_add(copy.minimum_memory_bytes())
                .ok_or_else(overflow)?;
            continue;
        }
        if !writer.joined {
            // A generic future can retain arbitrary memory or export resources.
            // Its mere JoinHandle is not a finite cost for another adapter to
            // borrow around. The legacy API remains non-certifying.
            if writer.drain.is_none() {
                return Err(io::Error::other(
                    "legacy writer has no bounded adapter budget",
                ));
            }
            drains = drains.checked_add(1).ok_or_else(overflow)?;
        }
    }
    let mut memory = drains
        .checked_mul(pipe_drains::TASK_BYTES)
        .and_then(|value| value.checked_add(delta.read_bytes))
        .and_then(|value| value.checked_add(finite_memory))
        .ok_or_else(overflow)?;
    for source in &members.source_files {
        memory = memory
            .checked_add(source.retained_read_bytes())
            .ok_or_else(overflow)?;
    }
    let state = ledger
        .inner
        .lock()
        .map_err(|_| io::Error::other("resource ledger unavailable"))?;
    let entry = state
        .entries
        .get(&custody.binding.reservation)
        .filter(|entry| entry.binding == custody.binding)
        .ok_or_else(|| io::Error::other("physical reservation unavailable"))?;
    if state.invalid
        || handles > entry.resources.handles
        || memory > entry.resources.peak_memory_bytes
    {
        return Err(io::Error::other(
            "physical graph adapters exceed charged budget",
        ));
    }
    Ok(())
}
