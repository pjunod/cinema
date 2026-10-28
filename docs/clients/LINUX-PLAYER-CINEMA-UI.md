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
| Linux launcher scenarios | 97 passed, including attachment-event ownership and early EOF/error handling |
| Lua 5.2 overlay scenarios | 30 passed, including Retry and popup ownership |
| Scoped server compiler, lint and store/HTTP regressions | Running on pinned Linux Rust 1.97.1 |
| Debian build, fresh install/render/removal, negative checksum self-test | Pending |
| Byte-identical second package build | Pending |
| ROG prior package and rollback receipt | Installed baseline inspected: 0.3.2; rollback package pending |
| ROG package installation and installed-file checks | Pending |
| Physical HDR viewing and end-to-end web launch | Pending |

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
