from __future__ import annotations

import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

from validation.release_artifact import BINARIES, create, verify
from validation.release_aliases import alias_action
from validation.release_dockerfile import (
    render,
    render_binary_export,
    required_binaries,
)


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/publish-release.yml"
ELF_MACHINES = {
    "x86_64-unknown-linux-gnu": 62,
    "aarch64-unknown-linux-gnu": 183,
}


def write_elf(path: Path, target: str, payload: bytes = b"fixture") -> None:
    header = bytearray(64)
    header[:4] = b"\x7fELF"
    header[4] = 2
    header[5] = 1
    header[6] = 1
    header[16:18] = (3).to_bytes(2, byteorder="little")
    header[18:20] = ELF_MACHINES[target].to_bytes(2, byteorder="little")
    path.write_bytes(header + payload)
    path.chmod(0o755)


class ReleasePublicationContractCase(unittest.TestCase):
    def test_release_source_is_immutable_and_bookworm_compatible(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")

        self.assertIn("workflow_call:", workflow)
        self.assertIn("workflow_dispatch:", workflow)
        self.assertIn('^v[0-9]+\\.[0-9]+\\.[0-9]+$', workflow)
        self.assertIn('git cat-file -t "refs/tags/$RELEASE_TAG"', workflow)
        self.assertIn('refs/tags/$RELEASE_TAG^{commit}', workflow)
        self.assertIn('EVENT_REF" != refs/heads/main', workflow)
        self.assertIn("ref: ${{ github.sha }}", workflow)
        self.assertIn("ref: ${{ needs.resolve.outputs.packaging_sha }}", workflow)
        self.assertNotIn("\n  binary:\n", workflow)
        image = workflow.split("\n  image:\n", 1)[1].split("\n  reuse:\n", 1)[0]
        self.assertIn("path: trusted-packaging", image)
        self.assertIn(
            'PYTHONPATH="$GITHUB_WORKSPACE/trusted-packaging"', image
        )
        self.assertIn("--binary-export", image)
        self.assertIn("--list-binaries", image)
        self.assertIn("target: release-binaries", image)
        self.assertIn("trusted-packaging/scripts/release-package-candidate", image)
        self.assertNotIn("actions/download-artifact", image)
        self.assertIn("PLURX_BUILD_REF=${{ needs.resolve.outputs.release_tag }}", image)
        self.assertIn("PLURX_BUILD_SHA=${{ needs.resolve.outputs.commit_sha }}", image)
        self.assertIn("plurx-cluster-check", image)
        self.assertIn("build-identity", image)
        self.assertIn("GLIBC_$max_glibc; Bookworm provides 2.36", workflow)

    def test_aliases_wait_for_both_smoked_platform_digests(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        image = workflow.split("\n  image:\n", 1)[1].split("\n  publish:\n", 1)[0]
        publish = workflow.split("\n  publish:\n", 1)[1]

        self.assertIn("push-by-digest=true", image)
        self.assertNotIn("cache-from:", image)
        self.assertNotIn("cache-to:", image)
        self.assertLess(
            image.index("Verify the pushed platform image"),
            image.index("release-image-digest-${{ matrix.arch }}"),
        )
        self.assertLess(
            image.index("release-image-digest-${{ matrix.arch }}"),
            image.index("release-binary-receipt-${{ matrix.arch }}"),
        )
        binary_receipt = image.split(
            "- name: Retain the exact tagged binary receipt for one day", 1
        )[1].split("\n  reuse:", 1)[0]
        self.assertIn("continue-on-error: true", binary_receipt)
        self.assertIn("needs: [resolve, image, reuse]", publish)
        self.assertIn("Reconfirm the remote tag has not moved", publish)
        self.assertIn("appeared after source resolution", publish)
        self.assertIn("registry state is indeterminate", publish)
        self.assertIn('grep -Fq "$ref" "$err"', workflow)
        self.assertNotIn("manifest unknown|name unknown|not found", workflow)
        self.assertIn("group: publish-release-aliases", publish)
        self.assertIn("python3 -m validation.release_aliases", publish)
        self.assertIn("verified-release-index-*", publish)
        self.assertIn('test "$candidate_digest" = "$verified_amd64"', publish)
        self.assertIn('test "$published_amd64" = "$(cat digests/amd64)"', publish)
        self.assertIn('test "$published_arm64" = "$(cat digests/arm64)"', publish)
        self.assertIn('source_ref="$REGISTRY_IMAGE@$candidate_digest"', publish)
        self.assertIn("for arch in amd64 arm64", publish)
        self.assertIn("{{.Os}}/{{.Architecture}}", publish)
        self.assertIn("org.opencontainers.image.source", publish)
        self.assertIn('scripts/container-smoke "$image_ref" >&2', publish)
        self.assertIn('test "$alias_digest" = "$immutable_digest"', publish)
        self.assertIn('remote_commit=$(printf', publish)
        self.assertIn('test "$remote_commit" = "$revision"', publish)
        self.assertNotIn("=$(alias_version", publish)
        self.assertIn(
            'alias_version "$minor_ref" minor.json minor_existing minor_before',
            publish,
        )
        self.assertIn("timeout-minutes: 30", publish)
        self.assertNotIn(
            '.manifests[].platform | select(.os == "linux")',
            workflow,
        )
        self.assertIn("if [ \"$minor_action\" = keep ]", publish)
        self.assertIn("if [ \"$latest_action\" = keep ]", publish)
        self.assertEqual(workflow.count("packages: write"), 2)

        reuse = workflow.split("\n  reuse:\n", 1)[1].split("\n  publish:\n", 1)[0]
        self.assertIn("version_exists == 'true'", reuse)
        self.assertIn("platform.architecture == $arch", reuse)
        self.assertIn("verified-release-index-${{ matrix.arch }}", reuse)
        self.assertIn("{{.Os}}/{{.Architecture}}", reuse)
        self.assertIn('verified_ref="$REGISTRY_IMAGE@$index_digest"', reuse)
        self.assertIn('test "$current_digest" = "$index_digest"', reuse)
        self.assertIn("scripts/container-smoke", reuse)

    def test_release_aliases_advance_monotonically(self):
        self.assertEqual(alias_action("minor", "0.2.7", None), "advance")
        self.assertEqual(alias_action("minor", "0.2.7", "0.2.7"), "advance")
        self.assertEqual(alias_action("minor", "0.2.7", "0.2.8"), "keep")
        self.assertEqual(alias_action("latest", "0.2.7", "0.3.0"), "keep")
        self.assertEqual(alias_action("latest", "0.3.0", "0.2.9"), "advance")
        with self.assertRaisesRegex(ValueError, "unrelated version"):
            alias_action("minor", "0.2.7", "0.3.0")
        with self.assertRaisesRegex(ValueError, "invalid release version"):
            alias_action("latest", "0.2.7", "main")

    def test_generated_dockerfile_keeps_only_the_tagged_runtime(self):
        generated = render((ROOT / "Dockerfile").read_text(encoding="utf-8"))

        self.assertIn("FROM debian:bookworm-slim", generated)
        self.assertIn(
            "COPY --chmod=0755 release-bin/plurxd /usr/local/bin/plurxd",
            generated,
        )
        self.assertIn(
            "COPY --chmod=0755 release-bin/plurx-cluster-check "
            "/usr/local/bin/plurx-cluster-check",
            generated,
        )
        self.assertNotIn("FROM rust:", generated)
        self.assertNotIn("cargo build", generated)
        self.assertNotIn("COPY --from=build", generated)

    def test_generator_refuses_an_unrecognized_runtime_contract(self):
        with self.assertRaisesRegex(ValueError, "one Bookworm runtime stage"):
            render("FROM alpine:3.22\n")

    def test_generator_supports_historical_one_binary_runtime(self):
        source = """FROM rust:1-bookworm AS build
FROM debian:bookworm-slim
COPY --from=build /plurxd /usr/local/bin/plurxd
"""

        self.assertEqual(required_binaries(source), ("plurxd",))
        generated = render(source)
        self.assertIn(
            "COPY --chmod=0755 release-bin/plurxd /usr/local/bin/plurxd",
            generated,
        )
        self.assertNotIn("plurx-cluster-check", generated)

    def test_generator_derives_current_two_binary_runtime(self):
        source = (ROOT / "Dockerfile").read_text(encoding="utf-8")

        self.assertEqual(required_binaries(source), BINARIES)
        exporter = render_binary_export(source)
        self.assertIn("FROM scratch AS release-binaries", exporter)
        self.assertIn("RUN rustc -Vv > /rustc-version", exporter)
        self.assertEqual(exporter.count("ARG PLURX_BUILD_SHA"), 1)
        for name in BINARIES:
            self.assertIn(f"COPY --from=build /{name} /{name}", exporter)
        self.assertNotIn("FROM debian:bookworm-slim", exporter)

    def test_trusted_helper_can_inspect_real_isolated_tag_checkouts(self):
        tagged_contracts = {
            "v0.2.7": ("plurxd",),
            "v0.3.0": BINARIES,
        }
        with tempfile.TemporaryDirectory() as raw_directory:
            tagged_checkout = Path(raw_directory)
            for tag, expected in tagged_contracts.items():
                with self.subTest(tag=tag):
                    source = subprocess.run(
                        ["git", "show", f"{tag}:Dockerfile"],
                        cwd=ROOT,
                        check=True,
                        capture_output=True,
                        text=True,
                    ).stdout
                    dockerfile = tagged_checkout / "Dockerfile"
                    dockerfile.write_text(source, encoding="utf-8")
                    result = subprocess.run(
                        [
                            sys.executable,
                            "-m",
                            "validation.release_dockerfile",
                            "--list-binaries",
                            str(dockerfile),
                        ],
                        cwd=ROOT,
                        check=True,
                        capture_output=True,
                        text=True,
                    )

                    self.assertEqual(result.stdout.splitlines(), list(expected))
                    exporter = render_binary_export(source)
                    self.assertEqual(
                        exporter.count("COPY --from=build /plurxd /plurxd"), 1
                    )
                    self.assertEqual(
                        "plurx-cluster-check" in exporter,
                        "plurx-cluster-check" in expected,
                    )
                    self.assertEqual(exporter.count("ARG PLURX_BUILD_SHA"), 1)

    def test_release_artifact_binds_both_binaries_to_the_candidate(self):
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            for name in BINARIES:
                write_elf(directory / name, "x86_64-unknown-linux-gnu", name.encode())
            manifest = create(
                directory,
                git_tree="1" * 40,
                git_commit="2" * 40,
                build_ref="v0.3.0",
                rustc="rustc 1.97.1 (fixture)",
                target="x86_64-unknown-linux-gnu",
                binary_names=BINARIES,
            )

            verified = verify(
                directory,
                git_tree="1" * 40,
                git_commit="2" * 40,
                build_ref="v0.3.0",
                target="x86_64-unknown-linux-gnu",
                binary_names=BINARIES,
            )

            self.assertEqual(verified, manifest)
            self.assertEqual(set(verified["binaries"]), set(BINARIES))

            (directory / "unexpected-binary").write_bytes(b"extra")
            with self.assertRaisesRegex(ValueError, "entry set mismatch"):
                verify(
                    directory,
                    git_tree="1" * 40,
                    git_commit="2" * 40,
                    build_ref="v0.3.0",
                    target="x86_64-unknown-linux-gnu",
                    binary_names=BINARIES,
                )

            (directory / "unexpected-binary").unlink()
            (directory / "unexpected-directory").mkdir()
            with self.assertRaisesRegex(ValueError, "entry set mismatch"):
                verify(
                    directory,
                    git_tree="1" * 40,
                    git_commit="2" * 40,
                    build_ref="v0.3.0",
                    target="x86_64-unknown-linux-gnu",
                    binary_names=BINARIES,
                )

    def test_release_artifact_rejects_tampering_and_wrong_tree(self):
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            for name in BINARIES:
                write_elf(directory / name, "x86_64-unknown-linux-gnu", name.encode())
            create(
                directory,
                git_tree="1" * 40,
                git_commit="2" * 40,
                build_ref="v0.3.0",
                rustc="rustc 1.97.1 (fixture)",
                target="x86_64-unknown-linux-gnu",
                binary_names=BINARIES,
            )

            with self.assertRaisesRegex(ValueError, "git_tree mismatch"):
                verify(
                    directory,
                    git_tree="3" * 40,
                    git_commit="2" * 40,
                    build_ref="v0.3.0",
                    target="x86_64-unknown-linux-gnu",
                    binary_names=BINARIES,
                )

            (directory / "plurx-cluster-check").write_bytes(b"tampered")
            with self.assertRaisesRegex(ValueError, "digest mismatch"):
                verify(
                    directory,
                    git_tree="1" * 40,
                    git_commit="2" * 40,
                    build_ref="v0.3.0",
                    target="x86_64-unknown-linux-gnu",
                    binary_names=BINARIES,
                )

    def test_release_artifact_supports_historical_one_binary_set(self):
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            write_elf(
                directory / "plurxd",
                "x86_64-unknown-linux-gnu",
                b"historical daemon",
            )
            manifest = create(
                directory,
                git_tree="1" * 40,
                git_commit="2" * 40,
                build_ref="v0.2.7",
                rustc="rustc 1.97.1 (fixture)",
                target="x86_64-unknown-linux-gnu",
                binary_names=("plurxd",),
            )

            verified = verify(
                directory,
                git_tree="1" * 40,
                git_commit="2" * 40,
                build_ref="v0.2.7",
                target="x86_64-unknown-linux-gnu",
                binary_names=("plurxd",),
            )

            self.assertEqual(verified, manifest)
            self.assertEqual(tuple(verified["binaries"]), ("plurxd",))

    def test_release_artifact_verifier_rejects_wrong_binary_machine(self):
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            for name in BINARIES:
                write_elf(directory / name, "x86_64-unknown-linux-gnu", name.encode())
            manifest = create(
                directory,
                git_tree="1" * 40,
                git_commit="2" * 40,
                build_ref="v0.3.0",
                rustc="rustc 1.97.1 (fixture)",
                target="x86_64-unknown-linux-gnu",
                binary_names=BINARIES,
            )

            binary = directory / "plurx-cluster-check"
            write_elf(binary, "aarch64-unknown-linux-gnu", b"wrong machine")
            digest = hashlib.sha256(binary.read_bytes()).hexdigest()
            manifest["binaries"]["plurx-cluster-check"]["sha256"] = digest
            (directory / "build-manifest.json").write_text(
                json.dumps(manifest, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )
            (directory / "plurx-cluster-check.sha256").write_text(
                f"{digest}  plurx-cluster-check\n", encoding="utf-8"
            )

            with self.assertRaisesRegex(ValueError, "machine mismatch"):
                verify(
                    directory,
                    git_tree="1" * 40,
                    git_commit="2" * 40,
                    build_ref="v0.3.0",
                    target="x86_64-unknown-linux-gnu",
                    binary_names=BINARIES,
                )

    def test_candidate_packager_binds_export_to_exact_source_tree(self):
        with tempfile.TemporaryDirectory() as raw_directory:
            fixture = Path(raw_directory)
            source = fixture / "source"
            export = fixture / "export"
            artifact = fixture / "artifact"
            source.mkdir()
            export.mkdir()
            shutil.copy2(ROOT / "Dockerfile", source / "Dockerfile")
            subprocess.run(["git", "init", "-q"], cwd=source, check=True)
            subprocess.run(
                ["git", "-c", "user.name=CI", "-c", "user.email=ci@example.test", "add", "Dockerfile"],
                cwd=source,
                check=True,
            )
            subprocess.run(
                [
                    "git",
                    "-c",
                    "user.name=CI",
                    "-c",
                    "user.email=ci@example.test",
                    "commit",
                    "-qm",
                    "fixture",
                ],
                cwd=source,
                check=True,
            )
            commit = subprocess.run(
                ["git", "rev-parse", "HEAD"],
                cwd=source,
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
            for name in BINARIES:
                write_elf(export / name, "x86_64-unknown-linux-gnu", name.encode())
            (export / "rustc-version").write_text(
                "rustc 1.97.1 (fixture)\nbinary: rustc\n", encoding="utf-8"
            )

            subprocess.run(
                [
                    str(ROOT / "scripts/release-package-candidate"),
                    str(source),
                    str(ROOT),
                    str(export),
                    str(artifact),
                    "x86_64-unknown-linux-gnu",
                    commit,
                    commit,
                ],
                check=True,
            )

            self.assertEqual(
                {path.name for path in artifact.iterdir()},
                {
                    "build-manifest.json",
                    "plurxd",
                    "plurxd.sha256",
                    "plurx-cluster-check",
                    "plurx-cluster-check.sha256",
                },
            )
            self.assertIn(
                "COPY --chmod=0755 release-bin/plurxd",
                (source / "Dockerfile.release").read_text(encoding="utf-8"),
            )
            wrong_tree = subprocess.run(
                [
                    str(ROOT / "scripts/release-package-candidate"),
                    str(source),
                    str(ROOT),
                    str(export),
                    str(fixture / "wrong"),
                    "x86_64-unknown-linux-gnu",
                    commit,
                    "f" * 40,
                ],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(wrong_tree.returncode, 0)
            self.assertIn("does not match", wrong_tree.stderr)


if __name__ == "__main__":
    unittest.main()
