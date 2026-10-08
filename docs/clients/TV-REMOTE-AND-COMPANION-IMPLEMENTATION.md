# Cinema remote implementation — build packets for Sol 6.1

**Status:** ready for staged implementation; hardware acceptance open ·
**Written:** 2026-10-07 · **Source base:** `8e242787c` · **Manager/reviewer:**
parent session · **Builders:** GPT-6.1 Sol agents.

Read [the user experience](TV-REMOTE-AND-COMPANION-PLAN.md), then
[the wire contract](TV-REMOTE-PROTOCOL.md). Work one packet at a time;
[the status ledger](TV-REMOTE-AND-COMPANION-STATUS.md) records what is built,
reviewed, compiled and physically demonstrated. A builder must raise a
contract conflict to the manager rather than change shared interfaces or
expand another packet's ownership. Source anchors below are real at the base;
re-verify them on the task branch before editing.

## 1. Deliver the entire couch-control path in reviewable packets

The finished experience has directional TV input, an explicitly paired
phone/tablet remote, a foreground suggestion when a paired TV is available,
and opt-in background invitations. Linux/Pi and Mac desktop support lead;
Windows uses the same host protocol. Android TV and Apple TV consume native
physical input and implement app-owned navigation for network control.

Unfinished components get an explicit Settings → Developer switch with
advisory readiness. Saving the user's choice must succeed even if an adapter,
entitlement or device is absent. Runtime status explains what is missing;
never fabricate availability. Graduate a switch only after the documented
acceptance. The existing Developer lifecycle rules still apply.

The earlier instruction “no unit tests, it's just a proposal” applies to
proposal/build documentation. Do not launch unit suites for docs. For actual
behavior changes, builders run the smallest meaningful regression and affected
compile checks; the batching coordinator schedules broad qualification once
for the composed candidate. Do not use CI as a compiler.

## 2. Integration and ownership — the parent reviews every packet

Integrate on `effort/cinema-remotes`. Each builder uses an isolated managed
worktree and `codex/cinema-remote-<packet>` branch based on the current effort.
Open task PRs into the effort, not main. Shared-file owners serialize changes;
merely having different functions in one file is not independent ownership.
No builder merges main, starts a full test matrix, deploys, or publishes an
extension/app-store build. The parent inspects diffs and reports actionable
findings directly to the builder, then rechecks fixes.

While an effort gate is pending, create a dependent task from the current
effort and merge its reviewed dependency locally. Record that dependency in
the task PR and revalidate the final integration tree; a local dependency
merge is not permission to merge the shared effort.

After review and required effort checks, integrate each packet through its
PR. Keep corrective `Regression-Test:` lines in both PR and landing message.
Native or desktop changes under `clients/` also need an accurate
`tests/client-fixes.toml` source/test anchor for every corrective authored
commit. That mapping does not assert a test execution. Run `make history-check`
before push; a missing mapping stops the gate before compilation. Normal
hooks remain enabled. Freeze the completed effort and provide a
main-bound PR and evidence to the batching session:
`codex://threads/01a11907-f720-71b1-8c51-89902b919e6f`.
That session owns batch placement, shared tests and landing in main. The
parent owns readiness; never hand an incomplete implementation over as done.

