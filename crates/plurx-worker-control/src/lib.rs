//! Deterministic decisions for the library worker's post-claim protocol.
//!
//! Tokens, guards, async I/O and scheduling remain in the daemon driver.
#![no_std]
#![forbid(unsafe_code)]

pub mod library_lifecycle;

#[cfg(kani)]
mod proofs {
    include!("proofs.rs");
}
