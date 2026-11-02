# HTTPS implementation — optional encrypted access to the public API

**Status:** implementation built; physical acceptance open; merge qualification tracked by task and promotion PRs · **Written:** 2026-10-15
**Decider:** Paul · **Baseline:** `9eb7f6738`

[Adversarial review](HTTPS-REVIEW.md) is resolved. This is the behavior contract. [HTTPS-BUILD.md](HTTPS-BUILD.md) owns tasks and
evidence. Re-verify interfaces against current source before implementation.
Read [security](../SECURITY.md) and [operations](../OPERATIONS.md) alongside it.
The independently reviewed workflow amendment in the build ledger integrates
H1–H4 as checkpoints of one end-to-end task PR #1118; the behavior contract,
exact-head effort gate and full final promotion requirements are unchanged.

## 1. Objective and scope

Add native HTTPS with an explicit enable/disable switch, persistent generated
self-signed certificate when none is supplied, and operator-provided PEM
chain/key paths. Existing installations default to HTTPS off.

HTTPS is an additional listener on port 32443, on the interface selected by
`server.bind`. Existing HTTP, Jellyfin standard HTTP, cluster and sharing
transports stay as configured. The UI explicitly says “HTTP remains
available” and names both ports: enabling HTTPS does not enforce encryption.
Operators may restrict HTTP with their firewall/proxy. No redirect or HSTS
policy is added because HTTPS can be disabled and self-signed certificates
need explicit trust. No ACME, certificate upload endpoint, client trust
bypass, native pairing redesign or deployment is part of this request.

Settings → Developer owns the control while acceptance is incomplete and
names remaining evidence using `devGraduation`. Once acceptance is complete,
the permanent toggle moves to Settings → System. Advisory readiness never
overrides saved enablement or blocks Save.

## 2. Architecture decisions

1. **Add a listener.** Replacing HTTP would break existing discovery and
   cluster ingress and remove the recovery path. Preserve truthful HTTP
   Bonjour/GDM records; HTTPS is manually selected by URL in this increment.
2. **Extend the existing connection driver.** `HttpAcceptor` and `serve_http`
   in [main.rs](../../crates/plurxd/src/main.rs) own timeouts, upgrades, peer
   address, cancellation and bounded drain. Keep those guarantees over TLS.
3. **Use existing TLS dependencies.** rustls, tokio-rustls and rcgen already
   exist in the graph. Public TLS gets separate identity and normal Web PKI
   semantics; never reuse sharing's key or pin-only verifier.
4. **Keep configuration local to the node.** Certificate paths and private
   bytes never enter replicated Settings. A dedicated endpoint edits the
   receiving node, with a required node identity guard against load balancing.
5. **Apply at restart.** Saved intent and active socket/config are separate.
   Saving never disconnects streams or automatically changes browser origin.
6. **Fail closed for enabled TLS.** Invalid identity or bind failure refuses
   startup, with an actionable sanitized error. Never silently fall back to
   a generated identity when supplied files are broken. Disabled TLS ignores
   unavailable retained certificate files.

Reverse-proxy-only support does not satisfy native HTTPS; swapping the main
port makes rollout fragile; ACME requires extra credentials/reachability.
These alternatives are deliberately excluded.

## 3. Configuration and node administration

Extend `ServerConfig` in [config.rs](../../crates/plurx-core/src/config.rs):

```toml
[server.https]
enabled = false
port = 32443
cert_file = ""
key_file = ""
sans = ["localhost", "127.0.0.1", "::1"]
```

Both paths empty selects generated mode; both nonempty selects supplied mode;
one alone is invalid. Supplied paths must be absolute paths on the daemon
host/container. Port zero is invalid. Reject enabled collisions with main
HTTP, Jellyfin, sharing, Raft and cluster API ports. SANs are DNS names or
IP addresses: at most 32, at most 253 bytes each; reject URL/port/wildcard
strings and unspecified IPs. Always include localhost/loopbacks and the
concrete bind IP in generated mode. Operators enter additional LAN names/IPs.
Never put wildcard bind addresses into the certificate.

Precedence is defaults → TOML → node-local desired file under final `data_dir`
→ explicitly present environment values. Resolve `data_dir` including its
environment override before reading the desired file. Add
`PLURX_HTTPS_ENABLED`, `PLURX_HTTPS_PORT`, `PLURX_HTTPS_CERT_FILE`,
`PLURX_HTTPS_KEY_FILE`, `PLURX_HTTPS_SANS` (comma-separated). Both path envs
empty intentionally select generated mode. Invalid env syntax is an error.

