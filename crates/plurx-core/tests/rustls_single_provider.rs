//! `ring` must be the only rustls provider the workspace compiles.
//!
//! K-08 §3.6 option A (`docs/cluster/HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md`)
//! removed the three routes by which `vendor/hiqlite` asked for `aws-lc-rs`:
//! `axum-server/tls-rustls`, `rustls/prefer-post-quantum`, and `tokio-rustls`'s
//! default features. The process already installed `ring` explicitly, so that
//! provider was compiled (with its CMake/C `aws-lc-sys` build) and never used.
//!
//! The observable difference is how rustls behaves when nothing has installed a
//! provider yet. With exactly one provider feature compiled it selects that
//! provider itself; with two it refuses to choose and panics. So this test
//! builds a client config in a fresh process and requires rustls to pick `ring`
//! unaided. Put any of the three features back and `ClientConfig::builder`
//! panics here, which is the point: a second provider cannot return quietly.
//!
//! This test owns its binary for the same reason `hiqlite_tls_provider.rs`
//! does: the provider is process-global, so sharing a binary with anything that
//! installs one would let the assertion pass with two providers compiled.

#![cfg(feature = "hiqlite-store")]

use std::panic::{catch_unwind, AssertUnwindSafe};

#[test]
fn rustls_selects_ring_unaided_because_it_is_the_only_provider_compiled() {
    assert!(
        rustls::crypto::CryptoProvider::get_default().is_none(),
        "this test must be the only one in its binary: a provider is already \
         installed, so rustls never has to choose one below"
    );

    let config = catch_unwind(AssertUnwindSafe(|| {
        rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth()
    }))
    .unwrap_or_else(|_| {
        panic!(
            "rustls could not pick a provider on its own, so more than one \
             provider feature is compiled into the workspace; K-08 option A \
             requires ring alone (check `cargo tree -i aws-lc-rs`)"
        )
    });

    let ring = rustls::crypto::ring::default_provider();
    let provider = config.crypto_provider();
    let suites = |provider: &rustls::crypto::CryptoProvider| {
        provider
            .cipher_suites
            .iter()
            .map(|suite| suite.suite())
            .collect::<Vec<_>>()
    };
    let groups = |provider: &rustls::crypto::CryptoProvider| {
        provider
            .kx_groups
            .iter()
            .map(|group| group.name())
            .collect::<Vec<_>>()
    };
    assert_eq!(suites(provider), suites(&ring));
    assert_eq!(groups(provider), groups(&ring));
}
