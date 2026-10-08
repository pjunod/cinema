# Cinema desktop CEC

This source implementation receives TV remote input for one explicitly bound Cinema tab. It uses a Chromium Manifest V3 extension and a per-user Python native-messaging host named `tv.plurx.cinema_remote`. It does not inject operating-system keys, run a loopback HTTP server, or require Cinema's server remote-control service for local transport.

Status: source and injected-protocol regressions built; physical acceptance, real libCEC binding interoperability, interactive origin permission prompts, and distributable packaging remain open. A real Chromium smoke loads the unpacked extension and verifies MAIN/document lifecycle with an injected native port and a fixture-only pregranted origin. Linux/Pi ARM64 and Linux x86_64 use the kernel CEC backend where the HDMI hardware exposes `/dev/cecN`. Mac uses a separately installed supported USB CEC adapter and libCEC. Windows shares the extension and structured libCEC adapter; its registry and launcher plan are tested with mocks, and execution in a real Windows browser remains unverified. Safari and Firefox are not supported by this manifest. Raspberry Pi 32-bit ioctl layouts are not supported.

## Load and register

Use Python 3.10 or newer. Load `extension/` as an unpacked extension in Chrome/Chromium/Edge developer mode. Copy its exact 32-character extension ID; a different ID is rejected by both the native-host manifest and host configuration. This is a review installation, not a browser-store publication or signed release.

On Linux/Pi with a kernel CEC device, register as the logged-in user:

```sh
python3 clients/desktop-remote/install.py --prefix "$HOME/.local/share/cinema-cec" --extension-id EXTENSION_ID --browser chromium --backend kernel --device /dev/cec0
```

Only the selected device is opened. The user must already have read/write device permission through their distribution's device policy. The installer does not create broad permissions, udev rules, privileged services, or root registrations. It refuses occupied logical addresses or another exclusive owner. HDMI/EDID physical addressing must be supplied by the kernel; unplugged or unconfigured hardware reports `waiting_for_tv` rather than guessing an address. The kernel backend disables RC passthrough and claims a playback logical address for its own lifetime.

For Mac or a libCEC-backed desktop, independently obtain upstream libCEC **8.1.6**, including the structured Python `cec` binding, using your platform's package/build instructions. Choose the actual `strComName` adapter identifier from that installation's structured adapter discovery; no diagnostic text is parsed:

```sh
python3 clients/desktop-remote/install.py --prefix "$HOME/.local/share/cinema-cec" --extension-id EXTENSION_ID --browser chrome --backend libcec --device ADAPTER_IDENTIFIER
```

The selected interpreter must import that user-installed binding under `python -I`; user-site and current-directory imports are intentionally excluded. A package that installs only into user-site is therefore insufficient. Version mismatch, missing binding, no adapter, and failed open are reported explicitly. Upstream `Open(False)` does not distinguish busy from permissions, so the wrapper reports `open_failed` without claiming a diagnosis. The host never loads an adapter path requested by a page.

On Windows, run `install.py` with Python and a per-user prefix and `--browser chrome`, `chromium`, or `edge`. The implementation generates a `.cmd` isolated-interpreter launcher and an exact HKCU native-host registration. The launcher has not been qualified against a Windows browser; a verified executable launcher may be required before Windows support can graduate. No Windows execution or libCEC device test is claimed.

`--dry-run` prints intended files and registry key without changing them. `--manifest-dir` can select a temporary manifest directory for review; production browser discovery still requires the documented browser directory. Remove only this verified installation with:

```sh
python3 clients/desktop-remote/install.py --prefix "$HOME/.local/share/cinema-cec" --uninstall
```

The receipt records installed file hashes. Changing browser or manifest/registry target requires uninstalling first or choosing a separate prefix; reinstall never replaces the old registration receipt silently. Installation refuses foreign files and foreign registry values; uninstall refuses changed files, symlinks, and registry keys outside the exact Cinema Chrome/Chromium/Edge allowlist. Extra unrelated files in the prefix are preserved. Registry removal occurs only when the default still points to this installation's manifest. Remove the unpacked extension separately in the browser.

## Bind and use

Open an authenticated Cinema tab and enable **Local TV remote** in the extension popup. This persisted local choice is separate from Cinema's server setting. Press **Bind this Cinema tab**, granting the exact selected HTTP(S) origin. Only one tab, top-level document, window, authenticated account, and epoch may receive input. Navigation, logout, server/account changes, hidden or unfocused document, leaving the bound tab/window, helper disconnect, or browser restart disconnects the binding. Return to the tab and bind explicitly again. Initial Play/fullscreen can require a real local browser gesture; extension delivery cannot manufacture browser user activation.

