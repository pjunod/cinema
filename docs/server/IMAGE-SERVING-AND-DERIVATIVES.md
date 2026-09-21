# Image serving and derivatives — verify once, remember the proof, serve the size the grid asked for

**Status:** blocked on fleet and device evidence · **Executes:** C6 / F-core-7 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
(assessment rows C6, F-core-7 and correction 9 in
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md))
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read §2 first: it quotes the two serving paths, the eight-permit gate, the
digest check and the file-identity struct as they are, and names the
deliberate choice (`original` backdrops) this plan must not touch. Then
build §5 in order: M1 (verified-digest cache, separate permits, `ETag`) and
M2 (`?size=` derivatives). M2 depends on M1's digest cache. One draft PR per
plan into `main` under the fast lane; milestones are logical commits in that
one PR. Every `file:line` is from
`88a3957a`; re-verify by function name.

**If a step seems to require changing `BACKDROP_SIZE`/`STILL_SIZE`, serving
a content-addressed file whose bytes have not been verified against its
name at least once for its current inode, skipping the corruption
quarantine, or lifting the bound on peer fetches, stop and flag it.**

**Correction to the review:** one of placement. C6 says image serving
"SHA-256s the whole file on the runtime per request". The *read* is already
off the runtime — `read_bounded_local_artwork` (`images.rs:283-305`) runs in
`spawn_blocking` — but the digest (`artwork_bytes_match_name`, `:838-847`)
is computed on the async thread that receives the bytes (`:266, 226`). So
the finding stands for the hash, not for the I/O, and M1 moves the hash
into the same blocking closure as well as caching its result.

---

## 1. Objective

1. A local artwork hit costs one `open` + `fstat` + read and **no SHA-256**
   once its bytes have been verified for the current file identity; the
   verification result is cached against an identity strong enough that a
   replaced or rewritten file is re-verified, and every verification and
   quarantine behaviour stays as it is.
2. Local reads and peer fetches are bounded by *separate* budgets, so a
   grid burst of local hits cannot be refused 503 because eight peer
   fetches are in flight — and local reads stay bounded by bytes in
   flight, not unbounded.
3. Every artwork response carries a strong `ETag` and answers
   `If-None-Match` with 304 without reading the file.
4. Grids can ask for a smaller derivative (`?size=w300|w500|w780`) that is
   generated once per (source digest, width), served from the same
   handler, and never replaces the original — TV heroes keep the
   `original` bytes `metadata/mod.rs:38-43` chose.

## 2. Contract today

Re-verify at build time.

### 2.1 Two handlers, one coordinator, one gate

`crates/plurxd/src/http/images.rs`:

- `:97-103` `GET /api/v1/images/{filename}` (`AuthUser`) →
  `serve_cluster_artwork`; `:110-129` `GET /api/v1/cluster/artwork/{filename}`
  (peer HMAC, `verify_artwork_peer_auth`) → `serve_local_artwork` only,
  never a peer hop.
- `:134-170` `serve_cluster_artwork`: local first; a capacity error returns
  as-is; any other miss goes to `fetch_and_materialize` under
  `ARTWORK_FETCH_TOTAL_TIMEOUT = 3 s` (`:41`), racing `PEER_RACE_CONCURRENCY
  = 3` peers to `PEER_RACE_DEADLINE = 3.25 s` (`:34-35`).
- `:53-94` `ArtworkCoordinator`: one `Semaphore::new(MATERIALIZE_CONCURRENCY
  = 8)` (`:33, 63`) shared by **both** local reads (`:260-263`) and peer
  fetches (`:224`), acquired with `try_acquire_owned` (`:88`) — no waiting,
  so the ninth concurrent local hit is a 503
  `artwork_response_capacity` (`:175-183`). A keyed `Weak<Mutex<()>>`
  single-flight per filename (`:68-85`) collapses concurrent misses.
- `:255-281` `serve_local_artwork`: permit → `read_bounded_local_artwork`
  → `artwork_bytes_match_name` on the runtime → response, or quarantine
  when a content-addressed name does not match.

### 2.2 The read, the digest and the identity

`:283-305` `read_bounded_local_artwork`: in `spawn_blocking`, open with
`open_read_nofollow_blocking`, refuse non-files, empty files and anything
over `MAX_ARTWORK_BYTES` (15 MiB, shared with the producers via
`plurx_core::metadata::MAX_ARTWORK_BYTES`), capture
`artwork_file_identity(&metadata)`, read exactly `len` bytes.

