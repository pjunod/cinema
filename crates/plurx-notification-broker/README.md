# Cinema notification broker

**Status:** open — software implementation with focused synthetic provider and
real loopback HTTP evidence; production provider/native delivery remains unverified.

This separate Apache-2.0 workspace binary enrolls a paired phone's notification
transport and sends one fixed Cinema invitation through APNs or FCM. It has no
Cinema login, browsing, arbitrary notification, recipient enumeration, URL,
power, or playback command API. Home and native clients own explicit consent,
registration cleanup, and tap revalidation; deploying this binary alone does not
finish that integration.

> Restoring an old database under unchanged external keys cannot be detected.
> Rotate the external generation, master key, and publisher proofs before an
> offline restore fence. Never silently rebuild a database or key over existing
> capabilities.

## Build and run

From the repository root, with its pinned Rust toolchain:

```bash
CARGO_TARGET_DIR="$PWD/target" rustup run 1.97.1 cargo build --locked -p plurx-notification-broker
CARGO_TARGET_DIR="$PWD/target" rustup run 1.97.1 cargo test --locked -p plurx-notification-broker
CARGO_TARGET_DIR="$PWD/target" rustup run 1.97.1 cargo clippy --locked -p plurx-notification-broker --all-targets -- -D warnings
```

Keep the external manifest and private keys outside the database's backup and
restore domain, in an operator-owned directory. All file paths below are resolved
against the process working directory; use absolute paths. Unix private-key and
master-key files must have no group/other permission bits. On Windows the operator
must enforce equivalent per-service-account ACLs; this binary does not prove
Windows ACL isolation. Protect the directory, database and WAL/SHM files too.

The master-key file contains exactly 32 random bytes. The publisher proof is a
separate random 32-byte value encoded as canonical unpadded base64url: exactly 43
ASCII characters. Configure **SHA-256 of those 43 text bytes**, not the decoded
32 bytes. Store the clear publisher proof only with its authorized home service.
The broker never creates or prints any key or proof.

A manifest has this strict shape (values shown are illustrative, not credentials):

```json
{
  "version": "cinema.broker.operator.v1",
  "generation": "a640bd62-1a69-4f06-8809-b725310bfad5",
  "master_key_file": "/srv/cinema-broker/private/master.key",
  "publishers": [{
    "publisher_id": "f482d5c0-12a4-4b07-9082-7484b60d7c58",
    "server_instance_id": "your-exact-home-instance",
    "proof_hash": "replace-with-lowercase-64-hex-digest-of-canonical-proof-text",
    "apple": null,
    "android": null
  }]
}
```

An empty publisher list disables every authority. A null platform refuses new
platform tickets with `provider_unconfigured`; readiness must report that
condition, never claim delivery. To configure Apple, replace `apple` with:

```json
{"team_id":"TEAM123456","key_id":"KEY1234567","topic":"tv.plurx.cinema","environment":"sandbox","private_key_file":"/srv/cinema-broker/private/apple.pk8"}
```

Use `production` only for a matching production topic/device token. For Android:

```json
{"project_id":"your-google-project","service_account_email":"your-service-account@your-google-project.iam.gserviceaccount.com","private_key_file":"/srv/cinema-broker/private/google.pk8"}
```

Private keys are **unencrypted PKCS#8 DER**, P-256 for Apple and RSA for Google.
Convert an operator-provided PEM with a restrictive umask; neither command prints
the key:

```bash
umask 077
openssl pkcs8 -topk8 -nocrypt -in AuthKey_SAMPLE.p8 -outform DER -out /srv/cinema-broker/private/apple.pk8
openssl pkcs8 -topk8 -nocrypt -in google-service-account-private.pem -outform DER -out /srv/cinema-broker/private/google.pk8
chmod 600 /srv/cinema-broker/private/*.pk8 /srv/cinema-broker/private/master.key
```

The Google PEM is the service-account JSON's `private_key` field, extracted to a
protected file by the operator; never put that JSON or key in source control.

```bash
# Initialize only a new DB; refuses an existing file.
target/debug/plurx-notification-broker --manifest /srv/cinema-broker/private/operator.json --database /srv/cinema-broker/state/broker.sqlite init
# Default listener is 127.0.0.1:8769; publish only through verified HTTPS.
target/debug/plurx-notification-broker --manifest /srv/cinema-broker/private/operator.json --database /srv/cinema-broker/state/broker.sqlite serve
```

