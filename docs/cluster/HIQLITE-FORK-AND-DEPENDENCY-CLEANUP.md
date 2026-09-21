# The hiqlite fork and what it drags in — decide the ownership, then cut the graph

**Status:** ready for review · **Executes:** §4.3 / F-sc-11 / F-hist-12 /
F-build-ops-codehealth-5, -6, -7 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `0f02b7ea`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read §2 before anything else: it contains the three manifests that decide
this, the executed `cargo tree` output, and the three independent feature
edges the review treats as one. §3 separates a *decision* (who owns the
fork) from four *cuts* (feature edges, a deleted vendor, a ban, a
build-time feature), and the cuts are the only part with a measurable
acceptance number. Execute §5 in order. If a step seems to require renaming
the vendored crate, banning `aws-lc-sys` outright, changing what the daemon
does at runtime, or turning a shipped configuration into one that nothing
compiles, stop and flag it — each is refused in §4, with its reason.

**Correction to the review:** five, and the fourth changes the plan.

1. **Removing the cryptr `s3` edge does not remove the second `reqwest`.**
   §4.3 lists `reqwest 0.12 + 0.13` among what that edge pulls.
   [vendor/hiqlite/Cargo.toml:286-294](../../vendor/hiqlite/Cargo.toml)
   depends on `reqwest = "0.13"` directly, so `reqwest 0.12 + 0.13` survives
   the fix. What the edge does pull, and what goes away with it:
   `s3-simple`, `quinn`/`quinn-proto`/`quinn-udp`, `aws-lc-sys 0.39.1`, and
   cryptr's own `reqwest` edge. Three of four.
2. **There are three independent edges to `aws-lc-rs`, not one.** §2.4
   traces them with `cargo tree -e features`. Two survive the cryptr fix and
   both live in the fork's own manifest:
   [`axum-server` features `["tls-rustls", "tls-rustls-no-provider"]`](../../vendor/hiqlite/Cargo.toml)
   (line 148-153, where `tls-rustls` enables `rustls/aws-lc-rs`), and
   [`rustls` features including `prefer-post-quantum`](../../vendor/hiqlite/Cargo.toml)
   (line 323-332), which rustls 0.23.42 defines as
   `prefer-post-quantum = ["aws_lc_rs"]`. `cargo tree -i aws-lc-sys` →
   empty requires all three.
3. **The workspace already does what the review asks the workspace to do.**
   §4.3 proposes adding `rustls = { default-features = false, features =
   ["ring","std","tls12"] }` to the workspace.
   [plurx-core/Cargo.toml:40-47](../../crates/plurx-core/Cargo.toml) already
   declares exactly that (`features = ["ring", "std"]`), and
   [migration.rs:1115-1116](../../crates/plurx-core/src/cluster/migration.rs)
   installs `rustls::crypto::ring::default_provider()` process-wide. Cargo
   features are additive, so a workspace declaration cannot subtract the
   fork's. The change belongs in the fork's manifest or nowhere.
4. **Renaming the vendored crate is not merely optional — it is expensive,
   for a reason the review does not name.** The patch stack is applied
   through `[patch.crates-io]`
   ([Cargo.toml:144-152](../../Cargo.toml)), which substitutes a package *by
   its registry name*. `vendor/hiqlite` depends on `hiqlite-wal` by name
   ([vendor/hiqlite/Cargo.toml:237-239](../../vendor/hiqlite/Cargo.toml)) and
   `cryptr` reaches `s3-simple` and `rust_decimal` by name as well — all
   three are substituted the same way. Rename the package to `plurx-raft`
   and the substitution stops applying to every transitive edge, so the
   vendored copies would have to be re-plumbed by hand. §3.1 therefore
   recommends **not** renaming, and says what to do instead to get the
   ownership the finding actually asks for.
5. **`cluster-wal-check` already runs on PRs.** F-build-ops-codehealth-5
   asks to "run `cluster-wal-check` + `cluster-store-check` on a schedule".
   [ci.yml:818-835](../../.github/workflows/ci.yml) runs
   `make cluster-wal-check` whenever the scope selector says
   `cluster_auth == 'true'`, and `vendor/hiqlite/**` is in `cluster.auth`'s
   path list ([points.toml:490-493](../../validation/points.toml)). The
   missing piece is upstream tracking, not a lane.

## 1. Objective

Board id **K-08**. Record, once and in the architecture document, that
hiqlite is a maintained private fork with an owner and a per-patch exit
condition; then remove the dependencies nothing calls, with
`cargo tree -d` before and after as the acceptance number and `cargo check`
wall time as the second. No runtime behaviour changes anywhere in this
plan — if a cut would change what the daemon does, it is out of scope by
construction.

## 2. Contract today