`:855-880` the identity:

```rust
struct ArtworkFileIdentity { device: u64, inode: u64, bytes: u64, changed_secs: i64, changed_nanos: i64 }
// unix: dev(), ino(), len(), ctime(), ctime_nsec()
```

Note it is **ctime**, not mtime — the inode change time, which userland
cannot set. Windows substitutes creation^last-write (`:882-895`).

`:795-813` `content_addressed_artwork_digest` recognises two immutable
name families — `{item}-{poster|backdrop}-c{16hex}.{ext}` (`Prefix16`)
and `{item}-poster-{64hex}.{ext}` (`Full64`) — and the legacy mutable
`{item}-poster.jpg` / `{item}-backdrop.jpg`, for which
`artwork_bytes_match_name` returns `true` unconditionally (`:839-841`).
`dto.rs:24-28` mints URLs as `/api/v1/images/{f}?v={item.updated_at}` so a
legacy filename with new bytes gets a new cache identity at the client.

`:313-364` quarantine: on mismatch, rename to
`.{filename}.corrupt-{uuid}`, re-read, and unlink only if the quarantined
inode still has the same identity and still mismatches; otherwise restore
with an exclusive rename. `:540-552` `install_artwork` refuses to write
bytes that do not match a content-addressed name and writes atomically
(`atomic_write_child`: temp + rename → a new inode on every install).

`:395-411` the response: `Content-Type` from the extension,
`Cache-Control: private, max-age=604800, immutable`, no `ETag`, no
`Last-Modified`.

### 2.3 The deliberate size choice

`crates/plurx-core/src/metadata/mod.rs:36-44`:

```rust
const POSTER_SIZE: &str = "w500";
// Backdrops and episode stills become full-width television heroes. The old
// w1280/w300 cache buckets were visibly upscaled there …
const BACKDROP_SIZE: &str = "original";
const STILL_SIZE: &str = "original";
```

A 4 MB backdrop on a phone grid is the cost of a sharp hero on a 65-inch
TV. The fix is a smaller *derivative* for the grid, not a smaller source.

### 2.4 Consumers

Web: `dto.poster`/`backdrop` URLs are used as-is in `<img src>` with
`?token=`. Apple: `AuthImage.swift:6` loads `/api/v1/images/…` with the
bearer and ImageIO downsamples client-side. Android: Coil through
`Session.mediaUrl`. Plex façade: `plex.rs:287-299` and `:304-330` serve
poster/backdrop through `serve_cluster_artwork`. None sends `If-None-Match`
today because nothing gives them an `ETag`.

No image-decoding crate is in the workspace (`Cargo.lock` has no `image`,
`png`, `jpeg-decoder` or `webp`). The shipped ffmpeg is resolved by
`ffmpeg::ffmpeg_bin()` (`ffmpeg.rs:72`) and the bounded child primitive is
`bounded_command_output_with_limits` (`ffmpeg.rs:1904-1943`: piped, 
`kill_on_drop`, output bound, timeout, job-owned).

## 3. Change

### 3.1 M1 — verified-digest cache, separate permits, ETag / 304

**The cache.** In `ArtworkCoordinator`, a
`verified: Mutex<lru::LruCache<String, VerifiedArtwork>>` (capacity
`VERIFIED_ARTWORK_CACHE = 4_096`; a `BTreeMap` with an insertion-order
`VecDeque` if adding `lru` is unwelcome — either is fine, the bound is the
point) where

```rust
struct VerifiedArtwork {
    identity: ArtworkFileIdentity,   // dev, ino, len, ctime_s, ctime_ns
    digest: [u8; 32],                // full SHA-256 of the bytes as read
    verified_at: Instant,
}
```

An entry is written **only** after a full read whose digest matched the
name (or, for a legacy name, after a full read — the digest is still
computed, because it becomes the ETag). It is consulted by
`read_bounded_local_artwork`'s replacement, `read_verified_local_artwork`,
which runs in the blocking closure: open, `fstat`, compute the identity,
compare with the cached entry; on an exact match of all five fields
*and* `verified_at` younger than `VERIFIED_ARTWORK_TTL = 24 h`, read the
bytes and return them with the cached digest, **skipping the hash**; on
any difference, read, hash in the same closure, verify, and replace the
entry (or quarantine exactly as today and evict).

