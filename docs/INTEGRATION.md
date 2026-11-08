# Integration — plurx's seams with Curator, and how to prove they work

The scan seam (§1–§2) is written from
[PIPELINE-CONTRACT.md](features/PIPELINE-CONTRACT.md) §4–§6, pipeline contract
v2, with every status, body, code, header and limit read from
`crates/plurxd/src/http/scan.rs`, `crates/plurx-core/src/store/scan_request_v2.rs`
and `crates/plurx-core/src/scan/containment.rs`.

Companion to [INTEGRATION-PLAN.md](features/INTEGRATION-PLAN.md) (the master plan and
why each seam exists) and [OPERATIONS.md](OPERATIONS.md) (running it day to
day) — this is *what each seam does, where you look at it, and the exact
command that proves it*. The trust model behind the scoped-key wall is in
[SECURITY.md](SECURITY.md).

plurx is the **last stage** of a three-application pipeline. It plays what the
other two put on disk. Its only integration partner is Curator: plurx and Runner
have no seam at all, by design — every path between them runs through Curator,
after the import, when the files are actually in place.

## The pipeline

```
   ┌──────────────┐   grab   ┌──────────────┐
   │    Curator    │ ───────▶ │     Runner     │      Runner and plurx never
   │  (decides)   │ ◀─────── │ (downloads)  │      speak. There is no seam
   └──┬────────▲──┘   SSE    └──────────────┘      between them, on purpose.
      │        │
      │        │  POST /api/v1/webhooks/plurx   (watch state, opt-in)
      │        │  GET  /api/v1/calendar         (the Coming soon rail)
      │        │  GET  /api/v1/system/status    (Test connection)
      │        │
      │ POST /api/v1/scan              (outbox, one row per directory)
      │ GET  /api/v1/scan/requests/{id} (reads back what it asked for)
      ▼        │
   ┌───────────┴──┐
   │    plurx     │
   │  (plays it)  │
   └──────────────┘
```

Default ports: Runner 6789 · Curator 7676 · **plurx 32400**.

## Wiring the two together in Docker

Do this first. Most "cannot reach Curator" is a container that cannot resolve a
name, not a wrong key or a wrong port.

**One shared user-defined network, and address everything by container name.**
That is the recommended setup, not merely one that works.

```bash
docker network create media          # once, on the host
```

Then in **each** compose file — plurx's, Curator's, Runner's — attach the service
and declare the network as external:

```yaml
services:
  plurx:
    container_name: plurx            # this name IS the hostname
    # ...the rest of your service...
    networks:
      - media

networks:
  media:
    external: true                   # created above; compose must not own it
```

`docker compose up -d` in each directory, then set the Curator URL to
`http://curator:7676`.

**Get the names from Docker, not from memory.** This is the single command
that answers "what do I put in the URL", and it answers the network question
in the same line:

```bash
docker ps --format '{{.Names}}\t{{.Networks}}'
```

```
plurxd	media
Runner	media
Curator	media
```

Column 1 is the hostname to use. Column 2 must contain the shared network for
every container that has to talk to the others — anything listed as `bridge`
or `host` cannot be reached by name from the others.

Read it literally. A container called `plurxd` is **not** reachable as
`plurx`, and the failure looks like a network problem rather than a spelling
one: Curator reports
`lookup plurx on 127.0.0.11:53: server misbehaving` — Docker's embedded
resolver saying the name does not exist, which is easy to misread as "the
other side is down". It is not; nothing was ever dialled.

Three more things that trip people up, each with its reason:

**The hostname is the `container_name`, or the service key if you did not set
one.** Not the image, not the directory, and not the product's name.

**Published ports are irrelevant on this path.** `ports: - "32400:32400"` maps
host→container; container→container traffic goes straight to the *internal*
port and works even with no published port at all. Keep the published ones for
your own browser, and stop reasoning about them when debugging a seam.

**`host.docker.internal` is a per-container setting, and it is the wrong tool
here.** It only exists in a container whose compose declares
`extra_hosts: ["host.docker.internal:host-gateway"]`. plurx is the process
making this call, so that line has to be on *plurx's* service — putting it on
Curator's does nothing, which is exactly the trap: Curator→Runner works, plurx→
Curator does not, and the two look symmetric from the settings screen. It also
routes container→host→container for traffic that never needed to leave the
bridge. A valid fallback when you cannot edit every compose file; not the good
answer.

**How to tell which one you are hitting.** Test connection reports the root
cause, not the wrapper: `dns error: failed to lookup address information`
means the name is not resolvable from this container — wrong network, or a
missing `extra_hosts`. `Connection refused` means the name resolved and
nothing was listening on that port.

## The seams plurx has

