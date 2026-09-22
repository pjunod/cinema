# Vendored s3-simple 0.8.0

This directory is the crates.io `s3-simple` 0.8.0 package, licensed under
Apache-2.0. Plurx carries dependency-only patches; no source file is modified.

**Owner:** Paul Junod (repository owner).

| # | Patch | Kind | Upstream | Drop condition |
|---:|---|---|---|---|
| 1 | `quick-xml` raised from 0.39 to 0.41 | dependency-only | — | cryptr accepts `s3-simple` 0.9 or newer, whose manifest already requires `quick-xml` 0.42. |
| 2 | Unreferenced `aws-lc-rs`, `aws-lc-sys`, `quinn` and `quinn-proto` edges removed | dependency-only | — | cryptr accepts `s3-simple` 0.9 or newer, whose manifest already declares none of the four. |

- `quick-xml` 0.39.4 is the release RustSec marks vulnerable under
  RUSTSEC-2026-0194 and RUSTSEC-2026-0195, and upstream 0.8.0 requires
  `quick-xml ^0.39`. Raising the constraint keeps the upstream API — the crate
  calls only `quick_xml::de::from_str`, `quick_xml::de::from_reader` and
  `quick_xml::de::DeError` — and avoids an advisory ignore.
- Upstream 0.8.0 declares `aws-lc-rs`, `aws-lc-sys = "0.39"`, `quinn` and
  `quinn-proto` as non-optional dependencies that `src/` never references.
  `aws-lc-sys ^0.39` cannot resolve past 0.39.1, which is the version
  `deny.toml` forbids and the second CMake/C build of the same library that
  `docs/cluster/HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md` §2.3 traces to this
  package. Removing the four edges is not a plurx invention: upstream
  `s3-simple` 0.9.0 declares none of them, and `aws-lc-rs` still reaches the
  graph transitively through `reqwest`'s rustls feature, at the single version
  the rest of the graph already uses.

Why this directory still exists after M1. Hiqlite 0.14 used to enable cryptr's
S3 feature unconditionally, which put this otherwise unused package in Plurx's
production resolution; `vendor/hiqlite` patch 16 gated that edge behind
Hiqlite's own `backup`/`s3` features, so the workspace graph no longer contains
`s3-simple` at all. What that gate does **not** do is make the configuration
Hiqlite still advertises safe: enabling `backup` or `s3` resolves registry
`s3-simple` 0.8.0 and brings back both the advisory-affected parser and the
banned `aws-lc-sys` version. cryptr 0.10.0 requires `s3-simple ^0.8.0`, so the
fixed upstream 0.9.x cannot be selected in its place.

The override therefore lives in
[`vendor/hiqlite/Cargo.toml`](../hiqlite/Cargo.toml)'s own
`[patch.crates-io]`, which is the manifest cargo honours when that feature is
enabled, and **not** in the workspace root, where the same row would patch a
package the workspace graph does not contain and would be reported as an unused
patch on every cargo invocation.

Because this package is outside the workspace graph, it is outside the
lockfile `scripts/vendor-audit-lock` synthesizes for `cargo audit`, and it is
not listed in that script's `VENDORED` table. What pins it instead is
`tests/operations/test_hiqlite_patch_ledger.py`, which resolves
`vendor/hiqlite/Cargo.lock` — the fork's own lockfile, which covers the
advertised `backup`/`s3` graph — against `deny.toml`'s ban list and against the
`quick-xml` floor recorded above. That contract must remain until this
directory is removed.

Remove this vendor when cryptr requires `s3-simple` 0.9 or newer, or when an
upstream Hiqlite release no longer reaches `s3-simple` at all.

Source: <https://crates.io/crates/s3-simple/0.8.0>

Only `Cargo.toml` and `src/` are load-bearing in Plurx's build. The retained
license and README are upstream provenance, not an in-place test suite; the
workspace excludes this directory deliberately.
