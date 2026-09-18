# Security assessment — where Plurx's trust boundaries need to change

**Status:** open · **Assessed:** 2026-09-13 · **Source baseline:**
`10f2afe60b3d177866fdcc5741acd9f494525d73`

Companion to [SECURITY.md](../SECURITY.md) (the current promises),
[ARCHITECTURE.md](../ARCHITECTURE.md) (the system design), and the
[September remediation ledger](../features/WEEKLY-REVIEW-REMEDIATION-STATUS.md)
(earlier fixes and deferred work). This assessment answers what to fix next,
why it matters, and what evidence should close each finding. It does not
change runtime behavior or authorize a deployment.

## Verdict — good local controls inside an overly trusted environment

Plurx has substantial security engineering already: Argon2id, random opaque
tokens stored as hashes, administrative extractors, scoped integration keys,
bounded publication resources, careful offline ownership, authenticated peer
messages, and filesystem capability helpers. Replacing these wholesale would
lose useful protections.

The dominant weakness is the assumption that the LAN, media supply chain,
cluster members, and build infrastructure can be trusted together. A home LAN
contains browsers, downloaded media, phones, and appliances with different
security properties. Compromising one should not expose the administrator,
every voter, and another application's API key.

**I would not approve direct internet exposure of the current default
deployment.** A TLS proxy improves public ingress, but does not fix setup
atomicity, cluster impersonation, library symlinks, plaintext replicated
integration secrets, or release authenticity. There is no demonstrated
unauthenticated remote-code-execution exploit in this assessment; that is
not a claim that none exists.

The first corrective work should address setup, media-root containment, and
credential exposure. Cluster identity and safe revocation are the most
important architectural work. Parser isolation and release verification
reduce the impact when prevention fails.

## Scope — broad source review, not an exhaustive penetration test

The checkout contained 2,095 tracked files, including 378 Rust files, 55 Swift
files, 137 Kotlin files, and 11 workflow files. Existing documentation edits
and untracked work were present at the start and were preserved. Application
source was unchanged during this assessment.

The review combined source searches, manual control-flow and data-flow
inspection, current primary-source guidance, a Git history secret scan, and
small non-destructive demonstrations. The table below records the coverage;
it does not mean every line in every listed subsystem received equal scrutiny.

| Surface | Work performed | Important limit |
|---|---|---|
| Native and Plex HTTP APIs | Router inventory; public/user/admin/key/capability boundaries; setup, login, reset, playback, settings, reading, scan and management handlers | No running-daemon authorization sweep |
| SQLite and Hiqlite | Compared user creation, token validation, settings writes, reset and offline-lease behavior | No live quorum or failover exercise |
| Cluster | Admission, certificate verification, shared-secret API authentication, per-node application proofs and cache-revocation coordination | No packet capture or live member-removal attack |
| Browser | Token storage and URL construction, output-escaping approach, response headers and EPUB isolation | No complete DOM-XSS or clickjacking browser campaign |
| Apple and Android | Credential storage, backup declarations, network clients, origin handling, image loading and offline authority | No device backup/restore or native runtime penetration test |
| Media and filesystem | Full and targeted scan traversal, direct serving, FFprobe launch, NFO reads, worker launch and existing secure-open helpers | No full parser fuzz campaign or codec exploit research |
| Dependencies and release | Lockfiles, vendored provenance audit, fuzz configuration, workflows, registry transport and deployment recipes | No fresh complete RustSec, OS-package, Android or image vulnerability scan |
| Repository history | Gitleaks with fully redacted output, followed by inspection of all four matches | Local reachable history only; not remote deleted refs or untracked files |

Severity is conditional on the stated attacker access. **High** means a
credible path to account/cluster compromise, sensitive file disclosure, or a
major failure of containment or incident response. **Medium** means a
material persistence, availability, or defense gap. These are remediation
priorities, not measured CVSS scores. “Source-confirmed” means the relevant
code path was traced; it does not imply an end-to-end exploit was executed.

## Threat model — separate five kinds of authority

```text
 Browser / phone / TV ── account session ──▶ public API
          │                                    │
          └── narrow media capability ──────────┤
                                               ▼
 Downloader / NAS ── untrusted bytes ──▶ parser / media worker
                                               │
                              SHOULD NOT inherit account/cluster secrets

 Public API ── explicit application operations ──▶ replicated store
 Voter / learner ── individual node identity ────▶ cluster transport
 Integration ── service-scoped credential ──────▶ named upstream origin
 Build runner ── verifiable signed artifact ────▶ deployment
```

