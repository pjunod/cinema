# Linux player Cinema UI — release and installation ledger

**Status:** open: 0.5.0 frontend source, package and system-renderer interfaces
qualified; merge, coordinated installation and physical acceptance pending.
The 0.4.0 installation and owner acceptance below remain earlier evidence. ·
**Updated:** 2026-11-09 · **Written:** 2026-10-28

Companion to [the Linux player plan](LINUX-PLAYER-PLAN.md) (the playback/render
contract) and [the launcher README](../../clients/linux-player/README.md)
(controls and package commands). This ledger separates source evidence,
package qualification, installation and physical acceptance. It does not
qualify a different Dolby Vision processor or change the display stack.

## Session control sequence repair (0.5.1)

**Owner:** Codex coordinator; GPT-6.1 Sol implementation. **State:** source
implemented and independently reviewed; final affected validation is recorded
in the pull request.
Deployment and physical acceptance remain owner-controlled.

Seeking outside the produced range creates a new server session, whose first
control exchange must use sequence 1. The frontend previously retained its
run-wide sequence counter, so the replacement's controls were refused as stale
until its lease expired. Quality replacement and retry shared the same ownership mismatch; next
episode already constructs a fresh controller. The counter now belongs to the advertised
session ID, generation and owner epoch. A provisional quality change retains
the incumbent counter so rollback continues monotonically on that session.
Server generation, client-instance and sequence fences retain their contract.

Regression coverage exercises seek recreation, retry, next episode and quality
rollback after candidate controls. A failed first heartbeat before server
acceptance remains a separate transport/replay concern; this repair does not
change retransmission policy.

The inherited Dolby Vision report is separate: initial native mapping, then a
reset diagnostic, followed by a seek and stale controls. The private provider
uses that diagnostic for both missing RPU and explicit stream resets; successful
mapping at the same target need not repeat its first-success message. It alone
cannot establish sustained HDR10 fallback or a changed server recipe. A fresh
owner-started run on November 13 reported active native DV beyond the inherited
6–24 second window. Sustained playback and physical picture acceptance remain
open; no causal claim about previous tests or calibrated panel accuracy follows.

## System renderer consolidation (0.5.0)

The launcher now selects an explicit `--mpv` override, then
`PLURX_PLAYER_MPV`, then executable `/usr/bin/mpv`, then `mpv` on PATH.
The former `/usr/lib/plurx-player/bin/mpv` never wins implicitly. The Debian
package depends on system mpv and installs only the frontend; it no longer
builds or ships a private mpv/libplacebo or upstream source components.

Cinema keeps its Lua controls, full-film timeline, scoped credentials,
server/session ownership, isolated configuration, renderer arguments and
loader-environment cleanup. The installed-interface smoke check uses real mpv
`loadfile`, properties, user-data and Cinema's Lua pause binding. Binary
selection and the exact frontend file manifest have focused regressions.

Native Dolby Vision is negotiated from the selected binary's compiled
`native-dovi-api-1` marker on its `List of enabled features:` version line.
The probe runs with `--no-config --version`, a three-second timeout and the
same loader-variable cleanup as playback. Marked builds retain P5/P7/P8;
unmarked, failed or timed-out probes request server conversion to the existing
HDR10 route and omit Dolby Vision in playback-control capabilities. The
selected renderer still opens; user overrides and saved choices are retained.
This is format negotiation, not a runtime feature switch or a version gate.
The marker's provider boundary requires SDK 2. ROG's unmarked 5.1 renderer must
be updated to its marked 5.2 successor before frontend installation.

The existing mapper/client-log/OSD fact fields recognize only the shared
renderer’s exact successful physical-target message from `vo/gpu-next` at
Info level. Its finite physical peak overrides the signal-envelope property;
the label is `shared native Dolby renderer`, without inventing a CM version.
Facts reset on each load/start-file and authoritative target-state changes.
The read-only `native-dovi-target-state` property reports eligibility and render
generation; a success must match its active current generation. One read on a
success or state notification prevents stale compositor/lease facts without
polling. Eligibility alone does not establish an active grade. A new
file with no new success stays unknown; a compiled marker alone never proves
an active grade. Cinema Info’s VO-target rows retain their existing signal
encoding observations. Independent source review and six focused new/legacy mapper regressions pass.

Qualification is scoped to the recorded frontend source and interfaces:

| Evidence | Result |
|---|---|
| Focused binary/capability/package/control regressions | 26 passed |
| Physical-target facts, spoof/nonfinite input, source/lease/generation lifecycle and legacy mapper regressions | 6 passed |
| Fresh package verification client suite | 113 passed; no skips |
| Installed Cinema Lua | 32 control scenarios and 3 live Info regressions passed |
| Normal Debian package and fresh verification | Passed; lintian zero errors/warnings, checksum negative control, ownership, system default, PQ gpu-next/Vulkan, config/ytdl controls and clean removal |
| Reproducibility | Second build byte-identical across all four binary/source artifacts |
| Frontend package payload | 41,652 bytes; no bundled renderer or library |
| Qualified mpv 5.2 plus exact frontend package | Marker/native capabilities and real generated-video Lua/IPC passed with null outputs; configured unsupported VO reports native target state unavailable |

