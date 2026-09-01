"""Replace a tagged Dockerfile's Rust build stage with verified binaries."""

from __future__ import annotations

import argparse
from pathlib import Path


RUNTIME_STAGE = "FROM debian:bookworm-slim"
BINARY_COPIES = (
    (
        "COPY --from=build /plurxd /usr/local/bin/plurxd",
        "COPY --chmod=0755 release-bin/plurxd /usr/local/bin/plurxd",
    ),
    (
        "COPY --from=build /plurx-cluster-check "
        "/usr/local/bin/plurx-cluster-check",
        "COPY --chmod=0755 release-bin/plurx-cluster-check "
        "/usr/local/bin/plurx-cluster-check",
    ),
)


def render(source: str) -> str:
    if source.count(RUNTIME_STAGE) != 1:
        raise ValueError("tagged Dockerfile must contain one Bookworm runtime stage")
    runtime = RUNTIME_STAGE + source.split(RUNTIME_STAGE, 1)[1]
    for source_copy, artifact_copy in BINARY_COPIES:
        if runtime.count(source_copy) != 1:
            binary = source_copy.split(" /", 1)[1].split(" ", 1)[0]
            raise ValueError(f"tagged runtime must copy one /{binary} build artifact")
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
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.write_text(render(args.source.read_text(encoding="utf-8")), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
