# Optical Blu-ray reader lab — October decryption result

**Status:** AACS-only Plurx VOD video/audio decode proven; BD+ and clients open ·
**Observed:** 2026-10-08 UTC · **Source:** `codex/optical-m0-foundations`
at `9cf5d6081`

Companion to the [optical status page](../features/OPTICAL-MEDIA-STATUS.html)
(live execution state) and the [September physical receipt](OPTICAL-BLURAY-PHYSICAL-2026-09-27.md)
(original protected-media refusal). This receipt covers two inserted
commercial Blu-rays and an isolated full-app probe of the second. It does not
claim client playback or broad BD+ support.

## The temporary reader reached the disc, but not a decoded frame

The lab used the Pioneer `BD-RW BDR-XD07U` firmware `1.01` on `nynuc`, with
`/dev/sr0` and its matching `/dev/sg1` passed to a disposable container. The
UDF filesystem was mounted `ro,nosuid,nodev,noexec`; the running Plurx service
was unchanged. MakeMKV 2.0.0 was built only in this lab under the operator's
EULA approval. The vendor-published October beta key was configured inside the
container, not in the repository; no expiry error was observed. The lab did
not change drive firmware, region state, or eject the disc.

MakeMKV's default LibreDrive probe again stalled. Its documented `SDF_STOP`
setting, scoped to this drive's exact ID, let the offline scan progress through
BD+ processing. MakeMKV reported one FUT for one clip using its built-in generic
SVQ, then returned `Failed to open disc` and `TCOUNT:0` on the complete scan.
Its trace reported that automatic SVQ downloading was disabled or failed in
the deliberately network-isolated container. That is a candidate cause, not
proof that a vendor download would make this disc playable.

The lab also tested FFmpeg's `bluray:` input against the read-only mount with
one video frame requested and null output. Dynamic-loader evidence confirmed
that `libmmbd.so.0`, not only Debian `libaacs`, was loaded. LibMMBD reported
the same BD+ processing, but libbluray reported `Unable decrypt unit (AACS)!`
and FFmpeg exited `183` before producing a frame. The absent Java VM emitted
a BD-J menu warning; menus are outside this release's scope and are not
counted as the AACS failure.

## Pinning both decryptors still did not produce a frame

A follow-up offline run explicitly set both `LIBAACS_PATH` and
`LIBBDPLUS_PATH` to `/usr/lib/libmmbd.so.0`. The first attempt denied the
drive access MakeMKV requires; the second exposed an unwritable temporary
MakeMKV state directory. Neither setup error is evidence about disc support.
With normal raw-drive access, a writable disposable state directory, the
same read-only UDF mount and no container network, MakeMKV completed BD+
processing but its full scan still reported `Failed to open disc` and
`TCOUNT:0`.

The final FFmpeg attempt used those corrected conditions and selected the
1:30:07 playlist from four candidates. LibMMBD reported successful
initialization and BD+ processing, then libbluray reported
`Unable decrypt unit (AACS)!` twice and FFmpeg exited `183` before a frame.
This rules out an omitted `LIBBDPLUS_PATH` setting and an unwritable state
directory as sufficient fixes for this disc. It does not prove whether the
remaining failure is missing disc-specific key material, BD+ handling,
drive behavior, or another reader interaction.

## Approved vendor-update access did not resolve the disc

