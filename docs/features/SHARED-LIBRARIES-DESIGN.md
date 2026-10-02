# Shared libraries — the Cinema-to-Cinema scope decision

**Status:** scope and Tailscale transport accepted; build contract ready to
build after Opus S0 corrections; topology qualification pending · **Written:** 2026-10-02 · **Decider:** Paul.

This is the short decision record. The companion
[implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md) contains the
protocol, storage, code seams, build sequence and acceptance matrix for Opus
to review and Sol to build. That contract owns implementation detail; this
record does not maintain a second milestone list or protocol specification.
Nothing described here has been implemented or deployed by this documentation
change.

## 1. What Paul selected

Cinema server A can share selected libraries with independent Cinema server
B. B's users browse and watch the shared media through their existing Cinema
setup. Either server may internally be a cluster, but sharing never joins
the two households' clusters, accounts, databases or administration.

Use Tailscale between server machines. Do not require router port forwarding,
publish Cinema to the internet, use Funnel, or build a Cinema-hosted relay
service. Plex shared-library compatibility comes later. Watch Together was
also requested in the initial discussion and remains a separate follow-on;
shared libraries do not imply synchronized playback.

Paul requested a detailed build document for Opus review and subsequent Sol
implementation, with the documentation committed to the repository.

## 2. The first-release design

```text
A's local media → Cinema A ══ private Tailscale connection ══ Cinema B → TV
                  chosen libraries                          local account
                  source playback                           local progress
```

A retains the media and performs any necessary transcoding. B relays the
stream to its player. TVs and phones need no Tailscale installation when
they already reach their own server. The source upload, private transport,
receiving server and player's connection all affect playback capacity.

There are two independent permissions. Tailscale controls access to the
private sharing service. Cinema's grant controls which libraries the peer
may browse and stream. Being a network peer confers no administrative or
cluster authority. Tailscale machine sharing and Cinema library invitations
are separate setup steps in the first version.

Tailscale Serve forwards raw TCP to a dedicated, pinned-TLS sharing listener
on host loopback. Docker publishes that listener on host loopback and keeps
its ordinary bridge network. The first supported topology still requires
shared-user Serve and actual container-egress qualification. Local
administration stays separate. Tailscale access is user-granular; Cinema's
credential authorizes the logical peer server. VPN loss makes shared libraries
unavailable without changing local Cinema or using a public fallback.

Tailscale handles NAT traversal and can use encrypted relays where a direct
connection is unavailable. Those relays cannot decrypt WireGuard traffic;
relay throughput still needs measurement before promising a video quality.
[Tailscale connection types](https://tailscale.com/docs/reference/connection-types)
and [DERP](https://tailscale.com/docs/reference/derp-servers) describe this
transport behavior.

## 3. Proposed defaults for Opus to review

These are implementation recommendations, not additional decisions attributed
to Paul:

- Start with movies and shows, including anime. Add other media types only
  with their own playback and authorization acceptance cases.
- A grants B access. B's administrator assigns local viewers, initially none.
  A trusts B to enforce those assignments; it does not attest B's user identities.
- Keep each viewer's remote watch state on B. A's household history is untouched.
- Label shared libraries with their source. Do not merge editions by provider
  ID or treat a remote numeric ID as a local item ID.
- Allow browsing and streaming. Exclude remote editing, offline packages,
  download UI, onward sharing and cross-server search aggregation initially.
  Authorized media bytes cannot be made uncopyable by application policy.
- Pair through an expiring invitation, scoped credential and explicit owner
  confirmation. Revocation stops new delivery within the build contract's
  measured bound, including already-open streaming responses.
- Support web and the existing Apple/Android clients; manage pairing and
  assignments through the web administration interface initially.

For separate Tailscale accounts, external machine sharing currently has
restrictions on tagged service nodes. The build contract specifies supported
user-owned recipient nodes and a separately qualified restricted common-tailnet
setup. Do not imply that joining two unrestricted household networks is
necessary. [Tailscale machine sharing](https://tailscale.com/docs/features/sharing)

## 4. Delivery and review

Read the [build contract](SHARED-LIBRARIES-IMPLEMENTATION.md) for the S0–S8
sequence, explicit principal ownership prerequisite, test commands and Opus
review brief and [Opus S0 findings](SHARED-LIBRARIES-REVIEW.md). The effort uses the repository's normal compiler, review and
promotion workflow. The unfinished feature has a Developer switch with
advisory readiness and graduates to Settings → Sharing after qualification.

Plex can later adapt the same catalogue, authorization and playback boundary.
Watch Together can later coordinate independently authorized viewer sessions.
Neither follow-on is part of the current implementation contract.
