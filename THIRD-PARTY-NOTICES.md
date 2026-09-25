# Third-party notices — what ships inside plurx, and under what terms

Companion to [LICENSE](LICENSE) (plurx's own terms) and [NOTICE](NOTICE) —
this file is *what else is in the box*, and the attribution those components
require. Full license texts that are not Apache-2.0 live in
[`licenses/`](licenses/); Apache-2.0 components are covered by the root
[LICENSE](LICENSE).

plurx is Apache-2.0. Nothing it ships is copyleft, so nothing here imposes a
copyleft obligation on plurx or on anything built with it. Two things sit
close enough to that line to be worth stating plainly: ffmpeg, which plurx
invokes but never links (§1), and JUnit, which is copyleft but test-scoped
and never reaches a shipped artifact (§5).

Dependency licenses come from `cargo metadata` over the resolved graph, not
from `Cargo.lock` — the lockfile records versions and sources, never license
fields, so an audit claiming to read licenses "from Cargo.lock" has not done
the work. 496 source-bearing crates in the root workspace · license fields
verified 2026-09-17; the graph itself re-checked against `Cargo.lock`
on 2026-09-22, when the vendored Hiqlite `backup` feature stopped
enabling S3 and fourteen crates reached only through it left the
resolution, and on 2026-09-24, when `ring` became the only rustls provider
and the six crates reached only through `aws-lc-rs` (`aws-lc-rs`,
`aws-lc-sys`, `cmake`, `dunce`, `fs_extra`, `jobserver`) left it. The
crate counts and the §4 license summary were recomputed from `cargo metadata`
on 2026-09-25; `tests/operations/test_license_notices.py` holds the summary,
the counts and the full list to one another.

---

## 1. ffmpeg — invoked as a subprocess, never linked

**What it is:** plurx shells out to `ffmpeg` and `ffprobe` for probing,
remux, transcode, frame grabs, and DVR capture. There are 105 such call
sites — 69 under `crates/plurxd/src/` and 36 under
`crates/plurx-core/src/` — and every one of them is a `Command::new(...)`.
There are no `libav*` bindings, no `ffmpeg-sys`/`ffmpeg-next` crate, no
`dlopen`/`libloading` of a media library, and neither `build.rs` links
anything (they stamp version strings). The single `#[link]` attribute in the
tree is `ntdll` in `crates/plurx-core/src/process_control.rs`.

**Why that matters:** running a separate program does not create a
derivative work of it. ffmpeg's license — LGPL-2.1 for a default build,
GPL-2.0 or GPL-3.0 once compiled with x264, x265, or other GPL components —
does not reach plurx, whatever build you point it at. This is the claim the
whole license choice rests on, which is why the call-site locations are named
above: check them, don't take them.

**Where the obligation does exist:** `deploy/install` installs ffmpeg from
the host's own package manager (`apt`, `dnf`, `pacman`, `zypper`, `apk`,
`brew`), so a source install redistributes nothing. The `Dockerfile` is
different — it installs both Debian `ffmpeg` and `jellyfin-ffmpeg8` into the
runtime image. Both are GPL builds. Publishing that image is redistribution
of GPL binaries and carries the GPL's source-offer obligation for those
binaries, not for plurx. Two ways to stay clean:

```bash
# Either: keep built images in your own registry, not a public one, or
# drop ffmpeg from the image and let the host provide it, the way
# deploy/install already does on bare metal.
```

`Dockerfile.store-shard` installs no ffmpeg and is unaffected.

**How to read it:** the distinction is linkage, not proximity. Shipping
ffmpeg *next to* plurx is aggregation and leaves plurx's license alone;
shipping it *inside* plurx's address space would not, which is why the
subprocess boundary is load-bearing and should stay that way.

---

## 2. Bundled in the web UI

These are served to every browser as part of the embedded SPA
(`crates/plurxd/src/web/`), so they are distributed with every copy of plurx
and every container image.