Why this identity is strong enough, and where it is not: `install_artwork`
always writes through temp+rename, so every legitimate replacement is a new
inode — the cache can never see plurx's own new bytes under an old
identity. An in-place rewrite by something other than plurx changes ctime
(the kernel sets it on every data write) and usually size. What is left is
a same-size in-place write within the ctime granularity of the filesystem
— nanoseconds on ext4/xfs/btrfs, one second on some NFS servers and on FAT
— which is why the entry also expires after 24 h and why the quarantine
path is untouched: a stale digest can only cause a *stale-but-previously-
valid* set of bytes to be served for at most a day on a coarse-ctime
filesystem, never an unverified one. This is the assessment's "cache a
verified digest against a suitably strong file identity and revalidate
changes", with the residual stated.

Two hooks evict: `quarantine_corrupt_artwork_with` (`:323`) and
`install_artwork` (`:540`) both call `coordinator.forget(filename)`, so a
repair or a fresh install is never answered from the old entry even
within the TTL.

**Permits.** Two budgets replace the one semaphore:

- `peer_permits: Semaphore::new(PEER_FETCH_CONCURRENCY = 8)` — unchanged
  semantics (`try_acquire`, 503 on exhaustion) for `fetch_and_materialize`,
  because a peer fetch allocates up to 15 MiB of response buffer and
  holds a network slot.
- `local_bytes: Semaphore::new(LOCAL_READ_BUDGET_KIB = 65_536)` — a byte
  budget in KiB units (64 MiB in flight). A local read acquires
  `ceil(len / 1024)` permits **after** the `fstat` (so it knows the size)
  with `acquire_many_owned` under `LOCAL_READ_ADMISSION_WAIT = 250 ms`;
  a wait beyond that is the same 503. Bounded by bytes because the memory
  cost is bytes: 64 MiB admits ~300 posters at once or four full-size
  backdrops, and a burst of tiny thumbnails is no longer refused because
  eight large backdrops are being read. The waiter count is bounded by
  the HTTP connection count, which M1 of
  [HTTP-LISTENER-TIMEOUTS-AND-ASSET-DELIVERY.md](HTTP-LISTENER-TIMEOUTS-AND-ASSET-DELIVERY.md)
  bounds in turn.

`fetch_and_materialize_inner` (`:210-253`) keeps its "local recheck under
the permit" ordering, now with the local recheck under `local_bytes` and
the fetch under `peer_permits`.

**ETag and 304.** `ETag: "<hex digest>"` (the full SHA-256 for both
families; for a content-addressed name it equals the name's digest by
construction, for a legacy name it is the digest of the bytes served).
`If-None-Match` is evaluated **before** the read: `open` + `fstat` +
identity comparison against the cache; if the cached digest matches a
listed tag, answer 304 with `ETag`, `Cache-Control` and no body — no bytes
read, no permit taken. If there is no cache entry, fall through to the
full path (a first request always verifies). `Cache-Control` stays
`private, max-age=604800, immutable`; a validator match never bypasses
`AuthUser` or the peer HMAC (the extractors run before the handler, as
today).

**Metrics.** `plurx_artwork_requests_total{route="user|peer|plex",
outcome="hit|not_modified|peer|miss|capacity|corrupt"}` and
`plurx_artwork_verifications_total{result="cached|hashed|invalidated"}`,
hand-rendered beside the offline metrics in `http/system.rs:5000`. The
ratio `cached / (cached + hashed)` is the number that says M1 works.

### 3.2 M2 — `?size=` derivatives without touching the originals

**Contract.** `GET /api/v1/images/{filename}?size=<bucket>` with
`bucket ∈ {w300, w500, w780}` (a closed enum; anything else is 400, so
the derived set per source is at most three). `size` absent or `original`
is today's behaviour. Derivatives are node-local, like the originals:
`<artwork_dir>/derived/<digest32hex>-<bucket>.<ext>` where `digest32hex`
is the first 32 hex characters of the **verified** source digest from M1.
Keying by the source's verified digest, not its filename, means a legacy
`{item}-poster.jpg` whose bytes change simply produces a new derivative and
the old one becomes an orphan; nothing is ever served under a stale key.

