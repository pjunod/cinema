"""Replace a tagged Dockerfile's Rust build stage with verified binaries."""

from __future__ import annotations

import argparse
import re
from pathlib import Path


RUNTIME_STAGE = "FROM debian:bookworm-slim"
RUNTIME_FINAL_STAGE = "FROM runtime-assets AS runtime"
IMMUTABLE_IMAGE = re.compile(
    r"^[a-zA-Z0-9][a-zA-Z0-9._:/-]*@sha256:[0-9a-f]{64}$"
)
SUPPORTED_BINARY_COPIES = (
    (
        "plurxd",
        "COPY --from=build /plurxd /usr/local/bin/plurxd",
        "COPY --chmod=0755 release-bin/plurxd /usr/local/bin/plurxd",
    ),
    (
        "plurx-cluster-check",
        "COPY --from=build /plurx-cluster-check "
        "/usr/local/bin/plurx-cluster-check",
        "COPY --chmod=0755 release-bin/plurx-cluster-check "
        "/usr/local/bin/plurx-cluster-check",
    ),
)


def _runtime(source: str) -> str:
    if source.count(RUNTIME_STAGE) != 1:
        raise ValueError("tagged Dockerfile must contain one Bookworm runtime stage")
    runtime = RUNTIME_STAGE + source.split(RUNTIME_STAGE, 1)[1]
    final_stage = "FROM runtime-assets AS runtime"
    if final_stage not in runtime:
        return runtime  # Historical one-stage release Dockerfiles.
    if runtime.count(final_stage) != 1:
        raise ValueError("tagged Dockerfile must contain one final runtime stage")
    assets, final = runtime.split(final_stage, 1)
    # CI-only stages may sit between runtime-assets and the shipped runtime.
    # The release packager needs the assets and final stage, never their
    # toolchains; the default Dockerfile still ends in the shipped runtime.
    intervening = re.search(r"(?m)^FROM ", assets[len(RUNTIME_STAGE) :])
    if intervening:
        assets = assets[: len(RUNTIME_STAGE) + intervening.start()]
    return assets + final_stage + final


def required_binaries(source: str) -> tuple[str, ...]:
    """Return the exact supported binary set copied by the tagged runtime."""

    runtime = _runtime(source)
    unrecognized = runtime
    binaries: list[str] = []
    for name, source_copy, _artifact_copy in SUPPORTED_BINARY_COPIES:
        count = runtime.count(source_copy)
        if name == "plurxd" and count != 1:
            raise ValueError("tagged runtime must copy one /plurxd build artifact")
        if count > 1:
            raise ValueError(f"tagged runtime copies /{name} more than once")
        if count == 1:
            binaries.append(name)
        unrecognized = unrecognized.replace(source_copy, "")
    if "COPY --from=build" in unrecognized:
        raise ValueError("tagged runtime copies an unsupported build artifact")
    return tuple(binaries)


def render(source: str, runtime_image: str | None = None) -> str:
    runtime = _runtime(source)
    binaries = required_binaries(source)
    if runtime_image is not None:
        if not IMMUTABLE_IMAGE.fullmatch(runtime_image):
            raise ValueError("media runtime image must have an immutable sha256 digest")
        if runtime.count(RUNTIME_FINAL_STAGE) != 1:
            raise ValueError("tagged Dockerfile must contain one final runtime stage")
        runtime = (
            f"FROM {runtime_image} AS runtime-assets\n\n"
            + RUNTIME_FINAL_STAGE
            + runtime.split(RUNTIME_FINAL_STAGE, 1)[1]
        )
    for name, source_copy, artifact_copy in SUPPORTED_BINARY_COPIES:
        if name in binaries:
            runtime = runtime.replace(source_copy, artifact_copy)
    generated = (
        "# syntax=docker/dockerfile:1\n\n"
        "# Generated from the tagged runtime stage by "
        "validation.release_dockerfile.\n"
        + runtime
    )
    forbidden = ("FROM rust:", "cargo build", "COPY --from=build")
    found = [token for token in forbidden if token in generated]
    if found:
        raise ValueError(f"generated packaging Dockerfile retained build tokens: {found}")
    return generated


def render_binary_export(source: str) -> str:
    """Keep the tagged build stage and export only its runtime binaries."""

    binaries = required_binaries(source)
    build_stage = source.split(RUNTIME_STAGE, 1)[0].rstrip()
    if " AS build" not in build_stage:
        raise ValueError("tagged Dockerfile must name its Rust stage build")
    if "ARG PLURX_BUILD_SHA" not in build_stage:
        marker = "WORKDIR /src\n"
        if build_stage.count(marker) != 1:
            raise ValueError("tagged Dockerfile must contain one /src build workdir")
        build_stage = build_stage.replace(
            marker,
            'ARG PLURX_BUILD_SHA=""\n'
            "ENV PLURX_BUILD_SHA=${PLURX_BUILD_SHA}\n"
            + marker,
        )
    copies = "\n".join(
        f"COPY --from=build /{name} /{name}" for name in binaries
    )
    return (
        build_stage
        + "\nRUN rustc -Vv > /rustc-version\n\n"
        + "FROM scratch AS release-binaries\n"
        + copies
        + "\nCOPY --from=build /rustc-version /rustc-version\n"
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--list-binaries", action="store_true")
    mode.add_argument("--binary-export", action="store_true")
    parser.add_argument("--runtime-image")
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path, nargs="?")
    args = parser.parse_args()
    source = args.source.read_text(encoding="utf-8")
    if args.runtime_image and (args.binary_export or args.list_binaries):
        parser.error("--runtime-image is only valid when rendering packaging")
    if args.list_binaries:
        if args.output is not None:
            parser.error("--list-binaries does not accept an output path")
        for name in required_binaries(source):
            print(name)
        return 0
    if args.output is None:
        parser.error("output is required unless --list-binaries is used")
    generated = (
        render_binary_export(source)
        if args.binary_export
        else render(source, args.runtime_image)
    )
    args.output.write_text(generated, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
