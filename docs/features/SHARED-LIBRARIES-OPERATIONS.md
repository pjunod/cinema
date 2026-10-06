# Shared libraries — operating the effort build

**Status:** implemented paths with qualification open · **Updated:** 2026-10-06.
This guide describes the effort build, not a released or fleet-qualified
feature. See [current status](SHARED-LIBRARIES-STATUS.md) for outstanding
cluster, topology and device work, and the
[implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md) for acceptance.

## Prepare the private listener

The default hosting profile accepts a loopback sharing listener. On a Linux
bare host, the node-local configuration is:

```toml
[sharing]
listener_profile = "host_loopback"
bind = "127.0.0.1:32444"

[sharing.egress]
mode = "interface"
name = "tailscale0"
```

These are implemented configuration keys. The peer port and certificate
directory are not fields in this configuration block. Provision the TLS
identity under the daemon's actual data directory, as the daemon's service
user, before enabling sharing:

```bash
plurxd sharing init-tls --key-directory /var/lib/plurx/sharing-tls
```

Replace `/var/lib/plurx` with the installation's data directory. Initialization
refuses existing identity files; do not delete a key to repair an endpoint or
certificate problem. Approved peers pin that key's identity.

The host's Tailscale service supplies raw TCP forwarding to this listener:

```bash
tailscale serve --bg --tcp=32443 tcp://127.0.0.1:32444
tailscale serve status --json
```

Do not expose the ordinary application port through this sharing service.
The contract requires private Tailscale reachability and approved pins. The
app does not install Tailscale or change tailnet policy. The two-home,
shared-machine and relay-path acceptance receipts remain outstanding.

**Linux Docker bridge hosting:** use the explicit `docker_bridge` profile and
[opt-in deployment recipe](../../deploy/README.md#sharing-uses-an-explicit-linux-bridge-profile).
It binds the process inside its namespace, publishes the sharing port only on
host loopback, and binds outbound sockets to the chosen static container
address. Compose 2.33.1 or later selects that bridge as the default gateway
with `gw_priority`; the normal deployment preflight requires its priority to
exceed every other attached network and checks the readable rendered recipe.
The ordinary Compose deployment and saved sharing choice are unchanged.
Configuration checks and compilation have passed; running-host isolation,
private egress and real Tailscale playback still require qualification.

## Pair and assign viewers

1. In Settings → Developer, save the explicit sharing choice. Readiness is
   advisory and does not override the saved setting. Settings → Sharing shows
   the current listener and peer state.
2. The exporting administrator selects local movie/show libraries and creates
   an invitation. The recipient administrator imports it, both compare the
   pairing code, and the exporter approves the recipient.
3. The recipient assigns individual local viewers. An import starts with no
   assigned audience. Reciprocal sharing uses a separate invitation/grant.
4. Viewers browse Source-labelled libraries on the recipient. Playback,
   history, Continue Watching and next episode keep that Source identity;
   matching Local title or file numbers do not merge their histories.

The recipient serves its own clients; TVs and phones do not need access to
the Source's tailnet. Pairing and assignment administration are web-first.

## Understand playback and retirement

Source-owned copy, encode, embedded subtitle burn, supported HDR and direct
routes are implemented in the effort. Dolby Vision conversion/re-encoding,
downloaded subtitle burns, native subtitles beside HDR and Shared progressive
`stream.mp4` remain typed unsupported shapes. The Source selects delivery
from the player's actual capabilities. A compiler pass does not establish
subtitle fidelity, HDR rendering or physical-device playback.

The recipient retains a cleanup obligation until Source End is confirmed.
`receiver_recovery` in `GET /api/v1/sharing/status` and the Sharing status UI
identify stranded obligations. Preserve those rows and their wrapping keys;
deleting a row or restarting a process is not proof that its producer stopped.
A lost reply may be retried with the same identity. Unreachable peers and
missing keys require restoring authenticated reachability or reviewing the
recorded failure, not fabricating a completed End.

When rotating endpoints, retain an approved reachable endpoint while the
authenticated manifest propagates. Address hints cannot introduce a new pin.
If every trusted endpoint/key is lost, use explicit administrator confirmation
and the pairing recovery flow. Never paste a peer credential into a client URL
or an operator log.

## Upgrade, restore and qualification

Follow the [coordinated upgrade procedure](SHARED-LIBRARIES-COORDINATED-UPGRADE.md)
and its recorded schema boundaries. Existing historical-binary receipts cover
specific Store and empty-workload cases; active-session upgrade and full
rollback qualification remain open. A mixed cluster cannot admit sharing
principals when participating readers/writers lack the required capability.

Use the supported restore workflow. Restore/clone disables sharing and
requires re-pairing or credential replacement before remote access resumes;
a clone also needs new server/catalogue and TLS identities. Preserve the
credential wrapping key with backups. Copying a live data directory and its
keys outside that workflow can restore old authority.

The feature stays in Developer until the network, cluster and device matrix
is qualified. The saved choice remains available throughout. Main promotion
is still held for Paul; this session has run compiler/lint checks and has
deferred test execution until promotion under his explicit instruction.