**Generation.** Through the shipped ffmpeg, not a new decoder crate: the
workspace has none (§2.4), adding one is a compile-weight decision the
review (§4.3) treats as its own question, and ffmpeg's image decoders are
already the qualified ones on the fleet.

```text
ffmpeg -nostdin -hide_banner -loglevel error -y
       -i <source>
       -vf "scale='min(<W>,iw)':-2:flags=lanczos"     # never upscale; keep aspect
       -frames:v 1
       -q:v 3                                          # jpeg quality; png/webp use their defaults
       -f image2 <tmp>
```

Run through the existing job-owned `BoundedDiagnosticChild` with its bounded
file-output path rather than widening the private probe helper. The child has
`DERIVATIVE_TIMEOUT = 5 s`, a 15 MiB media-output bound and an 8 KiB
diagnostic tail,
`configure_ffmpeg_runtime` applied (so `XDG_CACHE_HOME` and the Windows job
object hold — the drift §4.1 of the review names is not repeated here),
writing to a temp file in `derived/` and `rename`ing into place. Output
container follows the source extension; an animated GIF takes its first
frame (`-frames:v 1`) and is written as PNG; a source with alpha (PNG,
WebP with alpha) stays PNG/WebP so the grid does not gain black corners.

Bounded: `derive_permits: Semaphore::new(DERIVATIVE_CONCURRENCY = 2)`
(two ffmpeg processes; a grid burst queues behind them for at most
`DERIVATIVE_ADMISSION_WAIT = 500 ms`), single-flight per derived key
through the existing keyed-mutex pattern (`:68-85`), and on **any**
failure — timeout, non-zero exit, permit wait, unwritable dir — the
handler serves the original with `X-Plurx-Artwork: original-fallback` (a
response header, so a client can tell it got the big one) and counts it. A grid never breaks because a resize did.

**Serving.** A derivative is a plain file under `derived/`; it goes through
the same `read_verified_local_artwork` (its name is content-addressed by
the *source* digest, so `artwork_bytes_match_name` treats it as legacy —
no name-digest check — and its own SHA-256 becomes its `ETag`). Same
`Cache-Control`. The response also carries `Vary: Accept` — not needed
today, reserved so a later WebP negotiation does not have to change the
cache key. The peer route (`serve_peer`) does **not** serve derivatives:
peers replicate originals and derive locally; a request for `?size=` on
`/cluster/artwork/…` is 400.

**Orphans.** `sweep_orphan_artwork` (`:740-773`) gains a second pass over
`derived/`: a derivative whose 32-hex prefix matches no currently verified
source digest *and* is older than `CONTENT_ORPHAN_GRACE` (24 h, `:43`) is
removed under the same `ORPHAN_REMOVE_LIMIT = 256` per pass. The set of
current source digests comes from the M1 cache plus one bounded
`items_with_artwork()` pass that reads (not hashes) — for a source with no
cache entry the derivative is kept until the source is next verified; a
kept orphan costs disk, a wrong delete costs a re-derive, and disk is the
cheaper mistake.