Re-verify every line, number and command output below at build time.

### 2.1 The fork, in three manifests

```toml
# Cargo.toml:38-39 (workspace.dependencies)
hiqlite = { version = "=0.14.0", default-features = false, features = ["auto-heal", "macros", "sqlite"] }
hiqlite-wal = { version = "=0.14.0" }

# Cargo.toml:144-152
# Patch Hiqlite's sparse-membership and WAL-recovery compatibility plus unused
# optional dependency edges without advisory ignores. Path packages are
# skipped by cargo-audit, so rust-audit.yml also scans a synthesized
# registry-provenance lockfile built by scripts/vendor-audit-lock.
[patch.crates-io]
hiqlite = { path = "vendor/hiqlite" }
hiqlite-wal = { path = "vendor/hiqlite-wal" }
s3-simple = { path = "vendor/s3-simple" }
rust_decimal = { path = "vendor/rust_decimal" }
```

The four vendored packages are `exclude`d from the workspace
([Cargo.toml:10-15](../../Cargo.toml)) and each carries its own lockfile.

```toml
# vendor/hiqlite/Cargo.toml:45-52 and :97
[features]
__profiling = ["dep:console-subscriber"]
auto-heal = ["hiqlite-wal/auto-heal"]
backup = ["dep:cron", "s3", "sqlite"]
…
s3 = ["backup"]

# :190-192
[dependencies.cryptr]
version = "0.10"
features = ["s3"]

# :148-153
[dependencies.axum-server]
version = "0.8"
features = ["tls-rustls", "tls-rustls-no-provider"]

# :286-294
[dependencies.reqwest]
version = "0.13"
features = ["charset", "http2", "json", "rustls-no-provider"]
default-features = false

# :323-332
[dependencies.rustls]
version = "0.23.37"
features = ["logging", "prefer-post-quantum", "std", "tls12", "ring"]
default-features = false
```

plurx builds the fork without `backup` and without `s3`
(`features = ["auto-heal", "macros", "sqlite"]`), and the `cryptr` edge at
line 190-192 is unconditional, so `cryptr/s3` is on in every build.

From the registry copy of cryptr 0.10.0, which is what makes the fix a
one-liner rather than a fork of cryptr:

```toml
# ~/.cargo/registry/…/cryptr-0.10.0/Cargo.toml
default = []
s3 = ["streaming", "dep:reqwest", "dep:s3-simple", "dep:tokio-util", "dep:quinn-proto"]
streaming = ["dep:reqwest"]
```

cryptr's defaults are already empty. `default-features = false` on the
cryptr edge therefore removes nothing; only deleting `features = ["s3"]`
does. This is what §4.3 means and it is worth restating at the site, because
the obvious fix is the wrong one.

Note the feature cycle: `backup = [… "s3" …]` and `s3 = ["backup"]`. Cargo
unions them, so the two are inseparable — enabling either enables both. Any
change to `s3` must keep that true or it changes what `backup` means for a
downstream user of the fork.

### 2.2 The patch ledgers

[vendor/hiqlite/PLURX-PATCH.md](../../vendor/hiqlite/PLURX-PATCH.md) — 15
patches: duplicate-id rejection in `NodeConfig`, split-brain probe TLS
policy, concurrent TLS key publication, the OpenRaft 0.9 election trigger,
`local_db_raft_metrics`, `db_quorum_watermark`, snapshot duration
histograms, snapshot RPC `RemoteError` preservation, WebSocket frame
flushing with a 30 s write budget, connection supervisors owning both socket
tasks, retained reset notification, and more. Its exit condition:

> Remove this vendor when an upstream Hiqlite release contains all fifteen
> patches and Plurx has upgraded to it.

[vendor/hiqlite-wal/PLURX-PATCH.md](../../vendor/hiqlite-wal/PLURX-PATCH.md)
— 3 restart-recovery patches: `last_purged_log_id` reconstruction, atomic
`meta.hql` replacement, WAL incarnation guard. Same exit condition shape.

Before M1, `vendor/s3-simple/PLURX-PATCH.md` carried one dependency-only
patch and stated this plan's premise in its own words:

> Hiqlite 0.14 enables cryptr's S3 feature even when plurx builds hiqlite
> with only `macros` and `sqlite`. That makes this otherwise unused package
> part of the production resolution. … Remove the vendor when upstream
> hiqlite stops enabling the unused S3 path or a compatible s3-simple
> release accepts quick-xml 0.41 or newer.

All three ledgers name a per-patch removal condition in prose. None links an
upstream issue or PR, and none names an owner.

### 2.3 The duplicate graph, executed

```bash
cargo tree -d --workspace --offline
```

On `0f02b7ea`: **42 duplicate package versions across 19 crate names**, and
440 unique crate versions in the whole workspace graph. The 19:

