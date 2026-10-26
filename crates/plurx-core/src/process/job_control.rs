//! LIVE same-object Job control. Neither a successful termination request nor
//! closing/reopening a name is a population proof. No handle leaves ChildJob.
use super::ChildJob;
use std::io;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(super) use windows::WindowsJob;

impl ChildJob {
    /// Request termination through the retained original Job, including when its
    /// root has already exited. This is not a wait, extinction or refund.
    pub(crate) fn terminate_containment(&self) -> io::Result<()> {
        #[cfg(windows)]
        {
            self.job.terminate_containment()
        }
        #[cfg(not(windows))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no positive live Job containment on this platform",
        ))
    }

    /// Actual active-process count of the ORIGINAL Job. Zero is useful only
    /// after the graph seals every entry and retains this object; this operation
    /// alone grants no physical settlement, restart proof or acquisition right.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "The retained original-Job live-closure adapter is not yet a production settlement route; portable negative proofs still require this fail-closed query."
        )
    )]
    pub(crate) fn containment_population(&self) -> io::Result<u32> {
        #[cfg(windows)]
        {
            self.job.containment_population()
        }
        #[cfg(not(windows))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no positive live Job containment on this platform",
        ))
    }
}