| Packet | Owned files/surface | Depends on | Exit evidence |
|---|---|---|---|
| B01 | `plurx-core/src/remote_control.rs`, its module registration, protocol fixtures/regressions | This contract | Bounded wire and receiver guard compile + focused rejection regressions |
| B02 | Web semantic navigation/router and playback adapters, shell/WEB_ASSETS/layout table, web remote regressions | B01 vocabulary | Existing keyboard policy preserved; registered remote actions fenced |
| B03 | Apple navigation coordinator and app-owned focus/menu prototype, related view integration | B01 vocabulary | iOS/tvOS compilation; R4 walkthrough explicitly pending until device run |
| B04 | Core `RemoteStore`, migrations/backends; daemon owner actor/routes/auth/state/route classification; API reference | B01 | SQLite + replicated focused auth/revocation/owner regressions; daemon compile |
| B05 | Web receiver, mobile web remote/pairing/card, web settings registrations | B02 + B04 | Browser two-client pairing/control walkthrough; auth and stale reply regressions |
| B06 | Apple receiver/playback adapter and iOS/iPadOS companion, native settings | B03 + B04 | Both native builds; foreground lifecycle and command guard evidence |
| B07 | Android coordinator/receiver/companion, native settings and API client | B04 | Debug + test APK compile; focused navigation/lifecycle regression |
| B08 | `clients/desktop-remote/` host, Chromium extension, install/uninstall, packaging | B02 local adapter | Host/extension checks; Linux/Pi + Mac physical CEC acceptance |
| B09 | Invitation store/routes/broker component and Apple/Android notification adapters | B04 + B06 + B07 | Consent/rotation/cooldown checks; actual provider/device delivery recorded |
| B10 | Operations, compatibility matrix and final evidence; fixes remain with owning builder | All | Parent review, physical acceptance record, ready handoff to batch coordinator |

Paths prefixed `plurx-core` mean `crates/plurx-core`. Native surface roots are
`clients/apple/Sources/` and
`clients/android/app/src/main/java/tv/plurx/app/`. The manager owns these three
new build/protocol/status documents and their index rows. Builders return
receipt text in `/private/tmp/` for the manager instead of racing the ledger.
B02/B05 own all web integration files sequentially. B03/B06 own Apple files
sequentially. B04/B09 own server schema/routing sequentially. B08 must request
any shared Cargo workspace changes before adding them; prefer a standalone
helper package to avoid colliding with B04.

## 3. B01 — make the wire and final receiver guard executable

Implement typed `cinema.remote.v1` envelopes, action enums, target/context
structures and strict validation in `remote_control.rs`. Use serde tagged
variants with unknown-field rejection. Put bounds in named constants; use
the same constants for encode/decode and fixtures. The module is independent
of HTTP, store, browser and player internals.

Implement a small receiver guard whose caller supplies local monotonic
elapsed time, active target/control/context/focus and allowed capabilities.
It owns bounded credits and replay state. It does not mint wall-clock
leases, call a player, or infer administrator permission from an action name.
A command is only eligible after the caller's semantic authorization check.
Expose a reserve/commit design or an atomic validate-and-consume operation
so repeated Select cannot race before acknowledgement. Define outcomes in
one enum matching protocol §4. Overflow rejects; it never wraps into a valid
sequence/revision. Reserve zero for uninitialized sequence/revision.

Provide serialized fixtures for each action, invalid combinations, stale
contexts, and timeout/replay outcomes. Receiver code in other languages must
read the same fixture meanings; do not generate a Rust-only private dialect.
B01 owns command/action/target/outcome/guard types and fixtures. B04 owns
HTTP DTOs and must record exact request/response shapes before B05/B06/B07
consume them. Keep the initial module inert until B04/B05 wire it in. No settings switch
is needed for an unreachable library type.

**Acceptance:** pinned core check and denied-warning Clippy pass. Focused
regressions cover a delayed command across unrelated clocks, deadline equality,
duplicate Select, takeover, route/focus change, malformed actions, size and
integer limits. Record exact commands and test anchors, not a full suite.

## 4. B02 — web navigation is an explicit semantic registry

Read [WEB-SHELL-LAYOUT.md](WEB-SHELL-LAYOUT.md) before searching. Add plain
scripts `core/remote-navigation.js` and `core/remote-router.js` only with
matching `WEB_ASSETS`, shell tags and source-map rows. Do not introduce ESM.
A single `CinemaRemote` global owns registration, current safe snapshot,
context/focus revisions, invalidation and dispatch. Register stable IDs for
safe Home/library/detail/search controls; ordinary DOM focusability is not
permission to activate remotely. Owned modals have their own registry scope.
A route/modal change removes old registrations before admitting another
Select. Empty/disconnected DOM entries cannot be activated.

