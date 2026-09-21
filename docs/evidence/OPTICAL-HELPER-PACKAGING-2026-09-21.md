# Optical Linux helper and packaging evidence — 2026-09-21

**Status:** built; physical-media qualification remains open · **Branch:**
`codex/optical-m0-foundations` · **Toolchain:** Rust 1.97.1

## What this slice builds

`plurx-optical-helper` is a Linux-only, short-lived process with three narrow
commands: `presence`, `inspect`, and `eject`. It accepts only drive and
mount paths selected in node-local startup configuration. The daemon bounds
its runtime and reply bytes, suppresses inherited input, validates the
versioned JSON reply, and fences inspection and eject against the current
insertion.

Inspection recognizes DVD-Video from the VMG title table and Blu-ray from
sorted MPLS playlist identities. The disc fingerprint covers bounded
navigation metadata rather than movie bytes. Navigation traversal rejects
symlinks, inspection requires a read-only mount, probe output is bounded, and
public records retain typed title locators rather than host paths.

The source installer builds and installs the helper on Linux. The Docker image
build, tagged binary manifest, binary-only release Dockerfile, architecture
checks, and retained release artifacts now treat it as a required runtime
binary. Historical one- and two-binary release manifests remain verifiable.
The Compose example, example TOML, systemd unit comments and deployment runbook
describe the exact device, mount and group grants without recommending
privileged mode.

## Construction evidence

The helper's focused fixture suite passed 3/3 before the packaging edits. The
final helper source then formatted and the Linux implementation type-checked
with the pinned compiler through the `linux-host-check` feature on the
available Unix host. The installer passed `bash -n`; both release-validation
modules compiled as Python. The Rust check emitted only the existing
`plurx-core` dead-code warnings caused by compiling that dependency without
the daemon's normal feature union.

Those fixtures prove DVD title-table byte order, playlist enumeration, and
that navigation changes alter the fingerprint while video-byte changes do
not.

Per the requested resource policy, the packaging contract and final candidate
are not repeatedly test-gated during construction. The frozen candidate will
receive the single fast-lane run after the requested adversarial review.

## Deliberately open acceptance

- No reachable machine has an operator-identified optical drive.
- The available local FFmpeg 9.0.1 build reports both `dvdvideo` and
  `bluray` inputs missing.
- No physical insertion, removal, read, seek, angle selection, protected-disc
  response or eject has been observed.
- The production source-aware VOD controller and cluster owner routing remain
  later milestones; installing the helper does not claim playback is ready.

These are visible advisory failures and promotion blockers, not reasons to
override the operator's saved enable choice.