| # | Seam | Direction | Transport | Who starts it |
|---|---|---|---|---|
| 1 | Targeted scan | **inbound** | `POST /api/v1/scan`, `GET /api/v1/scan/requests/{id}` | Curator, from its outbox after each import |
| 2 | Scoped API keys | the wall around §1 | `/api/v1/keys` | you, once |
| 3 | Coming soon rail | outbound | `GET {Curator}/api/v1/calendar` | plurx, every 15 min |
| 4 | Test connection | outbound | `GET {Curator}/api/v1/system/status` | you |
| 5 | Watched outbox | outbound, durable | `POST {Curator}/api/v1/webhooks/plurx` | plurx, on watch |

---

## 1. Targeted scan — "this exact folder changed"

**What it does.** When Curator finishes importing a publication, it tells
plurx which directories changed and which files it placed in each. plurx
indexes exactly those files, ahead of any pending full scan, and reports back
per file. Curator calls the import **visible** only when plurx's report agrees
with what Curator wrote ([contract](features/PIPELINE-CONTRACT.md) §4.2). One
request per directory: a season pack that lands ten episodes in one folder is
one request, naming ten files.

The scan **enriches what it placed** before answering. It runs the same
provider pass a full scan does, narrowed to the items this request produced
and the seasons, shows and folders their artwork comes through. It is narrowed
because Curator may be waiting on the answer, and a per-episode import must
not become a per-episode full-library metadata pass. A poster that still does
not arrive is picked up by the artwork retry sweep (Settings → **Scheduled
maintenance**), so a TMDB rate limit during an import costs a delay, not a
permanently blank card.

### 1.1 What Curator sends

Curator does not post from memory. When a publication completes, it writes one
**outbox row per directory**, in the same transaction, with the stable
idempotency key `<transfer>/<generation>/<dir-hash>` and the media paths the
import placed there. A dispatcher drains each row on its own schedule:
`5 s → 30 s → 2 m → 5 m → 15 m → 1 h`, then hourly, with no end for transient
failures. Curator gives up permanently only on `401`/`403` (the key), `400`
(the body) and `422 path_outside_roots` (contract §4.1).

**Wire.** `POST /api/v1/scan` with a key of scope `scan:trigger`, as
`Authorization: Bearer plx_…` or `X-Api-Key: plx_…`. plurx accepts the second
header because every *arr application already uses it. The `plx_` prefix is
what separates a key from a login token, whichever header carries it.

```json
// an episode: the SHOW's id, under `series`
{"path":"/media/tv/Show/Season 02","hint":"episode","series":{"tmdb":1399},
 "correlation_id":"t-42-a3f9c1","source":"monarr",
 "request_key":"t-42-a3f9c1/1/9c2e/1","wait_ms":10000,
 "expected_files":[{"path":"/media/tv/Show/Season 02/Show.S02E01.mkv",
                    "bytes":8123456789,"mtime_ns":1794076138123456789}]}

// a movie: its own ids, under `ids`
{"path":"/media/movies/Some Film (1999)","hint":"movie",
 "ids":{"tmdb":603,"imdb":"tt0133093"},
 "correlation_id":"t-43-b1e207","source":"monarr"}

// a book: the path identifies local bytes; Curator supplies exact relations
{"path":"/media/books/Andy Weir/Project Hail Mary","hint":"book",
 "book":{"title":"Project Hail Mary","author":"Andy Weir","medium":"ebook",
         "work_id":"curator:openlibrary:OL20631799W",
         "edition_id":"curator:item:44:ebook",
         "cover_url":"https://covers.openlibrary.org/b/olid/OL12345M-L.jpg"},
 "correlation_id":"t-44-c4d812","source":"monarr"}
```

| Field | Required | Means |
|---|---|---|
| `path` | yes, unless `probe_paths` | Absolute directory or file under a configured library root |
| `request_key` | no | Curator's idempotency key, attempt number last. The same key from the same API key returns the same `request_id` and result, so a lost response never becomes a second scan. A new attempt suffix is a new request |
| `expected_files` | no | `[{"path","bytes","mtime_ns"}]`, each path absolute. Every listed file is probed whatever plurx's size+mtime cache says, and reported in `files` |
| `wait_ms` | no | Hold the response up to this long for the result. Default `0` (answer at once) |
| `hint` | no | `movie` · `episode` · `season` · `book`. Advisory: the library's kind decides how a file is parsed; the hint only picks which item an id applies to |
| `ids` | no | `{"tmdb","imdb"}` for the item itself |
| `series` | no | `{"tmdb"}` — the **show's** id for an episode; must be positive |
| `book` | no | Books libraries only: `medium` (`ebook`·`audiobook`), `work_id`, `edition_id`, optional `title`, `author`, `cover_url` (HTTPS Open Library cover) |
| `correlation_id` | no | Curator's transfer token (`t-<downloadID>-<hex>`), echoed back and logged, so one grep across three applications reconstructs a transfer |
| `source` | no | Who asked, for the log |
| `probe_paths` | no | Connection test (§1.6); takes no `path` |