Up/down/left/right navigate B02's registered safe actions; Select activates the registered action; Back and Home follow its authorized router. Play, pause, toggle, and Stop use the existing VOD/LiveTV owners. Unknown routes and modals fail closed. No DOM selectors from the host, arbitrary clicks, scripts, process arguments, file requests, power, or volume commands cross the boundary. Existing physical keyboard scrub behavior remains owned by the player.

Every CEC delivery calls `CinemaRemote.physicalInput()` before capturing the current semantic snapshot and dispatching as `local_cec`. This retires pending network gestures without cancelling physical keyboard scrubs. Local pause does not await an API call. LiveTV Stop pauses immediately while preserving its fullscreen exit-before-detach fence; server cleanup may remain pending.

The native stream uses strict native-endian 32-bit length framing and UTF-8 JSON, capped at 16 KiB before payload allocation. Only exact bind/heartbeat/release/unbind commands are accepted. The extension accepts native input only for its current epoch and positive monotonic sequence. Extension-issued random credits expire after 750 ms on its own clock; the MAIN receiver independently registers the same nonce with a 750 ms deadline on its own clock. A queued native frame or MAIN invocation cannot acquire a fresh lifetime after a stall. No timestamps are compared between processes. The host rechecks heartbeat eligibility after blocking open/poll; events older than 750 ms or with negative age are discarded. One-second heartbeat loss clears held input and closes the backend. Direction repeat starts after 350 ms, is capped at eight events per second, and ends after 750 ms without a press; Select and playback toggle never repeat. Reconnect delay is bounded at 1–30 seconds. A stale binding requires explicit rebind.

## License and dependency boundary

Newly authored adapter, host, extension, installer, and tests use the repository's Apache-2.0 license. This tree contains no copied, bundled, or downloaded libCEC source or native binary. libCEC 8.1.6 is an optional, separately user-installed dependency whose upstream terms are **GPL-2.0-or-later or a commercial license**. This source adapter does not alter those terms, assert Apache/GPL compatibility for a combined work, or make process separation a license waiver. Any combined package or redistribution requires a separate compatible-license decision. No commercial-only restriction is assumed. The kernel adapter uses public Linux CEC UAPI definitions; it does not vendor a kernel implementation.

`package.py --output /absolute/path/review.zip` creates an exclusive, source-only review archive containing the host, installer, README and unpacked extension. It contains no Python runtime, libCEC, native library, signing key, credentials, or browser-store bundle. This archive is not a qualified release.

## Focused evidence and remaining acceptance

```sh
python3 -m unittest discover -s clients/desktop-remote/tests -p 'test_*.py'
node --test clients/desktop-remote/tests/extension.test.mjs
PLAYWRIGHT_MODULE=/path/to/playwright CHROMIUM_EXECUTABLE=/path/to/chromium node --test clients/desktop-remote/tests/extension.browser.cjs
```

The tests inject backends, clocks, browser APIs and Windows registry observations. They cover framing, bounded events, stale credits, open/poll stalls, logical destination checks, structured callback parsing and cleanup, sender/document fencing, overlapping binds, focus loss, epoch-specific cleanup, and install ownership. Passing mocks does not prove upstream binding ABI or physical adapter behavior.

Still required: actual Linux/Pi kernel CEC and Mac libCEC 8.1.6 adapter tests; permission-denied/busy/unplug/reconnect and held-key/lost-release trials; real Chromium native-host installation and interactive origin permission prompt; offline VOD/LiveTV pause/Stop through this extension; focus/navigation/account-change and suspended-browser stale Select trials; first-gesture media behavior; native Windows launcher/registry/browser qualification; and a separate release/license decision for any combined distribution. No hardware is available in this build session.

## Platform references

- [Chromium native messaging and per-user host registration](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging)
- [Chromium document-targeted scripting and MAIN world](https://developer.chrome.com/docs/extensions/reference/api/scripting)
- [Extension service-worker lifetime](https://developer.chrome.com/docs/extensions/develop/concepts/service-workers/lifecycle)
- [Linux CEC userspace API](https://docs.kernel.org/userspace-api/media/cec/cec-intro.html)
- [Linux v6.12 public CEC ABI](https://github.com/torvalds/linux/blob/v6.12/include/uapi/linux/cec.h)
- [libCEC structured Python API](https://pulse-eight.github.io/libcec/python/)
- [libCEC 8.1.6 release](https://github.com/Pulse-Eight/libcec/releases/tag/libcec-8.1.6)
- [Exact upstream licensing terms](https://github.com/Pulse-Eight/libcec/blob/libcec-8.1.6/LICENSE.md)