Protect account and integration credentials, family photos and media,
watch/reading history, server files, the replicated database, GPU/CPU/disk
capacity, and release/signing authority. Consider an unauthenticated reachable
client, an ordinary signed-in user, an attacker controlling a media directory
or upstream response, a removed/compromised member, and a compromised runner
or registry. A household administrator deliberately adding a library is
authorized behavior; a media-directory writer escaping that library is not.

## Findings — concrete changes with closure evidence

| ID | Priority | Finding | Evidence class |
|---|---|---|---|
| S01 | High | Network setup is unclaimed and non-atomic | Source + SQL demonstration |
| S02 | High | Default public transport exposes reusable credentials | Source-confirmed design gap |
| S03 | High | Cluster TLS does not establish peer identity; shared authority survives removal | Source-confirmed design gap |
| S04 | High | Replayable integration secrets bypass envelope encryption | Source-confirmed |
| S05 | High | Media symlinks can escape configured roots during scan and read | Source + filesystem demonstration |
| S06 | High, conditional | Monarr redirects can forward its API key to another origin | Source-confirmed client behavior |
| S07 | High | An unavailable member can block credential revocation | Source-confirmed design gap |
| S08 | Medium | Account sessions have no expiry or per-device management | Source-confirmed |
| S09 | Medium | Browser account authority is exposed in URLs and script-readable storage | Source-confirmed defense gap |
| S10 | Medium | Android's reusable token is stored in backup-eligible preferences | Source-confirmed configuration |
| S11 | Medium | Password reset does not revoke existing offline media leases | Source-confirmed |
| S12 | Medium | Scanner subprocess and sidecar ingestion lack consistent bounds | Source-confirmed |
| S13 | High, containment | General media workers retain daemon-level access | Source-confirmed design gap |
| S14 | High | HTTP registry and unsigned artifacts leave release identity unproved | Source-confirmed deployment gap |
| S15 | Medium | Security scanning is not a complete candidate/release gate | Source-confirmed workflow gap |

### S01 — claim setup locally and create the first admin atomically