Add admin-only `GET`/`PUT /api/v1/system/https`, with existing `AdminUser`.
It must not forward writes to the cluster leader. GET returns `node_id`,
`desired`, `active`, `revision`, `restart_required`, env `overrides`, and safe
loaded certificate facts (mode, SHA-256 fingerprint, SANs, expiry). Desired
means effective next-boot configuration after environment overrides; active
means this process's loaded configuration. Return `saved` separately as the
pre-environment node-file/base configuration. Both are normalized. Never
return PEM/key bytes in the JSON DTO; file references are admin-only. PUT requires matching `node_id` and `revision`,
replaces the desired object, rejects unknown fields, and has a 16 KiB limit.
On the JSON API, revision is a canonical decimal u64 string, including values
above JavaScript’s exact integer range; the local envelope retains an integer.
Reject noncanonical strings and refuse overflow without mutation.
Stale revision/node mismatch returns 409 without mutation. Persist revisions
across restart; never reset the counter. The UI reports conflicts and asks
for a fresh load, never automatically retries on another node. Serialize writes;
atomically replace a restrictive, bounded node-local file. Failure before atomic publication leaves in-memory state unchanged. Rename
is the publication point: if the subsequent directory sync fails, the save
is committed with durability uncertainty, not rolled back. Adopt the new
in-memory desired/revision and return success with
`durability_confirmed: false` and a visible warning; never invite a blind
retry. A normal successful sync returns `durability_confirmed: true`. PUT changing an env-owned field returns an
explicit conflict; edits to other fields remain valid. Persist only editable
choices,
preserving the previous pre-env values of overridden fields: echoed env
values must not become sticky after that environment variable is removed.
The save worker owns serialization through publication and in-memory adoption,
even when its HTTP waiter disconnects. A canceled waiter cannot leave the
process serving a stale revision after a file was published.
The revision covers persisted configuration; the node guard and current env
validation still apply independently. Report restart needed without claiming a future bind can succeed.

Disabling succeeds without opening obsolete cert/key paths. Enabling checks
configuration and any existing identity read-only. For first enable with no
managed identity, validate shape and storage readiness without provisioning;
Save may succeed and startup alone creates the identity. Use a separate
settings loader/card, naming the node edited. Include enabled, port, supplied chain/key paths, and SAN inputs
in the Developer card and its lazy loader. Keep replicated `/settings`
unchanged. If the replicated settings read fails, retain its error and Retry,
and independently load only the local HTTPS card. Fence local GET results by
page generation and operation epoch so a late GET cannot undo a newer save.
Exact GET/PUT HTTPS intent and GET public-certificate routes remain eligible
on learners and during maintenance; existing AdminUser, removal and serving
fences still apply. No neighboring system route receives this exception.
Errors and logs never include PEM material, request bodies or private bytes.

The desired file is `data_dir/https-settings.json`, with a versioned envelope:

```json
{"version":1,"revision":1,"settings":{"enabled":false,"port":32443,"cert_file":"","key_file":"","sans":["localhost","127.0.0.1","::1"]}}
```

Reject unsupported versions and malformed files rather than ignoring intent.
Revision is monotonically incremented for successful API saves. Manual edits
are performed only while stopped and must increment it. File input is bounded
to 16 KiB. Document the exact format alongside recovery commands.

Add admin-only `GET /api/v1/system/https/certificate` to download the active
public chain as `application/x-pem-file`, attachment, `Cache-Control: no-store`.
Return 404 when inactive. Export from loaded certificate DER only, never by
returning the managed bundle. Tests parse the export, verify certificate-only
blocks and the active fingerprint, and deny non-admin access. The UI provides
a download and SHA-256 fingerprint so an operator can verify it through a
trusted channel before installation. Trust installation is explicit and
platform-dependent; never instruct users to import a combined private bundle.

## 4. Certificate ownership and lifecycle

Managed identities live under `data_dir/https`, separate from all existing
keys. Unix directory mode 0700 and key mode 0600 apply from creation; use the
repository's Windows protection conventions and document their limits.
Managed files reject symlinks and non-regular files. Atomic complete-unit
publication (one protected PEM bundle is acceptable) and exclusive creation
prevent half-identities and concurrent-start overwrite.