Use spatial focus among registered visible enabled items, with deterministic
tie-breaking and visible focus styling. Preserve the stable library item ID
on detail Back. Lazy grids scroll/load the next registered item rather than
calling focus on an unrealized DOM node. If a screen is not integrated,
report unsupported/restricted honestly; do not fall back to clicking it.
Privileged routes and modal overlays must fail closed, including overlays
opened by physical input. Home/Back cannot escape that boundary by triggering
an unregistered handler.

Keep existing keyboard hotkeys. CEC/network directions use the couch input
policy, including showing hidden player controls before horizontal seeking
where the player contract requires it. `player/transport.js` remains the
owner of VOD play intent. Add an idempotent desired-playing entry point there
if needed; do not call `video.play/pause` from the remote module. Route Live
TV through `PlaybackPolicy.routeLiveInput` and existing live TV owners.
Stopping playback pauses locally immediately. Preserve existing fullscreen
and PiP teardown ordering; server cleanup cannot delay the local pause.

For B02 expose a local development dispatch entry point to prove UI behavior;
B05 adds authenticated network admission, and B08 adds extension delivery.
Neither a JavaScript source string nor a DOM event is an auth credential.

**Acceptance:** existing asset-order/load checks and the smallest affected
player input regressions. New checks cover admin-modal rejection, physical
focus invalidating Select, stale registration after rerender, library Back
restoration and buffered playback stop/pause with API unavailable. Record
which routes are integrated; do not report full couch navigation from one
button demonstration.

## 5. B03/B06 — Apple app-owned navigation and one playback owner

Start at `HomeView.swift`, `LibraryView.swift`, `SearchView.swift`,
`PlayerView.swift`, `PlayerRemoteAdapter.swift`, `AppModel.swift` and
`PlurxApp.swift`. `RemoteNavigationCoordinator` is MainActor-owned: selected
Cinema tab, per-tab route paths, modal stack, semantic focus key, revisions,
safe text context and typed activation closures. Link native focus changes
to this same state. Network directions cannot be injected into the tvOS
system focus engine.

B03 implements Home → Library → Search → Detail and reusable accessible
app-owned choice controls, then inventories every remaining system-managed
control. B06 integrates Player and Live TV with those same controls. B03
observes physically opened overlays at the root before Select, Back or Home;
a screen registry alone must not activate controls behind a restricted modal. Replace controls which network navigation
cannot operate with accessible app-owned menus/pickers. Preserve labels,
selected traits, VoiceOver order and physical remote behavior. No new
AVPlayer or MPNowPlaying/remote command owner: B06 adapts to existing
`inputState`/`applyPlayerInputOutcome` and `PlayerController` actions.

B06 implements same-account pairing, receiver foreground session, epoch and
credit validation, phone/tablet device picker, touch remote, keyboard sheet,
and automatic card for a single paired available screen. Multiple screens
show Choose a screen. Dismissal lasts the phone app session; persistent
suggestions preference is per receiver. A receiver disappears after lease
expiry. Closing/suspending the phone does not stop TV playback. Logout,
server switch and tvOS background revoke pending work locally immediately.

**R4 device walkthrough:** open Library Sort and Filter, navigate/select/
dismiss each; move over two screenfuls and a server page boundary in a lazy
grid; search with a current text nonce, open a result and Back; enter player
audio/subtitle/quality menus; open/dismiss Live TV guide controls; alternate
phone and physical remote at every stage and recover focus. A screenshot of
a basic button row does not satisfy this. Record device/OS/build and any
remaining unsupported controls.

**Acceptance:** `make apple-build` compiles iOS and tvOS with signing off.
Focused native behavior evidence follows the packet's changed boundaries.
Simulator compilation is not physical tvOS acceptance. Use the repo's build
number process when producing a distributable; do not burn a distribution
number merely to compile an isolated navigation prototype.

## 6. B04/B05/B07 — connect the authenticated system

