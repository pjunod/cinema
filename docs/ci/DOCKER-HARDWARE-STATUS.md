# Docker hardware setup — automatic device and group configuration

**Status:** implementation complete; qualification and merge tracked in PR #849 · **Updated:** 2026-10-07

The failure is at the deployment boundary: the image contains FFmpeg and GPU
libraries, but Docker does not infer host device access or supplementary groups.
A non-root container can therefore fall back to software even on a capable host.
NVIDIA additionally needs its video driver libraries exposed by the toolkit.

PR [#849](http://forge.lan:3000/noirr/plurx/pulls/849) implements deployment-time
detection in both source and image startup
paths, preserves explicit selections and host overrides, and documents native
macOS VideoToolbox separately. No Rust encoder selection or feature gates change. Missing NVIDIA toolkit
setup is advisory: automatic discovery leaves that passthrough unset without
blocking startup or other encoders.

| Step | State | Evidence |
|---|---|---|
| Independent clone and branch | done | `codex/docker-gpu-auto` based on `854c206a7` |
| Hardware helper and startup integration | implemented | Temporary Compose overlay, numeric device groups, NVIDIA video libraries |
| Regression coverage and documentation | implemented | CPU-only, hybrid GPU, explicit configuration, non-local engines, cleanup |
| Adversarial agent review | addressed | Four findings: unrelated devices, explicit NVIDIA scope, hook-free CDI/VAAPI, Compose symlinks; each fixed with a regression |
| Focused regression | passed | 14 hardware tests and three rollout contracts; only the failed temporary-path case was rerun |
| Real Docker hardware smoke | passed | On rog, generated groups 44/992, preserved the override and group 1000, and completed CUDA decode plus NVENC encode as UID 1000 |
| Catalog lint | fixed | Registered the helper in the existing operations point after preflight reported the missing path |
| Fast lane | qualifying refreshed candidate | Main includes the four-call sharing inventory correction. Qualification exposed a legacy attempt-2 import refusal in its new evidence adapter; the correction now admits authenticated legacy retries, keeps receipt-era retries strict (including skipped jobs), and recognizes a source-bound prepare refusal before units. Adversarial review addressed; three focused CI regressions passed. The PR checks and description carry the live result. |
| Merge and cleanup | tracked in PR | Merge only after required checks pass; carry regression lines into the landing message |

The earlier diagnostic on rog confirmed its RTX 4080 Laptop GPU can perform a
720p H.264 NVENC encode in a disposable container. That is host evidence, not
acceptance evidence for this change. The new automation is delivered through
the PR, not patched into rog's working checkout.

Focused commands, run after the review:

```bash
python3 -m unittest discover -s tests/operations -p test_docker_hardware.py
PYTHONPATH=tests/operations python3 -m unittest \
  test_contracts.OperationsContractCase.test_hardware_failure_stops_both_rollouts_and_removes_temporary_overlay \
  test_contracts.OperationsContractCase.test_a_failed_budget_proof_stops_the_rollout_before_it_touches_a_container \
  test_contracts.OperationsContractCase.test_docker_up_preserves_override_discovery_and_stamps_the_build
```

The hardware smoke used a source-only archive of the committed helper, a
separate temporary Compose project, and the existing runtime image. It created
no production mounts and changed no running service. Its container, network,
and temporary files were removed after the successful run.