Generate only on first enabled startup when no identity exists. Use server
Auth, SANs, a supported modern algorithm, 365-day validity and five minutes
clock-skew allowance. Preserve the exact certificate/key across restarts and
off/on. Corrupt/partial/unreadable existing identity is an error. Do not
silently rotate or renew: clients may trust the exact self-signed leaf.
Report expiry; document explicit replacement: stop, securely back up/remove
the managed identity, restart, re-trust. Changing SANs requires this explicit
workflow: stop the service, edit SANs in `https-settings.json` (and any
controlling environment override), increment revision, back up/remove the
managed identity, restart, verify/download the new public certificate and
re-trust it. Removing the identity without editing intent recreates the old
SAN coverage. The managed identity filename must be recorded in operations
during H1. Validate existing coverage and explain this workflow before Save.
GET never provisions/rotates identity. Never destroy an identity on disable.

Supplied mode reads a leaf-first chain and one unencrypted supported key,
each bounded to 1 MiB. Verify PEM format, key match, current validity and
server-auth suitability; accept private CA/self-signed certificates without
requiring public-CA trust. Reject unrelated/malformed PEM material. On Unix, follow
legitimate supplied-file symlinks used by certificate managers but validate
the opened regular file/size. Windows accepts regular local disk PEM paths
through the existing no-reparse helper; device, pipe and UNC namespaces and
certificate-manager symlinks are unsupported and rejected before traversal.
Windows compilation/physical safety remains unqualified in this increment. Opening and reading must be bounded: use
nonblocking/safe regular-file opening so a FIFO/device cannot hang before
metadata validation. Test a named pipe and a symlink to one. Never overwrite
supplied files. Client trust
and hostname coverage remain operator responsibilities. Restart loads renewals;
GET reports the certificate actually loaded, not changed files on disk.

## 5. Serving, shutdown and URL behavior

Prepare enabled TLS configuration and bind before declaring full application
readiness. Both listeners serve the identical authorized router. HTTPS starts
when that router exists, including the `StartupObservationHttp` transition;
the earlier observation-only phase remains explicitly HTTP. Integrate HTTPS
stop/drain in both ordinary and observation-owner serving paths before
progress flush and clean-census marking. Bind or background task failure must
not leave an orphan listener or falsely healthy runtime status.

Use explicit existing rustls crypto-provider policy and h2/http1.1 ALPN.
Preserve TCP_NODELAY and `ConnectInfo`. Allow at most 128 pending handshakes,
with 10-second deadlines and shutdown cancellation. Handshakes run
concurrently: slow/malformed clients cannot serialize admission or trigger
TCP accept-error backoff. Handshake errors are connection-local; descriptor
exhaustion is a listener error. Reap owned tasks and release all permits.

Keep existing HTTP discovery truthful. The admin card constructs an HTTPS
link from the current browser hostname and configured port (correct IPv6),
without credentials/query/hash. Label this an unverified connection link,
especially behind a proxy/load balancer: the browser hostname does not prove
which node that port reaches. Keep the edited node identity visible.
Do not navigate automatically or transfer a
token between origins. Explain that the new origin may require sign-in.

Audit API, HLS/media, range, subtitles, artwork, WebSocket, remote-control and
QR URLs. Relative URLs inherit HTTPS. There is no current product WebSocket
endpoint; generic Hyper upgrades remain supported without a product playback
qualification claim. Existing
cluster peer proxy paths must retain the browser's origin. Any unavoidable
cross-node redirect must not downgrade an HTTPS browser to HTTP: use the
same-origin proxy or return an explicit bounded unsupported-route error.
Do not invent an HTTPS address/trust identity for another node or globally
rewrite internal transports. Record supported/unqualified scenarios.

## 6. Acceptance and recovery

[The build matrix](HTTPS-BUILD.md#3-regression-matrix) requires real TLS
sockets and test-local trust stores. A test disabling verification is not
trust evidence. Native clients use normal platform trust; no accept-any
certificate handler. Physical browser/native trust and playback are separate
acceptance, not inferred from server tests.

Recovery: stop, set `PLURX_HTTPS_ENABLED=false`, restart, then repair paths or
node-local desired settings. Existing HTTP remains available. Document env
precedence, secure backup and re-trust. Exclude private material from
telemetry/diagnostics/source control. Public examples use neutral addresses.