`--listen` is an explicit operator override. No TLS listener, global installation,
provider deployment, or distribution packaging is included. An external reverse
proxy must preserve the exact capability/generation headers and reject oversized
headers/connections at its own perimeter. The broker prints only a generic error
code on failure; no HTTP/provider bodies, tokens, proofs or private paths are
logged.

## Capability API

Requests accept `application/json` with an optional UTF-8 charset parameter;
other media types/charsets and duplicate Content-Type headers are refused.
Every JSON envelope uses `version: "cinema.invitation.v1"`, rejects unknown or
duplicate fields, and is at most 64 KiB. IDs are canonical lowercase UUIDs;
generations and Unix-second expiries are positive safe JSON integers. Numeric
lexemes such as `1.0`, `1e0`, `-0` and unsafe integers are rejected. Credentials
are headers only; query strings, `X-Api-Key`, duplicate authorization headers and
alternative Bearer spacing are refused.

Publisher endpoints require exactly one of each:

- `Authorization: Bearer <canonical publisher proof>`
- `X-Cinema-Publisher-Id: <configured UUID>`
- `X-Cinema-Broker-Generation: <external manifest generation UUID>`

The generation is checked before effects; a mismatch is
`409 stale_broker_generation`. Every JSON response, including errors and native
claims, carries the broker's verified manifest generation in that same header.
Home must check it before accepting any capability or acknowledging cleanup.
A missing/mismatched response header is uncertainty requiring retained cleanup
work, not success.

| Endpoint | Behavior |
|---|---|
| `POST /broker/v1/tickets` | Binds fresh home-selected `ticket_id`, proof-text hash, exact server/installation/receiver/consent IDs, three generations and platform. Pending TTL is 120 seconds from first issuance; an exact retry never extends it. |
| `POST /broker/v1/tickets/claim` | Phone's ticket proof only, no publisher/Cinema headers. Body has version, ticket_id, platform, device_token. Atomic single use; phone receives `{version,status:"claimed"}`, never publisher/enrollment authority. |
| `POST /broker/v1/tickets/status` | Publisher's version/ticket_id body returns every immutable binding and pending/claimed/revoked/expired status. Claimed status remains resolvable after the original pending TTL. |
| `DELETE /broker/v1/enrollments/{id}` | Empty body. Durable scoped tombstone, even before issuance/claim; enrollment_id equals ticket_id. Foreign/missing IDs produce identical own-scope revoke responses. |
| `POST /broker/v1/deliveries` | Exact active installation/enrollment/generations, opaque invitation_id and future expiry no more than 120 seconds away. Durable dedupe/attempt is committed before provider I/O. |

An opaque invitation is canonical 43-character base64url encoding of the
installation UUID bytes followed by a fresh event UUID's bytes. The prefix must
match the delivery installation. No title, URL, login, grant, or OS action enters
a provider payload. APNs receives a visible fixed alert, category
`CINEMA_REMOTE_INVITATION`, and the opaque invitation. FCM receives **data-only**
invitation/category with Android `HIGH` priority and a bounded TTL. Android's
native owner checks local consent/installation/dedupe before immediately showing
“A paired screen is ready” in its dedicated notification channel, without waiting
for a network request. A system-tray notification block would bypass those local
checks and is deliberately absent.

`accepted` means only provider HTTP acceptance, never physical delivery.
`duplicate` never sends again; `denied` consumes a policy/provider refusal;
`unknown` consumes an uncertain attempt. A timeout, crash, cancellation or lost
response cannot retry the same invitation. Invalid provider tokens revoke that
enrollment. Enrollment/config eligibility is checked again after OAuth and
immediately before starting the visible request; a revoke during authentication
consumes without sending. An already-started external send cannot be recalled,
and APNs may show an already-sent alert after local OFF; its tap still revalidates.

## Bounds and configuration changes

The SQLite store uses FULL synchronous commits and encrypts device tokens with
ChaCha20Poly1305, fresh nonce, and AAD covering publisher, external generation,
enrollment, platform, transport generation and the full immutable ticket binding.
The master key is independent from provider/publisher credentials. Zeroizing
buffers protect retained key/token material; no claim of complete allocator/TLS
buffer erasure is made.