B04 adds the protocol's storage operations to both backends and migrations.
Recheck actual schema names before using the illustrative SQL. Avoid
replicated per-keypress writes; consistent grant/user authorization reads
must remain authoritative, and token revocation must not leave a cached
receiver/controller credential valid indefinitely. Queue lifetime is bounded
and owner loss invalidates all work. A signed internal request never allows
an arbitrary asserted user. Follow `http/extract.rs`, `http/peer_transport.rs`
and existing signed internal handlers; update native-route classification.
Do not log secrets in rejection diagnostics or URLs.

Add an explicit unfinished feature switch `cinema_remote_control` through
the existing settings machinery with a Developer readiness explanation.
Saving true works; runtime capability/status remains truthful. A disabled
feature cancels network sessions, removes discovery and rejects new control.
Physical native TV input continues normally. B08 local CEC has its separate
local setting and does not depend on this network switch.

B05 implements the browser receiver and mobile web fallback as a full
vertical slice. Pairing requires visible TV approval from local input. QR
parsing has no login/grant secret and never auto-connects or starts playback.
Only approved grants get title/state. Use current server identity from login,
not an arbitrary host from a QR/deep link. Prompt before adding a new server
using the existing login flow. Test two browser contexts with distinct local
storage, not one global state object pretending to be phone and TV.

B07 uses `MainActivity.kt` NavController and the exact `NavBackStackEntry.id`
fence from `BackNavigation.kt`. Implement stable item focus with the existing
LazyGridState/TvFocus machinery. Route through `PlayerInputPolicy.route` and
`applyOutcome` in PlayerScreen/LiveTvScreen. No second ExoPlayer or
MediaSession. Own app menus explicitly; do not simulate Android accessibility
keypresses. Companion UI and lifecycle match B06.

**Acceptance:** B04 pinned affected Rust/all-target compilation and focused
SQLite/Hiqlite authorization/revocation/owner regressions. Any test of
replicated storage uses `--features hiqlite-store`, never bare core tests.
B05 pairing/expiry/control/revoke/owner-loss browser walkthrough. B07
`make android-instrumentation-build` plus the smallest relevant regression;
that target builds APKs and does not imply an emulator/device pass.

## 7. B08 — local desktop CEC without a server round trip

Use a Chromium Manifest V3 extension and native messaging host named
`tv.plurx.cinema_remote`. Browser launches the per-user host through
`connectNative`; keep stdout exclusively for length-framed JSON and send
logs to stderr. Bound every message at 16 KiB before allocation. Host framing
is native-endian unsigned 32-bit length followed by UTF-8 JSON. Check the
caller extension origin against installer configuration. Do not expose a
localhost HTTP listener or inject keys into the desktop.

The user grants one Cinema origin and binds one top-level tab. Scope delivery
to browser profile, tab ID, document ID and binding epoch. Validate runtime
message sender origin/frame 0/tab/document; unbind on document replacement,
cross-origin navigation, logout, server/account switch, browser restart or
explicit disconnect. Never pick another tab automatically. Treat page
messages as untrusted, even with a nonce. Page-to-host operations are limited
to validated local input status/configuration; never arbitrary process/file
access. No content script can call native messaging directly.

Input is structured CEC callbacks, not scraping `cec-client` diagnostic
text. Linux may use the kernel CEC interface with an explicit device allowlist
and installer-granted user access; Mac/Windows use supported USB-CEC through
a separately packaged adapter. Before bundling/linking libCEC, record its
exact version/license and compatible distribution choice. An unapproved
license/package is a distribution blocker, not permission to silently copy
GPL source into Apache-licensed modules. A user-installed backend can be
documented, but it still needs a real structured interface and smoke test.

Implement detection/error states: no adapter, permission denied, adapter
busy, disconnected, bound, receiver unavailable. Support reconnect with
bounded backoff; release holds on unplug/sleep/host exit. Do not start as root
or ship world-writable device/socket permissions. Install/uninstall only
Cinema's manifest, helper and permission rules, leaving other CEC software
untouched. Linux/Pi ARM64 and Mac are first physical targets; Windows gets
build/package coverage and a separately recorded physical row.

