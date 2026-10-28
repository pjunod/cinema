# Linux player Cinema UI — release and installation ledger

**Status:** open · **Written:** 2026-10-28 · **Executes:** the owner's approved
Cinema playback renders and request to complete metadata, next episode and
quality controls, package them and install on the ROG.

Companion to [the Linux player plan](LINUX-PLAYER-PLAN.md) (the playback/render
contract) and [the launcher README](../../clients/linux-player/README.md)
(controls and package commands). This ledger separates source evidence,
package qualification, installation and physical acceptance. It does not
qualify a different Dolby Vision processor or change the display stack.

## 1. Delivered source

Package 0.4.0 replaces the stock controller inside the existing mpv surface.
It adds scoped library metadata and synopsis, actual decoded resolution,
Auto/Original/advertised encode heights, next episode and persisted autoplay.
The existing mpv/libplacebo pins and display-management equations are retained.

The handoff token may read only its own context. Explicit Next resolves the
immediate ordered successor on the server and atomically changes the token's
file scope only if the expected predecessor is still current. The expiry and
minting-login binding do not change. A quality replacement has independent
provisional ownership and releases the incumbent after the candidate loads
and restarts. Early EOF cannot mark a whole film watched or trigger Next.

## 2. Qualification and installation

| Evidence | State |
|---|---|
| Linux launcher scenarios | 99 passed in fresh package verification; none skipped |
| Lua 5.2 overlay scenarios | 32 passed against the installed overlay, including selected-quality no-ops |
| Scoped server compiler, lint and store/HTTP regressions | Pinned Linux Rust 1.97.1 workspace Clippy passed; scoped advance contract passed on SQLite/three-voter hiqlite; 35 SQL-census tests passed; both scoped HTTP authorization/order cases passed after correcting the fixture's missing season number |
| Debian build, fresh install/render/removal, negative checksum self-test | Passed; private gpu-next/Vulkan and installed overlay verified |
| Byte-identical second package build | Passed for all six binary/source artifacts |
| ROG prior package and rollback receipt | 0.3.2 rollback Debian package saved and checksummed; 0.4.0 release staged/checksummed on ROG |
| ROG package installation and installed-file checks | Pending |
| Physical HDR viewing and end-to-end web launch | Pending |

The original implementation is commit `ab880ed254a09b5f822e7048f8b3248528e31b29`
under [PR #1307](http://forge.lan:3000/noirr/plurx/pulls/1307). Later receipt/fixture
changes do not change the qualified native package payload.

The qualified amd64 Debian package is
`0.4.0+lp7.387.0.mpv0.41.0+git20261026.e59d90c-1`, SHA-256
`f576789af1faf1a0a2d026dcb6b350368ab5bf88a62b0ac47028bf5768d2d406`.
This identifies the staged and reproducibly rebuilt binary; installation must
verify the same checksum before replacing the retained 0.3.2 package.

Release outputs, source identities, checksum manifests, logs and rollback
material belong outside compiler/cache directories. The release includes the
Debian binary and source packages, build metadata and verification/rebuild
receipts. No bearer or handoff URL belongs in those records.

## 3. Focused reproduction

```bash
make linux-player-check
lua5.2 clients/linux-player/tests/test_cinema.lua clients/linux-player/plurx_player/scripts/cinema.lua
cargo test -p plurxd --bin plurxd handoff_context_and_next_
cargo test -p plurx-core --features hiqlite-contract-tests --test store_contract playback_handoff_advances_only_a_live_expected_scope_without_extending_it
cargo test -p plurx-core --features hiqlite-store --lib store::placeholder_census
make linux-player-deb
```

The package command performs fresh-container installation and gpu-next
rendering, installed-overlay behavior, clean removal and a byte-identical
second build. Hardware playback still requires its own receipt.
