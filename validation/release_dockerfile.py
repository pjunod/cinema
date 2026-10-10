"""Replace a tagged Dockerfile's Rust build stage with verified binaries."""

from __future__ import annotations

import argparse
import re
from dataclasses import dataclass
from pathlib import Path


RUNTIME_STAGE = "FROM debian:bookworm-slim"
RUNTIME_ASSETS = "runtime-assets"
RUNTIME_FINAL_STAGE = "FROM runtime-assets AS runtime"
BUILD_STAGE = "build"
_FROM = re.compile(
    r"(?im)^[ \t]*FROM[ \t]+(?:--platform=\S+[ \t]+)?(\S+)(?:[ \t]+AS[ \t]+(\S+))?[ \t]*$"
)
_COPY_FROM = re.compile(r"--from=([^\s,]+)")
_MOUNT = re.compile(r"--mount=(\S+)")
_LEAD = re.compile(r"(?:^[ \t]*(?:#[^\n]*)?\n)*\Z", re.M)
_SOURCE_COPY = re.compile(r"(?im)^[ \t]*(?:COPY|ADD)[ \t]+(.+)$")
# The daemon's own sources. A retained runtime-asset stage that can see them
# could compile the daemon, which packaging must take only as verified binaries.
_DAEMON_SOURCES = ("Cargo.toml", "Cargo.lock", "crates")
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
SUPPORTED_DEBUG_COPIES = tuple(
    (name, f"COPY --from=build /{name}.dwp /usr/local/bin/{name}.dwp",
     f"COPY --chmod=0644 release-bin/{name}.dwp /usr/local/bin/{name}.dwp")
    for name, _, _ in SUPPORTED_BINARY_COPIES
)


@dataclass(frozen=True)
class _Stage:
    lead: int  # Start of the comment block written above the FROM line.
    start: int  # The FROM line itself.
    end: int  # Where the next stage's comment block begins.
    base: str
    name: str | None


def _stages(source: str) -> tuple[_Stage, ...]:
    """Every FROM stage with its source span, in Dockerfile order.

    A stage's comments sit above its FROM line, so each span starts at that
    comment block and ends where the next stage's block begins.
    """

    matches = list(_FROM.finditer(source))
    leads = []
    for index, match in enumerate(matches):
        floor = matches[index - 1].end() if index else 0
        block = _LEAD.search(source, floor, match.start())
        leads.append(block.start() if block is not None and index else match.start())
    return tuple(
        _Stage(
            leads[index],
            match.start(),
            leads[index + 1] if index + 1 < len(matches) else len(source),
            match.group(1),
            match.group(2),
        )
        for index, match in enumerate(matches)
    )


def _copy_sources(text: str) -> list[str]:
    """Stage or image names that `text` copies or mounts from."""

    names = _COPY_FROM.findall(text)
    for mount in _MOUNT.findall(text):
        names += [
            field[len("from="):] for field in mount.split(",") if field.startswith("from=")
        ]
    return names


def _sees_daemon_sources(text: str) -> bool:
    for line in _SOURCE_COPY.findall(text):
        arguments = [part for part in line.split() if not part.startswith("--")]
        if "--from=" in line or len(arguments) < 2:
            continue
        for argument in arguments[:-1]:
            path = argument.removeprefix("./").rstrip("/")
            if path in ("", ".") or path.split("/")[0] in _DAEMON_SOURCES:
                return True
    return False


def _runtime_start(source: str) -> int:
    """Locate the shipped runtime by its stage name, not by its base image.

    Stages that build or check private runtime assets may share the Bookworm
    base, so a count of Bookworm bases cannot identify the runtime. Tags that
    predate the `runtime-assets` name had exactly one Bookworm stage, and that
    stage is still how they are recognized.
    """

    stages = _stages(source)
    named = [stage for stage in stages if stage.name == RUNTIME_ASSETS]
    if len(named) > 1:
        raise ValueError("tagged Dockerfile must contain one runtime-assets stage")
    if named:
        if not named[0].base.startswith(RUNTIME_STAGE[len("FROM "):]):
            raise ValueError("tagged runtime-assets stage must use the Bookworm runtime base")
        return named[0].start
    bookworm = [
        stage for stage in stages
        if source[stage.start:].startswith(RUNTIME_STAGE)
    ]
    if len(bookworm) != 1:
        raise ValueError("tagged Dockerfile must contain one Bookworm runtime stage")
    return bookworm[0].start


def _references(text: str) -> list[str]:
    """Stage or image names that `text` copies from or builds upon."""

    return _copy_sources(text) + [stage.base for stage in _stages(text)]


