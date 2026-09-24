# Subtitle downloads — find captions for media without them

**Status:** implemented locally; review and promotion open · **Written:** 2026-09-24

Add manual subtitle search and download, with optional automatic acquisition
for missing preferred languages. OpenSubtitles.com is the first provider.
Downloaded captions must survive rescans and work through the existing web,
Apple and Android subtitle delivery routes.

## 1. Provider

Use the [OpenSubtitles REST API](https://opensubtitles.tawk.help/article/getting-started).
An administrator configures an API key; an optional account provides its own
download allowance. Keep credentials on the server. Provider quota responses
are authoritative: the provider's help pages currently disagree about the
free account allowance, so no allowance is hard-coded into the product.

Search by the media's catalog identity and language. Prefer a matching file
hash when available, since another release can have different cue timings.
Show release, language, hearing-impaired and forced flags before download.
Manual search may offer uncertain matches; automatic acquisition requires an
exact file match and must never guess from a title alone.

## 2. Durable captions

The current catalog describes embedded streams. Its subtitle cache is
disposable and local to a playback node; the PGS source store is also local
and represents tracks extracted from the original media. Neither is durable
storage for an acquired subtitle.

Store bounded, normalized WebVTT and its provenance in replicated catalog
storage, tied to the file ID, size and modification time. Retain embedded
track ordinals and append downloaded tracks to the selectable track list.
Replacing a media file invalidates its old subtitle associations. A rescan
of an unchanged media file preserves them. Downloading captions never rewrites
the movie or episode file.

Expose ordinary subtitle metadata to clients. Caption bodies are served by
the existing authenticated subtitle route and native HLS rendition path.
Cache eviction must regenerate acquired captions from durable storage without
contacting the provider or consuming another download.

## 3. Controls

Add **Find subtitles** to the media details screen, including files whose
current track list is empty. A language picker and search results lead to
an explicit download action. Report disabled provider, no matches, invalid
credentials, exhausted quota and temporary provider failure separately.

Settings expose provider setup and an optional automatic mode. Automatic
downloads are disabled initially. When enabled, a bounded background job
searches for missing configured languages; it skips tracks already present,
honors provider retry timing and records attempts to avoid repeated searches
and duplicate downloads after a restart or leader change. Playback does not
wait for this job.

## 4. Integration and acceptance

The feature is one integrated change on `codex/subtitle-downloads` and targets
`main` through the ordinary affected-surface lane in the repository's
[development pipeline](../DEVELOPMENT_PIPELINE.md). The initially created
local effort scaffold received no task merges; storage, playback and API
changes share one media-file contract and are reviewed together. The single
adversarial review, current-candidate gate and qualification receipt are
required before merge.

| Area | Result | Acceptance |
|---|---|---|
| Durable captions | Replicated and SQLite storage, source identity fences, ordinary selectable tracks | Store regression with `--features hiqlite-store`; preservation on rescan, invalidation on replacement, duplicate and concurrent write cases |
| Playback | WebVTT, native HLS, burn and offline consumers resolve downloaded tracks | Missing cache and missing original embedded subtitle regression; forward/backward seeks preserve cue times |
| Provider and API | Bounded OpenSubtitles search/download with server credentials and explicit errors | Fixture HTTP tests for search, download quota and unsafe link refusal; malformed-caption validation; bounded requests |
| Controls and automation | Search UI, settings, optional background acquisition | Web behavior tests; exact-match eligibility, missing-language detection and duplicate suppression regression |
| Promotion | Documentation and validation cover the complete feature | Current candidate passes Main promotion gate and has the qualification receipt |

Provider downloads are untrusted input: restrict download destinations, cap
response size and time, normalize captions, and escape provider text in the
UI. Stored captions have finite per-track and per-file limits so ordinary
metadata reads and replicated writes stay bounded.

The compiler loop was established before editing: Rust 1.97.1 and
`cargo check -p plurxd --all-targets --offline` passed on the starting tree.
This is baseline evidence only; each implementation still needs its focused
regressions, pinned compilation, formatting and Clippy before pushing.

## 5. Local verification

All Rust commands used the repository-pinned Rust 1.97.1. The build cache was
reused with `--target-dir /Users/pjunod/code/plurx/target`; tests used `--offline`.

| Command | Result |
|---|---|
| `cargo clippy --workspace --all-targets -- -D warnings` | Pass; all workspace targets compile |
| `cargo test -p plurxd --bin plurxd subtitle` | 104 subtitle, API, provider and playback tests pass |
| `cargo test -p plurx-core --features hiqlite-contract-tests --test store_contract downloaded_subtitles_` | Pass against both SQLite modes and three replicated voters, including fenced and concurrent writes |
| `cargo test -p plurx-core --features hiqlite-contract-tests --test store_contract replicated_v27_store_migrates_the_request_identity_on_daemon_open` | Pass; existing replicated databases advance through the new migration |
| `cargo test -p plurx-core --features hiqlite-store --lib store::hiqlite_import::tests` | 30 import and legacy-schema tests pass |
| `cargo test -p plurx-core --features hiqlite-store --lib daemon_schema_gate_accepts_the_complete_supported_chain` | Pass |
| `cargo test -p plurx-core --features hiqlite-store --lib v64_adds_field_order_to_the_existing_files_table` | Pass; SQLite upgrades through the new migration |
| `node --test tests/web/subtitle-downloads.test.js` | Four UI regressions pass |
| `node tests/web/settings-sections.test.js` | 30 settings assertions pass |
| `node tests/web/asset-order.test.js` and `node tests/web/asset-load.test.js` | Pass |
| `node tests/playback/web-policy.test.js` | Pass |
| `scripts/js-check` and `scripts/validate lint` | Pass |
| `python3 -m unittest discover -s tests/operations -p test_docs_index.py` | Four documentation contracts pass |

No live provider credentials were available, so an actual OpenSubtitles
account download still needs a smoke test. Native device playback was not
smoke-tested; acquired tracks use the existing WebVTT delivery contract.
Automatic matching requires the worker to read the media file for its hash.
No release qualification, merge or deployment has been performed.

## 6. Adversarial review before promotion

One independent adversarial review found two P2 issues. Both are addressed:

- Source hashing now compares the held file's size and modification time to
  the catalog before reading and checks its identity and nanosecond modification
  time again after both blocks. `subtitle_hash_rejects_a_replacement_before_catalog_rescan`
  covers both size and modification-time replacements.
- Search refuses files that have not completed a probe. Both SQLite and
  replicated acquisition updates also require `probe_json IS NOT NULL` in the
  atomic publication statement. The downloaded-subtitle store contract verifies
  refusal before the first probe, then preserves embedded ordinal zero while
  appending the acquired caption after it.

No second review was requested. Current main was integrated before the final
local checks. The initial 104-test receipt above predates the added hash test;
the final subtitle filter contains 105 tests.

The broader pre-promotion checks also passed: 303 core storage unit tests,
242 validation tests (one skipped), and 500 local operations tests. Three
unchanged operations fixtures assume Linux (`/bin/true` signing stubs and
Linux timeout behavior) and were excluded on macOS; the required Linux
promotion lane runs the full operations suite. The legacy SQLite downgrade
fixtures now remove the new caption column, the placeholder census resolves
the complete caption publication statement, and the ownership inventory names
the new shutdown-owned job, bounded hash worker, HTTP fixtures and timers.