| Limit | Refusal/lifetime |
|---|---|
| 64 publisher identities **including inactive** per external generation | New identities at capacity return `retention_limit` before any reconciliation mutation. Removing a publisher never releases its slot. |
| 4,096 pending and 4,096 claimed enrollments per publisher | Expired unclaimed tickets release pending capacity, retain identity. |
| 100,000 ticket/enrollment identities per publisher/generation | Every issuance reserves eventual revoke capacity. Known revoke succeeds at capacity; new tombstone/issuance is refused. |
| 100,000 delivery identities per publisher/generation | No live-generation pruning or replay resurrection. |
| 120 publisher admissions/minute; 30-minute installation cooldown | Persisted across restart and enrollment/token rotation. |
| 16 accepted connections, 16 in-flight HTTP tasks, 4 provider calls/publisher | Immediate rejection/drop; no unbounded semaphore waiters. Idle connections expire after 15 seconds; body reads after 5 seconds. |
| 5 seconds total provider attempt | Includes Google OAuth and visible send. Provider response ≤64 KiB; OAuth response ≤16 KiB. No redirects/proxies/automatic retries. |

For a normal change, stop the old serving process, update protected operator
files, and start again. Reconciliation is transactional. Adding a publisher,
rotating its proof, or rotating a provider signing key **within the same server,
Apple team/topic/environment or Google project** preserves compatible enrollment
and all identity/dedupe history. Former proofs and obsolete running-process
credential snapshots immediately lose registry authorization. Removing a
publisher or changing its server revokes all its capabilities; changing only one
platform realm revokes that platform. Re-adding the same publisher cannot
resurrect revoked IDs. Provider credentials are loaded once at startup; no hot
file watcher or automatic rebind is provided.

## Restore fence

Normal startup refuses a missing manifest/key/database, changed external
generation/master key, schema realm mismatch or observed clock rollback. It does
not repair a mismatch, rotate secrets automatically, or recreate an existing DB.
An old DB restored under the **same** external generation and keys can evade
rollback detection; that is an operator responsibility, not a DB property.

1. Stop every broker process and home publisher admission.
2. Preserve the current external manifest separately; restore the chosen DB only
   while offline.
3. Choose a fresh external generation UUID, a new independent 32-byte master key,
   and new proofs for every configured publisher. Configure matching home proof
   and generation only through its authorized operator path.
4. Run `restore-fence` with the new manifest and restored database. It refuses an
   unchanged generation, master key, or previously recorded configured proof.
   It clears all previous ticket/enrollment/delivery/cooldown authority and binds
   the new marker. Old ciphertext and proofs cannot be admitted afterward.
5. Start the broker and explicitly enroll consenting phones again. Do not ACK
   old-generation cleanup under a new-generation response; home retains unknown
   work for its documented remediation.

```bash
target/debug/plurx-notification-broker --manifest /srv/cinema-broker/private/operator.json --database /srv/cinema-broker/state/broker.sqlite restore-fence
```

## Evidence and remaining acceptance

Focused tests exercise strict wire/header/generation refusal, real Router and
loopback listener, bounded concurrent admission, issuer scope, immutable tickets,
tombstones, encrypted-token tamper/AAD, restart/dedupe/cooldown, interprocess
configuration races and explicit restore fences. Provider tests generate a
synthetic P-256 key and use a committed **test-only synthetic RSA key**, verify
both signatures, inspect exact fixed payload/host formation, and inject replies,
stalls, oversize bodies and revocation during OAuth. None connects to APNs/FCM.

Operator HTTPS/proxy deployment, production credentials/topics/projects, platform
permission prompts, phone background/tap/logout/OFF behavior and actual physical
provider delivery remain separate acceptance. Saved Cinema Developer choices
remain advisory and independent; provider configuration must not gate Save.

Authoritative protocol constraints: [Apple APNs requests](https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns),
[Google service-account OAuth](https://developers.google.com/identity/protocols/oauth2/service-account),
[FCM HTTP v1](https://firebase.google.com/docs/cloud-messaging/send/v1-api),
[Android background receive behavior](https://firebase.google.com/docs/cloud-messaging/android/receive-messages),
and [Android high-priority processing](https://firebase.google.com/docs/cloud-messaging/android-message-priority).
