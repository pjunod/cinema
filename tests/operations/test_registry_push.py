"""Exercise exact-image publication with a disposable, deterministic registry."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[2] / "scripts/registry-push"
IMAGE_ID = "sha256:" + "a" * 64
DIGEST = "sha256:" + "b" * 64
OTHER = "sha256:" + "c" * 64
DOCKER = """#!/usr/bin/env bash
set -eu
echo "$*" >> "$TRANSCRIPT"
case "$1 $2" in
  'buildx imagetools')
    if [[ "$SCENARIO" == missing || ( "$SCENARIO" == build && ! -f "$BUILT" ) ]]; then echo 'manifest unknown' >&2; exit 1; fi
    if [[ "$SCENARIO" == moved && -f "$PULLED" ]]; then echo "Digest: $OTHER"; else echo "Digest: $DIGEST"; fi ;;
  'pull --platform') touch "$PULLED" ;;
  'image inspect')
    case "$4" in
      '{{.Id}}') if [[ "$SCENARIO" == conflict ]]; then echo "$OTHER"; else echo "$EXPECTED"; fi ;;
      *Architecture*) echo linux/amd64 ;;
      *revision*) echo "$PLURX_BUILD_SHA" ;;
      *source*) echo "$PLURX_SOURCE_URL" ;;
      *) exit 2 ;;
    esac ;;
  'run --rm') if [[ "${!#}" == --version ]]; then echo "plurxd ($PLURX_BUILD_REF)"; else echo "$PLURX_BUILD_SHA"; fi ;;
  'build '*) touch "$BUILT" ;;
  'tag '*|'push '*) ;;
  *) exit 2 ;;
esac
"""


class RegistryPushCase(unittest.TestCase):
    def test_source_publication_build_forwards_registry_without_changing_the_media_pin(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                            "-c", "core.hooksPath=/dev/null", "commit", "--allow-empty",
                            "-qm", "fixture"], cwd=root, check=True)
            sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
            (root / "scripts").mkdir()
            docker = root / "docker"
            docker.write_text(DOCKER)
            docker.chmod(0o755)
            smoke = root / "scripts/container-smoke"
            smoke.write_text('#!/bin/sh\necho "smoke $1" >> "$TRANSCRIPT"\n')
            smoke.chmod(0o755)
            for override in (None, "mirror.invalid:5000"):
                with self.subTest(media_registry=override):
                    suffix = "default" if override is None else "mirror"
                    transcript = root / (suffix + ".log")
                    env = dict(os.environ, PATH=f"{root}:{os.environ['PATH']}",
                               PLURX_REGISTRY="registry.invalid:3000", PLURX_BUILD_SHA=sha,
                               PLURX_BUILD_REF=sha, PLURX_SOURCE_URL="https://forge.invalid/repo",
                               DIGEST=DIGEST, EXPECTED=IMAGE_ID, OTHER=OTHER, SCENARIO="build",
                               BUILT=str(root / (suffix + ".built")),
                               PULLED=str(root / (suffix + ".pulled")), TRANSCRIPT=str(transcript))
                    env.pop("PLURX_EXPECTED_IMAGE_ID", None)
                    env.pop("PLURX_MEDIA_REGISTRY", None)
                    if override is not None:
                        env["PLURX_MEDIA_REGISTRY"] = override
                    result = subprocess.run([str(SCRIPT)], cwd=root, env=env,
                                            text=True, capture_output=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    calls = transcript.read_text().splitlines()
                    builds = [call for call in calls if call.startswith("build ")]
                    self.assertEqual(len(builds), 1, calls)
                    self.assertIn("--build-arg PLURX_MEDIA_REGISTRY=" + (override or "registry.invalid:3000"), builds[0])
                    self.assertIn("--build-arg PLURX_BUILD_SHA=" + sha, builds[0])
                    self.assertNotIn("PLURX_MEDIA_RUNTIME_DIGEST=", builds[0])
                    self.assertFalse(any("Dockerfile.media-runtime" in call for call in calls))
                    self.assertFalse(any("linux-dolby-package" in call for call in calls))
                    smoke_index = calls.index("smoke registry.invalid:3000/noirr/plurxd:sha-" + sha[:12])
                    self.assertGreater(calls.index("tag registry.invalid:3000/noirr/plurxd:sha-" + sha[:12] + " registry.invalid:3000/noirr/plurxd:main"), smoke_index)

    def test_verified_publication_requires_the_reviewed_manifest_and_configuration(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                            "-c", "core.hooksPath=/dev/null", "commit", "--allow-empty",
                            "-qm", "fixture"], cwd=root, check=True)
            sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
            (root / "scripts").mkdir()
            docker = root / "docker"
            docker.write_text(DOCKER)
            docker.chmod(0o755)
            smoke = root / "scripts/container-smoke"
            smoke.write_text('#!/bin/sh\necho "smoke $1" >> "$TRANSCRIPT"\n')
            smoke.chmod(0o755)
            ref = "registry.invalid/noirr/plurxd"
            immutable = f"{ref}:sha-{sha[:12]}"
            for scenario in ("missing", "conflict", "moved", "empty", "malformed", "match"):
                with self.subTest(scenario=scenario):
                    transcript = root / f"{scenario}.log"
                    transcript.touch()
                    env = dict(os.environ, PATH=f"{root}:{os.environ['PATH']}",
                               PLURX_REGISTRY="registry.invalid", PLURX_BUILD_SHA=sha,
                               PLURX_BUILD_REF=sha, PLURX_SOURCE_URL="https://forge.invalid/repo",
                               PLURX_EXPECTED_IMAGE_ID={"empty": "", "malformed": "sha256:x"}.get(scenario, IMAGE_ID),
                               DIGEST=DIGEST, EXPECTED=IMAGE_ID, OTHER=OTHER, SCENARIO=scenario,
                               PULLED=str(root / f"{scenario}.pulled"), TRANSCRIPT=str(transcript))
                    result = subprocess.run([str(SCRIPT)], cwd=root, env=env,
                                            text=True, capture_output=True)
                    calls = transcript.read_text().splitlines()
                    self.assertFalse(any(call.startswith("build ") for call in calls))
                    if scenario != "match":
                        self.assertNotEqual(result.returncode, 0)
                        self.assertFalse(any(call.startswith(("tag ", "push ")) for call in calls))
                        continue
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn(f"pull --platform linux/amd64 {ref}@{DIGEST}", calls)
                    self.assertIn(f"image inspect --format {{{{.Id}}}} {ref}@{DIGEST}", calls)
                    self.assertIn(f"smoke {IMAGE_ID}", calls)
                    self.assertIn(f"tag {IMAGE_ID} {ref}:main", calls)
                    self.assertEqual(calls[-2:], [f"push {ref}:main", f"buildx imagetools inspect {ref}:main"])
                    self.assertEqual(calls.count(f"buildx imagetools inspect {immutable}"), 2)
                    self.assertTrue(all(IMAGE_ID in call for call in calls if call.startswith("run ")))