def _stage_closure(
    source: str, kept: str, roots: list[str], *, include_build: bool
) -> str:
    """Return the pre-runtime stages reachable from `roots`, in source order.

    Packaging keeps a stage only when retained text names it. Verified binaries
    replace the Rust `build` stage, so rendering may never reach it, and a
    retained stage may not see the daemon's sources; the binary export starts
    from `build` instead. A name that is neither a stage written into the
    output nor a pinned image is an unbuildable copy, and is refused rather
    than written into the output.
    """

    runtime_start = _runtime_start(source)
    stages = _stages(source)
    before = {
        stage.name: stage
        for stage in stages
        if stage.name is not None and stage.start < runtime_start
    }
    emitted = {stage.name for stage in _stages(kept) if stage.name is not None}
    selected: set[str] = set()
    pending = list(roots)
    while pending:
        name = pending.pop()
        if name in selected or name in emitted or name == "scratch":
            continue
        if name not in before and any(mark in name for mark in ":@"):
            continue  # A pinned image, not a stage of this Dockerfile.
        if name not in before:
            raise ValueError(
                f"tagged Dockerfile copies from {name}, which is neither a stage "
                "the packaging output keeps nor a pinned image"
            )
        if name == BUILD_STAGE and not include_build:
            raise ValueError("runtime assets must not depend on the Rust build stage")
        stage = before[name]
        body = source[stage.start:stage.end]
        if not include_build and _sees_daemon_sources(body):
            raise ValueError(f"runtime asset stage {name} copies the daemon's sources")
        selected.add(name)
        pending.append(stage.base)
        pending.extend(_copy_sources(body))
    return "".join(
        source[stage.lead:stage.end]
        for stage in stages
        if stage.name in selected and stage.start < runtime_start
    )


def _runtime(source: str) -> str:
    runtime = source[_runtime_start(source):]
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
    for _name, source_copy, _artifact_copy in SUPPORTED_DEBUG_COPIES:
        unrecognized = unrecognized.replace(source_copy, "")
    if "COPY --from=build" in unrecognized:
        raise ValueError("tagged runtime copies an unsupported build artifact")
    return tuple(binaries)


def required_debug_binaries(source: str) -> tuple[str, ...]:
    """Historical tags have no DWP; profile C must retain the whole binary set."""
    runtime = _runtime(source)
    binaries = required_binaries(source)
    debug = []
    for name, source_copy, _ in SUPPORTED_DEBUG_COPIES:
        count = runtime.count(source_copy)
        if count > 1:
            raise ValueError("tagged runtime copies packed debug more than once")
        if count:
            debug.append(name)
    if debug and tuple(debug) != binaries:
        raise ValueError("tagged runtime must retain packed debug for every binary")
    return tuple(debug)


def render(source: str, runtime_image: str | None = None) -> str:
    runtime = _runtime(source)
    binaries = required_binaries(source)
    debug = required_debug_binaries(source)
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
    for name, source_copy, artifact_copy in SUPPORTED_DEBUG_COPIES:
        if name in debug:
            runtime = runtime.replace(source_copy, artifact_copy)
    # A runtime that copies prebuilt private assets keeps the stages producing
    # them; the published runtime image of a digest-bound release already
    # contains them, so that form keeps none.
    dependencies = _stage_closure(source, runtime, _references(runtime), include_build=False)
    generated = (
        "# syntax=docker/dockerfile:1\n\n"
        "# Generated from the tagged runtime stage by "
        "validation.release_dockerfile.\n"
        + dependencies
        + runtime
    )
    forbidden = ("cargo build", "COPY --from=build")
    found = [token for token in forbidden if token in generated]
    if any(stage.name == BUILD_STAGE for stage in _stages(generated)):
        found.append(f"AS {BUILD_STAGE}")
    if found:
        raise ValueError(f"generated packaging Dockerfile retained build tokens: {found}")
    return generated


def render_binary_export(source: str) -> str:
    """Keep the tagged build stage and export only its runtime binaries."""

    binaries = required_binaries(source)
    debug = required_debug_binaries(source)
    runtime_start = _runtime_start(source)
    stages = _stages(source)
    if not any(stage.name == BUILD_STAGE and stage.start < runtime_start for stage in stages):
        raise ValueError("tagged Dockerfile must name its Rust stage build")
    # The export compiles the daemon binaries and nothing else: the stage
    # named build and whatever it derives from, never the runtime's assets.
    preamble = source[:stages[0].lead]
    build_stage = (
        preamble + _stage_closure(source, "", [BUILD_STAGE], include_build=True)
    ).rstrip()
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
    copies += "".join(f"\nCOPY --from=build /{name}.dwp /{name}.dwp" for name in debug)
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
    mode.add_argument("--list-debug-binaries", action="store_true")
    mode.add_argument("--binary-export", action="store_true")
    parser.add_argument("--runtime-image")
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path, nargs="?")
    args = parser.parse_args()
    source = args.source.read_text(encoding="utf-8")
    if args.runtime_image and (args.binary_export or args.list_binaries or args.list_debug_binaries):
        parser.error("--runtime-image is only valid when rendering packaging")
    if args.list_binaries or args.list_debug_binaries:
        if args.output is not None:
            parser.error("--list-binaries does not accept an output path")
        for name in (required_debug_binaries(source) if args.list_debug_binaries else required_binaries(source)):
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
