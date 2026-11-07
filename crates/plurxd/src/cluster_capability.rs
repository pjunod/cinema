//! Type of an optional, actual cluster service. Standalone cannot construct one.
#[cfg(feature = "cluster")]
pub(crate) type Membership = plurx_core::cluster::membership::MembershipManager;

/// An absent cluster is represented by None, never a simulated manager/watch.
#[cfg(not(feature = "cluster"))]
#[derive(Clone)]
pub(crate) enum Membership {}