```
aws-lc-sys 0.39.1 / 0.43.0        rand 0.8.7 / 0.9.5 / 0.10.2
base64 0.13.1 / 0.21.7 / 0.22.1   rand_chacha 0.3.1 / 0.9.0
block-buffer 0.10.4 / 0.12.1      rand_core 0.6.4 / 0.9.5 / 0.10.1
chacha20 0.9.1 / 0.10.2           reqwest 0.12.28 / 0.13.4
cpufeatures 0.2.17 / 0.3.0        sha2 0.10.9 / 0.11.0
crypto-common 0.1.7 / 0.2.2       syn 2.0.119 / 3.0.3
digest 0.10.7 / 0.11.3            thiserror 1.0.69 / 2.0.19
fancy-regex 0.18.0 / 0.19.0       thiserror-impl 1.0.69 / 2.0.19
getrandom 0.2.17 / 0.3.4 / 0.4.3  untrusted 0.7.1 / 0.9.0
hashbrown 0.16.1 / 0.17.1
```

Most of these are ordinary ecosystem lag (a `rand` or `thiserror` major
straddle) and are not this plan's business. Two are:
`aws-lc-sys 0.39.1 / 0.43.0` — two CMake/C builds of the same library — and
`reqwest 0.12 / 0.13`.

`aws-lc-sys 0.39.1` has exactly one parent chain:

```
aws-lc-sys v0.39.1
└── s3-simple v0.8.0 (vendor/s3-simple)
    └── cryptr v0.10.0
        └── hiqlite v0.14.0 (vendor/hiqlite)
```

`quinn v0.11.11`, `quinn-proto v0.11.16` and `quinn-udp v0.5.15` reach the
graph only through `s3-simple`.

### 2.4 The three edges to `aws-lc-rs`

```bash
cargo tree -e features -i aws-lc-rs --offline
```

reports `rustls feature "aws-lc-rs"` with three distinct parents:

```
rustls feature "aws-lc-rs"
├── axum-server feature "tls-rustls"            <- vendor/hiqlite/Cargo.toml:151
├── rustls feature "prefer-post-quantum"        <- vendor/hiqlite/Cargo.toml:327
│     (rustls 0.23.42: prefer-post-quantum = ["aws_lc_rs"])
└── reqwest feature "__rustls-aws-lc-rs"
      <- reqwest feature "rustls" <- cryptr (default and s3)
```

Only the third goes away with the cryptr fix. This is correction 2, and it
is why §3.6 is a separate milestone with its own decision rather than a line
in §3.3.

What the process actually uses at runtime is `ring`:

```rust
// crates/plurx-core/src/cluster/migration.rs:1115-1116
fn install_default_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
```

so `aws-lc-rs` is compiled and, as a provider, never selected —
`prefer-post-quantum` reorders the *aws-lc-rs* provider's key-exchange
groups, and that provider is not the installed default. That has to be
re-verified rather than assumed (M3), because it is the whole argument that
dropping the feature is behaviour-neutral.

### 2.5 `deny.toml` as it stands

```toml
# deny.toml:1-3 and :40-42
# License policy for plurx. Enforced by `make license-check`
# (`cargo deny check licenses`) so THIRD-PARTY-NOTICES.md cannot drift
# quietly …
[bans]
multiple-versions = "allow"   # not a license concern; the gate owns dependency hygiene

[advisories]
# Security advisories are the RustSec job, run separately. This file is
# licenses only, so an advisory must not make `check licenses` fail.
```

The file declares itself licenses-only, twice. Adding a `[bans] deny = […]`
list without saying so contradicts its own header — §3.5 changes the header
and adds a separate make target rather than smuggling a second policy in.

### 2.6 Semantic search, and which lane compiles it

```toml
# crates/plurxd/Cargo.toml:24-29
[dependencies]
# CPU-only inference, loaded only when semantic search is enabled.
candle-core = { version = "=0.11.0", default-features = false }
candle-nn = { version = "=0.11.0", default-features = false }
candle-transformers = { version = "=0.11.0", default-features = false }
tokenizers = { version = "0.22", default-features = false, features = ["onig"] }
```

One consumer:
[crates/plurxd/src/library_search/semantic.rs](../../crates/plurxd/src/library_search/semantic.rs),
423 lines. Subtree: 13 `gemm*` crates, `pulp`, `rayon`/`rayon-core`, `half`,
`safetensors`, `onig` + `onig_sys` (a C build in `cargo check` too),
`esaxx-rs`, `spm_precompiled`, `fancy-regex 0.18`. Nothing in the workspace
uses `rayon` directly — `rg -n 'rayon' crates/` returns no source match — so
the pool it spawns is entered only through candle/gemm.