**Evidence.** [system.rs:96](../../crates/plurxd/src/http/system.rs#L96)
accepts unauthenticated setup, checks `count_users()`, awaits password hashing,
then calls ordinary `create_user(..., true)`. Both
[SQLite users.rs:29](../../crates/plurx-core/src/store/sqlite/users.rs#L29)
and [Hiqlite:3504](../../crates/plurx-core/src/store/hiqlite.rs#L3504) perform
unconditional inserts. Different usernames do not conflict. Default binding
is all interfaces in [config.rs:63](../../crates/plurx-core/src/config.rs#L63).

**Impact.** Anyone who reaches a fresh installation can claim its first
administrator. Independently, concurrent setup calls can both observe zero
users and both become administrators, including a request that finishes after
the legitimate setup. Password-hash admission limits concurrency but does not
make this sequence atomic. This applies to pristine installations, not to
arbitrary later requests after setup is complete.

**Fix.** Require an installation-specific, short-lived bootstrap proof obtained
locally or from an owner-only file. Consume it and create the first user in a
single durable conditional transaction. An in-process mutex is insufficient
when different voters accept requests. Do not use a factory password or
silently re-enable setup following an operational failure.

**Close when.** Concurrent setup requests through two ingress nodes produce
exactly one administrator; missing/expired/replayed bootstrap proofs fail;
restart preserves the claim. The SQL demonstration here produced two admins
after two zero-count observations; an HTTP regression remains to be added.

### S02 — make protected transport the normal installation path

**Evidence.** [HTTP defaults](../../crates/plurx-core/src/config.rs#L63),
[Compose publication](../../deploy/docker-compose.yml#L102), and
[Android's cleartext permission](../../clients/android/app/src/main/AndroidManifest.xml#L36)
support plain HTTP on a reachable LAN. The trust assumption is explicit in
[SECURITY.md](../SECURITY.md#transport--public-http-internal-cluster-tls).

**Impact.** A passive observer on an accessible network path can obtain
passwords, bearer tokens, returned integration secrets and join material.
An active intermediary can change browser code or API responses. A private
IP address is not authentication, and HTTPS at an external proxy does not
protect an independently reachable HTTP listener or an unprotected upstream
network segment.

**Fix.** Ship a supported HTTPS ingress recipe with a defined public origin,
proxy-to-daemon boundary and firewall policy. Bind standalone setup to
loopback by default or require explicit LAN exposure. Native clients should
default to HTTPS and require an explicit server-specific exception for HTTP;
do not replace certificate validation with an “accept any certificate” switch.
Preserve secure cluster failover origins and test the native reader as well.

**Close when.** A fresh install has a complete protected enrollment path;
external clients cannot bypass the proxy; HTTP downgrades are refused; native
playback, discovery and local-network onboarding still function.

### S03 — authenticate individual cluster members and retire shared authority

**Evidence.** [Hiqlite TLS:190](../../vendor/hiqlite/src/tls.rs#L190)
accepts arbitrary certificates and signatures. Plurx's
[membership request:9675](../../crates/plurx-core/src/cluster/membership.rs#L9675)
accepts invalid certificates and sends `X-API-SECRET`. The
[internal HTTP guard](../../vendor/hiqlite/src/network/mod.rs#L66) compares a
shared API secret. The
[SQL API WebSocket:550](../../vendor/hiqlite/src/network/api.rs#L550)
authenticates that shared secret and discards the returned client identity.

**Impact.** An active network attacker impersonating an internal HTTP listener
can capture the API secret. A removed member retaining shared secrets is not
cryptographically excluded from those shared-secret APIs merely because its
membership row changes. This authority is broader than a public user token.
Application-level Ed25519 checks on newer peer routes are valuable, but do not
repair the underlying shared-secret SQL and membership surfaces.

The WebSocket handshake uses a challenge/response; this finding does **not**
claim it transmits the raw Raft secret. The concrete raw-secret exposure is
the HTTP API header over an unverified TLS connection.

**Fix.** Enroll per-node identities using authenticated bootstrap trust; use
verified mTLS or pinned, individually revocable keys; bind peer identity to
the current committed role on every privileged surface. Specify how existing
connections, certificates, retained shared secrets, learners, rolling upgrades
and removal interact. Refuse redirects on cluster credential requests.

**Close when.** A counterfeit certificate fails, a removed node's retained
credentials and open connections fail, learner privileges remain narrower,
and rotation works during supported mixed-version operation. A normal HTTPS
reverse proxy in front of the public API does not close this finding.

### S04 — encrypt every replayable integration secret before replication

**Evidence.** [system.rs:3106](../../crates/plurxd/src/http/system.rs#L3106)
writes TMDB/OMDb keys, the Trakt client secret and the Monarr API key through
ordinary settings. [Hiqlite:3352](../../crates/plurx-core/src/store/hiqlite.rs#L3352)
persists their string values without sealing. The
[settings response:1942](../../crates/plurxd/src/http/system.rs#L1942)
returns these values to administrators. The separate
[SealedSecret implementation](../../crates/plurx-core/src/secrets.rs) protects
Trakt access/refresh tokens, not these settings.

**Impact.** A copied database, WAL, snapshot or backup can contain usable
upstream credentials. Monarr access may have substantially more authority than
reading a calendar; the exact scope depends on Monarr's key. Routine settings
reads also needlessly expose the full secret to browser memory and responses.
Admin access is intentional, so the settings response is not an ordinary-user
authorization bypass.

**Fix.** Extend typed envelope encryption to replayable settings with
domain-separated associated data binding each envelope to its intended
setting. Enforce sealing at durable writes and imports. Return configured
state and a non-sensitive identifier; accept replacement secrets separately.
Rotate upstream credentials after migration because sealing current rows
does not erase historical plaintext. Back up wrapping keys separately and
prove restore before rotation. See
[OWASP secrets management](https://cheatsheetseries.owasp.org/cheatsheets/Secrets_Management_Cheat_Sheet.html).

**Close when.** Synthetic canary secrets are absent from database, WAL,
snapshot, export and ordinary settings responses; legacy migration, wrong-key
startup and restore tests cover every backend.

### S05 — enforce media-root authority at discovery and file open

**Evidence.** [Full scans:482](../../crates/plurx-core/src/scan/mod.rs#L482)
and [targeted directory walks:820](../../crates/plurx-core/src/scan/mod.rs#L820)
follow symlinks. The targeted scan validates its starting path, but does not
apply that root check to every walked descendant. Probe failure does not
prevent recording a file
([scan:1050](../../crates/plurx-core/src/scan/mod.rs#L1050)).
[Direct playback:2153](../../crates/plurxd/src/http/stream.rs#L2153) loads a
database path and [range serving:2550](../../crates/plurxd/src/http/stream.rs#L2550)
opens it normally, following symlinks again. Photos and original books reuse
this serving helper.

**Impact.** A writer of a library directory can create a media-named symlink
to a file readable by the daemon outside that library, or replace an indexed
file with such a link. A signed-in reader can then retrieve it by its media
ID. A database-derived ID prevents URL traversal but does not make the stored
path trustworthy. Read-only media mounts protect against daemon writes; they
do not prevent another host process from changing the mounted directory.

**Fix.** Treat configured roots as explicit filesystem capabilities. Reject
escaping descendants during scan and perform descriptor-relative, constrained
opens at use time, including intermediate components and regular-file checks.
Support intentional symlinked roots by resolving the configured authority once,
not by permitting arbitrary nested links. Reuse the existing
[fs_secure.rs](../../crates/plurx-core/src/fs_secure.rs) approach rather than
adding another `canonicalize(); open()` race.

**Close when.** Full scan, targeted directory scan, direct/Plex/photo/book
reads reject escaping links and post-index replacements. Test intermediate
directory swaps, special files and permitted symlinked roots. The local
primitive demonstration confirmed that a media-named link is a file and
ordinary open reads outside the root; no real server secret was read.

### S06 — stop forwarding Monarr credentials through redirects

**Evidence.** [probe_monarr:321](../../crates/plurxd/src/http/comingsoon.rs#L321)
and [calendar fetch:668](../../crates/plurxd/src/http/comingsoon.rs#L668)
build the default redirect-following Reqwest client and attach `X-Api-Key`.
The locally available locked Reqwest 0.12.28 source, `src/redirect.rs:239`,
strips selected standard sensitive headers on cross-host redirects, but not
this custom header. The repository's artwork clients already use narrower
redirect policies.

**Impact.** A compromised Monarr endpoint, malicious redirect configuration,
or intermediary on its HTTP connection can redirect the request to another
origin and receive the key. This is conditional on controlling that upstream
response; an ordinary viewer cannot configure the upstream URL. It is not an
unauthenticated arbitrary-URL API finding.

**Fix.** Refuse redirects for credential-bearing integration requests, or
follow only explicitly approved same-origin redirects while rebuilding
credentials after the destination check. Apply the same policy to all Monarr
consumers and cluster custom-secret headers. Bound response bytes before JSON
allocation and make proxy use an explicit integration policy.

**Close when.** Two local test origins prove that 301/302/307/308 responses
never deliver a credential to the second origin, including a TLS downgrade.
This session verified source behavior, not that two-origin HTTP regression.
The broader design should follow
[OWASP's outbound-request guidance](https://cheatsheetseries.owasp.org/cheatsheets/Server_Side_Request_Forgery_Prevention_Cheat_Sheet.html).

### S07 — revocation must remain possible when a member is unavailable

**Evidence.** [Logout:94](../../crates/plurxd/src/http/auth.rs#L94) and
[password reset:87](../../crates/plurxd/src/http/users.rs#L87) enter
`ClusterCacheRevocation` before changing credentials. The
[stable-roster contract:55](../../crates/plurxd/src/http/internal_auth_revocation.rs#L55)
requires every committed member to acknowledge the Begin phase. The earlier
[remediation ledger](../features/WEEKLY-REVIEW-REMEDIATION-STATUS.md) explicitly
defers moving ordinary revocation away from this all-member exclusion.

**Impact.** One unreachable or refusing member can prevent logout, password
reset or account-removal mutations even while a quorum could otherwise commit
them. Clearing a phone's local bearer cannot revoke a stolen copy. This is
particularly damaging during the incident in which revocation is needed.

**Fix.** Make revocation quorum-authoritative and design recovery proofs to
fail closed through bounded epochs/leases when their freshness cannot be
established. Keep the five-minute emergency-read behavior narrowly scoped;
do not “fix availability” by accepting stale ordinary authorization. Provide
a daemon-owned, OS-authenticated recovery channel for the last administrator.

**Close when.** With one committed member offline, a surviving quorum can
revoke access and a returning node cannot revive it. Test delayed writes,
stale proofs, promotion/reset races, membership changes and cancellation.

### S08 — add an explicit account-session lifecycle

**Evidence.** [SQLite token lookup:273](../../crates/plurx-core/src/store/sqlite/users.rs#L273)
and [Hiqlite lookup:3821](../../crates/plurx-core/src/store/hiqlite.rs#L3821)
check the token hash and user, with activity timestamps but no absolute or
idle expiry. [The router](../../crates/plurxd/src/http/mod.rs) exposes logout
but no account session inventory or selective device revocation. Password
creation enforces eight encoded bytes rather than eight Unicode characters
([auth.rs:128](../../crates/plurxd/src/http/auth.rs#L128)); login has bounded
Argon2 work but no identity-based guessing controls.

**Impact.** A leaked or forgotten device token remains useful until explicit
revocation. Users cannot readily identify and remove one lost device. Global
hash-worker limits contain resource use but do not prevent sustained guessing.

**Fix.** Define browser, TV, admin and integration credential lifetimes
separately. Use short-lived access with rotated, revocable device credentials
where persistent TV login matters; cap outstanding sessions; expose last use
and per-device revocation. Add throttling with bounded state, progressive
delay and trusted-proxy address handling. Improve password length semantics
and compromised-password checks; prefer passkeys or optional external identity
for administrators, with step-up authentication for sensitive operations.

**Close when.** Expiry and refresh replay tests use a controllable clock;
lost-device revocation works; rate limiting cannot itself create unbounded
state or trivially lock out another user. Product-specific lifetimes should
be documented using
[OWASP session guidance](https://cheatsheetseries.owasp.org/cheatsheets/Session_Management_Cheat_Sheet.html)
and [authentication guidance](https://cheatsheetseries.owasp.org/cheatsheets/Authentication_Cheat_Sheet.html).

### S09 — narrow browser media authority and add response-level defenses

**Evidence.** [index.html:3431](../../crates/plurxd/src/web/index.html#L3431)
loads the account token from localStorage;
[the URL helper:3530](../../crates/plurxd/src/web/index.html#L3530) appends it
to query strings. [extract.rs:547](../../crates/plurxd/src/http/extract.rs#L547)
accepts query credentials for ordinary account authentication, including admin
routes. [web.rs:51](../../crates/plurxd/src/http/web.rs#L51) returns the app
shell without a CSP or anti-framing policy; the shared router does not add
them. The shell relies heavily on inline handlers and `innerHTML` escaping.

**Impact.** A copied media URL can carry account-wide authority. URLs can reach
proxy logs, diagnostics and sharing surfaces despite Plurx's own trace
redaction. Any future same-origin script injection can read localStorage; the
shell has no independent CSP containment or application-supplied clickjacking
defense. No exploitable XSS payload was established here.

**Fix.** Generalize the existing narrow media-capability pattern to image and
direct-media access, with bounded resource scope and lifetime. Remove query
account authentication after clients migrate. Evaluate an HttpOnly, Secure,
SameSite browser session with explicit CSRF defenses while preserving native
header-based authentication. Move inline event handlers into trusted scripts,
then enforce a strict CSP. Add `frame-ancestors`, `nosniff`, an appropriate
referrer policy and `no-store` on credential responses.

**Close when.** Media URLs cannot invoke `/me`, settings or other files; proxy
logs do not retain capabilities; hostile metadata stays inert; framing fails;
the native reader and HLS paths still work. A CSP allowing unrestricted inline
script is not equivalent to completing this work. See
[OWASP CSP guidance](https://cheatsheetseries.owasp.org/cheatsheets/Content_Security_Policy_Cheat_Sheet.html).

### S10 — keep Android account credentials out of portable preferences

**Evidence.** [SettingsStore:14](../../clients/android/app/src/main/java/tv/plurx/app/data/SettingsStore.kt#L14)
uses ordinary Preferences DataStore and
[saveSession:139](../../clients/android/app/src/main/java/tv/plurx/app/data/SettingsStore.kt#L139)
stores the bearer string. [The manifest:26](../../clients/android/app/src/main/AndroidManifest.xml#L26)
enables backup. Both [legacy backup rules](../../clients/android/app/src/main/res/xml/backup_rules.xml)
and [data-extraction rules](../../clients/android/app/src/main/res/xml/data_extraction_rules.xml)
exclude offline media, but not the credential-bearing DataStore.

**Impact.** Device data and supported backup/transfer paths can carry a reusable
account token. This does not mean other sandboxed apps can freely read it or
that Android cloud backups are necessarily unencrypted. Android documents
the applicable platform protection and default inclusion rules in
[Auto Backup](https://developer.android.com/identity/data/autobackup).

**Fix.** Protect the token with a device-bound Android Keystore key and exclude
the credential store from cloud backup and device transfer. Keep ordinary
preferences portable. Require reauthentication on credential restore failure
and migrate existing plaintext values. Apple's
[ThisDeviceOnly Keychain storage](../../clients/apple/Sources/TokenVault.swift#L37)
is a useful existing counterpart.

**Close when.** Backup and transfer fixtures contain no reusable bearer;
reinstallation, key invalidation, logout and device restore fail safely without
losing non-sensitive settings.

### S11 — distinguish downloaded bytes from still-valid download authority

**Evidence.** [Password reset:155](../../crates/plurx-core/src/store/sqlite/users.rs#L155)
deletes account tokens but not offline leases. The
[lease lookup:846](../../crates/plurx-core/src/store/sqlite/offline.rs#L846)
accepts a ready package by lease hash and expiry without checking a password
generation, and successful reads extend expiry. Hiqlite implements the
corresponding lease behavior in
[hiqlite_durable.rs:2449](../../crates/plurx-core/src/store/hiqlite_durable.rs#L2449).

**Impact.** A stolen package lease can continue fetching that package after
password reset and keep extending its lifetime through reads. Its scope is
one package, not the whole account. Deleting the package removes that
authority. This is separate from the unavoidable fact that already-downloaded
bytes cannot be remotely erased.

**Fix.** Bind server-side leases to a credential/security generation, or revoke
them explicitly in the account-compromise operation. Set an absolute maximum
lifetime in addition to sliding activity. Decide whether ordinary logout
should preserve an intentional background transfer, and make that distinction
visible instead of conflating logout with compromise response.

**Close when.** Account-compromise reset stops subsequent lease reads on every
backend; a lease cannot renew forever; already-downloaded offline media retains
the product's documented behavior.

### S12 — bring ordinary scan ingestion under the newer resource controls

**Evidence.** [scan/probe.rs:55](../../crates/plurx-core/src/scan/probe.rs#L55)
uses `Command::output().await` without a local wall-time limit, counted output
cap or `kill_on_drop`. Scanner call sites await it directly. The NFO reader
uses an unbounded [std::fs::read:189](../../crates/plurx-core/src/scan/nfo.rs#L189).
These paths differ from the recently bounded startup/decode-identity probes.

**Impact.** A hostile or pathological media/sidecar file can monopolize a scan,
grow retained output or allocations, and leave a child running after the
awaiting future is abandoned. Admin-configured roots do not imply trusted
file content when downloaders and NAS shares supply the bytes.

**Fix.** Use a shared bounded subprocess runner with counted stdout/stderr,
an operation deadline, owned process-group termination/reaping and bounded
admission. Apply regular-file and byte caps before reading sidecars, and run
blocking parsing off the async executor. Pick explicit limits for scanner
work independently from long-running playback.

**Close when.** Fake children that hang, flood output, or leave descendants
cannot exceed time/memory/process budgets; oversized and special-file NFOs
are refused; the next ordinary scan completes.

### S13 — separate media decoding from daemon secrets and privileges

**Evidence.** The general
[FFmpeg launch path:1834](../../crates/plurxd/src/transcode.rs#L1834) and
[scanner FFprobe launch](../../crates/plurx-core/src/scan/probe.rs#L55)
execute as the daemon. Docker uses a non-root account and the
[systemd unit](../../deploy/plurxd.service) has useful hardening, but these do
not separate workers from the daemon's data and secrets. The newer
[decode-facts implementation](../../crates/plurxd/src/decode_facts.rs)
contains Landlock/seccomp work for its qualified probe path; it is not a
universal sandbox for all transcoding and scanning.

**Impact.** A decoder compromise inherits access to credentials and files the
service can read, and may reach cluster services. Argument vectors prevent
shell interpolation; they do not prevent vulnerabilities in codecs, parsers,
GPU drivers or delegated protocol handling. No codec RCE was reproduced.

**Fix.** Run parser/transcoder workers with a separate identity and constrained
filesystem, passing only held input/output descriptors and required GPU
devices. Deny network access for local-file work. Give Live TV a separate,
narrow egress profile. Remove inherited secrets/environment, add cgroup
budgets, and validate seccomp/AppArmor/Landlock choices on real GPU paths.
Strengthen container defaults with no-new-privileges, dropped capabilities and
a read-only root where supported. Do not claim a non-root container alone
provides this worker boundary.

**Close when.** Worker fixtures cannot read a synthetic daemon credential or
contact unauthorized listeners, and supported software/hardware decode,
subtitle, DVR and conversion paths still pass focused tests.

### S14 — establish release identity independently of the registry

**Evidence.** The assessed revision's `.github/buildkitd.toml` explicitly
enabled HTTP for the fleet registry.
[publish-release.yml:203](../../.github/workflows/publish-release.yml#L203)
disables provenance and SBOM output for the release-image step.
[registry-push:39](../../scripts/registry-push#L39) checks labels, reported
build identity and runtime behavior by executing the image, but does not
verify a trusted signature before execution. The workflows use self-hosted
runners with registry credentials.

**Impact.** Digests establish byte identity only when the expected digest
comes through a trusted path. A substituted image can claim the expected
labels and print the expected version while doing something else. Testing
that untrusted image executes it before origin is established. A compromised
runner or registry is a fleet-compromise boundary, not only a build failure.

**Fix.** Enable registry TLS with verified certificates. Sign the exact image
digest, attach build provenance and a complete SBOM, and verify trusted signer,
source and builder policy before any rollout or smoke execution. Separate
untrusted build/test jobs from release credentials and deployment reachability;
prefer disposable workers and short-lived credentials. Keep the existing
immutable action pins and `persist-credentials: false` settings.

**Close when.** A correctly labeled image with the wrong signer is refused
before running, and a tampered provenance statement or unauthorized source
fails. A disconnected clean host can verify and install the approved digest.
[SLSA](https://slsa.dev/spec/v1.1/levels) provides an incremental provenance
model; [GitHub's runner guidance](https://docs.github.com/en/actions/reference/security/secure-use)
explains why self-hosted execution needs isolation. Apply equivalent controls
to the Forgejo infrastructure; do not assume GitHub controls exist there.

### S15 — make security evidence part of candidate qualification

**Evidence.** [rust-audit.yml:3](../../.github/workflows/rust-audit.yml#L3)
has scheduled and manual triggers. Its deterministic `cargo audit` verdicts
and synthesized vendor lockfile are good, but this workflow is not a required
dependency of the reviewed candidate/release workflows. The
[fuzz manifest](../../fuzz/Cargo.toml) contains one PGS inspection target.
No tracked Gitleaks policy, broad SAST policy or complete release-image/mobile
advisory gate was identified in the reviewed workflow inventory.

**Impact.** A scheduled scan can report a problem after a candidate ships.
Rust-only advisory coverage omits bundled FFmpeg, OS/GPU packages, Gradle
dependencies and bundled browser code. Successful Clippy and compilation are
not evidence that those components or authorization boundaries are secure.

**Fix.** Attach fast incremental secret/SAST/dependency checks to candidate
qualification and rerun a fresh complete inventory for release. Preserve the
vendor-provenance scan. Generate SBOMs for shipped artifacts, define risk-based
advisory triage with expiring exceptions, and expand scheduled fuzzing to EPUB,
MP4/NAL/fragment parsing, XML/NFO, ranges and capability decoders. Keep costly
campaigns outside the ordinary compiler loop while making their unresolved
security verdicts visible to promotion.

**Close when.** Seeded synthetic secrets, a known-vulnerable fixture dependency
and a deliberately broken authorization fixture fail the intended gate; scan
infrastructure errors cannot look like a clean verdict. Use
[RustSec](https://rustsec.org/) for Rust advisories and the
[NIST SSDF](https://csrc.nist.gov/projects/ssdf) to connect ownership, build
protection, verification and vulnerability response.

## Additional decisions — security capabilities the product should define

| Decision | Assessment and next step |
|---|---|
| Library access | Ordinary signed-in users can read the shared catalogue and file IDs; the inspected direct-serving path has no per-library ACL. That matches a shared-household model and is not automatically an IDOR defect. If children, guests, private photos or multiple households are supported, add explicit library/download permissions across native API, Plex, search, thumbnails, channels, publication and offline creation. Hiding a UI tile is not enforcement. |
| Native credential destinations | Android's [Net.client](../../clients/android/app/src/main/java/tv/plurx/app/data/Net.kt#L23) attaches the bearer to every non-identity request, and Apple's [AuthImage](../../clients/apple/Sources/AuthImage.swift#L238) authorizes absolute URLs too. Current item DTOs produce local image paths; no attacker-controlled remote-image exploit was established. Separate public image/media clients from account clients and enforce canonical, approved origins at credential attachment so future external artwork cannot create a leak. |
| Security event trail | Add structured events for setup, authentication outcomes, role changes, secret replacement, join issuance/redemption, removal and recovery. Record actor, action, target identifier, outcome and correlation ID; exclude raw credentials and capability URLs. Set retention and protected export policy. [OWASP logging guidance](https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html) is a useful design reference. Existing playback diagnostics should not be mistaken for a security audit trail. |
| Recovery | Replication propagates deletion and does not substitute for backups. Define encrypted portable export, separately protected key recovery, rollback compatibility and isolated restore drills with measured recovery objectives. Define last-admin recovery through an OS-authenticated daemon control channel. These are already acknowledged deferred capabilities, not newly discovered promises. |
| Discovery and public metadata | Public metrics and discovery intentionally reveal deployment shape. Restrict them to the required network and treat discovered instance IDs as hints, not cryptographic identity. Require server trust before submitting saved credentials. |
| Documentation assurance | Replace exhaustive “safe” claims in the current injection table with named covered paths and limits. The statement that every API key is hashed conflates incoming scoped keys with replayable upstream keys. The claim that numeric database IDs settle path safety misses mutable symlinks. Keep precise positive claims, including EPUB's strong isolation, without extending them to unrelated routes. |

## Guidance — use verifiable controls rather than a Top 10 checkbox

Use [OWASP ASVS 5.0.0](https://owasp.org/www-project-application-security-verification-standard/)
as the server/web control catalogue, targeting applicable Level 2 controls
for remote-access and administrative surfaces. This is a proposed engineering
target, not a claim of compliance. Map requirements to a named owner, actual
code, a negative test and the deployment assumptions needed to make the
control effective. Version any requirement identifiers before putting them in
tests, because the numbering changes between ASVS editions.

Use [OWASP MASVS](https://mas.owasp.org/MASVS/) for mobile storage,
authentication, network, platform and privacy checks. Prioritize credentials,
backup behavior, WebView isolation and trusted destinations over anti-tamper
features that do not address Plurx's main risks. Preserve the existing
offline/publication capability separation and device-private media storage.

Do not implement every enterprise control by default. A household server does
not need a service mesh or a custom authentication protocol. It does need a
clear answer to who can claim it, which device is trusted, what a stolen token
can do, what a media parser can read, and how a compromised node is excluded.

## Delivery sequence — close concrete bugs before widening the architecture

These are proposed work packages, not changes applied by this assessment.
Follow [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) when implementing
them, including the pinned compiler loop before editing Rust. Recheck all
source anchors against the chosen implementation base.

| Order | Work package | Main closure evidence |
|---|---|---|
| 1 | Contain exposure: protected ingress, restrict internal ports, registry TLS, dedicated service identity and read-only media where possible | Port/path checks from each network zone; native connectivity; no bypass of ingress |
| 2 | S01, S05, S06: bootstrap transaction, constrained media opens, credential redirect policy | Concurrent two-node setup; symlink swap suite; two-origin credential tests |
| 3 | S04, S08, S10, S11: secret migration, device session lifecycle and recovery generation | Canary absence from durable state/backups; device restore tests; revocation across credentials |
| 4 | S03 and S07: per-node identity, rotation/removal and quorum-safe revocation | Removed-member, malicious-listener, stale-connection and partition campaign |
| 5 | S09, S12 and S13: browser policy, bounded scanner and media-worker isolation | Hostile metadata/browser tests; process budgets; filesystem/network sandbox tests on real GPU configurations |
| 6 | S14 and S15: verified releases and continuous security evidence | Wrong-signer rejection before execution; seeded scanner failures; clean-host verification and restore drill |

Assign one accountable security owner even if the implementation is shared.
Track findings by ID with severity, exposure assumptions, remediation PR,
verification command, remaining limitations and closure date. A “fixed” state
requires the negative test as well as successful normal playback.

The existing strong controls suggest good regression targets: automate the
route-to-authority inventory; test each protected route anonymously, with an
ordinary user, another user's resource, the wrong scoped key, an expired
capability and a revoked credential. Include Plex and background-download
paths. Add multi-node cases where a local passing test would miss a race.

## Evidence — what ran and what remains unverified

| Check | Result | Interpretation |
|---|---|---|
| `gitleaks git --redact=100 --no-banner --log-level warn --report-format json --report-path /tmp/plurx-security-gitleaks.json --timeout 180 .` using Gitleaks 8.30.1 | Exit 1, four matches; all inspected | One test deduplication string, two test cluster secrets, one upstream documented password-hash example. No confirmed live credential. Exit 1 is the scanner's match result, not four validated leaks. |
| Synthetic SQLite setup interleaving | Both count observations were 0; two distinct admin inserts succeeded | Demonstrates the database primitive behind S01; does not execute the HTTP handler or Raft. |
| Synthetic media-named symlink | `is_file=true`; resolved path outside the library; ordinary open read the synthetic external content | Demonstrates the filesystem primitive behind S05; no real credential or production path was touched. |
| Synthetic HLS playlist disguised as `.mkv`, probed against a loopback-only test receiver | Local FFprobe 9.0.1 rejected the format; receiver saw zero requests | The proposed direct network-fetch example did not reproduce. No FFprobe SSRF finding is claimed; this also does not establish behavior of the shipped Jellyfin FFmpeg build. |
| `python3 -m unittest discover -s tests/operations -p 'test_vendor_audit_lock.py'` | 1 test passed | The synthesized vendor-lock helper's regression passed; this is not a fresh advisory scan. |
| `python3 -m unittest discover -s tests/operations -p 'test_docs_index.py'` | 4 tests passed before and after report/index changes | Index/link consistency, not application security evidence. |

The synthetic checks used temporary files and a temporary loopback listener.
The first listener attempt was sandbox-blocked; the bounded check subsequently
ran with approved local execution. No deployed Plurx instance or remote service
was tested, and no repository source or credentials were uploaded for analysis.

No Rust, Swift or Kotlin code was changed or compiled. No production exploit,
full route campaign, device backup extraction, cluster incident exercise,
fresh complete advisory scan, broad SAST run or sustained fuzzing was performed.
The local `cargo` command did not have `cargo-audit` installed. Those missing
checks are explicit next verification work, not clean results. The source
review and demonstrated primitives are sufficient to prioritize these fixes;
they are not a certification that the rest of the codebase is secure.
