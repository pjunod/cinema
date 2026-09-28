# Optical Blu-ray physical evidence — protected-media refusal

**Status:** protection refusal proved; decoded-frame and seek acceptance open ·
**Source:** `codex/optical-m0-foundations` at `0da951f5` · **Observed:**
2026-09-27

Companion to
[OPTICAL-MEDIA-IMPLEMENTATION.md](../features/OPTICAL-MEDIA-IMPLEMENTATION.md)
(the acceptance contract),
[OPTICAL-MEDIA-STATUS.html](../features/OPTICAL-MEDIA-STATUS.html) (live
execution state), and
[OPTICAL-M0-2026-09-20.md](OPTICAL-M0-2026-09-20.md) (the earlier
capability baseline). This record covers one operator-selected commercial
Blu-ray on one physical Linux reader. It does not claim playable media.

## Host and reader — a real Blu-ray is reachable read-only

The `nynuc` host reported Linux 7.0.0-31-generic on x86-64 and a Pioneer
`BD-RW BDR-XD07U`, firmware 1.01, at `/dev/sr0`. The 38.2 GiB UDF volume was
mounted only for this acceptance run:

```text
/dev/sr0 /mnt/plurx-optical-acceptance udf ro,nosuid,nodev,noexec,relatime,iocharset=utf8
```

The disc contained the expected `BDMV`, `CERTIFICATE`, `AACS`, and `BDSVM`
trees. Its label is deliberately omitted. The helper's bounded presence call
returned:

```json
{"presence":"present"}
```

with exit status 0. This closes the earlier hardware-reachability gap for one
Blu-ray reader. It does not close DVD reachability.

## Reader capability — protocol present, decryption unavailable

Ubuntu FFmpeg 8.0.1-3ubuntu2 reports `bluray AVOptions` with `playlist`,
`angle`, and `chapter`. That is positive protocol evidence rather than an
exit-status inference.

The same reader could not open this protected disc. FFmpeg reported no usable
AACS configuration and refused decryption. Presence-only checks found no
`KEYDB.cfg` at the invoking user's XDG path or the two system paths checked by
the acceptance command. No key was downloaded, no region setting was changed,
and no protection mechanism was bypassed.

## Product outcome — one stable public failure

Commit `0da951f5` recognizes a bounded list of libaacs/libbdplus failure
markers inside the short-lived Linux helper. The daemon parses only the
helper's structured nonzero reply and maps it to the existing public optical
error envelope. The helper built with Rust 1.97.1 as an x86-64 Linux binary;
the exercised binary SHA-256 was:

```text
4bd3c69e03ac9154fc16872a8c25bb9684d4129ce0816be2ff956a5864496045
```

Against the mounted disc, inspection returned exit status 1 and exactly:

```json
{"code":"optical_protection_unsupported","message":"the inserted disc is protected and the configured reader cannot decrypt it"}
```

The response contains neither a mount path, disc label, title, raw library
diagnostic, nor key material. This is the intended unsupported-protection
outcome; it is not a generic read failure.

## Acceptance disposition — do not promote this to playback proof

This run proves physical presence detection and typed refusal for one
AACS-protected Blu-ray. It does not prove title enumeration, a decoded first
frame, forward or backward seek, chapter seek, boundary handling, subtitle
extraction, or playback on any client. Those rows remain open until an
operator supplies media that the configured reader can lawfully decode, or
configures an already-authorized decryption backend outside plurx.

The running `plurxd` container was not modified: it currently has neither the
optical helper nor `/dev/sr0` and the read-only mount passed through. Deployment
is therefore also not implied by this receipt.