The operator then approved a disposable network-enabled reader test with
possible disclosure of disc-derived identifiers to MakeMKV. The UDF mount
remained read-only, and the running Plurx service was unchanged. MakeMKV's
[Linux SVQ guidance](https://forum.makemkv.com/forum/viewtopic.php?t=6164)
identifies `wget` as a downloader dependency; the initial image lacked it.
The corrected image installed `wget` and set the vendor-documented headless
`app_UpdateEnable = "1"` option. The container could fetch MakeMKV's public
site over HTTPS.

MakeMKV 2.0.0 then announced `Downloading latest HK to /root/.MakeMKV` but
did not finish that stage within a bounded 100-second scan. A brief syscall
trace showed TLS traffic from the MakeMKV process while it was at that stage;
this was not an offline run or a proof that its update service returned usable
key material. The temporary state directory contained no saved HK or SVQ file.
An earlier network-enabled scan without `wget` still reported `Automatic SVQ
downloading is disabled or failed` and `TCOUNT:0`.

The vendor's archived 1.18.4 release, built separately with `wget` and a
fresh writable state directory, completed its title scan but likewise
reported `Automatic SVQ downloading is disabled or failed`, `Failed to open
disc`, and `TCOUNT:0`. Its LibMMBD path attempted SDF and HK downloads,
processed BD+ with the built-in generic SVQ, and selected the 1:30:07
playlist from four candidates. FFmpeg then reported `Unable decrypt unit
(AACS)!` and exited `183` before a frame. Neither version saved an HK or
SVQ file. The result does not distinguish unavailable vendor key data from
this disc's protection, drive interaction, or another reader defect.

## A public AACS database did not qualify BD+ playback

A separate offline comparison downloaded the English `KEYDB.cfg` archive
from the [FindVUK database](https://fvonline-db.bplaced.net/) without
submitting a disc identifier. Its ZIP SHA-256 was
`49fba5a35291502bf052aaf511c8137266a324501766c4110e539761dce6c01d`.
The archive had three title-text matches but no entry for the local SHA-1
of this disc's `AACS/Unit_Key_RO.inf`. Its contents were mounted only in a
network-disabled disposable reader.

Debian `libaacs0` with `libbdplus0` refused before video because the native
BD+ library lacked VM configuration (`VM configuration not found`). Removing
`libbdplus0` for an AACS-only diagnostic made libbluray refuse the BD+
protected media earlier (`No usable BD+ libraries found`). Neither run
verified an AACS key or produced a frame. A matching AACS entry alone would
not satisfy this disc's separate BD+ requirement.

## What this evidence changes

The expired September beta key is no longer the observed blocker. An installed
LibMMBD library and a successful BD+ processing message are still insufficient:
the physical reader must decode an actual title frame, then satisfy forward
and backward seek and the existing VOD segment path. The approved online
vendor-update attempt did not satisfy that acceptance. Further reader work
needs both a verified AACS key/decryption source and functional BD+ handling
for this disc, or a separate diagnosis of the physical drive and media;
another blind retry of the same container configuration is not evidence of
progress.

No fast-lane tests, adversarial merge review, release deployment, or main merge
are implied by this source-reader investigation. The requested main-promotion
handoff is to the user's batched-merge session only after the optical work is
actually ready.

After the first-disc runs, all of their disposable containers and images,
read-only mounts, temporary settings (including disc-derived metadata), the
downloaded key database, and local build files were removed. The production
`plurxd` container remained healthy and unchanged.

## A second retail disc decodes; its probe exposed a wrong timeline

After the operator changed media, the same Pioneer drive exposed a different
UDF Blu-ray. This one has an `AACS` directory and no `BDPLUS` directory. The
stock Jellyfin FFmpeg still refused it with `No usable AACS libraries found!`.
In a new, network-disabled disposable image, MakeMKV 2.0.0 with the approved
October beta key and this drive's exact `SDF_STOP` value completed a title scan
without the first disc's `Failed to open disc` result. The title-aware reader
selected playlist `00000.mpls` and reported a 1:44:58 feature.

LibMMBD supplied both libbluray decryptor interfaces to Jellyfin FFmpeg. The
reader decoded one 1920×1080 video frame at the start, another after seeking
to 600 seconds, and a distinct frame after reopening at 60 seconds. The same
600-second and 60-second frame-hash probes then succeeded with the repository's
source-pinned optical FFmpeg 8.1.3 from its built `runtime-assets` stage. That
stage was combined with MakeMKV components **only in a disposable local lab
image**; the committed Plurx image still does not redistribute them. The
packaged-reader probes ran as uid/gid `999:999` with only the drive's `cdrom`
group added, `/dev/sr0` and its matching `/dev/sg1`, a read-only disc mount,
a writable temporary MakeMKV state directory, and no network. Frame hashes
were emitted to stdout; no decoded video was saved. This proves physical
source decryption and two seek positions for this AACS-only disc, not Plurx
VOD segments, chapter/track selection, client playback, or BD+ compatibility.

The packaged FFprobe returned `format.start_time=4198.000000` and
`format.duration=91846.217689` for the feature, even though the MPLS play-item
intervals total `6298.958` seconds. The old helper passed the transport-stream
estimate into title facts and stored probe JSON, which would misstate the
duration to the UI and VOD planner. Commit `74a574c8b` now reads bounded MPLS
play-item IN/OUT times at 45 kHz and normalizes both fields from that
navigation timeline. Its focused regression is recorded but, at the
operator's direction, not executed before the final batched review/test lane.
Pinned Rust 1.97.1 helper all-target compilation, formatting and the tracked
pre-commit Clippy/static checks passed. A host-built helper could not run in
the Bookworm package because it required `GLIBC_2.39`; a second build of the
exact commit in a pinned Rust 1.97.1 Bookworm toolchain produced a compatible
helper (SHA-256 `13117e4f969e9d3765e53b7b48078fcd1f98bcec785f66164986a987061b8791`).
Mounted only into the disposable reader, that helper inspected all 11 playlist
files and published `duration_ms=6298958` for playlist zero. This confirms
the corrected duration survives the real helper's inspection and JSON output.
The separate Plurx daemon, VOD segment path and clients were not yet qualified
at this point in the lab.

The first disc's BD+ and AACS failure remains open. This successful second
disc narrows the problem; it does not turn the first disc into a passing case
or establish blanket protected Blu-ray support.

## The full app served decodable VOD from the second disc

An isolated Plurx instance on `nynuc` used the same read-only mount and exact
`/dev/sr0`/`/dev/sg1` device pair. Its internal-only Docker network, separate
data directory and service-uid-owned MakeMKV state did not change the running
production container. The licensed LibMMBD backend existed only in disposable
lab images; no vendor binary, license or disc key entered the source archive or
published Plurx image. The source archive came from this agent's independent
clone, without `.git` or credentials.

The full helper inspected 11 playlists and returned `6298958` ms for the main
title. The first app start exposed an actual Hiqlite placeholder-order error
in optical grants/matches; commit `6ba551994` corrected binding order. A
second start returned a complete VOD playlist but an identical retry was
refused before it could reach the manager's replay admission; commit
`c928e4abb` allows only the matching busy request to reach that check. The
producer then emitted malformed/near-empty fragments because the disc's
4,198-second transport clock was copied without normalization. A direct
20-second encode produced only 26 KB and reported a 4,198-second audio
compensation failure; adding `-start_at_zero` produced 3.27 MB of normal
fragments. Commit `4e096127b` applies that normalization only to managed
optical inputs. Finally, retrying an active session wrongly attempted durable
activation again and ended it; commit `9cf5d6081` returns the existing
response after exact request matching.

With all four corrections, the app returned a VOD response at both `start=0`
and `start=600`, each with the correct 6,298,958 ms title duration and a
complete 3,150-segment HLS playlist. The first media segment returned HTTP
200. Independently consuming each playlist with the packaged optical FFmpeg
decoded 120 video frames at each start; 66 frame hashes were distinct within
each sample. Their first hashes matched, but their final hashes differed,
so the common lead-in frame was not evidence of a frozen rendition. At the
600-second start, 100 AAC frames also decoded (three distinct frame hashes;
this is decode evidence, not a subjective audibility check). The exact same
start request returned the same session ID, `DELETE` returned 204, and the
drive returned to `ready` after stop. Only frame hashes and counts were
retained; no movie segment or decoded video was saved.

This qualifies the second disc's app-level title inspection, VOD production,
nonzero-start decode, idempotent replay and teardown in the isolated lab. It
does **not** qualify first-disc BD+ decryption, browser or native client
interaction, all tracks/subtitles/chapters, a full-length playback, or a
release deployment. The focused regressions are written but remain unrun at
the operator's direction until the batched merge review and fast lane.

After recording the evidence, the second-disc lab container, internal network,
read-only mount, source extracts, images, scripts, and disposable app and
MakeMKV state (including the lab license configuration) were removed. An
inventory found no optical-named `/tmp` asset or Docker image/network left on
`nynuc`. The running production `plurxd` and discovery containers remained
healthy and unchanged.