Unknown fields are ignored, so an older plurx simply does not see
`request_key`, `wait_ms`, `expected_files` or `probe_paths` (§1.8).

An episode's own id does not identify its series, and plurx never falls back
from item-level `ids.tmdb` to the show. The series id is applied after
placement with an absent-or-equal guard: the first hint wins, repeats are
idempotent, and a different known id is kept. That disagreement does not fail
the import or remove its playable file; the result carries a bounded problem
naming the show and both ids, and the correlated log records it. A `book`
object is accepted only under a Books root; plurx never invents an edition
relation from title plus author.

**Limits** — each read from the code; `tests/operations/test_integration_doc_literals.py`
fails if one drifts:

| Limit | Value | Code |
|---|---|---|
| `wait_ms` cap | 15000 ms | `MAX_WAIT_MS` |
| `Retry-After` / `retry_after_s` on `path_unreadable` | 60 s | `PATH_UNREADABLE_RETRY_S` |
| `probe_paths` entries | 1 to 64 | `MAX_PROBE_PATHS` |
| `request_key` length | 256 bytes, no control characters | `MAX_REQUEST_KEY_BYTES` |
| `expected_files` entries | 64, together at most 16 KiB of JSON; each path at most 4096 bytes, `bytes` and `mtime_ns` non-negative | `MAX_EXPECTED_FILES` |
| `waiting_on` length | 1024 bytes | `MAX_WAITING_ON_BYTES` |
| pending requests per library | 256 | `ENQUEUE_SQL` (the library `queue_full` bound) |

### 1.2 The answer

| Status | Body | Means |
|---|---|---|
| `200` | `{"status":"scanned","library_id","request_id","report","items","files","correlation_id"}` | Finished — within `wait_ms`, before the handler re-read it, or a replay of a finished `request_key` |
| `202` | `{"status":"queued","request_id","correlation_id"}` | Durably queued, not dropped; poll it (§1.4). `correlation_id` is omitted when there is none |
| `400` | `{"code":"path_required","message"}` | Neither `path` nor `probe_paths` |
| `400` | `{"code":"probe_paths_with_path","message"}` | Both — a connection test and a scan are different requests |
| `400` | `{"error"}` | A relative `path` or `expected_files[].path`, a non-positive `series.tmdb`, invalid `book` fields, a `request_key` or `expected_files` over the limits above, or `probe_paths` outside 1–64 |
| `401` | `{"error":"authentication required"}` | No key, a revoked or unknown key, or a login token on this key-only route |
| `403` | `{"error":"admin privileges required"}` | A real key without `scan:trigger` (the text is the shared forbidden message, not a statement about admins) |
| `422` | `{"code":"path_outside_roots","error":"path is not under any library root","path","roots":[…]}` | Under no library root — **permanent**. `roots` is there because "wrong path" is only actionable if you can see the right ones |
| `422` | plain text, no `code` | The JSON extractor refused the body: a missing required field or a field of the wrong type. Curator tells this from `path_outside_roots` by the body. Malformed JSON is a plain-text `400`, a missing JSON `Content-Type` a `415` |
| `500` | `{"code":"scan_failed","message","status":"failed","request_id","error","correlation_id"}` | The request, or the finished request a replayed `request_key` names, failed for a reason other than an unreadable root |
| `503` | `{"code":"path_unreadable","error":"library root could not be read: …","path","root","retry_after_s":60}` + `Retry-After: 60` | Under a root plurx cannot read right now — **transient**; the same request succeeds once the share is back. When a waited-for or replayed request failed because its root dropped mid-scan, the body also carries `request_id` and `"status":"failed"` and has no `root` |
| `503` | `{"error":"library work was not accepted: …"}` | The durable queue refused it: the library already has 256 pending requests, another queue bound is reached, or the same request is being cancelled. Nothing is lost; retry later |

`files` lists one entry per expected file, tagged by `state`:

```json
[{"state":"placed","path":"/media/tv/Show/Season 02/Show.S02E01.mkv",
  "item_id":812,"file_id":4410,"bytes":8123456789,"mtime_ns":1794076138123456789},
 {"state":"skipped","path":"/media/tv/Show/Season 02/sample.mkv","reason":"…"},
 {"state":"missing","path":"/media/tv/Show/Season 02/Show.S02E02.mkv"}]
```

