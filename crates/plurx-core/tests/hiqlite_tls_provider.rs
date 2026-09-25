//! The Hiqlite HTTP clients must carry their own rustls crypto provider.
//!
//! `vendor/hiqlite` declares `reqwest` with `rustls-no-provider`, so every
//! `ClientBuilder::build` reads the process-wide default provider and panics
//! when there is none — a crash on any node that talks to a peer, not a test
//! curiosity. That provider used to arrive by accident, through an ungated
//! `cryptr/s3` pulling a second `reqwest` whose provider feature Cargo unified
//! onto the first. Gating that edge removed the provider;
//! `http_client::ensure_rustls_crypto_provider` now names `ring` deliberately,
//! at the point of client construction.
//!
//! This is the branch that removes dependency edges for a living, so the
//! assertion on `ring` matters as much as the one on the panic: declaring
//! `reqwest`'s `rustls` feature would also stop the panic, but it would move
//! the process onto `aws-lc-rs` and close off option A of
//! `docs/cluster/HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md` §3.6, whose acceptance
//! is an empty `cargo tree -i aws-lc-sys`.
//!
//! This test owns its own binary on purpose. The provider is process-global, so
//! a test sharing a binary with anything that installs one would pass whether or
//! not the fix is present. Alone, it starts from the same empty state a fresh
//! process does, and it fails — by panic inside `build` — the moment the
//! deliberate install goes away.

#![cfg(feature = "hiqlite-store")]

use plurx_core::cluster::migration::status::HiqliteClient;

#[tokio::test]
async fn hiqlite_client_construction_installs_the_ring_crypto_provider() {
    assert!(
        rustls::crypto::CryptoProvider::get_default().is_none(),
        "this test must be the only one in its binary: something installed a \
         provider before the client was built, so the assertions below would \
         hold with or without the deliberate install"
    );

    // The same constructor `cluster::migration::status` uses for a remote
    // maintenance client, and the one whose `reqwest` client panicked.
    let _client = HiqliteClient::remote(
        vec!["127.0.0.1:1".to_owned()],
        false,
        false,
        "not-used".to_owned(),
        true,
        None,
    )
    .await
    .expect("construct a remote Hiqlite client");

    let installed = rustls::crypto::CryptoProvider::get_default()
        .expect("building a Hiqlite client must install the process-wide provider");
    let installed_suites: Vec<_> = installed
        .cipher_suites
        .iter()
        .map(|suite| suite.suite())
        .collect();
    let ring_suites: Vec<_> = rustls::crypto::ring::default_provider()
        .cipher_suites
        .iter()
        .map(|suite| suite.suite())
        .collect();
    assert_eq!(
        installed_suites, ring_suites,
        "the fork installs `ring`; a different provider here means the process \
         negotiates something other than what `plurx-core`'s \
         `install_default_crypto_provider` asks for, and that an `aws-lc-rs` \
         edge has come back into the graph this branch exists to shrink"
    );
}
