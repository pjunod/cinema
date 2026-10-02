# Jellyfin compatibility — measured build progress and remaining gates

**Status:** open · J0 in progress; J1–J6 not started · **Updated:** 2026-10-02 EDT.

Companion to [the reviewed build contract](JELLYFIN-COMPATIBILITY-BUILD.md)
(what must be built and proved) — this records execution and evidence. The
Emby phases in [the outline](JELLYFIN-EMBY-COMPATIBILITY-OUTLINE.md) remain
outside this build. A reference-server response is not physical playback
acceptance.

## 1. Contract publication — the real gate ran before merge

[PR #744](http://192.168.4.7:3000/noirr/plurx/pulls/744) published the reviewed
contract. Its earlier runs were skipped because their event payloads carried
`draft: true`; making the PR ready had not produced a new eligible run.
Closing and reopening the ready PR triggered the existing fast lane without
changing the reviewed source or workflow.

[Run 3881](http://192.168.4.7:3000/noirr/plurx/actions/runs/3881) passed scope,
policy/contract preflight and **Main promotion gate** on head `b3ff464b9` and
base `9a719fcb7`. Unaffected compiler jobs were skipped by the docs-only
scope. The gate checked that both refs remained current. PR #744 then merged
as `772b9cf23`; `effort/jellyfin-compat` starts there.

The [publication record](jellyfin/docs-publication-receipt.json)
retains those identities and job outcomes. It is ordinary docs fast-lane
evidence, not the final effort qualification receipt. The shared primary
checkout and its unrelated work were preserved.

## 2. J0 baseline — pinned reference, pending physical flows

The [baseline manifest](jellyfin/baseline-manifest.json)
records immutable provenance and hashes for synthetic fixtures. Exact versions
are established separately from interoperability:

| Surface | Measured or pinned baseline | Evidence boundary |
|---|---|---|
| Reference server | Jellyfin 10.11.11, upstream commit `1fbd8739292cce610231be93daf43368733edf63`; official container index digest and arm64 image ID in manifest | Disposable synthetic library; not a Plurx deployment |
| Schema | OpenAPI served by that container under `/jellyfin`, 315 paths; SHA-256 in manifest | Raw hash includes the served origin; separate canonical hash removes only `servers` |
| Infuse | Installed 8.5.6 / 8.5.5763 on connected physical AppleTV14,1, tvOS 27.0 / 24J361 | App/device inventory measured; client flows not tested |
| Android TV | Candidate official release 0.19.10, source commit `984181a3d6ab14e9a6d2dcc850c582e1c138bd95`; released APK SHA-256 in manifest | No physical ADB endpoint detected; installed version and player backend not measured |
| Android TV dependencies | Kotlin SDK 1.7.1, Media3 1.8.0 from pinned `gradle/libs.versions.toml` | Source provenance, not device capability evidence |

Server provenance is [the official released tag](https://github.com/jellyfin/jellyfin/releases/tag/v10.11.11).
The Android candidate is [the official release](https://github.com/jellyfin/jellyfin-androidtv/releases/tag/v0.19.10).
The pinned Android profile branches at server 10.11 for Dolby Vision range
predicates and declares VTT external/HLS delivery. Actual negotiated predicates,
requested formats and version-implied ancillary calls still need capture.

**Synthetic corpus:** an eleven-minute 1280×720, 24 fps H.264/AAC MP4; a
matching MKV with English/French AAC audio and an English SRT track; external
SRT cues at 1, 120 and 330 seconds; one local poster. The manifest records file
hashes and sizes. This corpus supports direct, remux, forced bitrate encode,
track selection and a pause longer than 300 seconds; it does not satisfy the
HDR/DV acceptance row. Movie/episode hierarchy, multiple pages and the HDR
corpus remain to be prepared.

**Reference preparation:** dedicated random-password test account, synthetic
movie library, `/jellyfin` base path, automatic port mapping disabled. A
temporary proxy binds to the controller's LAN address and removes credentials,
pseudonymizes IDs and omits non-JSON bodies before writing traces outside the
repository. Its harness login trace was checked for the original password,
token and user UUID; none appeared. Raw protocol credentials and personal
device identifiers are not retained in this repository. Prototype files and
downloads remain outside maintained source while J0 is active; remove owned
scratch after retaining minimized fixtures and findings.

| Probe | Result | Counts as |
|---|---|---|
| Harness public system info, public users, Quick Connect status under `/jellyfin` | 200, JSON read successfully | Reference bootstrap only |
| Harness authentication | 200; dedicated account authenticated | Reference account only |
| Harness movie projection | Eleven-minute movie, one media source | Reference library preparation only |
| Infuse connect/browse/direct/seek/transcode | Not tested | Required J0 client gate |
| Android TV connect/browse/HLS/remux | Not tested | Required J0 client gate |
| Both clients' image/subtitle/media credential carriers and version-implied calls | Not measured | Required policy/design evidence before J2 |
| Client pause over 300 seconds, kill/background, renegotiation without old Stopped | Not tested | Required lifecycle evidence before advancing J0 |
| Native encoded VOD without copy index; bounded capacity | Service regressions passed, including decoded GET bytes after forward/back restarts | Native seam evidence only; client activation and per-create policy pending |
| Native unindexed HEVC copy and preparation deduplication | Service regression passed; refused copy and one preparation request, no incomplete VOD session | Native seam evidence only; facade immediate fallback pending |
| Missing duration, first-minute engine attestation, long-pause real-client resurrection and VOD-only worker/relay propagation | Not exercised through the prototype | Required J0 hard-seam experiments |

## 3. Compiler loop — available before Rust changes

The local toolchain resolves through `/Users/pjunod/.cargo/bin` with
`RUSTUP_TOOLCHAIN=1.97.1`; `rustc --version` reported
`rustc 1.97.1 (8bab26f4f 2026-07-14)`. All-target `cargo check -p plurxd
--all-targets --locked` passed on the reviewed publication source before any
Rust edit. This uses the local compiler, not CI as a compiler.

The existing `vodserve::tests::idle_reap_and_same_id_resurrection_are_one_reader_transition`
regression passed: one test executed, 3,137 filtered. It proves the native
reader-transition race is fenced. It does not prove either external client's
pause/resume behavior or the new facade's adapter, which has not been built.

Additional native service regressions ran on the integrated base, each with
one test executed and 3,137 filtered:

| Test (`cargo test -p plurxd --bin plurxd <name> -- --nocapture`) | What actually passed |
|---|---|
| `encoded_vod_ntsc_gets_decode_after_forward_and_backward_restarts` | Encoded VOD attaches without a copy index; fetched init/segment bytes decode after 0, 90.09 and 3-second starts, with stable initialization bytes |
| `encoded_vod_capacity_read_has_one_deadline_and_no_orphaned_wait` | Existing capacity policy has a bounded read deadline and no orphaned wait |
| `first_play_queues_missing_source_preparation_with_discovery_off` | Missing HEVC copy preparation refuses; repeated first play queues one existing preparation request and attaches no incomplete VOD session |

These are existing native tests, not a new compatibility adapter or a proof
that an external client will request the resurrection path. The per-create
VOD-only policy and client-side timing remain unbuilt/unmeasured. Test linking
reported the macOS compact-unwind size warning; execution passed. Workspace
Clippy with `-D warnings`, formatting, catalog lint and served JavaScript
syntax passed through the normal tracked hook.

## 4. Milestone admission — J0 remains open

| Milestone | State | Remaining admission evidence |
|---|---|---|
| J0 | In progress | Both required physical flows; carrier/base-path traces; lifecycle/preparation experiments; frozen protocol subset and revised estimate |
| J1 | Waiting on J0 | Identity, deterministic retirement/replacement guard, token-only service seams |
| J2 | Waiting on J1 and traced artwork policy | Connect/browse, bounded misses and cold-node first sync |
| J3 | Waiting on J2 | Direct/watch, Store revisions and per-play final durability |
| J4 | Waiting on J3 and J0 hard-seam decisions | VOD-only negotiation/activation, refusal mapping, bounded retries and aliases |
| J5 | Waiting on J4 | Track, subtitle and observed ancillary completion |
| J6 | Waiting on J5 | Frozen physical/cluster matrix, qualification and graduation |

A concrete request for the connected Infuse device and an available Android
TV has been made to Paul. Hardware absence remains **not tested**. Do not mark
J0 complete, start the full facade route build or reduce the required client
matrix to compensate. No compatibility release, setting graduation or fleet
deployment has occurred.