`placed` carries the size and nanosecond mtime from plurx's own fresh `stat`,
not the caller's claim. `items` lists every file the scan placed,
`{"item_id","file_id","path"}`, expected or not; `report` is the scan's
counters (`added`, `updated`, `unchanged`, `skipped`, `unreadable`,
`degraded`, …).

**How Curator reads it** (contract §4.2). A directory is **visible** only when
every expected file is `placed` with matching `bytes` and `mtime_ns`. A
`scanned` result that indexed an old sibling and skipped or missed the new
file is a `warning` that names plurx's reason. Curator **reuses** the
`request_key` after an ambiguous outcome (a timeout, a connection error, no
response), so the replay returns the original result, and **advances** the
attempt after a confirmed retryable one (`scanned` without the file, `503
path_unreadable`, `failed`), so the retry is a fresh scan, not the cached skip.

### 1.3 Containment

Containment is decided **lexically first**: `.` and `..` are resolved and no
symlink is followed. The path is matched component-wise against every
configured root, and the longest root wins, so `/media/kids` beats `/media`
and `/data` never contains `/database`. Under no root is
`path_outside_roots`. Only then may the filesystem answer, and all it can say
is "readable" or "not readable now": a root that cannot be resolved or listed
(an unmounted share, `ENOTCONN`, `EIO`, `ESTALE`), or a path missing beneath a
root, is `path_unreadable`. An empty mount point lists fine, but the path
beneath it is missing, so it is `path_unreadable` too. The one filesystem
answer that is permanent is a symlink that resolves outside its root: that is
`path_outside_roots`. So an unmounted share is never mistaken for a wrong path.

### 1.4 Reading the request back

`GET /api/v1/scan/requests/{id}`. The key that created a request reads it with
`scan:trigger` alone; any other `scan:trigger` key gets `403` for it — and for
an id that does not exist, so a scan key cannot probe which requests exist. A
`status:read` key reads any request, including a library's own scans, which no
key created. Curator needs only `scan:trigger` (§2).

```json
{"request_id":"sr-…","at":1794076140,"library_id":2,
 "path":"/media/tv/Show/Season 02","correlation_id":"t-42-a3f9c1",
 "source":"monarr","status":"queued","waiting_on":"full_scan:TV","files":[]}
```

