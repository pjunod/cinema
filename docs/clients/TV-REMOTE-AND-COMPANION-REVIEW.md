# TV remote design review — close the authority and input gaps before build

**Status:** open · **Reviewed:** 2026-10-07 · **Verdict:** revise before
implementation · **Source base:** `890bca0fc` plus the uncommitted plan.

Independent adversarial agent review of
[TV-REMOTE-AND-COMPANION-PLAN.md](TV-REMOTE-AND-COMPANION-PLAN.md), requested
by Paul. The parent agent checked the findings against the plan and relevant
source. This record leaves the proposed design unchanged so the findings
and subsequent corrections can be reviewed separately. Plan line numbers
refer to its 432-line version at review time; section references remain useful
after revisions.

## Verdict — feasible, with four corrections

The architecture and background invitation options are plausible. One
authorization gap blocks finalizing the control contract; three narrower
gaps need explicit protocol, availability, or prototype decisions. There is
no finding that phone remote control, CEC, or background invitations are
inherently infeasible.

| Finding | Priority | Resolution needed before |
|---|---|---|
| R1: indirect UI actions inherit receiver privilege | P1 | M1/M2 contract finalization |
| R2: command expiry crosses undefined clock domains | P2 | Command wire contract implementation |
| R3: desktop physical input depends on server availability | P2 | M5 desktop architecture |
| R4: tvOS prototype can skip the difficult native controls | P2 | Completing M0 |

## R1 — authorize the action behind every remote Select

**Plan:** §5, lines 237–245, together with §1 lines 17–20 and §5 lines
282–288.

**Failure:** the plan permits pairing across accounts and allows navigation
into Settings. It promises that pairing conveys no administrator access,
but explicitly applies both parties' content restrictions only to “Play on
TV.” A lower-privilege controller can instead send arrows and Select to an
administrator's receiver. The receiver executes its ordinary UI handlers
under its own credentials. Restricting the `play_item` command alone also
leaves indirect selection of receiver-only content outside that check.

**Evidence:** [settings.js](../../crates/plurxd/src/web/pages/settings.js)
line 261 checks the receiver's `ME.is_admin` before showing Settings.
[users-admin.js](../../crates/plurxd/src/web/pages/users-admin.js) exposes
user/device mutations through the existing authenticated API helper.
[extract.rs](../../crates/plurxd/src/http/extract.rs) lines 810–821 authorizes
an administrator request from the presented credential. These are correct
for local use; forwarding remote activation without a controller-aware
boundary would give the receiver authority to the paired controller.

**Required correction:** define authorization for the semantic action
behind every remotely activated control, and for every returned state field.
Use the intersection of controller and receiver permissions. Alternatively,
exclude privileged routes/actions from remote control and require a local
confirmation where appropriate. An initial pairing confirmation must not
silently authorize all future administrator actions.

**Acceptance:** a lower-privilege paired phone, using only arrows and Select,
cannot execute an administrator operation or bypass a content restriction.
The same check holds if a local user opens a privileged modal while the
phone remains connected. State responses must not leak restricted content.

## R2 — define how a receiver rejects a late command

**Plan:** §5, line 282 and lines 304–312.

**Failure:** a server-stamped monotonic deadline cannot be compared directly
with another machine's monotonic clock. Checking expiry only at relay
admission also fails: a frame can stall in transport, arrive seconds later
in the same receiver epoch, and execute after its one-second navigation
lifetime. Epoch checks alone do not detect this case.

**Required correction:** specify the expiration authority and a defensible
clock-domain conversion or conservative freshness protocol. Distinguish
relay admission timeout from receiver application timeout. A relative TTL
that starts anew on receiver arrival would still allow the stalled-frame
failure. Document uncertainty and the receiver's final freshness check.

**Acceptance:** stall a connected transport, release a navigation command
after its lifetime, and prove it is rejected without changing focus. Include
clock offset and elapsed transport time in the protocol tests; do not rely
on synchronized monotonic timestamps.

## R3 — decide whether local CEC must survive a server interruption

**Plan:** §5, lines 299–300, and §6, lines 316–323.

