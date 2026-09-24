# Android credential exposure and the release build — implementation plan

**Status:** ready for review · **Executes:** D5 / D6 (release half) /
F-android-6 / F-android-7 / F-android-8 / F-android-12 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [SECURITY.md](../SECURITY.md) (the credential model this plan
narrows), [PUBLISHING.md](../PUBLISHING.md) §5 (the signing gap it closes)
and [CLIENT-DEPLOY-PROMPT.md](CLIENT-DEPLOY-PROMPT.md) (the deploy path that
must switch variants). Read review §3.7 rows D5 and D6, then assessment rows
`D5`, `D6`, `F-android-6/7/8/12`, then this document. Four independent
threads (grant · backup rules · dispatcher · release build); each thread's
milestones are ordered, the threads are not. Line numbers are from
`88a3957a`; re-verify by function name.

The standing instruction: **if a step seems to require changing the bearer
model itself (`Net.client`'s interceptor, `extract.rs`'s `?token=`
acceptance for every route, or the capability-URL authority of HLS/offline),
stop and flag it.** This plan adds one narrow grant type and removes one
misuse of the bearer; the general `?token=` narrowing is C8's, and it needs
client migration first (review §3.3 C8).

**Correction to the review:** D5 describes "Open in…" as handing the
bearer to an arbitrary *player*. On both clients it is the **book** path:
Android's button lives inside the `item.isBook` branch
(`DetailScreen.kt:447-491`) and Apple's `openBookExternally`
(`DetailView.swift:2408-2412`) does the same; both build
`/api/v1/files/{id}/content`, which is `stream::book_content`
(`http/stream.rs:2485-2500`) — EPUB/PDF bytes, no playback accounting. No
video "Open in…" exists today. The exposure is exactly as bad (the bearer
still leaves the app, and both clients do it), the consumer is a reader app
(PDF viewers issue Range requests and re-open the URL from their own
history), and the grant needs range and re-open semantics but **not** HLS.
The Apple client is in scope for the grant swap (same one-line call site)
even though this document is filed under Android.

---

## 1. Objective

1. "Open in…" hands a third-party app a URL that is valid for one file,
   for a bounded time, supports byte ranges and repeated opens, can be
   revoked, and carries no account authority — minted by the server, not
   derived from the bearer.
2. The DataStore holding the bearer is excluded from Android Auto Backup
   *and* device-to-device transfer, in both rule files. Optionally the
   bearer is wrapped by an Android Keystore key, with the key's lifecycle
   (logout, uninstall, key invalidation, migration) written down before the
   wrap ships.
3. Latency-sensitive API calls stop queueing behind image loads — after a
   trace shows they do, and without pushing image concurrency past the
   server's eight-permit image gate.
4. The devices run a **signed release** build: signing material in the
   vault, `assembleRelease` in the Makefile's sideload path, the mobile
   release role deploying it, and a Baseline Profile measured on that
   variant, not the debug one.

Done means: both clients use the grant; both rule files exclude the store;
the dispatcher decision is recorded with its trace; the fleet devices report
a release-signed package; the profile's startup numbers are in §6.

---

## 2. Contract today

Re-verify at build time.

### 2.1 The exposure

```kotlin
// DetailScreen.kt:486-491
val url = Session.mediaUrl("/api/v1/files/${playable.id}/content")
context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))

// Session.kt:117-123
fun mediaUrl(path: String): String {
    val base = url(path)
    val credential = token ?: return base
    val joiner = if ('?' in base) '&' else '?'
    return base + joiner + "token=" + URLEncoder.encode(credential, UTF_8)
}
```

`?token=` is read for every route by `token_from_parts`
(`http/extract.rs:547-578`, third branch); `book_content` takes `AuthUser`,
so the URL is a full account credential in a chooser intent, in the
receiving app's history, and in any log that app keeps. The
`MediaFile.isBook` branch is the only caller of `mediaUrl` for an external
intent; Coil and Media3 attach the header through `Net.client` and never
use it.

### 2.2 Storage and backup

