//! Native durable storage and stable local identity.
//!
//! This layer constructs no consensus runtime. Replicated storage is composed
//! by the daemon through its separate cluster adapter.

pub mod backup;
pub(crate) mod durability;
pub mod identity;
pub mod mode;
pub mod native;
pub mod telemetry;
pub mod transition;

pub use identity::LocalIdentity;
pub use native::{
    open_native_store, open_native_store_at, open_native_store_at_locked, open_native_store_locked,
    NativeDirectoryLock, NativeStoreHandle,
};

pub mod ingress_quiescence;
