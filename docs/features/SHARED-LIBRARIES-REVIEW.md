# Shared libraries: Opus S0 review of the Tailscale build contract

**Status:** historical S0 findings; accepted with corrections in the
[re-review](SHARED-LIBRARIES-RE-REVIEW.md).
The current [build contract](SHARED-LIBRARIES-IMPLEMENTATION.md#16-opus-s0-dispositions--corrections-are-not-runtime-evidence)
owns dispositions. The review below is historical, not the status of the
revised contract. The personal home-path example is normalized to `~`.

**Reviewed:** `docs/features/SHARED-LIBRARIES-IMPLEMENTATION.md` as committed in
`4e81bc38c` (with `SHARED-LIBRARIES-DESIGN.md`) · **Code checked against:**
Forgejo `main` @ `f1f1390f1` (2026-10-02) · **Date:** 2026-10-02

**Verdict: not ready to build.** There are 4 blockers and 7 major findings.
The trust model is sound. The B-initiated-only protocol is the right call,
because Tailscale quarantines shared machines. Pinned-SPKI identity,
idempotent claims, and the explicit playback principal are also right. Three
areas are wrong: how the listener reaches Tailscale in the deployment plurx
ships; how sharing sessions plug into the cluster session machinery; and how
much of the existing playback wire the relay exposes. If Sol builds the
contract as written, those three force a second protocol partway through the
effort.

All file and line references point at `main` @ `f1f1390f1`. Tailscale facts
are quoted from tailscale.com/kb (sharing, serve) as of today.

---

## Blockers

### SL-01 · §2.2, §2.1 · The listener contract can't be met by the shipped container deployment

**Evidence.** In `deploy/docker-compose.yml:99-105`, `plurxd` deliberately
runs on ordinary bridge networking with published ports. Only the
`plurx-discovery` companion uses `network_mode: host`. Inside that container
there is no `tailscale0` and no 100.x address, so "bind only the literal
address assigned to the configured Tailscale interface" can never pass.

**Failing sequence.** An operator publishes `100.x.y.z:32443:32443` instead.
After a reboot, Docker starts before `tailscaled` has its address, the publish
fails with "cannot assign requested address", and the whole `plurxd`
container fails to start. Local Cinema is down, which violates §2.2's "Local
Cinema stays up". Docker-published ports also bypass the host firewall chains
that §2.3 relies on as the ingress enforcement.

**Correction.** Make the first supported topology Tailscale's own TCP
forwarder:

`tailscale serve --bg --tcp=32443 tcp://127.0.0.1:<sharing-port>`

plurxd binds the sharing listener on host loopback. In a container, it
publishes `127.0.0.1:<port>`, which doesn't depend on `tailscaled`.

- Exposure is tailnet-only, governed by A's policy.
- `--bg` persists across restarts.
- Tailscale going down closes the path with nothing for plurxd to reconcile.
- Readiness becomes a read-only check of `tailscale serve status --json`
  rather than interface polling.

The client-IP loss doesn't matter, because the credential is the authority
(see SL-09). The optional PROXY protocol can restore it for diagnostics.

Keep literal-IP binding as a separately qualified bare-metal mode. Two things
need qualification:

- That a shared-in user can reach a served TCP port. The serve docs don't
  state it.
- The deployed Docker version's handling of loopback publishes. Older
  releases let same-L2 hosts reach `127.0.0.1` publishes.

### SL-02 · §3.2, §2.2 · The invitation's endpoint IP can be wrong in B's tailnet, with no recovery

**Evidence.** Tailscale's sharing doc says: "The IPv4 address of a shared
machine in the recipient's tailnet will typically be the same … However, if
that address is unavailable in the recipient's tailnet … a different address
will be assigned." The same doc says shared machines "can only be reached by
using their fully qualified domain name" (`<host>.<tailnet>.ts.net`).

**Failing sequence.** A's node has 100.101.102.103, but B's tailnet already
uses that address. B is given a different address for A. B dials the
invitation's literal IP and reaches nothing, or reaches its own unrelated
device. The pin check correctly fails, and the contract leaves nothing to
try: hostnames are forbidden and §5.2 has no endpoint edit.

**Correction.**

- **Invitation.** Endpoints carry `{ipv4, ipv6?, ts_fqdn, port, spki}` as
  hints.
- **Pairing.** B may resolve `*.ts.net` only through Tailscale's resolver,
  and accepts only an answer inside the tailnet range. B's admin may override
  the address at import.
- **Identity.** The pin remains the only identity check, so an override is
  safe.
- **API.** Add `PUT /api/v1/sharing/imports/{id}/endpoints`, admin-only, with
  generation CAS.
- **Simplification.** Pinning also makes the "prove no WAN fallback"
  requirement in §2.2 cheaper. A packet that leaks to an ISP's 100.64/10
  can't complete a pinned handshake, and the credential is sent only after
  verification. Keep `SO_BINDTODEVICE` where available, and replace the
  route-check-then-dial (a TOCTOU) with it.

### SL-03 · §7.1, S3 · The parallel `sharing_media_*` family can't reach the cluster session machinery

**Evidence.** `MediaSessionStore` (`crates/plurx-core/src/store/mod.rs:4590`)
has 36 methods. Only 16 take `user_id`. The other 20 are keyed by session,
incarnation or owner, and they carry everything clustered playback depends on:

- activation and preparation rejoin;
- handoff arm/complete and terminal projection;
- `media_session_route` / `_by_incarnation`;
- terminal acks;
- `renew_media_sessions`, `expired_media_sessions`,
  `claim_media_session_takeover`;
- `end_media_session[_if_owner]`, `maintain_media_sessions`,
  `owned_media_sessions`.

Non-owner requests already reach the producer through
`http/hls/relay.rs::relay_if_remote`, which resolves through
`state.media_sessions` against the `media_sessions` table.

**Failing sequence.** Clustered A starts a sharing session on A1, in
`sharing_media_sessions`. B's next segment request uses another pinned
endpoint and lands on A2. Route resolution reads `media_sessions`, finds it
`Absent`, and returns 404. Sol now has two choices. One is to duplicate route
resolution, relay, owner transition, P7 takeover, drain and producer recovery
for a second table family, which is exactly the fork §7.1 prohibits. The
other is to invent a cross-family lookup by session ID, which undoes the
isolation the family was meant to buy.

**Correction.** Put the principal in the existing family:

- Add `principal_kind TEXT NOT NULL DEFAULT 'local'` plus nullable
  `share_grant_id` / `share_viewer_key`.
- Add a CHECK that exactly one owner form is present. `user_id` becomes
  nullable through a table rebuild (precedent: `MEDIA_SESSIONS_V10_SCHEMA`,
  `hiqlite_sessions.rs:218`).
- Every one of the 16 user-keyed methods and every local lookup adds
  `principal_kind = 'local'`. The S3 census becomes "every user-keyed query",
  which is a finite list.
- The 20 session-keyed methods, the relay, and takeover work unchanged.
- The peer listener calls the same relay after a principal check, and the
  ordinary router refuses any non-local principal before relaying.

If the review prefers to keep a parallel family anyway, the contract must
enumerate those 20 operations and specify a single session-ID namespace
before S3 can start.

### SL-04 · §7.2, §7.3, §5.4, §8 · The relay wire is thinner than the playback the clients run

**Evidence (start response).** The existing `StartResponse`
(`http/hls/session_guard.rs`) carries:

- `ladder`, `quality_candidates`, `quality_candidate_id`;
- `display_aware_auto_protocol`, `prior_kbps`;
- `vod`, `height`, `encoder`, `start_seconds`, and the delivered dynamic
  range.

The quality menu, the Auto controller and the stall watchdog all consume
these. The §7.2 sketch drops all of them.

**Evidence (resources).** Players also fetch resources keyed by file, not by
session, so §5.4's "enumerated session-owned bytes" can't express them:

- sidecar VTT `/files/{id}/subs/{subtitle}`;
- PGS overlay `/files/{id}/subs/{index}/overlay.json` and `…/objects/{object}`;
- chapter thumbnails `/files/{id}/chapters/{index}/thumb`;
- `/files/{id}/decision`, `stream.mp4`, `direct`.

**Failing sequence.** A shared direct-play title with a PGS track: the client
asks for the overlay manifest, and no peer route exists. Separately, Auto
starts with no ladder, and the menu is empty.

**Correction: a wire-transparent relay.**

- A's peer start returns the existing `StartResponse`/decision types.
- B returns the same type, with `session_id` = B's relay ID and B-relative
  URLs.
- B mounts the relay under a prefix whose sub-grammar is the existing
  `/hls/{session}/…` grammar, including `control`.

The engine's playlists already use relative URIs: `init.mp4`, `seg…m4s`, and
`subs/{index}/index.m3u8` (`http/hls/playlist_text.rs:187`,
`produce.rs:456`). Path-prefix forwarding over a closed grammar therefore
replaces the HLS-rewriting parser and the 4,096-entry `sharing_resource_map`.
Absolute URIs are still rejected as `sharing_resource_unsupported`.

Add grant-scoped peer routes for the file-keyed side resources listed above.

Side effects:

- §10's "B ingress change must not remap URLs" becomes trivially true.
- S7 shrinks from threading a `PlaybackContext` through a
  10,871-line `PlayerController.swift` and 3,090-line `PlayerScreen.kt` to:
  shared browse/detail screens, a different start URL, and a progress adapter.

---

## Major

### SL-05 · §3.3 vs §9 · One generation counter kills all playback on benign changes

Credential rotation and scope expansion both increment `generation`, and §9
cancels every session on any generation change.

**Failing sequence.** A adds a third library to a grant. Every B viewer's
film stops mid-play. A rotation, which §3.3 makes recoverable, does the same.

**Correction.**

- Split the counter into `scope_generation` and `credential_generation`.
  - `scope_generation` advances on narrowing, disable or revoke.
  - `credential_generation` advances on rotation and never touches sessions.
- A session stays valid while the grant is active and its item's library is
  still in scope.
- Additions invalidate only catalogue cursors.

### SL-06 · §4.1, §7.3, §10 · B's relay should be a local-user media session, not a new table

On B, the viewer is a local user. A relay modelled as an ordinary
`media_sessions` row, with a "remote source" producer kind, inherits the
existing machinery for free:

- owner lease and P7 takeover;
- non-owner relay;
- admin activity and admin stop;
- login-token revocation;
- cleanup.

**Failing sequence (as written).** Clustered B: the relay lives in
`sharing_relay_sessions` on B1, and the TV's next request hits B2 through the
keepalived VIP. Nothing in the contract forwards it, so B2 must dial A
itself. B2 needs its own Tailscale node, a share from A, and the sealed
upstream material. Readiness then becomes per-node, and none of that is
specified.

**Correction.** Use the existing family on B (`PlaybackPrincipal` is needed
only on A). Keep only a thin `sharing_relay_upstream` side table holding the
A session handle and sealed upstream capability. Relay placement is
restricted to nodes whose readiness includes Tailscale, using the existing
owner assignment.

### SL-07 · §6 · Durable 100k-ID catalogue snapshots are Raft write amplification for a browse

Every browse open materialises up to 100,000 IDs into a replicated table,
with up to 64 such snapshots per server, so that A's ingress nodes agree.
Local browse is offset-paged today (`http/browse.rs:211`).

**Correction.** Use opaque keyset cursors that need no durable state:

- Contents: `(sort key, item id, filter digest, scope_generation)`, MAC'd
  with a node-shared key.
- Every node validates them identically.
- They never duplicate or skip an item that existed when paging began. New
  scans may appear, which is acceptable.

This removes `sharing_catalogue_snapshots`, the 100k cap and the 10-second
materialisation deadline.

### SL-08 · §3.1 · The key census contradicts "missing key ⇒ import unavailable"

Today, plurx refuses to start when sealed rows outlive `credentials.key`
(OPERATIONS.md:3073). `SealedRowCensus::observe_row(access, refresh)`
(`secrets.rs:533`) is Trakt-shaped. Once sharing envelopes join the census as
§3.1 requires, a missing key stops the whole server, not one import.

**Pick one:**

- **(a) Same as Trakt (recommended).** Refusal is the safe, consistent
  behaviour, and the key already reaches joining nodes via the admission
  token. Fix the text to match.
- **(b) Per-purpose census.** Sharing envelopes degrade the import instead.
  This needs its own tests.

Either way, generalise `observe_row` to a purpose-tagged envelope.

### SL-09 · §2.1, §2.3, §13.3 · Tailscale reach is per user; the acceptance row can't pass

Shares are accepted by users ("only users can accept machine shares"), tags
are stripped on the recipient side, and A's policy governs shared access via
`autogroup:shared` or the user. A can't let B's server in while keeping B's
admin's laptop and phone out.

**Failing sequence.** The §13.3 row "unrelated node denied" fails whenever the
unrelated node is another device of the recipient user.

**Correction.**

- **Authority.** State that the grant credential is the per-server authority
  and Tailscale is user-granular.
- **Acceptance rows.** Rewrite them as:
  - non-recipient users denied;
  - SSH, ordinary HTTP and cluster ports denied to `autogroup:shared`.
- **Key expiry.** B's server must be a user-owned node, so node-key expiry
  applies to it. The setup guide must disable key expiry on both servers, and
  readiness should surface expiry where it can be read. Otherwise sharing dies
  silently at the expiry date.

### SL-10 · §2.2 · TLS provisioning, renewal and adding a node are unspecified

As written:

- The operator supplies `tls.crt` and `tls.key`, with no generation step.
- "Verify certificate validity" on a self-signed pin means sharing breaks on
  `notAfter`.
- A new or replaced A node requires a full re-pair, and there is no route for
  "confirm new endpoint".

**Correction.**

- **Generate the key.** plurxd creates the node key and self-signed cert on
  first enable: one command, owner-only, outside snapshots. If a new crate is
  needed, route it through the ring-only dependency-ban checks.
- **Pin semantics.** Treat the pin as an SSH-style host key and ignore
  validity dates, or else auto-renew with the same key.
- **Endpoint updates.** Add `GET /sharing/v1/endpoints`, served over an
  already-pinned and credentialed connection. B accepts new endpoint pins only
  through that authenticated channel. Adding or rotating an A node then needs
  no re-pair.

### SL-11 · §6, §5.4 · Continue Watching needs a batch metadata route

B's Continue Watching list is local, but each row needs A's current title,
artwork and authorisation. The only peer route is `GET /items/{id}`, and §6
itself forbids serial fan-out.

**Correction.** Add `POST /sharing/v1/items:batch` (≤200 IDs). It returns each
item's metadata plus whether it is authorised now, and uses the 30-second
metadata TTL.

---

## Medium and low

- **SL-12 (medium) · §7.2 · Throughput priors.** #669's node-local
  sustained-throughput prior (`prior_kbps`) on A would seed a remote start
  from LAN measurements. The first rung would then be too high for an upload-
  or DERP-limited path.
  - Key priors by grant, or disable them for sharing starts.
  - Replace "account for A→B and B→player separately" with that rule. A
    pass-through relay already makes the client's ABR measure end to end.
  - Add a server-wide cap on remote sessions. With 32 grants × 4 sessions,
    direct plays are bounded by nothing on A's upload.
- **SL-13 (medium) · §4.1, §7.3 · Reuse `file_grants`.** `store/file_grants.rs`
  is the in-tree pattern for a hashed capability bound to the login token,
  with logout invalidation via `source_token_hash`/`source_active`. Model
  `sharing_delivery_grants` on it, or reuse it. Also decide whether the B
  relay ID is itself the capability, as `/hls/{session}` is today for local
  sessions. Consistency argues yes.
- **SL-14 (medium) · §4.1 · DDL gaps.**
  - Whole-set assignment CAS needs a per-import assignment generation, but
    the rows each carry their own `generation`.
  - The meaning of `sharing_imports.generation` vs `remote_generation` is
    undefined.
  - `UNIQUE(source_server_id, catalogue_epoch)` is populated from an untrusted
    blob before TLS verifies identity. Define a collision as "already
    imported, re-pair explicitly".
  - `ON DELETE CASCADE` on `sharing_export_libraries` deletes rows without
    the generation bump §5.4 requires. Library deletion must go through a
    store operation.
- **SL-15 (low) · §9 · Derive constants from the engine's.**
  - Source start timeout from `START_DEADLINE` (50 s, `media_sessions.rs:54`)
    plus a transport margin.
  - Media idle from `MEDIA_BODY_NO_PROGRESS_TIMEOUT` (30 s, `:115`).
  - Control timeout from `EXCHANGE_DEADLINE` (4 s,
    `playback_control.rs:25`).

  Otherwise a later engine change silently inverts the ordering.
- **SL-16 (low) · §2.3 · Revocation acceptance.** "Disabling Tailscale closes
  sharing within 5 s" should observe the connection being refused, not the
  interface disappearing. On Linux, `tailscale down` doesn't necessarily
  remove `tailscale0`. SL-01's serve topology makes the point moot.
- **SL-17 (low, process) · The document itself.**
  - It exists only as local commit `4e81bc38c` on `codex/playback-seek-30s`,
    stacked on an unrelated player fix (`4d7257019`) and the Jellyfin/Emby
    docs. It isn't on Forgejo, and its base (`91917940e`) is 114 commits
    behind `main`.
  - §13.2 names `~/.rustup/...`, which breaks the public-mirror
    naming rule; write it as `~`.
  - Land the two sharing docs in their own docs PR from current `main`.

## What should stay as written

- All traffic B-initiated, including pending polling. This is correct, and
  required by quarantine.
- No tagged recipient. This is correct.
- Pinned SPKI instead of pretending a hostname was verified.
- 256-bit secrets with domain-separated hashes.
- The idempotent claim, with the invitation secret erased after confirmation.
- The pairing code over both IDs and the credential hash.
- 401 from A mapped to 502 on B, so a peer auth failure never signs out B's
  viewer.
- Per-import viewer pseudonyms.
- Fresh identities on clone or restore.
- The Developer switch with advisory readiness, which matches the standing
  Developer-tab rule.
- The S0–S8 shape, once S3 and S5 are rebased on SL-03, SL-04 and SL-06.

## Disposition table

| ID | Sev | Section | Disposition |
|---|---|---|---|
| SL-01 | Blocker | §2.1–2.3 | open: adopt serve topology; qualify shared-user reach |
| SL-02 | Blocker | §3.2, §2.2, §5.2 | open: address hints + FQDN via TS resolver + admin override route |
| SL-03 | Blocker | §7.1, S3 | open: principal discriminator in existing family (or enumerate 20 ops) |
| SL-04 | Blocker | §7.2–7.3, §5.4, §8 | open: wire-transparent relay; file-keyed side routes |
| SL-05 | Major | §3.3, §9 | open: split generations |
| SL-06 | Major | §4.1, §7.3, §10 | open: B relay = local media session + upstream side table |
| SL-07 | Major | §6 | open: stateless keyset cursors |
| SL-08 | Major | §3.1 | open: needs ruling, (a) recommended |
| SL-09 | Major | §2, §13.3 | open: credential is authority; key-expiry guidance |
| SL-10 | Major | §2.2 | open: generated keys, pin-as-host-key, authenticated endpoint updates |
| SL-11 | Major | §5.4, §6 | open: batch item route |
| SL-12–17 | Med/Low | various | open |