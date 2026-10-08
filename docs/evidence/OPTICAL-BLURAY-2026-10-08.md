# Optical Blu-ray reader lab — October offline decryption result

**Status:** decoded-frame acceptance open · **Observed:** 2026-10-08 UTC ·
**Source:** `codex/optical-m0-foundations` at `7bee0b16e`

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

## What this evidence changes

The expired September beta key is no longer the observed blocker. An installed
LibMMBD library and a successful BD+ processing message are still insufficient:
the physical reader must decode an actual title frame, then satisfy forward
and backward seek and the existing VOD segment path. The next controlled
experiment is a vendor-update-enabled lab only if the operator authorizes the
possible disclosure of disc-derived identifiers to MakeMKV. The execution
safety reviewer rejected that outbound test without this specific approval;
it has not been attempted indirectly.

No fast-lane tests, adversarial merge review, release deployment, or main merge
are implied by this source-reader investigation. The requested main-promotion
handoff is to the user's batched-merge session only after the optical work is
actually ready.

After the run, both disposable containers, the read-only mount, temporary key
file, lab directory and lab image were removed. The production `plurxd`
container remained healthy and unchanged.