**Clients.** This PR changes no client. The DTO gains
`poster_sizes: ["w300","w500","w780"]` on `ItemDto` only when the source
exists, so each client can adopt `?size=` in its own PR
([ANDROID-CLIENT-PARITY.md](../clients/ANDROID-CLIENT-PARITY.md) /
[APPLE-CLIENT-PARITY.md](../clients/APPLE-CLIENT-PARITY.md) are the
trackers). The web grid (`layouts/library-grids.js`, W9's "`?w=` variants")
is the first consumer and is a separate small PR after M2 lands.

**Metric.** `plurx_artwork_derivatives_total{bucket, outcome="served|
generated|fallback_original|refused"}` (3 × 4 label values).

## 4. Guardrails (non-goals)

- **Never serve unverified content-addressed bytes** (correction 9: "a
  bare size/mtime/inode check does not prove an artwork file matches its
  content-addressed name"). Every cache entry is born from a full
  verification; identity includes ctime with nanoseconds; entries expire
  in 24 h; install and quarantine evict. §3.1 states the residual honestly.
- **Keep the corruption quarantine and cancellation accounting** (C6,
  F-core-7). `quarantine_corrupt_artwork_with` is called from the same
  two places with the same arguments; the `_permit` travels with the
  bytes into the response exactly as `AdmittedArtworkBytes` does today
  (`:375-388`), so a client that disconnects mid-body still releases its
  budget when the body drops.
- **Local reads stay bounded** (F-core-7 "unbounded local reads are
  not [sensible]"). A byte budget with a 250 ms admission wait, not an
  unbounded queue; the 503 code is unchanged.
- **Peer fetch capacity reserved** (F-core-7 "reserve bounded peer-fetch
  capacity"). Its own eight-permit `try_acquire` semaphore.
- **Do not revert `original`** (C6, correction "add smaller derivatives
  for grids rather than globally reverting the TV/detail quality fix").
  `BACKDROP_SIZE`/`STILL_SIZE` are not in the diff; a test pins them.
- **A validator never bypasses authorisation** (F-core-13's rule, applied
  here). The 304 path runs after `AuthUser`/peer HMAC, and reads the cache
  only — it cannot leak whether a file exists to an unauthenticated caller
  because it is never reached by one.
- **No new decoder dependency, no unbounded child.** ffmpeg through the
  bounded primitive with `kill_on_drop`; two at a time; failure falls back
  to the original.
- **No feature gate.** The derivative route does nothing until a client
  asks for `?size=`, and the original path is unchanged for everyone else.
- **Measured before credited** (F-core-7 "request concurrency and
  blank-image rates need observation"). The metrics above and §6.3.

## 5. Milestones

### 5.1 M1 — verified-digest cache, separate permits, ETag (`server/artwork-verified-digest`)

1. `VerifiedArtwork`, `read_verified_local_artwork` (hash inside the
   blocking closure), `forget()` hooks in install and quarantine, the two
   semaphores, `If-None-Match` before the read, `ETag` on every response,
   the two metrics.
2. Tests (in `images.rs`'s test module, which already builds a tempdir
   artwork tree and a mock peer):
   `a_verified_hit_is_not_rehashed` (count `Sha256::digest` calls via a
   test-only counter; second request → 0);
   `an_in_place_rewrite_with_a_new_ctime_is_reverified_and_quarantined`
   (write different same-length bytes into the inode; the request after
   must quarantine — the existing quarantine assertions reused);
   `a_replaced_inode_is_reverified` (install new bytes under the same
   legacy name; ETag changes);
   `install_and_quarantine_evict_the_entry`;
   `if_none_match_answers_304_without_a_read` (assert no permit taken and
   no read counted);
   `local_reads_are_bounded_by_bytes_not_count` (twelve 100 KB posters
   concurrently succeed; five 15 MiB backdrops → the fifth is 503);
   `peer_fetches_keep_their_own_eight_permits`;
   `the_peer_route_still_never_recurses` (existing, pinned);
   `backdrop_and_still_size_are_original` (pins `metadata/mod.rs:42-43`).

Acceptance: `cargo test -p plurxd http::images` green;
`curl -sI -H "Authorization: Bearer $T" http://10.42.1.11:32400/api/v1/images/287-poster-c0123456789abcdef.jpg`
shows an `etag`; repeating with `-H 'If-None-Match: <that etag>'` returns
304; after a Home page load on the living-room TV,
`plurx_artwork_verifications_total{result="cached"}` exceeds
`{result="hashed"}` on the second load.

### 5.2 M2 — `?size=` derivatives (`server/artwork-derivatives`)

1. The `size` query parameter, the closed bucket enum, `derived/` layout,
   generation through the bounded ffmpeg primitive, single-flight, the
   fallback-to-original path, the orphan pass, `poster_sizes` on the DTO,
   the metric.
2. Tests: `w300_never_upscales_a_small_source` (a 200-px poster → the
   original bytes are served, `outcome="refused"` is not counted — it is a
   served original); `derivative_key_is_the_verified_source_digest` (change
   the legacy source bytes → new key); `a_failed_ffmpeg_serves_the_original`
   (point `PLURX_FFMPEG` at `/bin/false` for the test);
   `derivatives_are_single_flight` (ten concurrent first requests → one
   ffmpeg spawn); `peer_route_refuses_size`; `orphaned_derivatives_are_swept_after_grace`;
   `an_animated_gif_yields_one_png_frame`; `an_alpha_png_stays_png`.
   Generated fixtures only (`ffmpeg -f lavfi -i testsrc2=size=1920x1080
   -frames:v 1`), no real artwork in the tree.

Acceptance: `cargo test -p plurxd http::images::derivatives` green;
`curl -s -o /tmp/p.jpg -H "Authorization: Bearer $T" 'http://10.42.1.11:32400/api/v1/images/287-backdrop-c…jpg?size=w780'`
then `ffprobe -v error -show_entries stream=width -of csv=p=0 /tmp/p.jpg`
prints `780`; the same request without `?size=` still prints the
original width; `ls <artwork_dir>/derived | wc -l` grows by one per
distinct request, not per request.

## 6. Verification and rollout

### 6.1 Lanes

`cargo test -p plurxd http::images` per milestone; `make unit` once before
un-WIP; `make validate-staged` before every push. No Node or Python gate.

### 6.2 Rollout

M1 to `lab1`, then the fleet after one evening of browsing from the TV
and a phone. M2 to the fleet after `lab1`; the web-grid consumer PR
follows separately. Nothing invalidates: original filenames, their
digests and the item revision URLs are unchanged; the new `derived/` dir
is created on demand. Rollback is the previous `sha-` image; a rollback
leaves `derived/` on disk, harmless and reaped by the next forward deploy's
sweep.

### 6.3 What to measure (F-core-7)

On `lab1`, before and after M1, from a laptop on the LAN: `hey -n 400 -c
40` (or `oha`) against twenty distinct poster URLs with a valid bearer.
Report p50/p99 latency, the 503 count, and CPU seconds of `plurxd` during
the run (`pidstat -p $(pidof plurxd) 1`). After M2, repeat against
`?size=w300` and report the derivative metrics plus the bytes transferred
for one Home page load in Chrome DevTools (cold and warm).

### 6.4 What only a device can prove — GPT prompt

```text
On the living-room Apple TV and the Android TV box, both signed in to lab1
running PR <M1 number>:
1. Open Home, scroll every rail to its end, open three detail pages with
   backdrops. Report any blank poster or backdrop, and how long the first
   poster row took to fill (count seconds).
2. Go back to Home and repeat once. Report whether it felt faster; then
   from a laptop run
   curl -s http://10.42.1.11:32400/metrics | grep plurx_artwork_
   and paste the lines.
3. On the Apple TV only, open Settings → Developer and paste the
   "artwork" line from the readiness list if one appears.
Do not clear the app's cache between steps.
```

## 7. Open questions

1. **Cache size and TTL.** 4,096 entries × ~100 bytes is negligible; 24 h
   is a guess at the coarse-ctime residual. If `plurx_artwork_verifications_total{result="hashed"}`
   stays high on a fleet where every node has the whole library cached,
   the TTL is the first thing to raise.
2. **WebP derivatives.** Every client decodes WebP; a `w300.webp` at
   quality 80 is roughly half the JPEG. Deferred until the bucket set has a
   consumer, and it would be negotiated on `Accept` (the `Vary` is already
   reserved).
3. **Plex façade `?size=`.** `/photo/:/transcode` takes `width`/`height`;
   mapping them to the nearest bucket is a two-line follow-up once M2 is
   in, and only matters if Kodi/PKC is in use
   ([PLEX-FACADE-PAGING.md](PLEX-FACADE-PAGING.md)).
4. **Should the peer route also carry the verified digest to the puller?**
   A peer already verifies by name; for legacy names an `ETag` from the
   origin node would let the puller skip a re-hash. Small; not needed for
   M1.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | Claim | [#431](http://192.168.4.7:3000/noirr/plurx/pulls/431) | Claimed `plan/C-03` from `main` @ `9deb58a2`; M1-M2 remain pending. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M1 | [#431](http://192.168.4.7:3000/noirr/plurx/pulls/431) | Identity-bound digest cache, separate local-byte/peer-fetch permits, authenticated 304, strong ETag and bounded metrics implemented; `cargo test -p plurxd http::images -- --test-threads=1` passed 25 tests. Lab and device observations remain pending. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M2 | [#431](http://192.168.4.7:3000/noirr/plurx/pulls/431) | Closed `size` buckets, digest-keyed derivatives, two-child/timeout/output bounds, single-flight, original fallback, conservative orphan cleanup, DTO readiness and fixed-cardinality metrics implemented. The existing job-owned bounded-file child was used instead of widening the probe-only output helper. Focused image tests pass; §6.2-§6.4 fleet, browser and device observations remain pending. |
