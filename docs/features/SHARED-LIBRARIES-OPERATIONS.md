# Shared libraries — operating the effort build

**Status:** implemented paths with qualification open · **Updated:** 2026-10-29.
This guide describes the effort build, not a released or fleet-qualified
feature. See [current status](SHARED-LIBRARIES-STATUS.md) for outstanding
cluster, topology and device work, and the
[implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md) for acceptance.

## Prepare the private listener

Managed native installations can prepare the private connection when sharing
is enabled, including verified legacy macOS server-only LaunchAgents. A
manually started native daemon observes the private connection without changing
host forwarding and reports native setup guidance when that forward is absent.
Supported Linux Compose hosts use a separately installed host
component; its installer prepares the bridge and its worker follows the switch.
See [automatic host setup](SHARING-HOST-SETUP.md) for installation, observations
and source qualification. The configuration below describes the manual path.

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
2. The exporting administrator chooses **Share my libraries**, selects local
   movie/show libraries and sends the invitation. The recipient chooses
   **Accept an invitation**, imports it and completes any linked Tailscale
   authorization using the account used by the recipient's Cinema server.
   Keep that form open until it confirms the import was saved.
3. The recipient sends the displayed pairing code to the exporter. Under
   **People I share with**, the exporter compares the code and approves
   pairing. Connection lists refresh automatically while the main page is
   open. Revoked recipients are hidden unless **Show revoked connections**
   is checked; their underlying records remain intact.
4. Under **Libraries shared with me**, the recipient chooses **Assign
   viewers** and selects individual local users, including their own user
   if they want to browse the libraries. An import starts with no
   assigned audience. Reciprocal sharing uses a separate invitation/grant.
5. Viewers browse Source-labelled libraries on the recipient. Playback,
   history, Continue Watching and next episode keep that Source identity;
   matching Local title or file numbers do not merge their histories.

The recipient serves its own clients; TVs and phones do not need access to
the Source's tailnet. Pairing and assignment administration are web-first.

### When an import does not appear

Seeing a shared node in Tailscale does not establish a Cinema connection.
The recipient first verifies the invitation's pinned private listener; no
Cinema import exists before that succeeds. The import form shows the failed
connection reason and pauses after ten automatic checks. Resolve that reason,
then choose **Try connection again**. Leaving the form cancels the unsaved
attempt; invitation secrets are not persisted in browser storage.

Open **Connection setup and advanced options** on both servers. A listening
TLS socket and configured addresses do not prove private forwarding. On
Docker, `host_helper_required` means Cinema has no fresh authenticated host
component report; use the [host setup procedure](SHARING-HOST-SETUP.md).
An identity mismatch requires checking the source addresses and a fresh
invitation, rather than bypassing its identity verification.

Daemon logs distinguish `phase="probe"` (private connection verification),
`phase="verify"` (invitation import verification), and `phase="claim"`
(a saved import waiting to claim its source grant). Failures include a fixed
code and correlation UUIDs; they do not include invitation secrets, Tailscale
consent links or peer credentials. A probe failure never substitutes for an
actual import or pairing approval.

### When shared playback cannot prepare

The receiver distinguishes upstream, decision decoding and resource projection
failures in daemon warnings with `phase="upstream"`, `phase="decode"` and
`phase="project"`; Source planning failures use `phase="source_plan"`.
Warnings include fixed codes and import/file identifiers, without peer
credentials, file paths or raw decision payloads. The generic viewer error
alone does not identify a codec or HDR problem.

The browser also logs fixed client-validation phases (for example
`details_identity` or `decision_identity`) and the HTTP status of refused
requests. These warnings contain no peer payloads, locators or credentials;
they distinguish a client contract refusal from a Source planning failure.

The peer decoder accepts the current named 5.1/7.1 and limited-default audio
downmix plans while refusing unknown fields. Process-local measured-output
advisories are omitted from the private Source response; they cannot become
peer authority.

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

In a cluster, the node accepting the client connection forwards to the
retained playback owner. Source placement probes the exact authorized file
before claiming a new invocation; a lost response retains that selected
worker and process identity. It does not select a second worker for the same
uncertain Start. Receiver failover likewise cannot adopt playback from a
database row when the physical owner is absent.

End may first return a closing/unresolved response when it arrives over a
connection that still owns playback bytes. The owner seals new registrations,
closes the actual transports and records authenticated closure acknowledgments
before confirming End. Retry the same End identity on a fresh connection.
A completed HTTP response, changed owner row or expired deadline alone does
not prove closure. Cleanup retains one absolute deadline; retrying does not
restart that budget.

When rotating endpoints, retain an approved reachable endpoint while the
authenticated manifest propagates. Cleanup first tries its retained endpoint,
then current approved replacements from the same import lineage. It retries
the exact End identity after a dial failure, refused End or stalled exchange,
reserving time for alternatives within one 315-second budget. A re-paired
import cannot supply replacement pins for an older obligation. Deadline expiry
leaves cleanup unresolved. Address hints cannot introduce a new pin.
If every trusted endpoint/key is lost, use explicit administrator confirmation
and the pairing recovery flow. Never paste a peer credential into a client URL
or an operator log.

Rewrapping a retained receiver obligation preserves its exact Source identity.
The live actor authenticates the current encrypted capsule against its held
Source before using the replacement ciphertext for cleanup. Preserve wrapping
keys until existing obligations have been rewrapped; changing a credential or
Source tuple is not an equivalent rewrap.

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

## Qualification handoff

When Paul releases the main-promotion hold, use this prompt for the remaining
device and topology work:

> Continue plurx shared-library qualification from the current status page and
> implementation contract. Use your own clone and the exact promotion candidate.
> First obtain the one adversarial review, address its findings, and run the
> required fast-lane checks once, rerunning only failures. Preserve receipts
> against the code they cover. Qualify isolated instances on lab4
> (10.42.4.8) and lab6 (10.42.4.14), with separate ports and data directories;
> do not replace their fleet services. Establish approved Tailscale access and
> a real second-NAT/relay topology before claiming those cells. Use permitted
> Apple devices, excluding Paul's iPhone 18 Pro and Apple TV, and the Android
> Google TV Streamer available through lab3. Exercise native direct play,
> prepared handoff and refusal recovery, subtitle burn/HDR, revocation, cluster
> loss, remote-only file placement and active Shared upgrade/restore. A Local
> HLS harness or same-host two-node fixture cannot stand in for those receipts.
> Record unsupported or unavailable cells honestly. Promote only under the
> required main gate and qualification receipt; use the repository's Ansible
> and versioning workflow for any subsequent authorized deployment. Remove
> only the qualification resources you created.
