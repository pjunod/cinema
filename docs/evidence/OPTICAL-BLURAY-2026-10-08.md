# Optical Blu-ray reader lab — October decryption result

**Status:** decoded-frame acceptance open · **Observed:** 2026-10-08 UTC ·
**Source:** `codex/optical-m0-foundations` at `03153da44`

Companion to the [optical status page](../features/OPTICAL-MEDIA-STATUS.html)
(live execution state) and the [September physical receipt](OPTICAL-BLURAY-PHYSICAL-2026-09-27.md)
(original protected-media refusal). This receipt isolates the reader stack for
the same inserted commercial Blu-ray. It does not claim Plurx application or
client playback.

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

After the runs, all disposable containers and images, read-only mounts,
temporary settings (including disc-derived metadata), the downloaded key
database, and local build files were removed. The production `plurxd`
container remained healthy and unchanged.