`fancy-regex` is already in the graph twice (0.18 via
`candle-transformers`, 0.19 via `jsonschema` in `plurx-cluster-check`), so
the `tokenizers` backend swap removes the C build without adding a crate
name.

The lanes that would and would not compile a non-default feature:

| Lane | Command | Compiles `plurxd` default features? |
|---|---|---|
| main fast lane, Rust gate ([main-fast-lane.yml:127-130](../../.github/workflows/main-fast-lane.yml)) | `make effort-rust-check` → `cargo check --workspace --locked --all-targets`, then `make hiqlite-vendor-clippy` | yes |
| `make unit` ([Makefile:44-46](../../Makefile)) | `cargo test --workspace --exclude plurx-cluster-check --no-fail-fast` | yes |
| Docker ([Dockerfile:21](../../Dockerfile)) | `cargo build --locked --release -p plurxd` | yes |
| vendor clippy ([Makefile:156-159](../../Makefile)) | `cargo clippy --manifest-path vendor/hiqlite/Cargo.toml --lib --no-default-features --features auto-heal,cache,macros,sqlite` | n/a — and note it enables `cache`, which production does not |

Today every lane compiles the same `plurxd` configuration. That property is
what correction 17 tells us not to lose.

### 2.7 What ARCHITECTURE.md still says

```
docs/ARCHITECTURE.md:441 (§6 table row)
| Cluster | hiqlite 0.14 (spike) → else openraft 0.9 + redb + rusqlite | §2.1 |

docs/ARCHITECTURE.md:545 (§9 risk row)
| hiqlite is a small project (bus factor) | `Store` trait isolation;
  openraft fallback is the same shape; both MIT/Apache |
```

Neither is true. The "spike" is a fork carrying 18 patches across two
packages, and "openraft fallback is the same shape" describes a substitution
that 18 patches' worth of behaviour now depends on.

## 3. Change

### 3.1 The decision, stated once

**Own the tested patch stack and upstream the generic fixes. Do not rename.**
The two are not alternatives; the review says so and correction 4 gives the
reason the rename half is actively costly.

- **Own** means: a named owner, a ledger with an exit condition and an
  upstream link per patch (§3.2), the vendor lane already running on PRs
  (correction 5), and the risk row in ARCHITECTURE §9 rewritten to describe a
  maintained fork rather than a fallback that no longer exists.
- **Upstream** means: open issues/PRs for the patches that are generic bugs
  rather than plurx policy. From the ledger, the candidates are the
  durability and transport repairs — snapshot `RemoteError` preservation,
  WebSocket frame flush before wait, connection-supervisor socket ownership,
  the retained reset notification, `NodeConfig` duplicate-id rejection, and
  all three `hiqlite-wal` repairs (purge-boundary reconstruction, atomic
  `meta.hql` replacement, mmap incarnation guard). The plurx-policy patches
  — the election trigger exposed because 0.9 has no leader transfer, the
  metrics wrappers, the validation helpers — are not upstream candidates and
  the ledger should say so rather than carrying a removal condition that will
  never fire.
- **Do not rename** (correction 4). The ownership the finding asks for is a
  ledger, an owner and a lane, none of which needs a package name. If the
  name is felt to be misleading, the honest fix is the `PLURX-PATCH.md`
  title and the ARCHITECTURE rows, not `[patch.crates-io]`.

Recorded in [ARCHITECTURE.md](../ARCHITECTURE.md) §7 (a new numbered decision
with its reason, in the style of the existing ones) and §9 (the risk row
rewritten). §6's "(spike)" is deleted in the same edit.

### 3.2 The fork ledger

The three `PLURX-PATCH.md` files stay where they are — they are next to the
code they describe, which is why they have survived — and gain a table at
the top of each:

| # | Patch | Kind | Upstream | Drop condition |
|---|---|---|---|---|
| 1 | `NodeConfig` duplicate-id rejection | generic bug | *(issue link)* | upstream release rejects duplicate ids |
| … | … | plurx policy | — | never; this is a plurx requirement |

`Kind` ∈ {`generic bug`, `plurx policy`, `dependency-only`}. `Upstream` is a
URL or `—`. `Drop condition` is what a future reader tests to decide the
patch can go; `never` is a legitimate value and is more useful than a
condition nobody will check.

A test in [tests/operations](../../tests/operations) asserts each ledger's
table has one row per bullet in the prose below it, so the two cannot drift
— the repo already has text-contract tests of this shape, and this is one
where the text *is* the artefact.

Owner: named in the table header line, not in this plan.

### 3.3 The cryptr edge

```diff
  [dependencies.cryptr]
  version = "0.10"
- features = ["s3"]
+ default-features = false   # cryptr's default is already [], so this only
+                            # documents intent; deleting `features` is the fix

  [features]
- s3 = ["backup"]
+ s3 = ["backup", "cryptr/s3"]
```

