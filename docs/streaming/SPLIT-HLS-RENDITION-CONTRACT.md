# Split HLS rendition contract (P62 S2)

**Status:** design accepted 2026-11-27, building · **Owner:** Claude Opus 5.5 ·
**Effort:** `E-5F2C2E9EA4` · **Written:** 2026-11-27 against `main` @ `59344bc3b`
([inventory](SPLIT-HLS-DEFAULT-INVENTORY.md))

S2 makes four changes:
- every split presentation gets one description and one master writer;
- clients get one way to ask for a split, with an explicit muxed opt-out;
- split eligibility becomes one predicate;
- a cross-node offer for a split copy names its audio-free video, so placement
  no longer depends on the selected audio.

It does not widen admission. The same sessions are split or muxed after S2 as
before it, and S3 widens admission.

`P/` = `crates/plurxd/src`, `C/` = `crates/plurx-core/src`.

## 1. Today

There are three split master writers, each with its own rules:

| Writer | Group | Per-audio attributes | Variant attributes |
|---|---|---|---|
| P49 `VodSplitCopyFamily::master_playlist` (`C/transcode/vod.rs`) | `audio` | NAME, LANGUAGE (raw tag), DEFAULT (one), CHANNELS, `STABLE-RENDITION-ID="a{index}"` | BANDWIDTH = video + max audio, RESOLUTION, CODECS, `INDEPENDENT-SEGMENTS` |
| Continuous `VodPresentationFamily::master_playlist` (same file) | `shared` | NAME="Audio", CHANNELS | One variant per rung, FRAME-RATE from the grid |
| Native `native_split_master_text` (`P/http/hls/playlist_text.rs`) | `audio` | NAME="Main" | Audio codec hard-coded `mp4a.40.2`, `VIDEO-RANGE=PQ` hard-coded, one muxed budget |

Each writer's output is then re-parsed by `continuous_master_with_subtitles`
to splice in subtitles.

Child URIs already share one shape, `{video|audio}/{64 hex}/index.m3u8`, which
`ChildMediaRequest` (`P/vodserve.rs`) validates. The identity behind the hex
differs by path:
- VOD uses the rendition key (`P/vod/plan.rs` `rendition_key`).
- Native uses the init digest.

Both are content-bound and stable for the life of the presentation, so S2
keeps both.

## 2. The contract

A new module, `C/transcode/split_master.rs`, holds:
- `SplitPresentation { variants, audio, subtitles, independent_segments }`;
- `SplitVideo { rendition_id, codec, supplemental_codecs, width, height,
  frame_rate, video_range, budget }`;
- `SplitAudio { rendition_id, source_index, name, language, default, channels,
  codec, budget }`;
