# Docker hardware setup — automatic device and group configuration

**Status:** implementation · **Updated:** 2026-10-07

The failure is at the deployment boundary: the image contains FFmpeg and GPU
libraries, but Docker does not infer host device access or supplementary groups.
A non-root container can therefore fall back to software even on a capable host.
NVIDIA additionally needs its video driver libraries exposed by the toolkit.

One PR implements deployment-time detection in both source and image startup
paths, preserves explicit selections and host overrides, and documents native
macOS VideoToolbox separately. No Rust encoder selection or feature gates change.

| Step | State | Evidence |
|---|---|---|
| Independent clone and branch | done | `codex/docker-gpu-auto` based on `854c206a7` |
| Hardware helper and startup integration | in progress | Temporary Compose overlay, numeric device groups, NVIDIA video libraries |
| Regression coverage and documentation | in progress | CPU-only, hybrid GPU, explicit configuration, non-local engines, cleanup |
| Adversarial agent review | pending | Requested only when implementation is ready |
| Focused regression and fast lane | pending | Run after review; retry only failed checks |
| Merge and cleanup | pending | Carry regression lines into landing commit |

The earlier diagnostic on rog confirmed its RTX 4080 Laptop GPU can perform a
720p H.264 NVENC encode in a disposable container. That is host evidence, not
acceptance evidence for this change. The new automation is delivered through
the PR, not patched into rog's working checkout.