`backup` continues to imply `s3` (§2.1's cycle), so a downstream user
enabling `backup` still gets a working S3 backend — the feature's meaning is
unchanged for everyone; only plurx, which enables neither, stops paying for
it.

This is a change to a vendored third-party manifest, so it gets a row in
[vendor/hiqlite/PLURX-PATCH.md](../../vendor/hiqlite/PLURX-PATCH.md) as
patch 16, kind `generic bug`, with an upstream issue: the unconditional
edge is an upstream defect and every downstream user of hiqlite without
`backup` pays it.

### 3.4 Delete `vendor/s3-simple`

Once §3.3 lands, `cargo tree -i s3-simple` is empty for every feature
combination plurx builds. Delete the directory, its `[patch.crates-io]` row
([Cargo.toml:151](../../Cargo.toml)), its `VENDORED` entry in
[scripts/vendor-audit-lock:12-17](../../scripts/vendor-audit-lock), and its
path glob in [points.toml](../../validation/points.toml).

The condition on "every feature combination plurx builds" is load-bearing
and is the assessment's F-build-ops-codehealth-6 disposition ("Verify the
whole resolved graph and optional backup/S3 combinations after changing
defaults"). The acceptance check in §5.2 enumerates them rather than
asserting one.

### 3.5 Ban the duplicate versions, not the crate

```toml
# deny.toml — header amended to say the file now carries two policies
[bans]
multiple-versions = "allow"   # still allow in general: ecosystem lag is not a defect
deny = []
# A second copy of a CMake/C crypto build is a build-time and attack-surface
# cost, not a licensing one. Pin the *versions*, so a future single-version
# unification is not blocked by a blanket crate ban.
skip = []
[[bans.deny]]
name = "aws-lc-sys"
version = "=0.39.1"
[[bans.deny]]
name = "openssl-sys"
[[bans.deny]]
name = "native-tls"
```

`aws-lc-sys` is denied at the *version* that only `s3-simple` pulls, not as
a crate, because §3.6 may legitimately land on aws-lc as the single
provider. `openssl-sys` and `native-tls` are denied outright: neither has
ever been in this graph and both would be a TLS-stack decision arriving by
accident.

Enforced by a new `make bans-check` running `cargo deny check bans`, wired
into the same check as `license-check`. `cargo deny check licenses` is
unaffected, which is what [deny.toml:44-47](../../deny.toml) requires.

### 3.6 The TLS provider, as its own decision

After §3.3, two edges remain (§2.4). Each is a separate question:

| Option | `axum-server` | `rustls` in the fork | Effect |
|---|---|---|---|
| **A — ring only** | `["tls-rustls-no-provider"]` | drop `prefer-post-quantum` | `cargo tree -i aws-lc-sys` empty; no PQ hybrid key exchange available; two C builds gone |
| **B — keep PQ, drop the duplicate** | `["tls-rustls-no-provider"]` | keep `prefer-post-quantum` | one `aws-lc-sys` (0.43.0) remains; PQ groups available to an aws-lc provider that is not installed |
| **C — switch to aws-lc** | unchanged | unchanged | drop the `ring` provider install in `plurx-core`; one provider, one C build; a change to what the process negotiates |

The evidence that decides it: A and B are behaviour-neutral only if the
installed default provider really is `ring` at every TLS entry point
(`plurx-core`'s explicit install, hiqlite's `start_node`, axum-server's
`bind_rustls`, and both `reqwest` clients). M3's first job is a test that
asserts the installed provider is `ring` and that no code path reaches TLS
before `install_default_crypto_provider`. If that holds, A is a pure
subtraction and is the recommendation; if it does not, the finding is larger
than a dependency cut and belongs in a security plan, not this one.

`axum-server`'s `tls-rustls` is required for `RustlsConfig`,
`bind_rustls` and `from_tcp_rustls`, which the fork calls at
[tls.rs:2, 85, 97](../../vendor/hiqlite/src/tls.rs),
[server/proxy/mod.rs:75](../../vendor/hiqlite/src/server/proxy/mod.rs) and
[start.rs:178, 267](../../vendor/hiqlite/src/start.rs). Whether
`tls-rustls-no-provider` alone exposes all four is the first thing M3 must
compile, not assume.

### 3.7 `semantic-search` as a build-time feature, and the compile that must not drift

Three parts, in dependency order:

**(a) Tokenizer backend.** `tokenizers = { default-features = false,
features = ["fancy-regex"] }`, removing `onig` + `onig_sys` and their C
build. The assessment requires an equivalence benchmark first
(correction 17): tokenize a fixed corpus — the library's own titles,
overviews and taglines from a lab snapshot, plus a Unicode edge set — with
both backends and assert identical token id sequences. Not "similar";
identical, because the embedding is a function of the ids and a divergence
would silently change search results. If they diverge on any input, (a) does
not land and (b) and (c) still can.

**(b) The feature.** `semantic-search = ["dep:candle-core", "dep:candle-nn",
"dep:candle-transformers", "dep:tokenizers"]` in
[plurxd/Cargo.toml](../../crates/plurxd/Cargo.toml), with
`library_search::semantic` behind `#[cfg(feature = "semantic-search")]` and
the call site degrading to the existing FTS path when absent.

This is a build-time feature, not a runtime switch, which is the distinction
the crate's own comment draws
([plurxd/Cargo.toml:11-17](../../crates/plurxd/Cargo.toml)): the
`live-hls-recovery` flag was deleted because "nothing in the Makefile,
Dockerfile, deploy or scripts ever passed `--no-default-features`, so the
switch decided nothing while hiding ~100 code sites". The rule that follows
from that note, and that this plan is bound by: **a cargo feature is
admissible only when a named lane actually builds each side of it, and a
required check compiles the shipped side.** Whether semantic search *runs*
stays a runtime setting, unchanged.

**(c) The compile that ships.** With the feature non-default:

- `Dockerfile:21` becomes `cargo build --locked --release -p plurxd
  --features semantic-search`.
- The main fast lane's Rust gate
  ([main-fast-lane.yml:127-130](../../.github/workflows/main-fast-lane.yml))
  gains a second step: `cargo check --locked -p plurxd --features
  semantic-search`. Required, not optional — this is correction 17 and it is
  non-negotiable.
- A text contract in [tests/operations](../../tests/operations) asserts the
  Dockerfile's feature list and the fast lane's feature list are the same
  string, so the two configurations cannot diverge silently. The repo
  already asserts Dockerfile strings
  (`test_contracts.py` pins `dockerfile.count("&& apt-get clean") == 2`), so
  this is the established mechanism.

With (c) in place the feature does not create a new shipped configuration:
it creates a *second, smaller* configuration that CI also builds, and the
shipped one keeps a required compile. If the measurement in M0 shows the
saving is small — the runner cache is persistent and candle rebuilds only
when its inputs change — then (b) and (c) are not worth their complexity and
the milestone stops after (a). That decision is data, not preference, and
M0 produces the data before M5 is written.

**(d) The rayon pool, bounded without touching the global one.** The review
proposes `rayon::ThreadPoolBuilder::new().num_threads(2).build_global()`.
`build_global` reconfigures the process-wide pool and fails if anything has
already initialised it; the assessment's disposition is explicit that "a
process-wide Rayon pool change also affects other Rayon users". So instead:

```rust
// a pool owned by the embedder, entered only for inference
static EMBED_POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
let pool = EMBED_POOL.get_or_init(|| {
    rayon::ThreadPoolBuilder::new()
        .num_threads(EMBED_THREADS)   // 2, measured in M5
        .thread_name(|i| format!("plurx-embed-{i}"))
        .build()
        .expect("embed pool")
});
pool.install(|| model.forward(&input))
```

`install` switches the calling thread's registry for the duration, so gemm's
internal `par_iter` runs on this pool and the global pool is never built.
`rayon` becomes a direct dependency of `plurxd`, gated by the same feature.

### 3.8 What this plan does not do

No settings key. No metric. No recipe identity or cache-digest change:
nothing here reaches a transcode recipe, a manifest digest or a cache key.
The one user-visible surface that could move is semantic search, and (a)'s
equivalence benchmark is there precisely to prove it does not.

## 4. Guardrails (non-goals)

- **Added lines are churn, not fork size** (F-hist-12 and
  F-build-ops-codehealth-5 rows: "Gross patch insertions across history are
  not net divergence from upstream"). Honoured: this plan quotes no
  `+15k`/`+78k` figure. The ledger's unit is *patches* (15 + 3 + 1), and M1's
  acceptance is a reproducible diff against the pristine 0.14.0 release, not
  a `git log --shortstat`.
- **Security fixes are backportable** (F-sc-11 row: "they are not
  categorically unavailable because the dependency is forked"). Honoured:
  §3.1 says upstream *and* own, and §3.2's `Kind` column is what makes a
  backport findable. The existing `scripts/vendor-audit-lock` already
  restores registry provenance for the advisory scan and stays.
- **Ownership does not require renaming or abandoning upstreaming**
  (F-hist-12 row). Honoured by §3.1 and correction 4.
- **Verify the whole resolved graph and the optional backup/S3
  combinations** (F-build-ops-codehealth-6 row). Honoured by §5.2's
  enumeration rather than a single `cargo tree` run.
- **Choosing a provider is a decision with a feature matrix, not a name
  ban** (same row). Honoured by §3.6's three-option table and by §3.5
  banning `aws-lc-sys =0.39.1` rather than `aws-lc-sys`.
- **Preserve the shipped compilation** (correction 17). Honoured by §3.7(c),
  including the text contract that pins the two feature lists to the same
  string.
- **Benchmark tokenizer equivalence** (correction 17). Honoured by
  §3.7(a), with identical token ids as the bar and an explicit "then (a)
  does not land" branch.
- **Do not reconfigure the global rayon pool** (F-build-ops-codehealth-7
  row). Honoured by §3.7(d)'s scoped pool and `install`.
- **Package size and idle RSS gains need measurements**
  (F-build-ops-codehealth-7 row). Honoured: M0 measures before M5 decides,
  and §3.7(c) says the milestone may stop after (a).
- **No in-code runtime feature gate.** `semantic-search` is a build-time
  feature whose both sides a named lane builds (§3.7(b)); whether semantic
  search runs stays the existing runtime setting.
- **No runtime behaviour change.** Every cut is a dependency the process
  never calls. §3.6 is the only place that could break the rule, which is
  why its first acceptance check is a provider assertion.

## 5. Milestones

One draft PR per milestone into `main` under the fast lane. M1 and M2 are
independent of M3–M5 and can land first.

### 5.1 M0 — the baseline both halves are judged against

No production change. A short markdown table appended to this document with
three measurements, each taken twice (cold cache, warm cache) on a lab
runner:

1. `cargo tree -d --workspace` — 42 versions / 19 names on `0f02b7ea`.
2. `cargo check --workspace --locked --all-targets` wall time.
3. `cargo check --locked -p plurxd --no-default-features` wall time, as a
   stand-in for the post-(b) lean lane, plus `du -sh target/debug` for both.

Acceptance: the table exists in this document with six numbers and the
runner's identity, and the M5 decision in §3.7(c) cites it.

```text
GPT prompt (build host): On the lab runner that normally runs the fast Rust
gate, with <sha> checked out: (1) `cargo clean && time cargo check
--workspace --locked --all-targets`, (2) `time cargo check --workspace
--locked --all-targets` again immediately (warm), (3) `cargo clean && time
cargo check --locked -p plurxd --no-default-features`, (4) the same warm,
(5) `cargo tree -d --workspace | grep -cE '^[a-z]'`, (6) `du -sh target`.
Give me all six numbers plus `nproc` and the runner label. Then repeat (1)
and (2) with `CARGO_BUILD_JOBS=1` so I can see how much of the cold time is
the C builds (aws-lc-sys, onig_sys) rather than parallel Rust codegen.
```

### 5.2 M1 — the cryptr edge and the deleted vendor

§3.3 and §3.4 in one PR, because the second is only safe after the first.

Acceptance, all four commands:

```bash
cargo tree -i s3-simple --offline                 # error: nothing depends on it
cargo tree -i quinn --offline                     # error: nothing depends on it
cargo tree -i aws-lc-sys@0.39.1 --offline         # error: nothing depends on it
cargo tree -d --workspace --offline | grep -cE '^[a-z]'   # 42 -> 38 or fewer
```

plus the combination enumeration, which is the assessment's requirement:

```bash
for f in "" "backup" "s3" "backup,s3" "cache" "dashboard" "full"; do
  cargo tree --manifest-path vendor/hiqlite/Cargo.toml \
    --no-default-features --features "auto-heal,macros,sqlite${f:+,$f}" \
    -i s3-simple --offline >/dev/null 2>&1 \
    && echo "s3-simple present with [$f]" || echo "absent with [$f]"
done
```

`absent` for the empty and `cache`/`dashboard` rows; `present` for every row
containing `backup` or `s3`, which proves the feature still works for a
downstream user. `make unit`, `make hiqlite-vendor-clippy` and
`make cluster-wal-check` green; `cargo deny check licenses` green (the
license set shrinks, and the allow-list must shrink with it — §2.5's own
rule that "an allow-list that permits more than the build actually uses
stops being evidence of anything").

### 5.3 M2 — the ledger, the ban, and the architecture rows

§3.2, §3.5 and the ARCHITECTURE §6/§7/§9 edits. No dependency change.

Acceptance: `make bans-check` green and, with `vendor/s3-simple` restored in
a scratch commit, red on `aws-lc-sys =0.39.1`; the ledger tables exist with
a `Kind` and a `Drop condition` for all 19 patches and an upstream URL for
every row whose `Kind` is `generic bug`;
`python3 -m unittest discover -s tests/operations -p 'test_*.py'` green,
including the new ledger-table contract; `docs/ARCHITECTURE.md` no longer
contains the string "openraft fallback is the same shape" or "(spike)".

### 5.4 M3 — the TLS provider decision

The provider assertion first, then whichever of §3.6's A/B/C it licenses.

Acceptance: `cargo test -p plurx-core cluster::migration::tests::installed_provider_is_ring`
green; the chosen option's row in §3.6 is marked as taken with the date; if
A, `cargo tree -i aws-lc-sys --offline` errors and
`cargo tree -d --workspace --offline | grep -cE '^[a-z]'` drops by two more;
`make cluster-wal-check` and `make cluster-check` green, because this touches
the transport's TLS configuration and the three-voter lane is the only place
that proves it still handshakes.

### 5.5 M4 — the tokenizer equivalence benchmark

§3.7(a)'s benchmark only. No manifest change yet.

Acceptance: `cargo test -p plurxd library_search::semantic::tokenizer_backends_agree
-- --ignored` compares `onig` and `fancy-regex` token ids over the fixture
corpus and prints the corpus size; the PR body records pass or the first
divergence. On divergence, M5 drops (a) and says so.

### 5.6 M5 — the feature, the lanes, and the pool

§3.7(b), (c) and (d), only if M0's numbers justify (b) and M4 passed for
(a).

Acceptance: `cargo check --locked -p plurxd --features semantic-search`
green; `cargo check --locked -p plurxd` green with `semantic.rs` compiled
out and library search still answering through FTS
(`cargo test -p plurxd library_search` green in both configurations);
`python3 -m unittest discover -s tests/operations -p 'test_*.py'` green
including the Dockerfile/fast-lane feature-string contract; the before/after
of M0's measurements 2 and 3 in the PR body; and `rg -n 'build_global'
crates/` empty.

```text
GPT prompt (fleet): With <sha> built as the Docker image and deployed to
media1, confirm semantic search still answers: search the library for three
phrases that only a semantic match finds (not literal title substrings) and
give me the top five results for each, plus `curl -s
localhost:32400/metrics | grep -i semantic` if any family exists, and the
container's RSS from `docker stats --no-stream` before and after the three
searches. Then tell me the image size from `docker images` and compare it
with the previous tag.
```

### 5.7 M6 — upstream

Open the issues/PRs of §3.1 and fill the `Upstream` column. Not gated on any
of the above.

Acceptance: every `generic bug` row in the three ledgers has a URL, and this
document's Execution log records which were accepted, rejected or ignored.

## 6. Verification and rollout

Fast lane per PR. M1 and M3 additionally need `make cluster-check`, because
they change what the replicated transport links against; M2 and M5 need the
operations contract suite. Nothing here changes a running binary's
behaviour, so a mixed fleet is not a consideration and rollback is a revert
— except M5, whose Docker image drops the semantic module if the feature
flag is wrong, which is exactly why §3.7(c)'s text contract exists.

Order: M0 → (M1, M2 in either order) → M3 → M4 → M5 → M6. M1 before M3
because M3's duplicate count is measured against M1's result; M4 before M5
because M5 (a) is conditional on it.

## 7. Open questions

1. Who owns the fork? §3.2's table needs a name in its header and this plan
   will not invent one.
2. Upstream status of the 15 + 3 patches — the review's own open question 4,
   still unanswered from code: "are any of the 15 patches in flight
   upstream, and what release is upstream on now (verified 0.14 in
   2026-07)?" M6 answers it by opening them; nothing before M6 depends on
   the answer.
3. §3.6 option C (switch to aws-lc) is listed for completeness and is not
   recommended, because it changes what the process negotiates and
   `install_default_crypto_provider`'s comment records why `ring` was chosen.
   Confirm it stays unrecommended, or it needs its own security review.
4. `vendor/rust_decimal` is patched
   ([Cargo.toml:152](../../Cargo.toml)) and has no `PLURX-PATCH.md` in the
   set the review reads. It should get one under §3.2's format, or the
   reason it does not need one should be written down. Not in scope here;
   flagged.
5. The vendor clippy lane builds the fork with `cache` enabled
   ([Makefile:158](../../Makefile)) while production does not. That is a
   fourth configuration, and by §3.7's own rule it should either be the
   production set or be justified. Whose plan?
6. Whether the `fancy-regex` duplicate (0.18 via candle-transformers, 0.19
   via jsonschema) collapses after M5's backend swap, or whether candle
   pins 0.18 regardless. M5's `cargo tree -d` answers it; if it does not
   collapse, that is one more name on the M0 baseline and not a defect.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | claim | `plan/K-08` | Claimed K-08 from `9deb58a2`; one whole-plan draft PR will preserve the `hiqlite` patch name and remove only dependency edges supported by repository evidence. |