`SettingsStore.kt:14`: `preferencesDataStore(name = "plurx")` — file
`datastore/plurx.preferences_pb` under the app's `files` domain. Keys
include `TOKEN` (`:39`), `USERNAME`, origin. Manifest:
`android:allowBackup="true"`, `android:fullBackupContent="@xml/backup_rules"`,
`android:dataExtractionRules="@xml/data_extraction_rules"`.

```xml
<!-- res/xml/backup_rules.xml (API ≤ 30) -->
<full-backup-content>
    <exclude domain="file" path="offline/" />
</full-backup-content>

<!-- res/xml/data_extraction_rules.xml (API ≥ 31) -->
<data-extraction-rules>
    <cloud-backup><exclude domain="file" path="offline/" /></cloud-backup>
    <device-transfer><exclude domain="file" path="offline/" /></device-transfer>
</data-extraction-rules>
```

Both exclude only `offline/`. The DataStore is backed up to Google and
transferred device-to-device (F-android-7: "excluding only cloud backup
leaves device-to-device transfer policy unresolved").

### 2.3 The one OkHttp client

`Net.kt:23-38`: `Net.client` with 20 s connect / 60 s read and the bearer
interceptor; `Net.api(origin)` builds Retrofit on it; `Net.dataSourceFactory()`
hands it to Media3; `PlurxApp.newImageLoader()` (`PlurxApp.kt:22-26`) hands
it to Coil. One `Dispatcher` — OkHttp default `maxRequests = 64`,
`maxRequestsPerHost = 5` — applies to **asynchronous** calls
(`Call.enqueue`): Retrofit suspend functions and Coil both enqueue; Media3's
`OkHttpDataSource` uses `execute()` and is not counted (F-android-8). The
server's image route admits eight local reads in flight and 503s above
(review C6, `images.rs:255-263`).

### 2.4 The build that reaches devices

`Makefile:1739-1751`: `android` → `:app:assembleDebug`; `android-publish`
copies `app-debug.apk` to the server's data dir for `/download/plurx-android.apk`.
`build.gradle.kts:60-69`: `release { isMinifyEnabled = true;
isShrinkResources = true; proguardFiles(...) }` and **no `signingConfigs`**
(PUBLISHING.md §5.1 names this as the remaining blocker). The mobile
release role (`~/code/plurx-agent/ansible/media/mobile-physical.yml`, per
CLIENT-DEPLOY-PROMPT.md §2) installs whatever the build step produces; which
variant it builds is in that repository, not this one — verify before M10.
A debuggable APK lets `adb shell run-as tv.plurx.app cat files/datastore/plurx.preferences_pb`
read the bearer from any machine the device trusts.

### 2.5 Existing narrow-authority precedent

Offline packages: `PUT /offline/packages/{id}/lease {token: 64 hex}` stores
`sha256(token)`; media is then read at `/offline/media/{token}/…` with the
token in the path and no bearer (`http/offline.rs:515-541`,
[API.md](../API.md) §"offline"). Live TV uses `ltv1.…` capabilities in the
path. Neither is reusable as-is (both are session-shaped), but the pattern —
client-generated random, server stores the hash, path-carried, capability
auth, bounded expiry — is the one to copy.

---

## 3. Change

### 3.1 The file grant

```
 client                          server
  │ POST /api/v1/files/{id}/grants   (bearer)
  │   {"purpose":"open_in","ttl_secs":900}
  │◀── 201 {"url":"/api/v1/grants/<64hex>/content",
  │         "expires_at":<unix>,"grant_id":"<uuid>"}
  │ ACTION_VIEW(origin + url) ──▶ external app
  │                                GET  …/content        Range ok, 206
  │                                HEAD …/content
  │ DELETE /api/v1/grants/{grant_id}  (bearer)  ── revoke early
```

Server:

- Table `file_grants(id TEXT PK, token_hash TEXT UNIQUE, file_id INTEGER
  REFERENCES files, user_id INTEGER REFERENCES users, purpose TEXT,
  created_at INTEGER, expires_at INTEGER, revoked_at INTEGER NULL)` STRICT,
  append-only migration, both backends (S10: add the new files to
  `cluster_auth` scope in `validation/points.toml`). Replicated: a grant
  minted on one voter must resolve on the ingress the reader app hits.
- Token: server-generated 32 random bytes → 64 lowercase hex; the client
  never chooses it (unlike the offline lease, the client here is about to
  hand the value away, so it should not be able to pick a guessable one).
  Stored hashed; the plaintext exists only in the 201 body.
- `GET|HEAD /api/v1/grants/{token}/content`: capability auth (no bearer, no
  `?token=`), looks up `sha256(token)`, refuses expired/revoked with
  `410 grant_gone`, unknown with `404`, then serves exactly what
  `book_content` serves — same `serve_file_range`, same `Content-Type`,
  `Content-Disposition: inline; filename=…`, `Accept-Ranges: bytes`. Ranges
  and repeated opens work for the whole TTL, which is the reader-app need
  the assessment named (range/reconnect). Each read touches nothing
  durable; expiry is the only clock.
- TTL: default 900 s, max 3 600 s. Fifteen minutes covers "open, pick an
  app, wait for it to download a 200 MB PDF"; an hour is the ceiling
  because the URL is now in someone else's process.
- Revocation: `DELETE /grants/{id}` by the owner; logout revokes all of the
  user's live grants in the same two-phase revocation the bearer uses
  (`internal_auth_revocation.rs`) — a grant that outlives its account's
  session is exactly the authority leak this closes. Nightly job prunes
  rows past `expires_at + 1 day`.
- Scope: `purpose = "open_in"` only serves book kinds today (`item.isBook`),
  mirroring `book_content`'s own check; a video grant is a future purpose
  with its own range/HLS design, not this route relaxed.
- API.md gains the three routes and their auth column in the same PR.

Clients: `DetailScreen.kt:486-491` and `DetailView.swift:2408-2412` call the
mint endpoint and open `origin + url`; on a mint failure they show the
existing error text and do **not** fall back to `mediaUrl`. `Session.mediaUrl`
stays for now (the web client's `<img src>` path still uses `?token=` —
W6/C8 territory) but gains a doc comment naming the grant as the only
sanctioned way to hand a URL to another process.

### 3.2 Backup exclusions, both files, both transfer kinds

```xml
<!-- backup_rules.xml -->
<full-backup-content>
    <exclude domain="file" path="offline/" />
    <exclude domain="file" path="datastore/" />
</full-backup-content>

<!-- data_extraction_rules.xml -->
<data-extraction-rules>
    <cloud-backup>
        <exclude domain="file" path="offline/" />
        <exclude domain="file" path="datastore/" />
    </cloud-backup>
    <device-transfer>
        <exclude domain="file" path="offline/" />
        <exclude domain="file" path="datastore/" />
    </device-transfer>
</data-extraction-rules>
```

`domain="file"` with the directory path is the valid form; a package-name
rule is not a file exclusion (F-android-7). The whole `datastore/`
directory goes, not one preferences file: the origin and username are
pairing state that must not be restored onto a device whose token was not
(`SettingsStore.kt:95-110` documents the pairing invariant), and a restored
half-pair would send a fresh device to the login screen with a stale server
already filled in — acceptable, but restoring the token would be a
credential copied by Google. Consequence, stated in the release note: a
restored phone signs in again.

### 3.3 Optional Keystore wrap — key lifecycle first

Only if Paul wants it after M2; it is defence in depth for a rooted device
and for the `run-as` case M9 removes, not a substitute for either.

- Key: `AndroidKeyStore` AES-256-GCM, `setUserAuthenticationRequired(false)`
  (the token is used headless by the download service), alias
  `plurx.session.v1`, `setInvalidatedByBiometricEnrollment(false)`.
- Stored value: `enc:v1:<iv>:<ciphertext>` in the same `TOKEN` preference;
  plaintext values (legacy) are read once, wrapped on the next
  `saveSession`, never rewritten in place on read (a read-time migration
  that writes is how a race loses a login).
- Lifecycle: **logout** deletes the preference and leaves the key (key
  reuse is fine; the ciphertext is gone); **uninstall** destroys both;
  **key invalidation** (`KeyPermanentlyInvalidatedException`, StrongBox
  reset, OS restore onto other hardware) is treated as logged out — clear
  the preference, show login; **StrongBox** not requested (availability
  varies on TV boxes, and the fallback path is the same key type).
  Every branch has a JVM test with a fake keystore.
- Not covered: a device without hardware-backed keystore (older TV boxes)
  gets software-backed keys, which only obscure. The doc comment says so.

### 3.4 OkHttp dispatcher — trace, then decide

Trace first (M5): `OkHttpClient.eventListenerFactory` logging
`callStart → connectionAcquired → responseHeadersStart` per call with the
URL's route class (`images` / `decision` / `other`), on the Lenovo, opening
Home cold and then pressing Play on a title within 3 s. The question is
whether `/files/{id}/decision`'s `callStart → connectionAcquired` gap
exceeds 100 ms while ≥5 image calls to the same host are in flight. If it
does not, stop: record the trace, no change.

If it does (M6): a **second `OkHttpClient` for Coil** sharing
`Net.client`'s `connectionPool` (so connections are reused) and the bearer
interceptor, with its own `Dispatcher(maxRequests = 16, maxRequestsPerHost
= 4)`. API calls keep `Net.client`'s dispatcher to themselves. Rules:

- Coil's per-host limit stays **at or below** the server's eight-permit
  local-read gate minus the other clients' expected share — four here.
  Raising image concurrency above eight just moves the queue to the server
  and adds 503s (F-android-8); the fix is separation, not more parallelism.
- No `H2_PRIOR_KNOWLEDGE` / h2c: it does not bypass the dispatcher's limit
  and assumes cleartext HTTP/2 the server does not speak.
- Media3 is unaffected (synchronous calls) and keeps `Net.client`.

### 3.5 Release signing and deployment

- Signing material: an upload keystore generated per PUBLISHING.md §5.1,
  stored in the fleet vault (the same store that holds the Forgejo registry
  token, per OPERATIONS.md: streamed through SSH, never in the repo, never
  in an argument). Gradle reads `PLURX_ANDROID_KEYSTORE`,
  `PLURX_ANDROID_KEYSTORE_PASSWORD`, `PLURX_ANDROID_KEY_ALIAS`,
  `PLURX_ANDROID_KEY_PASSWORD` from the environment; absent → the release
  build **fails** with a message naming them, never silently falls back to
  the debug key. Two reasons: a debug-signed "release" is the artefact Play
  refuses, and once one is installed a properly signed build cannot upgrade
  it in place — every device would need an uninstall. (`run-as` itself
  follows the `debuggable` flag, which `release` already clears; the signer
  is a separate property.)
- Makefile: `android-release` target (`:app:assembleRelease`, env passed
  into the container with `-e`), `android-publish` switched to it, `android`
  kept for local debug builds and renamed in its help text to say so.
  `tests/operations/test_contracts.py` gets a test that `android-publish`
  no longer references `app-debug.apk` — a text contract, but for a shipping
  path, which is the kind §4.4 keeps.
- Mobile release role: switch its Gradle task to `assembleRelease` and its
  install path to `app-release.apk`; because the signing key changes, the
  first release install needs `adb uninstall` first on every device — the
  role's `mobile_release_allow_downgrade`-style refusal semantics apply, and
  the CLIENT-DEPLOY-PROMPT gets a dated note.
- Diagnostics retained: R8 mapping file (`mapping.txt`) uploaded as a build
  artefact keyed by `versionCode`, so a release stack trace can be
  de-obfuscated (F-android-12: "retain diagnostics").
- `capabilityProbe` build type stays debug-derived; it is a diagnostic
  variant, not shipped.

### 3.6 Baseline Profile on the deployed variant

After M9 and only then: `androidx.profileinstaller` dependency, a
`baselineprofile` module with one Macrobenchmark journey (cold start →
Home → open a title → press Play → first frame), generated on the Lenovo
(the slowest fleet TV), committed as `app/src/main/baseline-prof.txt`.
Measure startup (`StartupTimingMetric`) and first-frame with and without the
profile **on the release APK**, five iterations each, and record the medians
in §6 before crediting anything; a profile on the debug variant measures
nothing that ships.

---

## 4. Guardrails (non-goals)

- **Do not narrow `?token=` globally** or change `token_from_parts`. That
  is C8 and needs the web client and TV downloader paths migrated first.
- **Do not reuse the offline lease or Live TV capability types** for the
  grant. They are session-shaped with sliding expiry; the grant is
  file-shaped with a fixed expiry and owner revocation.
- **Do not let the grant serve video** under `purpose = "open_in"`; a video
  grant needs range, HLS and reconnect semantics designed for a player and
  is a different purpose value.
- **Do not fall back to `mediaUrl` when minting fails.** A failed mint shows
  an error; the bearer never leaves the process again.
- **Do not exclude only `plurx.preferences_pb`** or only cloud backup. The
  directory, both files, both transfer kinds (§3.2).
- **Do not ship the Keystore wrap without the lifecycle table in §3.3
  implemented and tested.** Encrypted-at-rest without logout/invalidation
  handling locks users out (F-android-7).
- **Do not raise image concurrency above four per host** and do not add
  h2c. Separation is the change; parallelism is the server's gate.
- **Do not fall back to the debug signing key** in a release build, and do
  not keep deploying `assembleDebug` "until the key is sorted".
- **Do not measure the Baseline Profile on the debug variant** or credit an
  unmeasured startup gain.
- **No in-code feature gates.** The grant is the only path once M3 lands;
  the dispatcher split is a decision recorded from a trace, not a toggle.

---

## 5. Milestones

Draft PRs into `main` under the fast lane; Kotlin/Swift changes bump the
mobile counters (`validation/mobile_versions.py`).

### 5.1 M1 — server: `file_grants`, mint, serve, revoke, prune

Migration on both backends, the three routes, logout revocation hook,
nightly prune, API.md rows, `points.toml` scope for the new store files.

Acceptance: `cargo test -p plurxd grants` covers mint → 206 range read →
HEAD → expiry 410 → revoke 410 → unknown 404 → logout revokes; `make unit`
green; `python3 -m validation.ci_scope --event pull_request --base
origin/main` lists the new store files under `cluster_auth`.

### 5.2 M2 — both backup rule files

The two XML edits, a `tests/operations` check that both files list
`datastore/` under every section (the manifest is the contract).

Acceptance: `make android-test`; on the Pixel, `adb shell bmgr backupnow
tv.plurx.app` then `adb shell bmgr list sets` / `dumpsys backup` shows the
package's backup with no `datastore` entry; a factory-reset restore (or
`bmgr restore`) lands on the login screen.

