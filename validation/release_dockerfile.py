"""Replace a tagged Dockerfile's Rust build stage with verified binaries."""

from __future__ import annotations

import argparse
from pathlib import Path


RUNTIME_STAGE = "FROM debian:bookworm-slim"
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
    return RUNTIME_STAGE + source.split(RUNTIME_STAGE, 1)[1]


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


def render(source: str) -> str:
    runtime = _runtime(source)
    binaries = required_binaries(source)
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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--list-binaries", action="store_true")
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path, nargs="?")
    args = parser.parse_args()
    source = args.source.read_text(encoding="utf-8")
    if args.list_binaries:
        if args.output is not None:
            parser.error("--list-binaries does not accept an output path")
        for name in required_binaries(source):
            print(name)
        return 0
    if args.output is None:
        parser.error("output is required unless --list-binaries is used")
    args.output.write_text(render(source), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