- a budget that is either per role or combined (native has only the muxed
  copy's budget until S5);
- `SplitPresentation::master_text()`, the only writer of a split master.

Its rules are enforced in `master_text` and pinned by tests. A presentation
that breaks one is refused, and the session serves muxed.

1. **Groups.** Audio is optional. With audio there is one group,
   `GROUP-ID="audio"`, named on every variant. Without audio there is no group
   and no `AUDIO=`.
2. **Defaults.** With audio, exactly one rendition is `DEFAULT=YES`, and every
   rendition is `AUTOSELECT=YES`.
3. **Identity.** Rendition ids are 64 lowercase hex and unique across roles.
   `STABLE-RENDITION-ID` is `a{source audio index}` and unique. This is a
   client contract: the web player switches in place by it
   (`P/web/player/audio-sync.js`).
4. **Names and languages.** NAME is unique within the group. It is the title,
   else the language, else `Track N`; a collision appends ` (N)`. LANGUAGE goes
   through the subtitle normaliser (`language_tag`), so audio and subtitles
   agree.
5. **Bandwidth.** BANDWIDTH is the video peak plus the largest audio peak, or
   the combined budget. AVERAGE-BANDWIDTH is written only when every term has
   an average.
6. **Codecs and channels.** CODECS is the video codec followed by the distinct
   audio codecs, and it and CHANNELS come from the verified inits. Only AAC
   init facts exist today, so a non-AAC audio rendition is refused, and S3 adds
   AC-3 and E-AC-3 facts. Native keeps its `hvc1.`→`hev1.` and `dvh1.`→`dvhe.`
   rewrites, pinned in the table test.
7. **VIDEO-RANGE.** An absent VIDEO-RANGE means SDR, so the range is always
   written from a proved fact:
   - AVC and HEVC take it from the init or the transfer the plan proved.
   - Native takes PQ from its own Dolby Vision conversion.
   - An unknown range on HEVC or DV refuses the presentation.
8. **INDEPENDENT-SEGMENTS.** Written only when the layout proves every child
   starts each segment on a sync sample, and off by default. Copy video can cut
   on its 15 s ceiling (`C/fmp4.rs` `playlist_header`), so no layout proves it
   today. P49 loses the tag it carries now.
9. **Version and order.** `EXT-X-VERSION:10` when SUPPLEMENTAL-CODECS is
   present, else 7. Variants are ordered by height, width, then peak.
10. **Subtitles.** The subtitle group is an input, rendered by the contract.
    Nothing re-parses the master.

**Deliberate output changes:**
- Continuous: the group becomes `audio`, and the audio rendition gains a
  stable id plus a NAME and LANGUAGE from the source.
- Native: the audio codec and CHANNELS come from the projected init, and NAME
  and LANGUAGE from the source.
- P49: it gains `VIDEO-RANGE`, drops `INDEPENDENT-SEGMENTS`, and normalises
  LANGUAGE.

Outside `vod.rs` only test fixtures spell `shared`.

## 3. Split eligibility, the capability and the opt-out

**One predicate.** `split_copy_eligible(request, file)` is a pure function
that replaces the gate at the top of `prepare_vod_split_soundtracks`:
- an AVC copy without DV conversion or preservation;
- VOD, with no encoding, continuous role or subtitle burn;
- 2–3 audio streams.

The owner (admission) and the ingress (offer prediction, §4) both call it, so
S3 widens the rule in one place.

**The token.** Caps v2 `transports` gains `hls-split-av-v1`. It means the
client plays an HLS master with one alternate audio group of fMP4 renditions
and selects among them in place.
- On the owned create surface (`P/http/hls/create.rs`) the request is split
  when `copy && split_audio.unwrap_or(token)`. `split_audio: false` is the
  explicit muxed request, and the web capacity retry keeps using it.
- Shared-playback recipes do not map the token. Their grammar admits only
  subtitle groups, and receivers pin `audio_alternates: false`.
- The web player declares the token per start and only for the hls.js engine.
  Safari's native parser ignores audio groups. It stops sending
  `split_audio: true` and still sends `split_audio: false` on the capacity
  retry.
- No other client sends the token in S2. Each declares it in S7, once its
  player is proved. The continuous and native layouts are not gated by the
  token in S2.

## 4. The offer names its artifact (protocol 13)

The P49 canary on the fleet (2026-11-27) showed audio 0 of a title placed on
one node and audio 1 on another, repeatably. Two
viewers in different languages therefore package and store the same audio-free
video twice. Three causes combine:
- the offer fingerprint hashes `audio_index` (`P/media_pool.rs` `fingerprint`);
- rendezvous is computed over the fingerprint;
- `cache_hit` comes from the live-transcode plan for the selected audio.

The fix:
- **A new contract value.** `MediaOfferRequest.output_contract` gains a second
  value, `hls-split-fmp4-v1`, set by the ingress when `split_copy_eligible`
  predicts a split copy. No new field is added.
- **No audio in the offer.** For that value `is_valid` requires
  `audio_index == None`, so the fingerprint and rendezvous are audio-free. The
  selected default audio still travels in the `SessionRequest`.
- **What the worker answers.** It accepts both values. For the split value it
  reports `cache_hit` when it holds a live shared video of the source
  (`VodServe::holds_split_video`); retained generations on disk are not
  consulted. Unlike a held muxed output, the hit does not waive the source
  and scratch checks, because the soundtracks are still encoded there.
- **Protocol 13.** `PROTOCOL_VERSION` becomes 13, with a dated line under
  rolling upgrades in [OPERATIONS](../OPERATIONS.md). Protocols already form
  exact cohorts, so a 13 ingress places only on 13 workers. The worst case
  during an upgrade is one duplicate video per cohort.

If the owner then serves muxed (for example, a soundtrack fails to prepare),
only the affinity was wrong. S5 may bump again for relay and takeover.

## 5. Moved out of S2

- **Segment alignment to S3.** Today P49 audio uses a fixed 2.005 s AAC grid,
  while copy video uses its keyframe plan (6 s floor, 15 s ceiling). That
  caused the 2.6× request count in the S1 baseline. The invariant S3 must meet:
  every audio segment boundary of a split presentation falls on the AAC frame
  nearest a video segment boundary. It is a producer change, so it lands with
  the audio copy and projection.
- **The muxed master.** `master_playlist_with_shape` keeps writing the
  single-variant master. Muxed has no groups, and Apple's single-variant
  parsers depend on its exact shape.
- Native per-role budgets and an independent audio codec proof are deferred,
  along with P3 (native split without the muxed write). Native has neither
  while it projects from the muxed copy, and S5 owns that work.

## 6. Acceptance

- The three split writers and the subtitle re-parse are gone. Each layout
  builds a `SplitPresentation`.
- A table test pins each layout's master text, and a property test pins rules
  1–10 over generated presentations.
- `split_copy_eligible` has its own tests. Admission, and therefore the split
  or muxed outcome, is unchanged for every existing request shape.
- The web player gets the same P49 split session from the token alone, and gets
  muxed on `split_audio: false`.
- A split-predicted offer has an audio-free fingerprint, so two audio
  selections of one title rank the same peers in the same order. A worker that
  holds the video-only generation reports `cache_hit`.


## 7. Indexed VOD copy (S3)

New AVC VOD copy starts use the S3 layout when the client negotiates split
audio. Each supported soundtrack is evaluated against the client's audio
claim. A bounded source packet scan proves copy for supported 48 kHz AAC-LC,
AC-3 and E-AC-3. The proof reads at most 64 MiB of packet census output
per track in two seconds and retains up to 64 MiB of packet evidence for
the whole group. The AAC path handles tracks outside the proven copy
envelope. The plans cut on the nearest audio packet boundary to each
video boundary. The legacy continuous encoder
keeps its fixed AAC grid.

The owner opens the entire child group privately. It verifies each child
init, reserves AAC capacity and checks the master before attaching a
session. Alternate tracks use the configured resource budgets and can
serve larger groups. Refused private preparations use the ordinary muxed
copy before activation.

Protocol 14 persists the issued layout, video rendition and index key, and
each soundtrack's rendition and init identities. Recovery selects that
index and reproduces the frozen soundtrack modes and URLs. An unambiguous
historical split master rebuilds the P49 fixed-grid AAC layout. A historical
media playlist rebuilds a muxed session. An ambiguous native wrapper requires
a fresh session.


## 8. Encoded VOD uses one verified producer

A VOD transcode with split capability can publish separate video and audio
playlists for the selected AAC soundtrack. Both come from the same encoded
window. Projection preserves sample bytes, decode times, and composition
offsets. The video init supplies the codec, raster, and range. A processed
Dolby Vision Profile 8.1 init also supplies the supplemental codec and HDR10
compatibility.

The owner verifies the projected init and the window containing the viewer's
start position before sealing the master URL. Unsupported media shapes and
projection deadline refusals keep the encoded muxed media URL. The owner
retains the selected layout, producer identity, and both child identities
under media-pool protocol 15. Recovery verifies those identities before
serving the issued child URLs.
