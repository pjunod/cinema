# Sharing host setup — configure the private connection when sharing is enabled

**Status:** source implemented; broader qualification open · **Written:** 2026-10-28

Companion to [the sharing implementation](SHARED-LIBRARIES-IMPLEMENTATION.md)
(peer authority and transport) and [operations](../OPERATIONS.md#sharing-cinema-addresses--set-up-before-creating-invitations)
(the current manual setup). This change makes host setup part of enabling
sharing. It preserves pinned TLS, scoped library authority and the saved switch.

## 1. Failure and outcome

A native Mac used the default Linux-only `tailscale0` binding. The source
Docker host had neither private TCP Serve nor a published sharing listener.
Its saved endpoint also carried a different node's certificate pin. All three
failures appeared as `sharing peer unavailable`.

Enabling sharing must discover the host's Tailscale identity, select valid
outbound binding, configure persistent private TCP forwarding, and publish
this node's observed certificate with its own addresses. An unavailable host
helper, sign-in, permission or conflicting port gets an actionable status;
it never rejects or reverses the saved enable choice.

## 2. Host ownership and deployment

Native hosts run bounded Tailscale operations as the Cinema process owner.
Docker uses an independently installed host worker. The container exchanges
only a fixed desired-state request and bounded observations through its data
directory; it receives neither the Docker socket nor the Tailscale socket.
Host worker registration chooses its data directory and owner locally, never
from HTTP input. The deployment installer prepares the explicit private bridge
and read-only node configuration before enabling that worker.

The only managed route is raw private TCP port 32443 to host loopback 32444.
The worker preserves other Serve rules, refuses conflicting/public rules, and
removes a route on disable only when its ownership record and current target
both match. Installation/sign-in and native host authorization remain explicit
user actions where required. No public proxy, Funnel or router changes belong
to this flow.

## 3. Implementation and acceptance

| Surface | Implementation | Acceptance |
|---|---|---|
| Native and daemon coordination | Discover native Tailscale identity; reconcile enable/disable; accept fresh process-bound Docker observations; publish local endpoint; select native egress. | Focused parser, ownership, disabled, stale-generation and manifest-preservation regressions; pinned Rust check, Clippy and format. |
| Docker host | Bounded host worker and installer, explicit loopback publication and bridge configuration. | Fake Tailscale and filesystem lifecycle tests; exact-container/deployment checks; real host observation kept distinct from synthetic tests. |
| Web | Show setup progress and actionable host failures; advanced address editing remains available. | Focused sharing-management and Settings regressions, including saved enablement despite failed setup. |
| Integration | Preserve scoped pairing and certificate verification. | Native receiver to private Docker source identity/import/pairing; playback acceptance recorded separately. |

### Install the Docker host component once

The managed Docker path supports a local rootful Linux systemd host and a
numeric nonroot Cinema container without added capabilities. Rootless,
user-namespace-remapped and privileged containers retain the manual hosting
path because host-root file ownership cannot authenticate the same boundary.
The host needs Python 3.11 or later, Docker Compose 2.33.1 or later and
Tailscale. Run from the selected Compose checkout after upgrading Cinema:

```bash
python3 scripts/install-sharing-host  # Authorize one host installation locally.
```

The installer first asks the current image for `sharing host-capabilities`.
Older images are refused before configuration changes. It reproduces the
running service's full Compose hash, keeps the immutable image, backs up
configuration outside build caches, and prepares a free private bridge with
loopback-only publication. Explicit `--config` startup and unreproducible
environment selections are refused before replacement. Existing storage,
startup identity, environment, hardware and ports are checked after replacement.

The installed per-registration systemd worker follows the saved sharing
switch. A rule for the exact container source `/32` rejects destinations in
`100.64.0.0/10` on interfaces other than `tailscale0`; the worker verifies
that Docker's user chain and that rule precede terminal accept/return rules.
The generated node config sets `sharing.require_host_egress_guard = true`.
That transport has no socket source until its host guard is established.
Incoming forwarding and outgoing guard observations remain separate, so an
incoming port conflict does not prevent already guarded outgoing connections.
The private egress guard remains while sharing is disabled; only an owned,
unchanged Serve forward is removed. Other Serve rules remain untouched.

**How to read status:** `ready` means local host configuration was observed;
it does not certify another home, a recipient's policy or playback. Missing
Tailscale, sign-in, permission, conflicting port and missing host-component
states say which host action is needed. The saved switch remains authoritative.

### The host handoff carries facts rather than commands

The daemon writes `sharing-host/request.json` below its private data directory.
Schema 1 carries `generation`, `node_id`, `enabled`, `checked_at_ms`,
`listener_profile`, `listener_address` and `container_egress`. The process
generation changes on each start. The worker binds the data-directory inode,
container project/service, source UID, static address and loopback publication
to a locally installed registration; daemon input selects no executable or path.

The root-owned `report.json` repeats schema/generation/node/enablement and
carries `checked_at_ms`, `state`, `address` and `egress_guard`. The daemon
rejects self-owned replacements, stale reports older than 30 seconds, reports
over five seconds in the future, and mismatched generations or nodes. Managed
egress also refuses a root-running daemon, which cannot distinguish its own
files from host-root reports. Native ownership markers use a separate reader.

Native Mac sockets bind both the discovered Tailnet source address and its
owning Darwin interface. Interface loss has no ordinary-interface fallback.
Only managed native installations invoke host setup; developer/test daemons
do not acquire host changes merely by starting.

## 4. Execution ledger

- 2026-10-28: Native receiver binding and Docker source ingress repaired on
  the owner's installations without changing either runtime image. Verified
  the source certificate through SSH, corrected its manifest using the admin
  API, replaced the invitation with the same library scope, and verified that
  the receiver import became active after the owner's pairing approval.
- Automated setup is implemented for managed native hosts with Tailscale
  permission and the explicitly installed supported Docker host component.
  Native sign-in/OS authorization, Docker component installation and broader
  two-home/device/playback acceptance remain explicit boundaries.
- Local focused evidence: seven daemon host-setup regressions, the Darwin
  source-interface socket regression, 20 Python host/installer cases, all
  Sharing/Settings web checks, and 20 documentation/index/name cases pass.
  Pinned compiler checks and merge-lane results are recorded on the pull request;
  these local unit results remain source-bound evidence.
- The adversarial review findings were corrected: reproduce the complete
  Compose service before replacement, refuse ambiguous configuration, preserve
  four selected ingress hosts, revalidate before mutations, authenticate host
  reports by root ownership with an unprivileged container, verify firewall
  execution order, and restart an isolated per-registration worker on update.
- Passing focused unit results are retained across a main rebase when their
  covered source is unchanged, following the owner's 2026-10-28 instruction.


## External recipients — one Cinema invitation

Source implementation added 2026-10-28; real cross-tailnet acceptance remains
open until exercised with two independently authorized accounts. Host setup
and same-tailnet catalogue browsing do not certify this flow.

Connect the publishing Cinema node's Tailscale account once in Settings →
Sharing → Private network. [Tailscale’s API](https://tailscale.com/api) requires a
user-owned API key; OAuth client credentials cannot create these shares. The
node verifies it against the fixed HTTPS control API, seals it under its local
credential key, and returns configuration facts only. Keys expire according
to the Tailscale account's chosen deadline; rotating a key verifies existing
provider IDs before replacing the encrypted cleanup credentials.

The serving nodes discover and publish their own Tailscale FQDN, private IPv4,
optional IPv6, listener port and observed Cinema certificate pin. A user need
not copy those values. Manual endpoint editing remains advanced recovery;
an open invitation editor refreshes discovery without replacing library choices.
The provider's device inventory must agree with each selected serving endpoint;
Cinema never shares an arbitrary device supplied by an invitation request.

Creating an invitation defaults to one `cinema-connect-v1` envelope containing
the unchanged pinned Cinema invitation and single-use Tailscale consent URLs.
Exit-node access and reusable shares are disabled. Cinema sends no provider
email. Device sharing permits network access according to the tailnet policy;
library authority still requires the existing explicit pairing approval and
viewer assignments. Restrict the serving device's network policy accordingly.

A recipient pastes that one invitation into Cinema. Import presents Tailscale's
consent page inside the workflow, using the account of the **importing Cinema
server**, which can differ from the account on a phone. A bounded read-only
probe observes pinned reachability; success automatically continues import once.
Browser return alone grants nothing. Route/account/draft changes retire the
attempt; an uncertain import POST is never automatically replayed. Tailscale
installation/sign-in and the recipient's account consent remain necessary.
Personal device shares cannot be consumed by externally tagged recipient nodes;
Cinema does not silently change either tailnet's policy or device ownership.

Per-invitation cleanup journals contain exact provider IDs and sealed issuing
credentials. Known provider invitations are removed after Cinema cancellation,
expiry or final grant revocation. Active grants retain network connectivity
beyond bootstrap expiry. Cleanup retries independently; Cinema revocation is
immediate even if the control API is unavailable. Account replacement cannot
turn another account's 404 into deletion evidence. Creation is owned beyond
HTTP cancellation, bounded to 22 seconds, and incomplete jobs are retired on
restart. If a provider creation times out without returning its ID, Cinema
retains an unconfirmed-outcome observation and never guesses ownership or
replays that provider mutation. Its unused provider URL was never published;
review such observations in Private network rather than claiming cleanup proof.

The legacy `cinema-share-v1` format remains readable. Callers explicitly using
`include_network: false` retain the already-connected private-network path.
Focused tests cover the envelope's identity and URL boundaries, provider-device
matching, consent continuation/account retirement, and retained connectivity
across invitation and grant state changes. They do not certify real provider
permissions, policy, tagged-device compatibility or external playback.
