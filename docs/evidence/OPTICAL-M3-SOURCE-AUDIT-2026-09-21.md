# Optical M3 managed-source audit — 2026-09-21

**Status:** partial; encoded VOD source path built, copy/index and physical
acceptance open · **Branch:** `codex/optical-m0-foundations`

This receipt records the disposition of every source consumer named by
§7.1 of the optical implementation contract. It is an implementation audit,
not physical-drive evidence.

## Source-consumer disposition

| Consumer | Disposition | Evidence or remaining boundary |
|---|---|---|
| Regular-file metadata and open checks | Replaced for optical | `PlaybackMediaFacts`, `PlaybackSourceRef::Optical` and `ResolvedInput` carry typed facts and execution input without manufacturing a `MediaFile`, file ID or item ID. Existing file playback keeps its original checks. |
| Descriptor and source identity | Built for encoded VOD | The claimed `OpticalPlaybackLease` binds insertion generation, disc, title, angle and resolved input. Managed output identity and the encoded source-object version include the path-free source identity; the device path remains owner-local execution state. |
| Probe / decoder facts | Built for encoded VOD | The bounded helper publishes validated `probe_json`. `resolve_managed_movie_plan` derives decoder facts and cache identity from that document and the managed source identity rather than opening a catalog path. |
| Keyframe and fragment index builders | Open for copy | Encoded optical VOD uses the immutable frame grid derived from probed cadence. The regular-file fragment-index path still requires a `MediaFile`; no optical copy/index claim is made. |
| Copy inputs | Open | Managed optical recipes deliberately reject the copy branch today. The decision endpoint advertises encoded SDR VOD and `vod_indexed: false` rather than claiming direct or remux transport. |
| Transcode inputs | Built | `prepare_optical_vod_encoding` carries the held `ResolvedInput` into `Encoding.source_input`; the VOD producer emits managed FFmpeg arguments from that exact input while the session retains the reader lease. |
| Subtitle secondary inputs | Partial | Selected optical subtitles use the same managed title and timeline through the in-pipeline burn path. Native text rendition, DVD subpicture palette handling and PGS application-overlay extraction remain open and are not advertised as native tracks. |
| Thumbnail and analysis jobs | Deliberately absent | No optical route starts an independent thumbnail, detector or analysis read. Adding one requires the same reader admission and bounded staging contract; the current implementation does not create a competing reader. |
| Reservations and scratch admission | Built for encoded VOD | The exact lease moves into the VOD session only after attachment commits. Existing VOD admission, blocked-GET and materialization budgets govern encoded work; terminal cleanup retains the lease until producer cleanup settles. |
| Prewarm | Deliberately absent | Optical sessions do not schedule file prewarm work. The drive has one reader, so a future prewarm path must be part of the admitted producer rather than a background open. |
| Replacement | Built as serialized reclaim; physical acceptance open | Web and native source adapters keep one player owner. Native track/quality changes await acknowledged teardown of the exact incumbent before reclaiming the drive, so a viewer action cannot race a second reader. The server does not claim parallel prepared replacement for a physical source. |
| Retry | Partial | Replaying the same request identity recovers the same preparing/active session; a conflicting payload is rejected. Reader and helper failures stay typed and bounded. Physical retry behavior remains in the final hardware matrix. |
| Recovery and adoption | Built as owner-only / no adoption | The durable optical recipe is separately versioned and path-free. The owning node may recover its VOD session; another node cannot reinterpret it as a file recipe or adopt a missing physical source. |
| Activity | Built | Managed optical sessions publish through the existing local and cluster Activity inventory with `source: optical` and optional catalog IDs; no synthetic IDs are emitted. |
| Progress | Built | Progress writes require the current disc, title, drive generation and active session. Clients report the logical title position through the established player lifecycle. Unknown duration is preserved as unknown. |

## Acceptance boundary

The encoded path has a complete typed source-to-player route and pinned Rust,
Apple and Android compile evidence elsewhere in this effort. It does not prove
physical DVD/Blu-ray demux, cell/clip joins, near-end reads, native subtitle
extraction or copy indexing. Those rows require identified hardware and the
final physical acceptance matrix. Until optical copy/indexing exists, the API
continues to state encoded SDR VOD explicitly and never reports Original,
direct, remux or `vod_indexed: true`.