`status` is `queued` · `running` · `done` · `failed`. `report` and `items`
appear at `done` and `error` at `failed`, with `code: "path_unreadable"` when
the root dropped mid-scan; Curator treats that as retryable. Requests made
through `POST /scan` also carry `waiting_on` and `files`; their absence means a
request from before contract v2 (or a full scan's own request). `waiting_on`
is empty once the request's turn starts, and while it is queued is one of:

| `waiting_on` | Means |
|---|---|
| `full_scan:<library>` | A full scan or metadata refresh of the request's **own** library holds that library's fence |
| `root_unreadable:<path>` | The node that tried to claim it cannot read that root |
| `queue_position:<n>` | It joined a targeted pass already running, with `n` requests ahead |

The contract also names `heavy_lane:<holder>`; plurx never writes it, because
a targeted job takes neither the heavy lane nor a background source reader.
Request receipts and results persist for seven days. The recent list is
bounded to 256 records; an older record can still be read by id until its
receipt expires.

On `202`, Curator polls this record on the outbox schedule until `done` or
`failed`, for at most 30 minutes; then the sub-delivery is a `warning` showing
plurx's `waiting_on`, not a failure (contract §4.2).

### 1.5 The targeted job

A targeted request is its own job, dedupe key `library:targeted:{library_id}`,
separate from the library's full scan. It is claimed first among library work.
Its admission is a probe admission — `stat` and `ffprobe` on the named files,
no heavy-lane permit — so a busy GPU, a title scan or a playing viewer never
delays it. It takes **no background source reader**, so a full scan of another
library on the same storage does not hold it. Its one wait is the library's
mutation fence (`scan:library:{id}`), which a full scan also takes, so the two
never run at once and the full scan's known-minus-seen reconcile cannot prune
what the targeted scan just inserted. Beside a running full scan of its own
library, the request records `waiting_on: full_scan:<library>` and is retried
a quarter second later. The holder's release wakes the releasing node's library
loop, which claims it at once; any other node does within an idle poll. There
is no preemption. Fragment index, Dolby Vision and cache work for the new item
stay on the heavy lane as before. Settings → **Activity** lists a waiting
targeted job with its `waiting_on` and a Cancel.

The job keeps the `library_scan` kind with its own payload generation,
`library-targeted-v1` (whole-library work is `library-work-v1`). Older nodes
in a rolling upgrade decode it and leave it alone.

### 1.6 Connection test

`{"probe_paths":["/media/tv","/media/movies"]}` with no `path` answers
`200 {"roots","visible","missing","outside_roots"}`: which of Curator's library
roots plurx can see. Only a configured root, or a path under one, is ever
`stat`ed. A symlink beneath a root is found with `lstat`, listed under
`outside_roots` and never followed. Nothing is scanned or queued. It needs
`scan:trigger`, and it counts as a received notification, never as a scan.

Curator's notifier **Test** sends its configured library roots this way and
fails with the two paths side by side when plurx cannot see one. Against an
older plurx, which requires `path`, the JSON extractor answers `422` with no
`roots`; Curator tells that from the handler's `path_outside_roots` by the
body, falls back to its legacy probe and reports that path checking needs a
newer plurx (contract §4.3).

### 1.7 Reconcile

A library created now scans every six hours, so a notification that never
arrived is found by the next sweep. Existing libraries keep their schedule —
`Never` stays `Never` — and Settings → **Libraries** offers each one a
one-click "Enable 6-hourly reconcile" beside the notice: *"Never: a missed
Curator notification is found only by a manual scan, or at startup if 'Scan
every library at startup' is on (it is off by default)."*

### 1.8 Compatibility

| Curator | plurx | Works because |
|---|---|---|
| v2 | older | `wait_ms`, `request_key`, `expected_files` and `probe_paths` are ignored; `202` is handled; the status poll gets `403`, so the delivery shows `queued (cinema does not report results)`; the Test falls back to the legacy probe; the old single `422` stays permanent |
| older | v2 | Nothing changes: an old Curator sends none of the new fields and gets the same `200`/`202` it always did, now with the typed `422`/`503` bodies |

### 1.9 Where you see it, and how to prove it

Settings → **Libraries** shows the scan running: `scanning… N / M files` →
`fetching metadata…` → `X added, Y updated, Z unchanged`. It does not name the
trigger (see the honest gaps). The inbound request itself is visible in
Settings → **System → Logs** at `Info+`, target `plurxd::integrate`, and
Curator's delivery log shows `visible → plurx item <id>` or plurx's exact
`waiting_on` or skip reason.

```bash
PLURX=http://127.0.0.1:32400
PLXKEY=plx_…                        # from §2

curl -sS -X POST "$PLURX/api/v1/scan" \
  -H "Authorization: Bearer $PLXKEY" -H "Content-Type: application/json" \
  -d '{"path":"/media/movies/Some Film (1999)","source":"manual-test",
       "correlation_id":"t-0-verify","wait_ms":10000}'

curl -sS -H "Authorization: Bearer $PLXKEY" "$PLURX/api/v1/scan/requests/<request_id>"
```

**How to read it.** `422 path_outside_roots` proves the key, the scope and the
route all work — it is the wrong path and nothing else. `503 path_unreadable`
means the path maps to a root plurx cannot read now: check the mount on the
plurx side. `401` means the key is wrong or revoked; `403` means it lacks
`scan:trigger`. `202` with `"status":"queued"` means plurx took it (there is
no `accepted` status). Whether a file was *usable* is what `files` answers.

---

## 2. Scoped API keys — why Curator never holds an admin token

**What it does.** A key is what another application holds *instead of* a login
token. The difference is not cosmetic: a token **is** a user, so an admin
token handed to a neighbouring app also hands over every secret in
`GET /api/v1/settings`. A key carries a scope list and cannot widen itself.

**Scopes.** Exactly two exist. `scan:trigger` asks for scans, runs the
`probe_paths` test and reads back the requests this same key created — all
Curator needs. `status:read` reads **any** scan request, for monitoring or an
operator; Curator does not need it, so do not grant it to Curator's key.
Creating a key with an unknown scope is rejected rather than stored, because a
typo'd scope produces a key that looks correct in a list and authorizes nothing.

**Where you mint one.** There is **no UI for keys yet** — it is a curl.

```bash
# Log in for an admin token — there is no cookie to copy out of a browser.
TOKEN=$(curl -sS -X POST "$PLURX/api/v1/auth/login" \
  -H "Content-Type: application/json" \
  -d '{"username":"<admin>","password":"<password>"}' \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["token"])')

curl -sS -X POST "$PLURX/api/v1/keys" \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"monarr","scopes":["scan:trigger"]}'
```

The response carries `key_secret` — `plx_…` — **exactly once**. It is not
retrievable afterwards, which is the correct cost of never storing it. Lose it
and you issue a new key.

```bash
curl -sS -H "Authorization: Bearer $TOKEN" "$PLURX/api/v1/keys"   # shows last_used_at
curl -sS -X DELETE -H "Authorization: Bearer $TOKEN" "$PLURX/api/v1/keys/<id>"
```

**When the list comes back empty.** The keys live in plurx's database, so an
empty `/api/v1/keys` on a system that had a working key means the database is
gone — and users, libraries and watch state went with it. The shipped compose
bind-mounts `${PLURX_DATA:-/srv/plurx}` to `/var/lib/plurx` for exactly this
reason: a named volume lives at a path Docker chose, and pointing a container
at one that does not exist is not an error — Docker makes an empty one, plurxd
initialises a fresh database, and the first-run setup screen appears. Check
what is really mounted before minting a replacement:

```bash
docker inspect -f '{{range .Mounts}}{{.Type}} {{.Source}} -> {{.Destination}}{{"\n"}}{{end}}' <container>
```

Nothing mapping to `/var/lib/plurx` means the next rebuild will lose the new
key too. The other tell is the first-run setup screen reappearing.

**How to read it.** `last_used_at` on the listing is the cheapest proof that
Curator is really using the key you think it is. A key that has never been used
and a Curator that reports successful deliveries means Curator is holding a
*different* key. The timestamp refreshes no more than once per minute, so it
is an activity signal rather than an exact request counter.

Note what the listing does **not** return: `key_hash`. Listing keys is a
routine, frequently-open screen; the stored hash has no business on it, and
leaving it out means it cannot leak from there.

---

## 3. Coming soon rail — plurx reading Curator's calendar

**What it does.** The home screen shows episodes airing and films due in the
next four weeks, read from Curator's calendar. plurx proxies the call
server-side so the Curator key never reaches a browser.

**Wire.** `GET {monarr_url}/api/v1/calendar?start=&end=` with
`X-Api-Key: <Curator key>` and UA `plurx/<version>`. Cache TTL **15 minutes**,
horizon **28 days**.

**Where you configure it.** Settings → **Metadata** → the **Curator** card:
`Curator URL` (placeholder `http://curator:7676`), `Curator API key` (from
Curator's Settings → Security → Reveal), and the `Send watch state to Curator`
checkbox (§5).

**The URL is completed on save.** A bare host — `Curator`,
`host.docker.internal` — becomes `http://<host>:7676`, Curator's own default
port, and the field shows the completed form back so the change is visible
rather than magic. A scheme you supply is respected in full, port and all:
guessing `:7676` onto an `https://` URL behind a reverse proxy would break a
setup that was already correct.

See [Wiring the two together in Docker](#wiring-the-two-together-in-docker)
below — in a container setup, the URL is only half the problem.

**Where you see it.** Home → the **Coming soon** rail.

**Artwork is served by plurx, even before the title arrives.** Each entry
carries the item's external ids and provider poster path — the SHOW's, for an
episode. plurx first resolves the ids against its own library and uses that
cached poster when it exists. Otherwise it downloads Curator's provider path
from TMDB, TVmaze, or Open Library into the same artwork cache. The browser and
native apps therefore call only plurx; they never receive the provider URL.
Unknown hosts, non-HTTPS absolute URLs, redirects outside those three hosts,
non-image responses, and files larger than 15 MiB are rejected, because a peer
calendar must not become an SSRF or unbounded-download surface. Repeated
episodes share one cached file, and at most four distinct posters download at
once so a full four-week rail does not serialize its network waits.

Local resolution and provider-cache checks happen **after** the 15-minute
calendar cache read. A show that finished scanning two minutes ago therefore
switches to its library poster immediately, while a future film or a TVmaze
series outside plurx's TMDB id space still gets the art Curator already knows.
Only an entry whose provider named no poster falls back to initials.

**How to verify.**

```bash
curl -sS -H "Authorization: Bearer $TOKEN" "$PLURX/api/v1/coming-soon" | python3 -m json.tool
```

**How to read it.** This is the seam with the sharpest edge in the whole
integration: **the rail fails silently.** An unreachable Curator, a rejected
key, or a genuinely empty calendar all produce the same thing — no rail at
all. There is no error state on the home screen, on purpose (an error banner
on the home screen for an optional rail is worse than the missing rail), but
it means *absence of the rail is not evidence of an empty calendar*.

Worse, **Test connection (§4) probes `/api/v1/system/status`, not the
calendar.** A key valid for status but wrong for the calendar reads
`✓ connected` beside a permanently empty rail. When the rail is missing and
Test says connected, check the Logs card for target `plurxd::integrate` and
the message `coming-soon fetch failed`, or run the curl above.

---

## 4. Test connection — an active probe, deliberately

**What it does.** The **Test connection** button on the Curator card actively
calls Curator rather than repeating your config back at you. It distinguishes
three failures that look identical from the settings screen: cannot reach it,
reached it and the key was rejected, reached it and it works.

**Wire.** `GET /api/v1/monarr/status` (admin) → server-side
`GET {monarr_url}/api/v1/system/status` with `X-Api-Key`, UA
`plurx/<version>`, 10 s timeout.

**Where you see it.** Settings → **Metadata → Curator**. Literal results:

| Shown | Means |
|---|---|
| `checking…` | in flight |
| `✗ not configured` | no URL or no key saved |
| `✓ connected` / `✓ connected · Curator <version>` | reachable and authorized |
| `✗ Curator rejected the API key` | 401/403 — wrong key |
| `✗ Curator returned <status>` | reached something, but not Curator |
| `✗ cannot reach Curator at <url>: <err>` | DNS, port, or firewall |

`<err>` is the **root cause**, not reqwest's outer wrapper — the difference
between `dns error: failed to lookup address information` and
`Connection refused` is the entire diagnosis, and the outer message throws it
away. DNS means the other container is not resolvable from this one (a missing
`extra_hosts`, or a network they do not share); refused means the name
resolved and nothing was listening on that port.

Saving the card runs the test automatically. Beneath it, the queue line —
`Watch notifications — N sent, N waiting, N failed` — appears when any counter
is nonzero (§5).

Note the card's `Paired.` line, above the fields, is an **echo of stored
config**: it means a URL and a key are saved, nothing more. The status span is
the probe.

**How to verify.**

```bash
curl -sS -H "Authorization: Bearer $TOKEN" "$PLURX/api/v1/monarr/status" | python3 -m json.tool
```

**How to read it.** `✓ connected · Curator 0.9.0` proves URL + key + reachable,
and nothing about the calendar (§3) or the webhook (§5). It is the floor, not
the ceiling.

---

## 5. Watched outbox — plurx's only outbound push, off until you say so

**What it does.** When someone finishes something, plurx tells Curator — which
records it, displays it, and prefers upgrades for actively watched shows.
Nothing is deleted or unmonitored as a result; that path does not exist in
either application.

**Opt-in.** The `Send watch state to Curator` checkbox is off by default,
because the payload names the user who watched it. Per-user with usernames is
the deliberate choice — an aggregate "someone watched this" is useless for the
one thing Curator does with it — and that choice is precisely why it is opt-in
rather than on.

**Wire.** `POST {monarr_url}/api/v1/webhooks/plurx`, header `X-Api-Key`:

```json
{"event":"watched","kind":"episode","tmdb":1399,"season":2,"episode":5,
 "watched_at":1753500000,"user":"paul"}
```

`kind` is `movie` or `episode`. For an episode, `tmdb` is the **show's** id —
plurx walks episode → show to find it, because an episode's own id is not what
identifies the series it belongs to.

**Durability.** Deliveries go through a SQLite outbox with the retry schedule
as a column: backoff `5s → 30s → 2m`, batch of 20. Restart plurx mid-retry and
the retry still happens. A queue that lives in memory loses everything on the
one event you most want it to survive — a restart.

**Who delivers, and how fast.** In a cluster exactly one voter drains the
queue (it holds the `watched:outbox` job lease); if it dies another takes
over within about 105 s. A notification queued on the draining node goes out
within a second; one queued on another node within about ten seconds. An idle
queue is checked locally rather than by asking the cluster, so an empty
outbox costs no replicated writes beyond one forced check every 30 s.

**Where you see it.** Settings → Integrations → Curator card, the queue line
under **Test connection**: `Watch notifications — 41 sent, 2 waiting, 0
failed`. And on the Curator side, System → **Connections** lists `plurx` as
`calls Curator` / `calling`.

**How to verify.**

```bash
curl -sS -H "Authorization: Bearer $TOKEN" "$PLURX/api/v1/monarr/status" \
  | python3 -m json.tool | grep watched_
```

Then play something to the end and watch `watched_sent` move.

**How to read it.** `waiting` climbing is Curator being down; it drains itself.
`failed` is terminal — the delivery was classified as permanently unfixable (a
rejected key, a 4xx that will never become a 2xx) rather than retried every
two minutes forever. `sent` moving while Curator shows nothing means the ids
did not match: Curator matches on TMDB/IMDb id and never on title, and answers
`{"matched": false}` with a **200** for anything it does not manage. That is
not an error, and plurx correctly does not retry it.

---

## Metrics

`GET /metrics` (unauthenticated, counts only). Integration-relevant names:

| Metric | Type | Reads |
|---|---|---|
| `plurx_scan_total{trigger="…"}` | counter | scans started, by what asked for one |
| `plurx_notify_received_total` | counter | scan requests received from other applications |
| `plurx_watched_outbox{status="pending\|ok\|failed"}` | gauge | the §5 queue |
| `plurx_watched_outbox_ticks_total{outcome="skipped_hint\|skipped_unconfigured\|claimed\|empty_claim\|not_owner"}` | counter | why each §5 drain pass did or did not ask the cluster for rows |

`plurx_scan_total{trigger="targeted"}` climbing is the clearest machine-
readable proof that §1 is live. `plurx_notify_received_total` flat while
Curator's delivery log shows `ok` means the requests are reaching something
that is not this plurx.

---

## Verify the whole chain in five minutes

```bash
PLURX=http://127.0.0.1:32400;  MONARR=http://127.0.0.1:7676
PLXKEY=plx_…                   # §2
MKEY=<Curator Settings → Security → Reveal>
```

1. **plurx can reach Curator** — Settings → Integrations → Curator →
   **Test connection** → `✓ connected · Curator <version>`.
2. **The calendar half works too** (Test does not cover it) —
   `curl -sS -H "Authorization: Bearer $TOKEN" "$PLURX/api/v1/coming-soon"` returns entries, and the
   home screen shows a **Coming soon** rail.
3. **Curator can reach plurx, and plurx can see Curator's roots** — on
   Curator: Settings → Notifications → the plurx notifier's Test. It sends
   Curator's library roots as `probe_paths` (§1.6) and passes only when plurx
   lists every one as `visible`.
4. **plurx is visible from Curator** — Curator: System → Connections lists
   `plurx` as `calls Curator` / `calling`.
5. **End to end** — grab something in Curator. When it imports, Curator's
   delivery log reads `visible → plurx item <id>` for each directory,
   `plurx_scan_total{trigger="targeted"}` increments, and the file appears in
   plurx without you touching a scan button.
6. **Watch state, if enabled** — play something to the end;
   `watched_sent` increments on the Curator card and Curator records the watch.

---

## What plurx deliberately does not do

- **Integrations never write media storage.** Scanning, metadata, playback, and
  every integration in this document use read-only libraries and never rename,
  move, or delete a file. The only product-wide exception is the unrelated,
  admin-only, per-library, off-by-default Dolby Vision on-disk conversion under
  the explicit storage contract in [OPERATIONS.md](OPERATIONS.md); no integration
  can enable or invoke it.
- **Never talks to Runner.** There is no seam. plurx learns about new files from
  Curator, after the import, when they are actually in place — telling it
  earlier would only announce files that are not there yet.
- **Never accepts a scan from a login token.** `POST /api/v1/scan` is
  key-scoped only. The wall runs both ways: an application cannot use a user's
  token, and a key cannot become a user.
- **Never returns a key secret twice**, and never returns `key_hash` at all.
- **Never sends watch state by default.** §5 is opt-in because it names people.
- **Never blocks playback on an integration.** Every outbound call is queued or
  cached; Curator being down costs you a rail and a queued notification.

## Honest gaps, as of 2026-11-08

Recorded rather than quietly fixed, because a reader who knows the gap can
work around it and a reader who doesn't will misread the UI:

- **The Coming soon rail fails silently** (§3). Absence of the rail is not
  evidence of an empty calendar, and Test connection does not cover the
  calendar path.
- **plurx surfaces no inbound integration state in its UI.**
  `GET /api/v1/system` returns `notifications_received`, `scans_by_trigger`,
  `last_notification_at`, `last_notification_source`, `last_correlation_id`
  and `scan_requests` — and nothing renders them. Today the only way to see
  that Curator is calling is the Logs card or `/metrics`. This is the mirror of
  the gap Curator's inbound caller registry was written to close.
- **Library scan status does not name its trigger.** A scan shows the same
  way whether you clicked it or Curator asked for it, even though the request
  record carries `source` and `correlation_id`.
- **No keys UI** (§2). Minting is a curl.
- **A cancelled request reads `cancelled`.** Cancelling a targeted job in
  Activity cancels its requests. Their record then reads `"status":"cancelled"`,
  a value outside the contract's four, and replaying the request's
  `request_key` answers `202 queued`. Curator should send a new attempt.
- **A stale `waiting_on` can outlive its reason.** After a fence refusal wrote
  `full_scan:<library>`, a later claim refused for another reason (playback's
  foreground gate) writes nothing. The old value stays until the targeted pass
  starts. The contract has no value for that wait.

## Keeping this honest

Every literal in this document — result strings, scope names, wire fields,
metric names — was read out of the code on the branch that ships it, and
`tests/operations/test_integration_doc_literals.py` checks the scan seam's
limits, error codes, statuses and `waiting_on` values against the source. When a
seam changes, this file changes in the same commit; a doc that is confidently
wrong about a wire format costs more than no doc at all.