### 5.3 M3 — clients use the grant

Android `DetailScreen`, Apple `DetailView`; JVM test that the intent URL
contains `/grants/` and never `token=`; Apple unit test the same on the URL
builder.

Acceptance: `make android-test`; `make apple-test` (or the fast lane's
Apple job); on the Pixel and iPhone, "Open in…" on a PDF opens in a reader
that can page through it (ranges) and re-open it from the reader's recents
within 15 minutes; after 15 minutes the reader gets 410; the server's
Activity page shows no viewer for the open.

### 5.4 M4 — Keystore wrap (optional, Paul's call after M2)

Per §3.3 with the lifecycle tests.

Acceptance: `make android-test` includes `SessionKeystoreTest` covering
wrap, unwrap, legacy read-then-wrap-on-save, invalidated key → logged out;
on the Pixel, sign in, reboot, still signed in; `adb shell run-as` is
refused on the release build (M9), and on a debug build the preference file
shows `enc:v1:`.

### 5.5 M5 — dispatcher trace

The event listener behind a debug-only log tag, the Lenovo capture, the
result pasted into §6.

Acceptance: a table of `decision` call gaps with concurrent image count,
from three cold-start runs, attached to the PR; a written decision line:
"split" or "no change".

### 5.6 M6 — separate Coil client (only if M5 says split)