**Acceptance:** disconnect the server during buffered VOD and Live TV;
physical CEC pause/stop and rendered-screen navigation still work. New media
loading may require the server and should say so. Also exercise two tabs,
malicious-origin messages, lost key-up, adapter unplug, browser restart and
first-interaction restrictions. Extension events cannot manufacture browser
user activation for fullscreen/autoplay. Supply a local first-gesture setup
step where required. Keep all hardware claims in the compatibility table.

Official implementation references:
[Chrome native messaging](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging),
[libCEC API](https://pulse-eight.github.io/libcec/), and
[Linux CEC API](https://docs.kernel.org/userspace-api/media/cec/cec-intro.html).

## 8. B09 — a background service is platform-specific, not impossible

Foreground discovery works without background privileges. Android also gets
an opt-in, user-started resident service with ongoing notification and Stop.
Use `connectedDevice` only with a legitimate qualifying device/network
interaction and actual prerequisite. The current INTERNET/network-read
permissions alone do not satisfy the documented service prerequisites.
Investigate Companion Device Manager association or a genuine scoped local
network discovery requirement; do not add a misleading permission solely to
make service startup pass. Record the selected API/permission and Play
eligibility before enabling the shipping service. If eligibility is absent,
show unavailable with the foreground alternative while preserving Save.

For iOS/iPadOS use visible APNs invitations, with FCM as an Android option.
Add enrollment/rotation/revocation adapters and a separately configured
notification broker. The broker owns provider keys; never distribute the
shipping app's signing key to a home server. Server-to-broker enrollment uses
explicit user consent, random revocable delivery grants and verified broker
TLS. Payload is an opaque invitation ID plus generic screen-ready text, no
media title, playback URL, login credential or control secret. Tap reauthenticates
and checks current receiver/grant/presence before showing Remote.

Do not replace the existing Apple `OfflineAppDelegate` notification delegate:
route reminders and invitations through one handler. Do not use silent push,
audio, location, VPN, or VoIP to promise continuous background execution.
Investigate `NEAppPushProvider` separately for restricted local networks;
Apple entitlement approval is an external dependency, not assumed support.

Opt-in is per phone/TV, defaults off, with once-per-TV-foreground-session
notification and thirty-minute cooldown per pair. Reconnect flaps do not
create fresh foreground sessions. Dedupe in durable invitation records so
server restarts cannot resend. Disable/logout/grant revoke stops delivery
and removes enrollment. Notification permission denial leaves foreground
remote use intact. Never label server reachability as physical proximity.

**Acceptance:** consent off means zero sends; opt-in emits once; flaps and
server restart do not resend; token rotates; revoked grant stops invitations;
permission denied and stale tap have truthful UX. Real iOS suspended-app
APNs and Android Doze/OEM delivery require a provider configuration and
physical devices. No source-only check can establish that evidence.

Official constraints:
[Android foreground service types](https://developer.android.com/develop/background-work/services/fgs/service-types),
[APNs registration](https://developer.apple.com/documentation/usernotifications/registering-your-app-with-apns),
[Apple Local Push Connectivity](https://developer.apple.com/documentation/networkextension/local-push-connectivity).

## 9. Review findings become concrete acceptance, not erased history

| Original finding | Build decision | Evidence still required |
|---|---|---|
| R1 privilege escalation | Same-account grants plus semantic action allowlist; no admin routes/modals | Attempts through arrows/Select and locally opened admin modal |
| R2 undefined expiry clock | Receiver-issued nonce credits with receiver-local monotonic deadlines | Delayed delivery cannot alter focus; clock offset irrelevant |
| R3 server-dependent CEC | Per-user native host/extension directly to bound tab | Server-offline buffered pause/stop/navigation on real CEC hardware |
| R4 shallow tvOS prototype | Full menu/lazy-grid/search/modal/mixed-input walkthrough | Physical tvOS walkthrough, replacement/accessibility inventory |

The design decisions resolve the documents' ambiguities. They do not close
runtime findings until evidence exists. The parent reviews each resulting
patch for these failures, ordinary correctness, bounded resource use,
shutdown/cancellation, credential exposure and existing input regressions.

## 10. Builder receipt and final handoff

Each receipt includes packet ID, exact commit/base/tree, changed files,
compile toolchain and commands, focused regression anchors/results, known
limitations, remaining physical checks and review findings resolved. Return
the PR URL and leave the branch stable for review. Do not claim another
platform passes because a shared type compiled on macOS.

Before B10 handoff, the parent reconciles actual implementations with the
protocol, updates API/operations/compatibility documentation, and records
accepted versus unverified platform behavior. Preserve an explicit unfinished
status where credentials, entitlements or physical hardware are unavailable.
The merge coordinator receives the exact candidate, all task PRs, parent
review verdict, evidence applicability, known blockers and smallest remaining
qualification set. No unit suites are requested for documentation alone.

## 11. Complete ordinary couch navigation after the transport packets

The B02/B03/B06/B07 reviews distinguish safe transport from complete everyday
navigation. A route intentionally fails closed until it has an owner. That
restriction is not a substitute for implementing an ordinary browsing flow.
Freeze reviewed transport heads, then use separate follow-up branches based
on their reviewed dependencies. The same Sol 6.1 builders own these changes;
the parent reviews them and the coordinator composes qualification.

| Follow-up | File ownership | Concrete completion |
|---|---|---|
| Web couch | B02/B05 web navigation/router, item/watch/preplay and Live TV files; web regressions; synchronized shell asset metadata if needed | Start-over; app-owned preplay/version choices; episode disclosure, season and episode controls; Live TV entry, channel list/scrub and guide view/watch/close |
| Apple couch | B03/B06 Apple navigation/views, library/detail/Home routes, pairing-surface lifetime and state publisher; Apple regressions | Expanded library groups; ordinary shared-library/Coming Soon browsing; episodes, start-over and preplay/version choices; stale TV pairing response retirement; normalized state budget and strict integer lexical consistency |
| Android couch | B07 Android navigation/views, library/detail/Live TV and existing playback adapters; Android regressions | Expanded library dialog ownership; detail/episode/start-over/preplay choices; channel/guide navigation; complete common-screen inventory before release |

An Android item may finish inside B07 before its head is frozen. The receipt
must say which branch owns each remaining item. Do not edit a frozen packet
silently or let parallel builders modify the same native/web file.

For each new safe surface, register bounded stable keys in an owned route or
presentation scope. Use the existing authorization, loader and player.
Realize offscreen lazy items, wait for actual native/browser focus before
Select, and restore the exact opener on Back. Data, route, menu and physical
focus changes invalidate pending actions. A generic DOM click, OS key
injection or native Picker activation is not an implementation of this
contract. Preserve physical scrubs and accessible labels/selection traits.

Unknown system, credential, administrative, sharing-management and destructive
presentations stay restricted. Ordinary already-authorized shared-library
browsing is distinct from managing shares. Guide navigation/watch is distinct
from scheduling or deleting a recording. Do not expand authority merely to
make a navigation inventory look complete.

Follow-up receipts enumerate every common surface as implemented, intentionally
restricted, or still requiring a named software task. Include focused evidence
for route/Back ownership, stale or unrealized Select, owned choices, lazy
boundaries and physical-input arbitration. Repeat affected compile checks on
the final source. Physical TV, VoiceOver and mixed-device walkthroughs remain
separate evidence; an unimplemented control is not a hardware-only limitation.


## 12. Background invitation build decisions

The parent approved B09's server/broker design on 2026-10-08. The server Sol
builder owns the independent invitation store, home API, bounded owner worker,
notification broker and API reference. Native adapters follow the frozen API;
the web Developer card is assigned only after B05 freezes. Shared protocol,
status and build instructions remain manager-owned. This extends the packet
ownership above; it does not authorize parallel edits to another builder's
files.

Invitations default off. Save preserves the selected setting even when a
provider, permission or native transport is unavailable. Turning consent off
requires the current same-user Native login and phone proof, but never a
still-valid TV grant, provider connection or enabled global feature. Lost-phone
revocation remains available to the same user without that phone's proof.
Enabling consent and enrolling delivery require current grant authority.

Background authorization reads must not refresh a dormant login's idle expiry.
Atomic admission rechecks phone, grant, receiver, login and consent generations,
then admits at most one event per receiver/foreground/enrollment and one per
receiver/phone in thirty minutes. Keep live-authority dedupe identities across
reconnects, restarts, consent toggles, replacement grants and transport rotation.
The approved account cap is 100000 event identities; exhaustion reports
`retention_limit` without changing saved consent. Cleanup may remove revoked
scopes, never quietly reset a live scope's dedupe or cooldown.

Persist an attempted transition before provider I/O. Unknown outcomes consume
that attempt; neither home nor broker silently retries an uncertain visible
notification. Broker dedupe, tickets, revocation work and all request/reply
bodies need explicit bounds. Reject expired invitation identities before any
history pruning could make them replayable. Poll/list responses use bounded
prefixes or pagination. A notification carries a locally resolvable opaque
identity, never a bearer proof, media title or credential destination. Tap
reauthenticates and resolves a unique live receiver; it neither acquires control
nor starts playback.

Phone registration may return existing metadata to a proved retry, but cannot
recover a one-time plaintext secret. A lost initial response requires explicit
revocation/re-registration. Enrollment generation changes suspend old delivery
before network I/O. Missing provider readiness never undoes enabled consent.
OS device tokens go directly to the configured HTTPS broker; provider keys
remain there. The home server has only its scoped publisher credential.

Restore/import disables portable invitation capabilities. Broker restore uses
an explicit operator procedure and an external generation/key manifest;
missing or mismatched generation fails closed. Do not claim a raw copied old
database can detect its own rollback without independent state. Preserve the
existing local-reminder delegate and media/download owners in native clients.
Android resident service work must establish actual permitted connected-device
networking, explicit user start and an ongoing Stop action. Stop cannot silently
restart; discovery is only a hint and never authorizes a credential destination.

Build the strict DTOs, schema and atomic authority/admission contracts first,
then the worker, home routes and real APNs/FCM request formation/signing. Use
focused SQLite and actual Hiqlite contracts plus synthetic provider transport
evidence. Physical push delivery, entitlements, Android service eligibility on
devices and OS notification behavior remain explicit acceptance items.


## 13. Reviewed follow-up ownership refinements

Local CEC pairing uses a separately admitted physical-input path. A fixed
Home control can open a local pairing surface and explicitly enable receiving
or register this installation. It never changes the server's admin setting.
The network registry excludes both this entry and its approval controls. Bind
approval to the current native-host/extension identity, fresh credit, dialog
generation, exact pending request and actual focused control. Pending-list
replacement immediately retires queued selection. Start on a safe Close or
show-code control, and restore the live opener on Back. Do not manufacture
`Event.isTrusted` or add arbitrary selectors to the extension protocol.

Native destination ownership must bound the scope stack and bind every lazy
realization to its token, parent and current route. Replacement/disposal cannot
change a newer scope. Back restores its exact surviving opener or a deterministic
available fallback. Apple couch also owns the remaining TV pairing-surface
lifetime, visible code expiry, strict negative-zero decoding, normalized state
budget, monotonic phone pairing deadline and ACK-before-queued correction.
Android received the latter two corrections before its foundation froze; its
couch packet still owns TV code-expiry presentation and the ordinary surface
inventory in §11.

An authorized shared-library browse path is not complete playback control if
it lands in an unowned shared player. Reuse the existing shared controller and
its remote-server authorization. The selected v1 representation is nullable
playback metadata with capability-driven transport controls, never a foreign
item ID presented as a current-server item. Bind the full shared reference and
controller lifetime into context. Unknown playing state requires explicit Play
and Pause controls. The native builder must submit the deferred-effect admission
and typed outcome design for parent review before implementing it: bounded
in-flight reservations consume sequence before effect, never replay unknown
outcomes, and cannot affect a replacement owner after cancellation/disposal.
No second player or bypass of shared-server control authority is permitted.
