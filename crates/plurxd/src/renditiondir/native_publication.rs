//! Daemon access to the shared concrete Unix publication implementation.
#[cfg(test)]
use super::INIT_NAME;
pub(super) use plurx_core::fs_secure::publication::PublicationIo;
#[cfg(test)]
pub(super) use plurx_core::fs_secure::publication::PublicationRequest;

#[cfg(test)]
mod tests;