**Failure:** “physical input always works” conflicts with sending desktop
CEC through the server relay. A Mac or Pi can continue buffered playback
during a server restart, leadership transition, or LAN interruption, while
the TV remote can no longer pause, stop, or navigate Cinema. Native TV
physical input has a different failure boundary. The plan records added
latency as a trade-off but does not record this loss of availability.

**Required correction:** choose an authenticated local CEC-to-receiver path
for essential controls, or explicitly narrow the guarantee and accept the
desktop dependency. If choosing a local path, retain target scoping and avoid
desktop-wide key injection. This finding does not prescribe a particular IPC
or browser integration mechanism.

**Acceptance:** disconnect the server while a desktop plays buffered media
and operate the physical TV remote. Record the intended pause/stop/navigation
behavior, including recovery after reconnection. Adapter unplug/reconnect
tests alone do not cover this failure.

## R4 — make the tvOS spike exercise native menus and lazy content

**Plan:** §6, lines 343–348, and M0 at line 369.

**Failure:** proving arrows/Select/Back on one screen of app-owned buttons
can pass M0 without proving remote control of existing menus, pickers,
modals, and lazily loaded grids. A generic network direction cannot simply
be injected into tvOS's physical focus engine.

**Evidence:** [LibraryView.swift](../../clients/apple/Sources/LibraryView.swift)
around line 155 uses `Menu` and `Picker`.
[PlayerView.swift](../../clients/apple/Sources/PlayerView.swift) around line
2505 uses a tvOS audio/subtitle `Menu`. Apple's
[FocusState](https://developer.apple.com/documentation/swiftui/focusstate/)
supports app-owned focus state, while its
[tvOS focus documentation](https://developer.apple.com/documentation/uikit/about-focus-interactions-for-apple-tv)
does not establish a general network key-injection interface.

**Required correction:** expand M0 to open, navigate, select, and dismiss an
existing menu/picker; enter a search; scroll beyond currently visible lazy
grid items; dismiss a modal with Back; and alternate phone and physical
remote input. Identify system-managed controls that require app-owned
replacement before estimating the native receiver work.

**Acceptance:** demonstrate those interactions on physical tvOS hardware.
Record any replacement controls and their accessibility implications. This
is a scope and feasibility spike, not a claim tvOS control is impossible.

## Platform checks that do not warrant new findings

The plan already treats platform-dependent support as something to verify.
Android documents foreground services for network-connected device
interaction; its service type, power behavior, and distribution checks remain
necessary. Apple's Local Push Connectivity supports the proposed investigation
but comes with the restricted-network purpose and entitlement caveat.
Pulse-Eight documents Linux, macOS, Windows, and Raspberry Pi support; actual
hardware acceptance and the libCEC distribution decision remain necessary.
[Android service types](https://developer.android.com/develop/background-work/services/fgs/service-types),
[Apple Local Push Connectivity](https://developer.apple.com/videos/play/wwdc2020/10113/),
[Pulse-Eight platform guide](https://support.pulse-eight.com/support/solutions/articles/30000053002-what-is-the-pulse-eight-cec-adapter-and-libcec-).

The existing typed media relay is not proof of a generic bidirectional remote
channel. However, §4 already requires a cluster streaming/fencing spike, so
this is a recorded dependency rather than an additional finding.

## Evidence limits

This was an independent agent design/source review with official platform
documentation checks, followed by parent-agent verification. No physical
hardware, push delivery, focus prototype, or executable remote protocol was
tested. Documentation link/index checks validate this record's integration,
not the design's runtime behavior. All four findings remain open until the
plan defines the corrections and implementation supplies the named evidence.


## Build disposition — 2026-10-07

[Implementation §9](TV-REMOTE-AND-COMPANION-IMPLEMENTATION.md#9-review-findings-become-concrete-acceptance-not-erased-history)
specifies corrections for all four findings; the
[protocol](TV-REMOTE-PROTOCOL.md) is the executable handoff contract.
R1 uses same-account pairing and a semantic allowlist; R2 receiver-issued
credits; R3 a direct local desktop bridge; R4 the expanded native walkthrough.
These are design dispositions, not runtime closure. The
[status ledger](TV-REMOTE-AND-COMPANION-STATUS.md) retains each evidence gap.