Per §3.4. JVM test asserts Coil's client is not `Net.client` and shares its
pool.

Acceptance: `make android-test`; M5's trace re-run shows the `decision` gap
under 100 ms with the same image load; the server's image 503 count
(`images.rs` gate) over the same run does not increase.

### 5.7 M7 — release signing config

Gradle `signingConfigs.release` from the environment, failing loudly when
unset; keystore in the vault; `docs/PUBLISHING.md` §5.1 updated to say the
blocker is closed and where the key lives (not the key).

Acceptance: `make android-release` fails with the four variable names when
they are unset; with them set (on Paul's Mac, per CLIENT-DEPLOY-PROMPT), it
produces `app-release.apk` and `apksigner verify --print-certs` shows the
upload certificate.

### 5.8 M8 — Makefile and role switch to release

`android-release`, `android-publish` on it, the operations test, the role
change in `plurx-agent`, the deploy-prompt note about the one-time
uninstall.

Acceptance: `make operations-check`; `make android-publish
ANDROID_DATA_DIR=…` serves a release APK; `adb shell dumpsys package
tv.plurx.app | grep -E 'flags|signatures'` on a deployed device shows no
`DEBUGGABLE` and the upload signer.

### 5.9 M9 — every fleet device on release

Run the mobile release role; confirm per device.

Acceptance: for each Android device in the role's inventory, `adb shell
run-as tv.plurx.app id` returns "not debuggable"; `adb shell dumpsys package
tv.plurx.app | grep versionCode` matches the built counter.

### 5.10 M10 — Baseline Profile, measured

Per §3.6.

Acceptance: `baseline-prof.txt` committed; the five-run medians for cold
start and first frame with/without the profile on the release APK on the
Lenovo recorded in §6; `make android-test` still green.

---

## 6. Verification and rollout

Fast lane: `make unit` (M1), `make android-test` (every Android PR), the
Apple compile job (M3). Devices: the GPT prompt below covers what only
hardware proves.

```text
Pixel and iPhone, Lenovo TV; the lab server on the M1+ build.
1. Open in…: pick a PDF ≥ 50 MB, use a third-party reader, page to the
   end (ranges), close and re-open from the reader's recents; note the URL
   the chooser shows contains /grants/ and no token=. Wait 16 minutes,
   re-open: expect the reader to report an error (410).
2. Backup: `adb shell bmgr backupnow tv.plurx.app`; `adb shell dumpsys
   backup | grep -A5 tv.plurx.app`; report whether datastore appears.
3. Release build: after the role run, on every Android device `adb shell
   run-as tv.plurx.app id` and `adb shell dumpsys package tv.plurx.app |
   grep -E 'flags=|versionCode'`; report both lines per device.
4. Dispatcher trace (M5): with `adb logcat -s PlurxNet`, cold-start the app
   on the Lenovo, press Play on the first Home tile within 3 s; save the log.
5. Baseline Profile (M10): run the macrobenchmark on the Lenovo release APK
   five times with and without the profile; report the medians.
Report refusals as refusals.
```

Rollout: M1 is server-side and backwards compatible (old clients keep using
`mediaUrl` until their M3 build lands — the window is the client deploy
lag, and it is why M1 ships first). M2 and M3 ship on the next mobile
release. M7–M9 are one deploy with the one-time uninstall; Paul does it from
his Mac. M10 follows.

---

## 7. Open questions

1. **Grant TTL for televisions.** A TV reader app is rare; 900 s is a phone
   number. Leave it unless a device shows a need.
2. **Web "Open in…".** The web client has no such button; if one is added it
   uses the grant, and the web's `<img src>?token=` remains W6/C8's problem.
3. **Keystore wrap — do it at all?** It protects against a rooted or
   backed-up-by-other-means device once M2 and M9 have closed the two
   concrete exposures. Paul decides after M2; the lifecycle cost in §3.3 is
   the price.
4. **Where the mobile release role builds.** CLIENT-DEPLOY-PROMPT says the
   playbooks live in `~/code/plurx-agent/ansible/`; the exact Gradle task
   line to change is in that repository and was not read for this plan.
5. **Play App Signing.** PUBLISHING.md recommends enrolling; sideload-only
   deployment does not need it, but the upload key generated in M7 should
   be the one enrolled later so the fleet and Play share a lineage.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M2 | (this branch) | Both rule files exclude `datastore/`; `data_extraction_rules.xml` does so under **both** `cloud-backup` and `device-transfer`. Pinned by `tests/operations/test_android_credential_exposure.py::AndroidBackupExclusionCase` (5 cases), which also asserts the manifest still references both files and that the bearer still lives under a `preferencesDataStore` — so moving the token out from under `datastore/` fails here instead of leaving the exclusion pointing at nothing. Revert proofs: dropping `datastore/` from `backup_rules.xml` fails 2 cases; dropping it from `<device-transfer>` **only** (the F-android-7 mistake) fails 2; deleting the whole `<device-transfer>` section fails 2. Device acceptance (`adb shell bmgr backupnow`, restore-to-login-screen) is **not** done: no device in this session. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M7 | (this branch) | `signingConfigs.release` added to `clients/android/app/build.gradle.kts`, reading the four `PLURX_ANDROID_*` values through `requiredSigningValue`, which fails naming the missing one. The `release` build type selects it; there is no debug fallback. Populating is guarded by `releaseTaskRequested` because `signingConfigs { }` is evaluated on every invocation and debug builds must keep working without a key; an operations case pins the premise that every Gradle entry point names its tasks. `docs/PUBLISHING.md` §5.1 rewritten: the structural blocker is closed, generating the key and vaulting it is what remains. **Uncompiled** — no Gradle here; `make android-release` has never run. Revert proofs: removing the `signingConfig` assignment fails 1 case; pointing it at the debug config fails 2; making `requiredSigningValue` return `""` instead of failing fails 2. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 (repo half) | (this branch) | `make android-release` added (mounts the vault keystore read-only, forwards the three secrets, runs `:app:assembleRelease`); `android-publish` depends on it and copies `apk/release/app-release.apk`; `make android` renamed in help text to say it is a debug build that does not ship. Pinned by `AndroidShippingVariantCase`; revert proof: restoring the `app-debug.apk` copy fails 2 cases. **The `plurx-agent` role half is NOT done** — it is a different repository, unreachable from this session; CLIENT-DEPLOY-PROMPT.md §2 carries a dated note naming the exact change and the one-time `adb uninstall`. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M1, M3 | — | **Not started.** M1 is a new replicated table across both store backends plus three routes, a logout revocation hook and a prune job; M3 is the Android and Apple call sites, neither of which can be compiled in this session. Until both land the bearer still leaves the app in the "Open in…" intent. Nothing in this branch touches `Session.mediaUrl` or `DetailScreen.kt`, because changing the call site without a mint endpoint would break the button rather than secure it. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M4, M5, M6, M9, M10 | — | **Not done, by the plan's own terms.** M4 is explicitly Paul's call after M2. M5 requires a Lenovo trace before any dispatcher change is justified, and §3.4 says to stop and record "no change" if the gap is under 100 ms; inventing a split without the trace would be the guess the plan forbids. M6 is conditional on M5. M9 and M10 need the fleet and a Macrobenchmark run on a release APK. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 (repo half), review of PR #469 | #469 | The review found the M8 row above overstated: `scripts/ship-physical`, the documented no-Ansible fallback, still built and installed `app-debug.apk`, so devices installed through it stayed `debuggable`, and its signer-mismatch text told the operator to uninstall and rerun — straight back onto a debug build. It now builds `:app:assembleRelease` and installs `app-release.apk`, requires the four `PLURX_ANDROID_*` inputs before any work (relative keystore resolved to absolute), refuses a `CN=Android Debug` signer, does not skip a `DEBUGGABLE` install at an equal versionCode, and names the one-time uninstall and its cost (sign-in and offline downloads) instead of blaming a changed debug keystore. The Gradle entry-point case now scans `scripts/` as well as the `Makefile`. `make android-release` resolves a relative `PLURX_ANDROID_KEYSTORE` before `docker run -v`, which otherwise mounted an empty named volume as a directory. The §3.5 "Diagnostics retained" bullet had not been carried out: `android-publish` now keeps `mapping.txt` as `plurx-android-<versionCode>.mapping.txt` in `ANDROID_DATA_DIR` (versionCode from `output-metadata.json`), written before the APK is replaced, never overwritten (a reused versionCode moves the earlier one aside), and refuses to publish without one. Pinned by `ShipPhysicalReleaseVariantCase` (6), `AndroidReleaseMakeTargetCase` (4, running the real recipes against a `docker` stub) and `test_contracts.py::test_ship_physical_is_a_self_contained_device_path`; each production hunk was reverted and its tests seen to fail. Still outstanding: the `plurx-agent` role switch, and device evidence. |