The frontend payload was built from `3024053`; current-main integration
preserves all 61 transferred source/test-input blobs and executable modes.
The shared renderer's active/inactive generations, read-only setter and
teardown notification have separate native qualification. Interface and
software-Vulkan checks do not establish optical HDR calibration or Dolby
conformance. Installation remains pending. Earlier private-renderer CPU
controls and 0.4.x receipts retain only their original scopes. This change
makes no claim about unrelated desktop notification behavior.

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

### 1.1 Playback Info correction (0.4.1)

Both Info entry points now open live playback diagnostics rather than library
synopsis. The panel uses documented mpv 0.41 properties for decoded picture,
color/Dolby Vision observations, VO target, renderer/decoder, audio, FPS,
packet bitrate, drops and demuxer buffer/input estimates. Whitelisted launcher
enums identify delivery and session kind; no decision/session dictionary,
URL, token or private file path is forwarded. Missing readings remain
Unavailable and diagnostic polling runs only while the panel is open.
Title and episode metadata remain visible outside the panel.

The Lua regression exercises both buttons, live/paused changes, late missing
properties, narrow-window scrolling and closed-panel polling. The Python
regression checks delivery facts and rejects credential/path forwarding.
0.4.1 package qualification, installation and owner acceptance are pending;
the 0.4.0 receipts below remain evidence for that earlier package only.
The engine pins and display-management equations are unchanged. VO target
readings do not establish calibrated luminance or physical HDR acceptance.

## 2. Qualification and installation

| Evidence | State |
|---|---|
| Linux launcher scenarios | 99 passed in fresh package verification; none skipped |
| Lua 5.2 overlay scenarios | 32 passed against the installed overlay, including selected-quality no-ops |
| Scoped server compiler, lint and store/HTTP regressions | Pinned Linux Rust 1.97.1 workspace Clippy passed; scoped advance contract passed on SQLite/three-voter hiqlite; 35 SQL-census tests passed; both scoped HTTP authorization/order cases passed after correcting the fixture's missing season number |
| Debian build, fresh install/render/removal, negative checksum self-test | Passed; private gpu-next/Vulkan and installed overlay verified |
| Byte-identical second package build | Passed for all six binary/source artifacts |
| ROG prior package and rollback receipt | 0.3.2 rollback Debian package retained and checksummed |
| ROG package installation and installed-file checks | Passed on 2026-10-28: package checksum, `dpkg --verify`, installed launcher VERSION 0.4.0, private mpv 0.41.0/plurx5 with libplacebo 7.387 and `plurx-player.desktop` protocol registration |
| Main promotion and landing | PR #1307 passed Main promotion gate run 6113 (run ID 6145); merged as `530e2edad2b678c563e8d3d9c7ec6df1d30a076b` with the qualified source tree unchanged |
| Ingress server deployment | Standard `make docker-up` deployment from the merged commit passed: healthy container, `/healthz` and `/readyz` HTTP 200, unauthenticated context HTTP 401; host override and 2535 s health start period retained |
| ROG local server deployment | Standard `make docker-up` deployment from the merged commit passed: healthy container, `/healthz` and `/readyz` at the local and configured ingress origins returned HTTP 200 from the ROG, unauthenticated context HTTP 401; private override/environment hashes and 1500 s health start period retained |
| Live movie launch | Owner-launched Dolby Vision movie verified on 2026-10-28: 3840×2160 p010, Dolby Vision matrix, PQ/BT.2020, gpu-next and NVDEC; title/year/runtime/overview and Auto/Original/advertised heights present, player error null |
| Owner-confirmed Cinema acceptance | On 2026-10-28 the owner confirmed that the UI and quality switching work after the requested Original → Auto check. This is functional owner feedback, not an instrumented position-preservation trace |
| Remaining live acceptance | TV Next was not exercised because the launched title was a movie, correctly with no next episode. Automated Next/store/HTTP regressions passed; live episode advance remains open |

The original implementation is commit `ab880ed254a09b5f822e7048f8b3248528e31b29`
under [PR #1307](http://forge.lan:3000/noirr/plurx/pulls/1307). The gate passed
for reviewed head `f0d81c92dbca42903173dbdd48301cdf2d6fea58`; its tree and the
merged tree are both `3a8886d02d99c0482144dccccbea6118351c4a7b`. Later
receipt/fixture changes do not change the qualified native package payload.
The full corrective-history audit passed for 4311 commits.

The qualified amd64 Debian package is
`0.4.0+lp7.387.0.mpv0.41.0+git20261026.e59d90c-1`, SHA-256
`f576789af1faf1a0a2d026dcb6b350368ab5bf88a62b0ac47028bf5768d2d406`.
The installed package archive matches this checksum and the reproducibly
rebuilt package. The 0.3.2 rollback package remains retained.

The ingress runtime image is
`sha256:3aca46319a9bb78591e11e9c3376cb49a24830679abaa6cb31ab5eb3ccee3b41`,
and the ROG runtime image is
`sha256:c920a70a03dfa809915b77456ae02a30bbb995d187121db1bbdf07716be7f66c`.
Both were built from the merged source above; the ROG running image's OCI
revision was verified against that commit. Server health and package checks
establish installation readiness. The owner's live movie launch also verifies
the decoded video, renderer and populated Cinema metadata/quality state. The
owner then confirmed that the UI and quality switching work; the player had
closed before the follow-up IPC read, so no final position-preservation trace
was captured. Live TV episode advance remains open. These receipts do not
qualify a different HDR processor or close the broader Linux player plan's
physical display matrix.

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
