# Remote diagnostics — operating the independent support service

**Status:** deferred — archive-first release does not require deployment · **Updated:** 2026-10-13 · **Implementation:** merged #1037.

Paul superseded the private-pilot rollout with user-exported ZIP attachments on
2026-10-13. These instructions describe retained source for future hosted
delivery; they are not current deployment work.

The support executable receives fixed-format Cinema reports and serves a
passkey-authenticated operator inbox. It has no Cinema playback, household
login, discovery or cluster authority. Client implementation and release
acceptance remain in the [canonical ledger](../features/REMOTE-DIAGNOSTICS-STATUS.md).

## Build and private state

Build `cargo build --locked --release -p plurx-support` with pinned Rust 1.97.1.
The WebAuthn verifier requires OpenSSL development headers at build time and
the matching OpenSSL runtime library. Cinema's outbound TLS remains rustls.
Named MPL-2.0 dependency exceptions and narrow OpenSSL parent exceptions are
documented in the [dependency decision](../features/REMOTE-DIAGNOSTICS-IMPLEMENTATION.md#16-build-decisions--recorded-before-dependent-implementation).
Retain those upstream license notices with a distributed support executable.

Run as a dedicated `cinema-support` user. Keep its state directory mode 0700;
the 32-byte storage key and publisher/enrollment output files are mode 0600
or stricter. The key lives outside the SQLite database and report exports.
Never reuse a publisher credential as an operator credential.

```sh
plurx-support --state-dir ./support-state --key-file ./support-key init-key
plurx-support --state-dir ./support-state --key-file ./support-key publisher \
  --id cinema-installation --credential-out ./publisher-credential \
  --origin https://cinema.example
plurx-support --state-dir ./support-state --key-file ./support-key operator \
  --name operator --token-out ./operator-enrollment
```

These commands write secrets only to the selected protected files. Output
files must be new. Enroll the named operator at the operator HTTPS origin
with the one-use ten-minute token, then sign in using the newly registered
passkey. There is no shared inbox password or public operator registration.
Sessions last fifteen minutes and require an independent CSRF proof for
every inbox read/write. Revoke a named operator using `revoke-operator
--name operator`; revoke an installation using `revoke-publisher --id
cinema-installation`. Its child capabilities then fail closed.

## Two origins and two listeners

Use separate support and operator HTTPS origins. Bind the binary to two
distinct loopback endpoints behind their corresponding TLS proxies. The
[systemd unit](../../crates/plurx-support/deploy/plurx-support.service) and
[nginx configuration](../../crates/plurx-support/deploy/nginx.conf.example)
are deployment examples; their hostnames and certificates require operator
configuration. They do not provision DNS, certificates or hosting.

```sh
plurx-support --state-dir ./support-state --key-file ./support-key \
  --limits-file ./limits.json serve \
  --support-origin https://support.example \
  --operator-origin https://operators.example
```

Public HTTP is refused, without redirecting a credential-bearing request.
The ingress example disables access logs, caches, request/response disk
buffering and automatic upstream retries. Keep error logs at a level that
does not record request paths, headers or bodies. HTTP upload clients refuse
redirects. Proxy connection/body/deadline limits supplement application
admission and must be retained in production. The nginx directives follow
its [proxy](https://nginx.org/en/docs/http/ngx_http_proxy_module.html) and
[request-limit](https://nginx.org/en/docs/http/ngx_http_limit_req_module.html)
documentation.

By default, rate limiting uses the actual TCP peer. When operating behind
the provided local ingress, `--trust-loopback-ingress` permits exactly one
`X-Cinema-Client-IP` address from a loopback peer. The proxy always replaces
that header with its actual client address. Never forward an incoming value,
enable this trust for an unmanaged proxy, or trust arbitrary forwarded-IP
headers. Without this explicit setting, all clients behind one proxy share
its strict rate bucket. `/healthz` discloses this setting and effective limits.

`--development-local` permits localhost HTTP origins for isolated fixtures.
It does not permit public HTTP origins or non-loopback binary listeners.
Production client destinations remain fixed HTTPS origins.

## Configure Cinema delivery

The Cinema node reads these local configuration references; it never sends
the publisher credential to an app or includes its value in a report.

```toml
[support]
origin = "https://support.example"
publisher_id = "cinema-installation"
publisher_secret_file = "/etc/cinema/support-publisher"
```

Register each exact browser Cinema origin on that publisher. Cross-origin
requests validate both the allow-list and the capability's publisher. No
Cinema Authorization header, cookie or request body is forwarded to support.
User-scoped installation enrollment can be provisioned, rotated or revoked
through Cinema. Restricted offline authority lasts at most 24 hours; the
support service cannot claim knowledge of an account revocation it has not
received. The direct route requires current Cinema authentication.

## Storage, bounds and receipt meaning

Chunks, the immutable ZIP, manifest and user description are encrypted with
XChaCha20-Poly1305 and independent random nonces. Associated data binds their
publisher, report and purpose. SQLite WAL with synchronous FULL is the sole
transactional publication boundary; a successful staged/submitted receipt
requires committed content and metadata. The receipt means durable delivery,
not that an operator has read the report. Upload and report-management grants
have separate authority. Upload-only or enrollment credentials cannot preview,
enumerate or submit a report.

The [limits example](../../crates/plurx-support/deploy/limits.example.json)
shows safe ceilings. An operator can lower positive budgets; larger values
are refused. Defaults allow two pending drafts per enrollment, five per
publisher, ten/100 daily submissions, 1 GiB per publisher and 20 GiB globally.
The fixed archive ceiling is 16 MiB, with 64 MiB expanded data and 32 entries;
chunks are 1 MiB. Eight request workers and two inspections bound concurrent
work. Metadata rows, capability rows, publisher identities and rate buckets
have independent ceilings. Provision disk for twice the retained bytes plus
SQLite/WAL overhead and preserve the process/ingress memory limits.

Unfinished uploads expire after one idle hour and at most 24 hours absolute.
Staged drafts expire after 24 hours. Submitted reports retain thirty days
by default; an operator can select up to ninety days from receipt. TV offers
last ten minutes; approved phone editing lasts thirty minutes, bounded by
the draft's lifetime. Cancel erases the staged draft unless its creator
explicitly keeps it. The original client capture remains independent.

ENOSPC or quota exhaustion refuses new work with a typed failure, preserves
existing committed reports and never reports a successful receipt. Maintenance
replays expiry, checkpoints WAL and reclaims free pages. `reconcile` performs
the same work while the network service is stopped.

## Backups and restore fences

Back up the SQLite database using its online backup API or while the service
is stopped; copying a live database without its WAL is not a backup. Keep
encrypted database backups for at most seven days. Protect the external
storage key, `generation.guard`, `deletions.journal` and `revocations.journal`
separately. Retain their current versions when restoring an older database.
The fsynced journals record report deletion and authority revocation before
the database mutation, so startup replays them against a restored snapshot.
They contain opaque IDs/digests, never report contents or bearer credentials.

A missing/mismatched generation guard, either missing external journal, or a
storage-key fingerprint mismatch refuses startup. Journals are created only
with a fresh namespace; a live namespace also refuses database operations while
either protected journal is unavailable. Do not restore an old guard/journal just to make an old database
serve. Preserve the failed state for offline recovery, stop its ingress and
start a fresh private namespace with a fresh key, publisher credentials and
operator enrollment if the current fencing records cannot be recovered.
That fences all old capabilities. Importing evidence into that replacement
namespace or rotating encryption in place is not implemented and must not
be improvised by disabling the startup checks. Both journals are capped at
8 MiB; reaching that ceiling refuses mutations requiring a new fence. Plan
namespace renewal before exhaustion and keep the old namespace offline.

An authenticated operator can search an exact reference, filter safe
date/platform/build/category/label fields, inspect coverage and typed timeline,
download the validated archive, label a report, adjust retention or delete it.
Viewing, download, deletion and retention changes are audited without report
text. Description/evidence render as text; no report content is executed.
The local `diagnostic-inspect` core example validates the same ZIP/schema
before printing its readable summary.

## Release evidence still required

Compiler results and declared regression fixtures do not prove restart,
ENOSPC, passkey ceremonies, real ingress TLS or physical TV-to-cellular-phone
delivery. Record those receipts in the ledger, with exact source and limits.
Production hostnames, TLS material, publisher/operator credentials and device
access remain deployment inputs. No production deployment is claimed here.