| Component | Version | License | Copyright |
|---|---|---|---|
| [hls.js](https://github.com/video-dev/hls.js) | 1.6.16 | Apache-2.0 | Dailymotion and the hls.js contributors |
| [eventemitter3](https://github.com/primus/eventemitter3) | bundled in `hls.min.js` | MIT — [full text](licenses/eventemitter3-MIT.txt) | Copyright (c) 2014 Arnout Kazemier |
| [url-toolkit](https://github.com/tjenkinson/url-toolkit) | bundled in `hls.min.js` | Apache-2.0 | Tom Jenkinson |
| [Material Design icons](https://github.com/google/material-design-icons) | inline SVG paths | Apache-2.0 | Google LLC |
| [Inter](https://github.com/rsms/inter) | subset `woff2` | SIL OFL 1.1 — [full text](licenses/Inter-OFL.txt) | Copyright (c) 2016 The Inter Project Authors (https://github.com/rsms/inter) |
| [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono) | subset `woff2` | SIL OFL 1.1 — [full text](licenses/JetBrainsMono-OFL.txt) | Copyright 2020 The JetBrains Mono Project Authors (https://github.com/JetBrains/JetBrainsMono) |

**hls.js and what it carries.** `hls.min.js` is the upstream minified `dist`
build with its license header stripped by the minifier. hls.js 1.6.16
declares no runtime `dependencies`; it bundles its build-time dependencies
directly into `dist`, so eventemitter3 and url-toolkit ship inside that one
file. They are separate copyright holders with their own terms — MIT
requires its notice in all copies — and one row for hls.js does not discharge
them. This table plus `licenses/` is their attribution.

**Material icons.** Four icon paths are inlined as SVG in
`crates/plurxd/src/web/detail/helpers.js` (search `Apache-2.0 Google Material
icon paths`) so the self-hosted client needs no icon font and no CDN.

**The fonts.** Both are inlined as `data:font/woff2` URIs in
`crates/plurxd/src/web/app.css` — three faces: JetBrains Mono 500 and 700, and
Inter variable 100–900. Both are
**subset** (Inter to ~330 codepoints, JetBrains Mono to ~512) and remain
under the OFL, which permits subsetting, modification, bundling, and
commercial use. Its one operative condition here is that each copyright
notice and the complete license text travel with the font — which is what
`licenses/Inter-OFL.txt` and `licenses/JetBrainsMono-OFL.txt` are for.
Neither upstream declares a Reserved Font Name, so no renaming is required.

Both paths above moved out of `index.html` when the web shell was cut into a
tree (`docs/clients/WEB-SHELL-LAYOUT.md`). Nothing about what is served
changed — only which file to open.

---

## 3. Vendored Rust crates — upstream sources, locally modified

Four crates are vendored under `vendor/` rather than pulled from crates.io.
Each carries a `PLURX-PATCH.md` recording what plurx changed and why.

| Crate | Version | License | Upstream | Changes |
|---|---|---|---|---|
| `hiqlite` | 0.14.0 | Apache-2.0 | Sebastian Dobe · [sebadob/hiqlite](https://github.com/sebadob/hiqlite) | [15 clustering patches](vendor/hiqlite/PLURX-PATCH.md) |
| `hiqlite-wal` | 0.14.0 | Apache-2.0 | Sebastian Dobe · [sebadob/hiqlite](https://github.com/sebadob/hiqlite) | [3 restart-recovery patches](vendor/hiqlite-wal/PLURX-PATCH.md) |
| `s3-simple` | 0.8.0 | Apache-2.0 | Sebastian Dobe · [sebadob/s3-simple](https://github.com/sebadob/s3-simple) | [quick-xml bump for RUSTSEC-2026-0194/0195 and four unreferenced edges dropped](vendor/s3-simple/PLURX-PATCH.md) |
| `rust_decimal` | 1.42.1 | MIT | Paul Mason · [paupino/rust-decimal](https://github.com/paupino/rust-decimal) | [rkyv 0.7 removal for RUSTSEC-2026-0235](vendor/rust_decimal/PLURX-PATCH.md) |

Each directory carries its upstream license at `vendor/<crate>/LICENSE`.

`s3-simple` is vendored but is not part of the default resolution: plurx
builds hiqlite without `backup`/`s3`, so it reaches no shipped binary and
carries no row in the resolved-dependency table below. It is attributed here
because the repository redistributes the modified source. See
[vendor/s3-simple/PLURX-PATCH.md](vendor/s3-simple/PLURX-PATCH.md).

For the three Apache-2.0 crates, §4(b) asks that modified files carry
prominent notices of the change. `PLURX-PATCH.md` records every change at the
directory level, which is how the patches are kept reviewable against
upstream; it is not a per-file annotation. `rust_decimal` is MIT, where no
such requirement applies.

---

## 4. Rust dependencies

496 source-bearing crates resolve into a plurx build, excluding the five
first-party crates and the vendored ones above (`s3-simple` resolves only in
the fork's optional backup graph, never in this one). Every one is permissive:

| License expression | Crates |
|---|---:|
| `MIT OR Apache-2.0` | 261 |
| `MIT` | 117 |
| `Apache-2.0 OR MIT` | 25 |
| `Unicode-3.0` | 18 |
| `MIT/Apache-2.0` | 16 |
| `Apache-2.0` | 11 |
| `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | 5 |
| `Unlicense OR MIT` | 4 |
| `Unlicense/MIT` | 4 |
| `Apache-2.0 OR ISC OR MIT` | 3 |
| `Apache-2.0/MIT` | 3 |
| `Zlib OR Apache-2.0 OR MIT` | 3 |
| `Apache-2.0 OR MIT OR Zlib` | 2 |
| `BSD-2-Clause` | 2 |
| `BSD-2-Clause OR Apache-2.0 OR MIT` | 2 |
| `CDLA-Permissive-2.0` | 2 |
| `ISC` | 2 |
| `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | 2 |
| `MIT OR Apache-2.0 OR Zlib` | 2 |
| `(Apache-2.0 OR MIT) AND BSD-3-Clause` | 1 |
| `(MIT OR Apache-2.0) AND Unicode-3.0` | 1 |
| `0BSD OR MIT OR Apache-2.0` | 1 |
| `Apache-2.0 / MIT` | 1 |
| `Apache-2.0 AND ISC` | 1 |
| `Apache-2.0 OR BSL-1.0` | 1 |
| `BSD-3-Clause` | 1 |
| `MIT AND BSD-3-Clause` | 1 |
| `MIT OR BSD-3-Clause` | 1 |
| `MIT OR Zlib OR Apache-2.0` | 1 |
| `MIT-0` | 1 |
| `Zlib` | 1 |

`r-efi` is the only crate whose license expression names a copyleft license
at all (`MIT OR Apache-2.0 OR LGPL-2.1-or-later`). It is a disjunction, so
plurx takes it under MIT — and the crate is a UEFI-target shim that does not
compile for Linux or macOS.

Crates under `Unicode-3.0` (18) and `CDLA-Permissive-2.0` (2, the webpki root
stores) are permissive with attribution requirements, satisfied by this file.

Two limits worth stating rather than hiding. This table reproduces each
crate's declared SPDX expression only: `onig_sys` and `lz4-sys`
statically link bundled C whose own upstream notices are not carried here.
And the repo has more lockfiles than the root one. `fuzz/Cargo.lock`,
`spikes/hiqlite-m0/Cargo.lock` and `spikes/tokenizer-backends/Cargo.lock`
together resolve 56 crate versions the root lock does not (`libfuzzer-sys`,
`arbitrary`, `rkyv`, `fancy-regex` 0.14.0, and others), and
`vendor/hiqlite/Cargo.lock` and `vendor/hiqlite-wal/Cargo.lock` pin the
vendored crates' own test lanes. All are permissive and none of those
workspaces ships in any artifact, so they are out of scope for distribution,
but they are not covered by the table above.

<details>
<summary>Full crate list (496)</summary>

| Crate | Version | License |
|---|---|---|
| `adler2` | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| `aead` | 0.5.2 | MIT OR Apache-2.0 |
| `ahash` | 0.8.12 | MIT OR Apache-2.0 |
| `aho-corasick` | 1.1.4 | Unlicense OR MIT |
| `allocator-api2` | 0.2.21 | MIT OR Apache-2.0 |
| `android_system_properties` | 0.1.6 | MIT OR Apache-2.0 |
| `anstream` | 1.0.0 | MIT OR Apache-2.0 |
| `anstyle` | 1.0.14 | MIT OR Apache-2.0 |
| `anstyle-parse` | 1.0.0 | MIT OR Apache-2.0 |
| `anstyle-query` | 1.1.5 | MIT OR Apache-2.0 |
| `anstyle-wincon` | 3.0.11 | MIT OR Apache-2.0 |
| `anyerror` | 0.1.13 | Apache-2.0 |
| `anyhow` | 1.0.104 | MIT OR Apache-2.0 |
| `arc-swap` | 1.9.2 | MIT OR Apache-2.0 |
| `argon2` | 0.5.3 | MIT OR Apache-2.0 |
| `arrayvec` | 0.7.8 | MIT OR Apache-2.0 |
| `asn1-rs` | 0.7.2 | MIT OR Apache-2.0 |
| `asn1-rs-derive` | 0.6.0 | MIT OR Apache-2.0 |
| `asn1-rs-impl` | 0.2.0 | MIT/Apache-2.0 |
| `async-stream` | 0.3.6 | MIT |
| `async-stream-impl` | 0.3.6 | MIT |
| `async-trait` | 0.1.91 | MIT OR Apache-2.0 |
| `atomic-waker` | 1.1.2 | Apache-2.0 OR MIT |
| `autocfg` | 1.5.1 | Apache-2.0 OR MIT |
| `axum` | 0.8.9 | MIT |
| `axum-core` | 0.5.6 | MIT |
| `axum-server` | 0.8.0 | MIT |
| `base64` | 0.13.1 | MIT/Apache-2.0 |
| `base64` | 0.21.7 | MIT OR Apache-2.0 |
| `base64` | 0.22.1 | MIT OR Apache-2.0 |
| `base64ct` | 1.8.3 | Apache-2.0 OR MIT |
| `bincode` | 2.0.1 | MIT |
| `bincode_derive` | 2.0.1 | MIT |
| `bit-set` | 0.8.0 | Apache-2.0 OR MIT |
| `bit-vec` | 0.8.0 | Apache-2.0 OR MIT |
| `bit-vec` | 0.9.1 | Apache-2.0 OR MIT |
| `bitflags` | 2.13.1 | MIT OR Apache-2.0 |
| `bitstream-io` | 4.10.0 | MIT/Apache-2.0 |
| `bitvec` | 1.1.1 | MIT |
| `bitvec_helpers` | 4.0.2 | MIT |
| `blake2` | 0.10.6 | MIT OR Apache-2.0 |
| `block-buffer` | 0.10.4 | MIT OR Apache-2.0 |
| `block-buffer` | 0.12.1 | MIT OR Apache-2.0 |
| `borrow-or-share` | 0.2.4 | MIT-0 |
| `borsh` | 1.8.0 | MIT OR Apache-2.0 |
| `borsh-derive` | 1.8.0 | Apache-2.0 |
| `bumpalo` | 3.20.3 | MIT OR Apache-2.0 |
| `byte-unit` | 5.2.5 | MIT |
| `bytecount` | 0.6.9 | Apache-2.0/MIT |
| `bytemuck` | 1.25.2 | Zlib OR Apache-2.0 OR MIT |
| `bytemuck_derive` | 1.12.1 | Zlib OR Apache-2.0 OR MIT |
| `byteorder` | 1.5.0 | Unlicense OR MIT |
| `bytes` | 1.12.1 | MIT |
| `candle-core` | 0.11.0 | MIT OR Apache-2.0 |
| `candle-nn` | 0.11.0 | MIT OR Apache-2.0 |
| `candle-transformers` | 0.11.0 | MIT OR Apache-2.0 |
| `castaway` | 0.2.4 | MIT |
| `cc` | 1.4.0 | MIT OR Apache-2.0 |
| `cfg-if` | 1.0.4 | MIT OR Apache-2.0 |
| `cfg_aliases` | 0.2.2 | MIT |
| `chacha20` | 0.10.2 | MIT OR Apache-2.0 |
| `chacha20` | 0.9.1 | Apache-2.0 OR MIT |
| `chacha20poly1305` | 0.10.1 | Apache-2.0 OR MIT |
| `chrono` | 0.4.45 | MIT OR Apache-2.0 |
| `cipher` | 0.4.4 | MIT OR Apache-2.0 |
| `clap` | 4.6.2 | MIT OR Apache-2.0 |
| `clap_builder` | 4.6.2 | MIT OR Apache-2.0 |
| `clap_derive` | 4.6.1 | MIT OR Apache-2.0 |
| `clap_lex` | 1.1.0 | MIT OR Apache-2.0 |
| `colorchoice` | 1.0.5 | MIT OR Apache-2.0 |
| `combine` | 4.6.7 | MIT |
| `compact_str` | 0.9.1 | MIT |
| `const-oid` | 0.10.2 | Apache-2.0 OR MIT |
| `core-foundation` | 0.10.1 | MIT OR Apache-2.0 |
| `core-foundation-sys` | 0.8.7 | MIT OR Apache-2.0 |
| `cpufeatures` | 0.2.17 | MIT OR Apache-2.0 |
| `cpufeatures` | 0.3.0 | MIT OR Apache-2.0 |
| `crc` | 3.4.0 | MIT OR Apache-2.0 |
| `crc-catalog` | 2.5.0 | MIT OR Apache-2.0 |
| `crc32fast` | 1.5.0 | MIT OR Apache-2.0 |
| `crossbeam` | 0.8.4 | MIT OR Apache-2.0 |
| `crossbeam-channel` | 0.5.16 | MIT OR Apache-2.0 |
| `crossbeam-deque` | 0.8.7 | MIT OR Apache-2.0 |
| `crossbeam-epoch` | 0.9.20 | MIT OR Apache-2.0 |
| `crossbeam-queue` | 0.3.13 | MIT OR Apache-2.0 |
| `crossbeam-utils` | 0.8.22 | MIT OR Apache-2.0 |
| `crunchy` | 0.2.4 | MIT |
| `crypto-common` | 0.1.7 | MIT OR Apache-2.0 |
| `crypto-common` | 0.2.2 | MIT OR Apache-2.0 |
| `cryptr` | 0.10.0 | Apache-2.0 |
| `csv` | 1.4.0 | Unlicense/MIT |
| `csv-core` | 0.1.13 | Unlicense/MIT |
| `darling` | 0.20.11 | MIT |
| `darling_core` | 0.20.11 | MIT |
| `darling_macro` | 0.20.11 | MIT |
| `dary_heap` | 0.3.9 | MIT OR Apache-2.0 |
| `data-encoding` | 2.11.1 | MIT |
| `deadpool` | 0.13.0 | MIT OR Apache-2.0 |
| `deadpool-runtime` | 0.3.1 | MIT OR Apache-2.0 |
| `der-parser` | 10.0.0 | MIT OR Apache-2.0 |
| `deranged` | 0.5.8 | MIT OR Apache-2.0 |
| `derive_builder` | 0.20.2 | MIT OR Apache-2.0 |
| `derive_builder_core` | 0.20.2 | MIT OR Apache-2.0 |
| `derive_builder_macro` | 0.20.2 | MIT OR Apache-2.0 |
| `derive_more` | 1.0.0 | MIT |
| `derive_more-impl` | 1.0.0 | MIT |
| `digest` | 0.10.7 | MIT OR Apache-2.0 |
| `digest` | 0.11.3 | MIT OR Apache-2.0 |
| `displaydoc` | 0.2.7 | MIT OR Apache-2.0 |
| `dolby_vision` | 3.4.0 | MIT |
| `dotenvy` | 0.15.7 | MIT |
| `dyn-clone` | 1.0.20 | MIT OR Apache-2.0 |
| `dyn-stack` | 0.13.2 | MIT |
| `dyn-stack-macros` | 0.1.3 | MIT |
| `either` | 1.17.0 | MIT OR Apache-2.0 |
| `email_address` | 0.2.9 | MIT |
| `encoding_rs` | 0.8.35 | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| `enum-as-inner` | 0.6.1 | MIT/Apache-2.0 |
| `equivalent` | 1.0.2 | Apache-2.0 OR MIT |
| `errno` | 0.3.14 | MIT OR Apache-2.0 |
| `esaxx-rs` | 0.1.10 | Apache-2.0 |
| `fallible-iterator` | 0.3.0 | MIT/Apache-2.0 |
| `fallible-streaming-iterator` | 0.1.9 | MIT/Apache-2.0 |
| `fancy-regex` | 0.18.0 | MIT |
| `fancy-regex` | 0.19.0 | MIT |
| `fastrand` | 2.5.0 | Apache-2.0 OR MIT |
| `fastwebsockets` | 0.10.0 | Apache-2.0 |
| `find-msvc-tools` | 0.1.9 | MIT OR Apache-2.0 |
| `flate2` | 1.1.9 | MIT OR Apache-2.0 |
| `float8` | 0.7.0 | MIT |
| `fluent-uri` | 0.4.1 | MIT |
| `flume` | 0.12.0 | Apache-2.0/MIT |
| `fnv` | 1.0.7 | Apache-2.0 / MIT |
| `foldhash` | 0.2.0 | Zlib |
| `form_urlencoded` | 1.2.2 | MIT OR Apache-2.0 |
| `fraction` | 0.16.0 | MIT OR Apache-2.0 |
| `fs-err` | 3.3.1 | MIT OR Apache-2.0 |
| `fs4` | 1.1.0 | MIT OR Apache-2.0 |
| `funty` | 2.0.0 | MIT |
| `futures` | 0.3.33 | MIT OR Apache-2.0 |
| `futures-channel` | 0.3.33 | MIT OR Apache-2.0 |
| `futures-core` | 0.3.33 | MIT OR Apache-2.0 |
| `futures-executor` | 0.3.33 | MIT OR Apache-2.0 |
| `futures-io` | 0.3.33 | MIT OR Apache-2.0 |
| `futures-macro` | 0.3.33 | MIT OR Apache-2.0 |
| `futures-sink` | 0.3.33 | MIT OR Apache-2.0 |
| `futures-task` | 0.3.33 | MIT OR Apache-2.0 |
| `futures-util` | 0.3.33 | MIT OR Apache-2.0 |
| `gemm` | 0.19.0 | MIT |
| `gemm-c32` | 0.19.0 | MIT |
| `gemm-c64` | 0.19.0 | MIT |
| `gemm-common` | 0.19.0 | MIT |
| `gemm-f16` | 0.19.0 | MIT |
| `gemm-f32` | 0.19.0 | MIT |
| `gemm-f64` | 0.19.0 | MIT |
| `generic-array` | 0.14.7 | MIT |
| `getrandom` | 0.2.17 | MIT OR Apache-2.0 |
| `getrandom` | 0.3.4 | MIT OR Apache-2.0 |
| `getrandom` | 0.4.3 | MIT OR Apache-2.0 |
| `h2` | 0.4.16 | MIT |
| `half` | 2.7.1 | MIT OR Apache-2.0 |
| `hashbrown` | 0.16.1 | MIT OR Apache-2.0 |
| `hashbrown` | 0.17.1 | MIT OR Apache-2.0 |
| `hashlink` | 0.12.1 | MIT OR Apache-2.0 |
| `heck` | 0.5.0 | MIT OR Apache-2.0 |
| `hermit-abi` | 0.5.2 | MIT OR Apache-2.0 |
| `hex` | 0.4.3 | MIT OR Apache-2.0 |
| `hiqlite-derive` | 0.14.0 | Apache-2.0 |
| `hmac` | 0.12.1 | MIT OR Apache-2.0 |
| `hostname` | 0.4.2 | MIT |
| `http` | 1.4.2 | MIT OR Apache-2.0 |
| `http-body` | 1.1.0 | MIT |
| `http-body-util` | 0.1.4 | MIT |
| `http-range-header` | 0.4.2 | MIT |
| `httparse` | 1.10.1 | MIT OR Apache-2.0 |
| `httpdate` | 1.0.3 | MIT OR Apache-2.0 |
| `hybrid-array` | 0.4.14 | MIT OR Apache-2.0 |
| `hyper` | 1.10.1 | MIT |
| `hyper-rustls` | 0.27.9 | Apache-2.0 OR ISC OR MIT |
| `hyper-util` | 0.1.20 | MIT |
| `iana-time-zone` | 0.1.65 | MIT OR Apache-2.0 |
| `iana-time-zone-haiku` | 0.1.2 | MIT OR Apache-2.0 |
| `icu_collections` | 2.2.0 | Unicode-3.0 |
| `icu_locale_core` | 2.2.0 | Unicode-3.0 |
| `icu_normalizer` | 2.2.0 | Unicode-3.0 |
| `icu_normalizer_data` | 2.2.0 | Unicode-3.0 |
| `icu_properties` | 2.2.0 | Unicode-3.0 |
| `icu_properties_data` | 2.2.0 | Unicode-3.0 |
| `icu_provider` | 2.2.0 | Unicode-3.0 |
| `ident_case` | 1.0.1 | MIT/Apache-2.0 |
| `idna` | 1.1.0 | MIT OR Apache-2.0 |
| `idna_adapter` | 1.2.2 | Apache-2.0 OR MIT |
| `if-addrs` | 0.15.0 | MIT OR BSD-3-Clause |
| `indexmap` | 2.14.0 | Apache-2.0 OR MIT |
| `inout` | 0.1.4 | MIT OR Apache-2.0 |
| `ipnet` | 2.12.1 | MIT OR Apache-2.0 |
| `is_terminal_polyfill` | 1.70.2 | MIT OR Apache-2.0 |
| `itertools` | 0.14.0 | MIT OR Apache-2.0 |
| `itoa` | 1.0.18 | MIT OR Apache-2.0 |
| `jni` | 0.22.4 | MIT OR Apache-2.0 |
| `jni-macros` | 0.22.4 | MIT OR Apache-2.0 |
| `jni-sys` | 0.4.1 | MIT OR Apache-2.0 |
| `jni-sys-macros` | 0.4.1 | MIT OR Apache-2.0 |
| `js-sys` | 0.3.103 | MIT OR Apache-2.0 |
| `jsonschema` | 0.50.1 | MIT |
| `jsonschema-regex` | 0.50.1 | MIT |
| `jsonschema-value` | 0.50.1 | MIT |
| `kamadak-exif` | 0.6.1 | BSD-2-Clause |
| `lazy_static` | 1.5.0 | MIT OR Apache-2.0 |
| `libc` | 0.2.189 | MIT OR Apache-2.0 |
| `libm` | 0.2.16 | MIT |
| `libpgs` | 0.6.0 | MIT OR Apache-2.0 |
| `libsqlite3-sys` | 0.38.1 | MIT |
| `linux-raw-sys` | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `litemap` | 0.8.2 | Unicode-3.0 |
| `lock_api` | 0.4.14 | MIT OR Apache-2.0 |
| `log` | 0.4.33 | MIT OR Apache-2.0 |
| `lru-slab` | 0.1.2 | MIT OR Apache-2.0 OR Zlib |
| `lz4-sys` | 1.11.1+lz4-1.10.0 | MIT |
| `macro_rules_attribute` | 0.2.3 | Apache-2.0 OR MIT OR Zlib |
| `macro_rules_attribute-proc_macro` | 0.2.3 | Apache-2.0 OR MIT OR Zlib |
| `maplit` | 1.0.2 | MIT/Apache-2.0 |
| `matchers` | 0.2.0 | MIT |
| `matchit` | 0.8.4 | MIT AND BSD-3-Clause |
| `mdns-sd` | 0.20.3 | Apache-2.0 OR MIT |
| `memchr` | 2.8.3 | Unlicense OR MIT |
| `memmap2` | 0.9.11 | MIT OR Apache-2.0 |
| `micromap` | 0.3.0 | MIT |
| `mime` | 0.3.17 | MIT OR Apache-2.0 |
| `mime_guess` | 2.0.5 | MIT |
| `minimal-lexical` | 0.2.1 | MIT/Apache-2.0 |
| `miniz_oxide` | 0.8.9 | MIT OR Zlib OR Apache-2.0 |
| `mio` | 1.2.2 | MIT |
| `monostate` | 0.1.18 | MIT OR Apache-2.0 |
| `monostate-impl` | 0.1.18 | MIT OR Apache-2.0 |
| `mutate_once` | 0.1.2 | BSD-2-Clause |
| `no_std_io2` | 0.9.4 | Apache-2.0 OR MIT |
| `nom` | 7.1.3 | MIT |
| `nu-ansi-term` | 0.50.3 | MIT |
| `num` | 0.4.3 | MIT OR Apache-2.0 |
| `num-bigint` | 0.4.8 | MIT OR Apache-2.0 |
| `num-cmp` | 0.1.0 | MIT/Apache-2.0 |
| `num-complex` | 0.4.6 | MIT OR Apache-2.0 |
| `num-conv` | 0.2.2 | MIT OR Apache-2.0 |
| `num-integer` | 0.1.46 | MIT OR Apache-2.0 |
| `num-iter` | 0.1.46 | MIT OR Apache-2.0 |
| `num-rational` | 0.4.2 | MIT OR Apache-2.0 |
| `num-traits` | 0.2.19 | MIT OR Apache-2.0 |
| `num_cpus` | 1.17.0 | MIT OR Apache-2.0 |
| `oid-registry` | 0.8.1 | MIT OR Apache-2.0 |
| `once_cell` | 1.21.4 | MIT OR Apache-2.0 |
| `once_cell_polyfill` | 1.70.2 | MIT OR Apache-2.0 |
| `onig` | 6.5.3 | MIT |
| `onig_sys` | 69.9.3 | MIT |
| `opaque-debug` | 0.3.1 | MIT OR Apache-2.0 |
| `openraft` | 0.9.25 | MIT OR Apache-2.0 |
| `openraft-macros` | 0.9.25 | MIT OR Apache-2.0 |
| `openssl-probe` | 0.2.1 | MIT OR Apache-2.0 |
| `outref` | 0.5.2 | MIT |
| `parking_lot` | 0.12.5 | MIT OR Apache-2.0 |
| `parking_lot_core` | 0.9.12 | MIT OR Apache-2.0 |
| `password-hash` | 0.5.0 | MIT OR Apache-2.0 |
| `paste` | 1.0.15 | MIT OR Apache-2.0 |
| `pastey` | 0.2.3 | MIT OR Apache-2.0 |
| `pem` | 3.0.6 | MIT |
| `percent-encoding` | 2.3.2 | MIT OR Apache-2.0 |
| `pin-project` | 1.1.13 | Apache-2.0 OR MIT |
| `pin-project-internal` | 1.1.13 | Apache-2.0 OR MIT |
| `pin-project-lite` | 0.2.17 | Apache-2.0 OR MIT |
| `pkg-config` | 0.3.33 | MIT OR Apache-2.0 |
| `poly1305` | 0.8.0 | Apache-2.0 OR MIT |
| `potential_utf` | 0.1.5 | Unicode-3.0 |
| `powerfmt` | 0.2.0 | MIT OR Apache-2.0 |
| `ppv-lite86` | 0.2.21 | MIT OR Apache-2.0 |
| `proc-macro-crate` | 3.5.0 | MIT OR Apache-2.0 |
| `proc-macro2` | 1.0.107 | MIT OR Apache-2.0 |
| `pulp` | 0.22.3 | MIT |
| `pulp-wasm-simd-flag` | 0.1.1 | MIT |
| `qrcode` | 0.14.1 | MIT OR Apache-2.0 |
| `quick-xml` | 0.41.0 | MIT |
| `quinn` | 0.11.11 | MIT OR Apache-2.0 |
| `quinn-proto` | 0.11.16 | MIT OR Apache-2.0 |
| `quinn-udp` | 0.5.15 | MIT OR Apache-2.0 |
| `quote` | 1.0.47 | MIT OR Apache-2.0 |
| `r-efi` | 5.3.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| `r-efi` | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| `radium` | 0.7.0 | MIT |
| `rand` | 0.10.2 | MIT OR Apache-2.0 |
| `rand` | 0.8.7 | MIT OR Apache-2.0 |
| `rand` | 0.9.5 | MIT OR Apache-2.0 |
| `rand_chacha` | 0.3.1 | MIT OR Apache-2.0 |
| `rand_chacha` | 0.9.0 | MIT OR Apache-2.0 |
| `rand_core` | 0.10.1 | MIT OR Apache-2.0 |
| `rand_core` | 0.6.4 | MIT OR Apache-2.0 |
| `rand_core` | 0.9.5 | MIT OR Apache-2.0 |
| `rand_distr` | 0.5.1 | MIT OR Apache-2.0 |
| `rand_pcg` | 0.10.2 | MIT OR Apache-2.0 |
| `raw-cpuid` | 11.6.0 | MIT |
| `rayon` | 1.12.0 | MIT OR Apache-2.0 |
| `rayon-cond` | 0.4.0 | Apache-2.0/MIT |
| `rayon-core` | 1.13.0 | MIT OR Apache-2.0 |
| `rcgen` | 0.14.8 | MIT OR Apache-2.0 |
| `reborrow` | 0.5.5 | MIT |
| `redox_syscall` | 0.5.18 | MIT |
| `ref-cast` | 1.0.26 | MIT OR Apache-2.0 |
| `ref-cast-impl` | 1.0.26 | MIT OR Apache-2.0 |
| `referencing` | 0.50.1 | MIT |
| `regex` | 1.13.1 | MIT OR Apache-2.0 |
| `regex-automata` | 0.4.16 | MIT OR Apache-2.0 |
| `regex-syntax` | 0.8.11 | MIT OR Apache-2.0 |
| `reqwest` | 0.12.28 | MIT OR Apache-2.0 |
| `reqwest` | 0.13.4 | MIT OR Apache-2.0 |
| `ring` | 0.17.14 | Apache-2.0 AND ISC |
| `rsqlite-vfs` | 0.1.1 | MIT |
| `rusqlite` | 0.40.1 | MIT |
| `rust-embed` | 8.12.0 | MIT |
| `rust-embed-impl` | 8.12.0 | MIT |
| `rust-embed-utils` | 8.12.0 | MIT |
| `rustc-hash` | 2.1.3 | Apache-2.0 OR MIT |
| `rustc_version` | 0.4.1 | MIT OR Apache-2.0 |
| `rusticata-macros` | 4.1.0 | MIT/Apache-2.0 |
| `rustix` | 1.1.4 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `rustls` | 0.23.42 | Apache-2.0 OR ISC OR MIT |
| `rustls-native-certs` | 0.8.4 | Apache-2.0 OR ISC OR MIT |
| `rustls-pki-types` | 1.15.0 | MIT OR Apache-2.0 |
| `rustls-platform-verifier` | 0.7.0 | MIT OR Apache-2.0 |
| `rustls-platform-verifier-android` | 0.1.1 | MIT OR Apache-2.0 |
| `rustls-webpki` | 0.103.13 | ISC |
| `rustversion` | 1.0.23 | MIT OR Apache-2.0 |
| `ryu` | 1.0.23 | Apache-2.0 OR BSL-1.0 |
| `safetensors` | 0.8.0 | Apache-2.0 |
| `same-file` | 1.0.6 | Unlicense/MIT |
| `schannel` | 0.1.29 | MIT |
| `schemars` | 1.2.2 | MIT |
| `scopeguard` | 1.2.0 | MIT OR Apache-2.0 |
| `security-framework` | 3.7.0 | MIT OR Apache-2.0 |
| `security-framework-sys` | 2.17.0 | MIT OR Apache-2.0 |
| `semver` | 1.0.28 | MIT OR Apache-2.0 |
| `seq-macro` | 0.3.6 | MIT OR Apache-2.0 |
| `serde` | 1.0.229 | MIT OR Apache-2.0 |
| `serde_core` | 1.0.229 | MIT OR Apache-2.0 |
| `serde_derive` | 1.0.229 | MIT OR Apache-2.0 |
| `serde_json` | 1.0.151 | MIT OR Apache-2.0 |
| `serde_path_to_error` | 0.1.20 | MIT OR Apache-2.0 |
| `serde_plain` | 1.0.2 | MIT/Apache-2.0 |
| `serde_rusqlite` | 0.43.0 | MIT OR Apache-2.0 |
| `serde_spanned` | 0.6.9 | MIT OR Apache-2.0 |
| `serde_urlencoded` | 0.7.1 | MIT/Apache-2.0 |
| `sha1` | 0.10.7 | MIT OR Apache-2.0 |
| `sha2` | 0.10.9 | MIT OR Apache-2.0 |
| `sha2` | 0.11.0 | MIT OR Apache-2.0 |
| `sharded-slab` | 0.1.7 | MIT |
| `shlex` | 2.0.1 | MIT OR Apache-2.0 |
| `signal-hook-registry` | 1.4.8 | MIT OR Apache-2.0 |
| `simd-adler32` | 0.3.10 | MIT |
| `simd_cesu8` | 1.2.0 | Apache-2.0 OR MIT |
| `simdutf8` | 0.1.5 | MIT OR Apache-2.0 |
| `slab` | 0.4.12 | MIT |
| `smallvec` | 1.15.2 | MIT OR Apache-2.0 |
| `socket-pktinfo` | 0.4.1 | MIT |
| `socket2` | 0.6.5 | MIT OR Apache-2.0 |
| `spin` | 0.9.9 | MIT |
| `spm_precompiled` | 0.1.4 | Apache-2.0 |
| `sqlite-wasm-rs` | 0.5.5 | MIT |
| `stable_deref_trait` | 1.2.1 | MIT OR Apache-2.0 |
| `static_assertions` | 1.1.0 | MIT OR Apache-2.0 |
| `strsim` | 0.11.1 | MIT |
| `strum` | 0.28.0 | MIT |
| `strum_macros` | 0.28.0 | MIT |
| `subtle` | 2.6.1 | BSD-3-Clause |
| `syn` | 2.0.119 | MIT OR Apache-2.0 |
| `syn` | 3.0.3 | MIT OR Apache-2.0 |
| `sync_wrapper` | 1.0.2 | Apache-2.0 |
| `synstructure` | 0.13.2 | MIT |
| `sysctl` | 0.6.0 | MIT |
| `tap` | 1.0.1 | MIT |
| `tempfile` | 3.27.0 | MIT OR Apache-2.0 |
| `thiserror` | 1.0.69 | MIT OR Apache-2.0 |
| `thiserror` | 2.0.19 | MIT OR Apache-2.0 |
| `thiserror-impl` | 1.0.69 | MIT OR Apache-2.0 |
| `thiserror-impl` | 2.0.19 | MIT OR Apache-2.0 |
| `thread-priority` | 3.1.1 | MIT |
| `thread_local` | 1.1.10 | MIT OR Apache-2.0 |
| `time` | 0.3.51 | MIT OR Apache-2.0 |
| `time-core` | 0.1.9 | MIT OR Apache-2.0 |
| `time-macros` | 0.2.30 | MIT OR Apache-2.0 |
| `tinystr` | 0.8.3 | Unicode-3.0 |
| `tinyvec` | 1.12.0 | Zlib OR Apache-2.0 OR MIT |
| `tinyvec_macros` | 0.1.1 | MIT OR Apache-2.0 OR Zlib |
| `tokenizers` | 0.22.2 | Apache-2.0 |
| `tokio` | 1.53.1 | MIT |
| `tokio-macros` | 2.7.1 | MIT |
| `tokio-rustls` | 0.26.4 | MIT OR Apache-2.0 |
| `tokio-util` | 0.7.18 | MIT |
| `toml` | 0.8.23 | MIT OR Apache-2.0 |
| `toml_datetime` | 0.6.11 | MIT OR Apache-2.0 |
| `toml_datetime` | 1.1.1+spec-1.1.0 | MIT OR Apache-2.0 |
| `toml_edit` | 0.22.27 | MIT OR Apache-2.0 |
| `toml_edit` | 0.25.13+spec-1.1.0 | MIT OR Apache-2.0 |
| `toml_parser` | 1.1.3+spec-1.1.0 | MIT OR Apache-2.0 |
| `toml_write` | 0.1.2 | MIT OR Apache-2.0 |
| `tower` | 0.5.3 | MIT |
| `tower-http` | 0.6.11 | MIT |
| `tower-layer` | 0.3.3 | MIT |
| `tower-service` | 0.3.3 | MIT |
| `tracing` | 0.1.44 | MIT |
| `tracing-attributes` | 0.1.31 | MIT |
| `tracing-core` | 0.1.36 | MIT |
| `tracing-futures` | 0.2.5 | MIT |
| `tracing-log` | 0.2.0 | MIT |
| `tracing-serde` | 0.2.0 | MIT |
| `tracing-subscriber` | 0.3.23 | MIT |
| `try-lock` | 0.2.5 | MIT |
| `typed-path` | 0.12.3 | MIT OR Apache-2.0 |
| `typenum` | 1.20.1 | MIT OR Apache-2.0 |
| `unicase` | 2.9.0 | MIT OR Apache-2.0 |
| `unicode-general-category` | 1.1.0 | Apache-2.0 |
| `unicode-ident` | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| `unicode-normalization-alignments` | 0.1.12 | MIT/Apache-2.0 |
| `unicode-segmentation` | 1.13.3 | MIT OR Apache-2.0 |
| `unicode-xid` | 0.2.6 | MIT OR Apache-2.0 |
| `unicode_categories` | 0.1.1 | MIT OR Apache-2.0 |
| `universal-hash` | 0.5.1 | MIT OR Apache-2.0 |
| `untrusted` | 0.9.0 | ISC |
| `unty` | 0.0.4 | MIT OR Apache-2.0 |
| `url` | 2.5.8 | MIT OR Apache-2.0 |
| `utf-8` | 0.7.6 | MIT OR Apache-2.0 |
| `utf8-width` | 0.1.9 | MIT |
| `utf8_iter` | 1.0.4 | Apache-2.0 OR MIT |
| `utf8parse` | 0.2.2 | Apache-2.0 OR MIT |
| `uuid` | 1.24.0 | Apache-2.0 OR MIT |
| `uuid-simd` | 0.8.0 | MIT |
| `validit` | 0.2.6 | MIT OR Apache-2.0 |
| `valuable` | 0.1.1 | MIT |
| `vcpkg` | 0.2.15 | MIT/Apache-2.0 |
| `version_check` | 0.9.5 | MIT/Apache-2.0 |
| `virtue` | 0.0.18 | MIT |
| `vsimd` | 0.8.0 | MIT |
| `walkdir` | 2.5.0 | Unlicense/MIT |
| `want` | 0.3.1 | MIT |
| `wasi` | 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `wasip2` | 1.0.4+wasi-0.2.12 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `wasm-bindgen` | 0.2.126 | MIT OR Apache-2.0 |
| `wasm-bindgen-futures` | 0.4.76 | MIT OR Apache-2.0 |
| `wasm-bindgen-macro` | 0.2.126 | MIT OR Apache-2.0 |
| `wasm-bindgen-macro-support` | 0.2.126 | MIT OR Apache-2.0 |
| `wasm-bindgen-shared` | 0.2.126 | MIT OR Apache-2.0 |
| `wasm-streams` | 0.4.2 | MIT OR Apache-2.0 |
| `web-sys` | 0.3.103 | MIT OR Apache-2.0 |
| `web-time` | 1.1.0 | MIT OR Apache-2.0 |
| `webpki-root-certs` | 1.0.9 | CDLA-Permissive-2.0 |
| `webpki-roots` | 1.0.9 | CDLA-Permissive-2.0 |
| `widestring` | 1.2.1 | MIT OR Apache-2.0 |
| `winapi-util` | 0.1.11 | Unlicense OR MIT |
| `windows` | 0.62.2 | MIT OR Apache-2.0 |
| `windows-collections` | 0.3.2 | MIT OR Apache-2.0 |
| `windows-core` | 0.62.2 | MIT OR Apache-2.0 |
| `windows-future` | 0.3.2 | MIT OR Apache-2.0 |
| `windows-implement` | 0.60.2 | MIT OR Apache-2.0 |
| `windows-interface` | 0.59.3 | MIT OR Apache-2.0 |
| `windows-link` | 0.2.1 | MIT OR Apache-2.0 |
| `windows-numerics` | 0.3.1 | MIT OR Apache-2.0 |
| `windows-result` | 0.4.1 | MIT OR Apache-2.0 |
| `windows-service` | 0.8.1 | MIT OR Apache-2.0 |
| `windows-strings` | 0.5.1 | MIT OR Apache-2.0 |
| `windows-sys` | 0.52.0 | MIT OR Apache-2.0 |
| `windows-sys` | 0.61.2 | MIT OR Apache-2.0 |
| `windows-targets` | 0.52.6 | MIT OR Apache-2.0 |
| `windows-threading` | 0.2.1 | MIT OR Apache-2.0 |
| `windows_aarch64_gnullvm` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_aarch64_msvc` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_i686_gnu` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_i686_gnullvm` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_i686_msvc` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_x86_64_gnu` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_x86_64_gnullvm` | 0.52.6 | MIT OR Apache-2.0 |
| `windows_x86_64_msvc` | 0.52.6 | MIT OR Apache-2.0 |
| `winnow` | 0.7.15 | MIT |
| `winnow` | 1.0.4 | MIT |
| `wit-bindgen` | 0.57.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `writeable` | 0.6.3 | Unicode-3.0 |
| `wyz` | 0.5.1 | MIT |
| `x509-parser` | 0.18.1 | MIT OR Apache-2.0 |
| `yasna` | 0.6.0 | MIT OR Apache-2.0 |
| `yoke` | 0.8.3 | Unicode-3.0 |
| `yoke-derive` | 0.8.2 | Unicode-3.0 |
| `zerocopy` | 0.8.56 | BSD-2-Clause OR Apache-2.0 OR MIT |
| `zerocopy-derive` | 0.8.56 | BSD-2-Clause OR Apache-2.0 OR MIT |
| `zerofrom` | 0.1.8 | Unicode-3.0 |
| `zerofrom-derive` | 0.1.7 | Unicode-3.0 |
| `zeroize` | 1.9.0 | Apache-2.0 OR MIT |
| `zerotrie` | 0.2.4 | Unicode-3.0 |
| `zerovec` | 0.11.6 | Unicode-3.0 |
| `zerovec-derive` | 0.11.3 | Unicode-3.0 |
| `zip` | 8.6.0 | MIT |
| `zmij` | 1.0.23 | MIT |

</details>

---

## 5. Android client

Every **runtime** dependency is Apache-2.0 — AndroidX and Compose, Media3
and ExoPlayer, OkHttp, Retrofit, Coil, and the kotlinx libraries — with one
exception:

| Dependency | Scope | License | Consequence |
|---|---|---|---|
| `com.google.android.gms:play-services-code-scanner` | runtime | Google APIs ToS / Android SDK License | Proprietary, not open source |
| `junit:junit` 4.13.2 | `testImplementation` | EPL-1.0 | Copyleft, but never in a shipped artifact |
| Gradle wrapper (`gradlew`, `gradlew.bat`, `gradle-wrapper.jar`) | build | Apache-2.0 · Gradle Inc. | Checked into the repo, so redistributed with every clone |

**How to read it:** the Play Services scanner is redistributable inside a
compiled app, so it does not affect plurx's license or the APKs you ship. It
does mean the Android client is not open source end to end, and that it needs
Google Play Services on the device — a Play-Services-free variant has to drop
or replace the QR scanner.

JUnit is the one copyleft license anywhere in plurx's graph. EPL-1.0 is
file-level copyleft and applies to distribution of the covered work; JUnit is
`testImplementation` only, so it is absent from every APK and every server
artifact. It is listed because "no copyleft anywhere" would be false, and a
notices file that overstates its own cleanliness is not worth reading.

The Gradle wrapper is third-party code committed into this repo rather than a
resolved dependency, which is exactly the kind of thing a blanket
repo-wide copyright claim swallows by accident. It remains Apache-2.0,
copyright Gradle Inc. and the original authors, and plurx claims no copyright
in it.

## 6. Apple client

No third-party runtime dependencies — the iOS and tvOS clients are SwiftUI
and AVFoundation only, and the EPUB reader they embed
(`reader.js`, `offline-reader.js`) is first-party.
[XcodeGen](https://github.com/yonaskolb/XcodeGen) (MIT) generates the project
at build time and ships in nothing.

---

## 7. Checked into this repo, never shipped

Not a dependency of anything plurx builds or serves — a source file living in
the tree, so it is redistributed with every clone the way the Gradle wrapper in
§5 is, and it needs its attribution for that reason alone.

| Component | Version | Where | License | Copyright |
|---|---|---|---|---|
| [acorn](https://github.com/acornjs/acorn) | 8.18.0 | `tests/vendor/acorn.js` | MIT — [full text](licenses/acorn-MIT.txt) | Copyright (c) 2012-2022 by various contributors |

`tests/web/asset-order.test.js` needs a real JavaScript parser: it refuses a
web-asset order in which a file runs something at load that names a binding
declared in a later file, and answering that over 1.3 MB of source with regular
expressions would be a guess. This repo has no `package.json` and no
`node_modules`, and adding either to run one test is a worse trade than 245 KB
of vendored source. It is not in `WEB_ASSETS`, never reaches a browser, and
never reaches the binary.

---

## Keeping this file honest

This document is a claim about what is in the dependency graph, so it goes
stale the moment the graph moves. `deny.toml` is the mechanical half:
`cargo deny check licenses` fails the build on any license not on the
allow-list, so a new copyleft dependency cannot land quietly. Run it with
`make license-check`.

`cargo deny` cannot see the parts that are not crates — the web UI bundle,
the fonts, the client dependencies, the Dockerfile's ffmpeg, the source checked
into the tree. Update §1, §2, §5, §6 and §7 by hand when those move, in the
same commit that moves them.
