"""Select the expensive CI jobs justified by a pull-request diff."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from validation.runner import (
    Catalog,
    CatalogError,
    REPO_ROOT,
    changed_paths,
    load_catalog,
    matches,
    select_points,
    selected_checks,
)


SCOPE_KEYS = (
    "rust",
    "apple",
    "android_jvm",
    "android_device",
    "web_layout",
    "release_build",
    "container",
    "mobile_version",
    "hiqlite_spike",
    "cluster_auth",
    "docs_only",
)

# Selector and aggregate-workflow edits can suppress evidence, so exercise the
# four Rust lanes that pin routing and cluster behavior. Static preflight pins
# the workflow structure itself; unrelated clients, containers, and cross
# builds still receive their full proof on merge_group and nightly runs.
#
# One of these entries is judged by content instead of by name. When
# `validation/points.toml` is the only routing path a diff touches, the
# replicated Store lane is forced only when the catalog edit could actually
# hide it: the `paths` or `checks` list of a cluster point, the presence or
# absence of one of those points, or any field of the `cluster-auth` check.
# A `contract` prose edit, or an edit to an unrelated point, can suppress
# nothing, and that lane costs 26-29 minutes of queue and runtime. Every other
# routing path — the two CI workflows, the lint workflow, this selector, and
# the runner — still forces the lane unconditionally, because a change to the
# job graph or to selection itself is not reducible to a catalog comparison.
# The narrowing applies to `cluster_auth` alone: `rust` stays forced for every
# routing path, `points.toml` included. See
# `catalog_edit_forces_cluster_lanes`, where every uncertainty resolves to
# forcing the lane.
CI_ROUTING_PATHS = (
    ".github/workflows/ci.yml",
    ".github/workflows/effort-ci.yml",
    ".github/workflows/lint.yml",
    "validation/ci_scope.py",
    "validation/points.toml",
    "validation/runner.py",
)

# The routing entry whose edits are judged by content, and the catalog
# selectors a catalog edit could use to hide the replicated Store lane: the
# three points `scope_for_paths` reads for `cluster_auth`, and the check they
# name as that lane's evidence.
CATALOG_ROUTING_PATH = "validation/points.toml"
CLUSTER_LANE_POINTS = ("cluster.auth", "cluster.membership", "cluster.page-reads")
CLUSTER_LANE_CHECK = "cluster-auth"

FFMPEG_ACTION_PATHS = (".github/actions/ffmpeg/**",)
PLAYWRIGHT_ACTION_PATHS = (".github/actions/playwright/**",)

# Documentation can still be executable evidence: validation unit tests pin
# inventories and links between docs and source anchors. The regression map is
# also allowed because it is consumed only by the history audit that remains in
# preflight; unlike the catalog and selector code, it cannot select or suppress
# a runtime suite. Keep the required workflow alive for these PRs, but route
# them through fast contracts rather than compilers and environment suites.
DOCS_ONLY_PATHS = (
    "**/*.md",
    ".github/**/*.md",
    "docs/**",
    "LICENSE",
    "LICENSE.*",
    "NOTICE",
    "NOTICE.*",
    "validation/regressions.d/**",
)

# Markdown under a shipped source tree is still a release-path change. Client
# build counters and crate packaging rules own those trees independently of a
# file's extension, so the documentation lane must never hide them.
SHIPPED_SOURCE_PATHS = (
    "clients/**",
    "crates/**",
)

# The pull-request lane is the iteration loop, so client suites run only when
# a diff can reach their compiled sources; the impact graph's server→client
# fan-out still runs where it belongs — on every merge_group and push event,
# which resolve through all_scope() before a commit can land on main. The one
# cross-surface file that compiles into BOTH native clients is the shared wire
# fixture: editing it re-runs both client suites on the PR itself.
APPLE_PATHS = (
    "clients/apple/**",
    "tests/contracts/native-api.json",
    "tests/playback/player-input-contract.json",
    "tests/playback/playback-info-fields.json",
)

ANDROID_JVM_PATHS = (
    "clients/android/**",
    "tests/contracts/native-api.json",
    "tests/playback/player-input-contract.json",
    "tests/playback/playback-info-fields.json",
)

# The layout golden is exercised by booting the real server, so its inputs are
# the embedded web sources and the tooling that drives or grades the sweep —
# the `web.experience` point's compiled surface, minus prose.
WEB_LAYOUT_PATHS = (
    "brand/tokens.css",
    "crates/plurxd/src/http/web.rs",
    "crates/plurxd/src/web/**",
    "scripts/contrast-*",
    "scripts/control-reporter-browser-check",
    "scripts/js-check",
    "scripts/themes-proposed.json",
    "scripts/ui-baseline",
    "tests/playback/player-input-contract.json",
    "tests/playback/playback-info-fields.json",
    "tests/ui-structure.golden",
)

# The cargo gate cannot be affected by native-client sources: a Kotlin or
# Swift diff compiles nothing under crates/. Everything else — scripts,
# deploy, validation, vendor — keeps the Rust lane, because those trees feed
# build, packaging, or selection behavior the workspace suite pins.
RUST_EXEMPT_PATHS = ("clients/**",)

# Device tests prove Android UI, focus, and packaging behavior. Server-only
# changes can select the Android consumer through the impact graph, but they do
# not change those on-device contracts and therefore do not justify an emulator.
ANDROID_DEVICE_PATHS = (
    "clients/android/Dockerfile",
    "clients/android/app/build.gradle.kts",
    "clients/android/app/src/main/**",
    "clients/android/app/src/androidTest/**",
    "clients/android/build.gradle.kts",
    "clients/android/gradle.properties",
    "clients/android/gradle/**",
    "clients/android/gradlew",
    "clients/android/settings.gradle.kts",
)

# The package-smoke job compiles in the Dockerfile's Bookworm stage and then
# packages those exact exported binaries. Its selector is deliberately broader
# than ordinary Rust source: any input that can change the builder, binary
# contract, artifact identity, runtime image, or smoke lifecycle selects it.
RELEASE_BUILD_PATHS = (
    ".dockerignore",
    ".github/actions/buildx-cache/**",
    ".github/buildkitd.toml",
    ".github/workflows/ci.yml",
    ".github/workflows/publish-release.yml",
    "Cargo.lock",
    "Cargo.toml",
    "**/Cargo.toml",
    "**/build.rs",
    "Dockerfile",
    "rust-toolchain.toml",
    "scripts/ci-buildkit-prune",
    "scripts/ci-execution-mode",
    "scripts/release-package-candidate",
    "tests/operations/test_release_publication.py",
    "validation/release_artifact.py",
    "validation/release_dockerfile.py",
    "validation/ci_scope.py",
    "vendor/**",
)

# The container smoke test additionally owns image, Compose, runtime-config,
# and lifecycle behavior.
CONTAINER_PATHS = (
    ".dockerignore",
    "Cargo.lock",
    "Cargo.toml",
    "Dockerfile",
    "deploy/**",
    "plurx.example.toml",
    "rust-toolchain.toml",
    "scripts/container-smoke",
)


def all_scope() -> dict[str, bool]:
    """Fail open when a diff cannot be trusted or the event is not a PR."""

    scope = {key: True for key in SCOPE_KEYS}
    scope["docs_only"] = False
    return scope


def is_docs_only(paths: tuple[str, ...]) -> bool:
    """Return true only when a non-empty diff cannot change shipped code."""

    return bool(paths) and all(
        matches(path, DOCS_ONLY_PATHS)
        and not matches(path, SHIPPED_SOURCE_PATHS)
        for path in paths
    )


def needs_rust_gate(paths: tuple[str, ...]) -> bool:
    """Fail open unless every changed path provably skips the cargo suite."""

    if not paths:
        return True
    return not all(
        matches(path, RUST_EXEMPT_PATHS)
        or (
            matches(path, DOCS_ONLY_PATHS)
            and not matches(path, SHIPPED_SOURCE_PATHS)
        )
        for path in paths
    )


def _git(repo_root: Path, *arguments: str) -> bytes:
    """Read from Git without inheriting an outer repository's environment.

    Git exports repository-local variables such as GIT_INDEX_FILE to hooks, and
    this helper runs against whatever repository `repo_root` names — the tests
    build their own. An inherited path would silently answer for the wrong tree.
    """

    environment = os.environ.copy()
    for name in (
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_PREFIX",
        "GIT_SHALLOW_FILE",
        "GIT_WORK_TREE",
    ):
        environment.pop(name, None)
    result = subprocess.run(
        ["git", *arguments],
        cwd=repo_root,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
    )
    return result.stdout


def _catalog_at_revision(repo_root: Path, revision: str) -> Catalog:
    """Parse `points.toml` as of `revision` with the catalog's own parser.

    `load_catalog` reads a path rather than a buffer, so the blob is staged in
    a temporary file. Reusing the parser is the point: the base side then has
    to satisfy exactly the same contract as the working tree, and any TOML or
    shape error it does not satisfy surfaces as `CatalogError`.
    """

    blob = _git(repo_root, "show", f"{revision}:{CATALOG_ROUTING_PATH}")
    with tempfile.TemporaryDirectory() as directory:
        staged = Path(directory) / "points.toml"
        staged.write_bytes(blob)
        return load_catalog(staged)


def _cluster_lane_selectors(catalog: Catalog) -> tuple[object, object]:
    """Reduce a catalog to everything a diff could use to hide the Store lane.

    The points map deliberately carries `paths` and `checks` only. A point's
    `contract` is prose that documents the lane rather than selecting it, and a
    point that disappears drops out of the mapping, so both the content and the
    presence of the three cluster points are compared. The check is compared as
    a whole record because every field on it — command, profiles, platforms,
    requires, timeout — can decide whether that evidence runs.
    """

    points = {
        point.id: (point.paths, point.checks)
        for point in catalog.points
        if point.id in CLUSTER_LANE_POINTS
    }
    check_map = catalog.check_map
    return (
        tuple(sorted(points.items())),
        check_map.get(CLUSTER_LANE_CHECK),
    )


def catalog_edit_forces_cluster_lanes(
    catalog: Catalog, base: str, *, repo_root: Path = REPO_ROOT
) -> bool:
    """Decide a catalog-only routing edit by what it changed, not by its name.

    Answers the one question `scope_for_paths` cannot ask from paths alone:
    between `base` and the working tree, did this `points.toml` edit touch a
    selector that could suppress the replicated Store lane? `catalog` is the
    working-tree catalog the caller is already scoping with.

    Every uncertainty answers yes. An unreadable base blob, a base that will
    not parse, an unexpected shape, a missing merge base, any exception at all:
    the lane runs. A narrowing that failed closed would silently drop cluster
    evidence, which is far worse than a wasted half hour, so no exception is
    allowed to become a `False` here.
    """

    try:
        # `changed_paths` diffs `base...HEAD`, so the comparison baseline is
        # the merge base, not the tip of a base branch that has moved on.
        merge_base = _git(repo_root, "merge-base", base, "HEAD").decode().strip()
        if not merge_base:
            raise CatalogError(f"no merge base with {base}")
        base_catalog = _catalog_at_revision(repo_root, merge_base)
        return _cluster_lane_selectors(base_catalog) != _cluster_lane_selectors(catalog)
    except Exception as exc:  # noqa: BLE001 - any failure must keep the lane
        print(
            f"ci-scope: cannot compare {CATALOG_ROUTING_PATH} against {base} "
            f"({exc}); enabling the replicated Store lane",
            file=sys.stderr,
        )
        return True


def catalog_is_the_only_routing_edit(paths: tuple[str, ...]) -> bool:
    """Report whether `points.toml` is the sole routing path a diff touches."""

    return {
        path for path in paths if matches(path, CI_ROUTING_PATHS)
    } == {CATALOG_ROUTING_PATH}


def scope_for_paths(
    catalog: Catalog,
    paths: tuple[str, ...],
    *,
    catalog_edit_hides_cluster_lanes: bool = True,
) -> dict[str, bool]:
    """Map changed paths to independently runnable CI surfaces.

    `catalog_edit_hides_cluster_lanes` carries the one fact a path list cannot:
    whether a `points.toml`-only routing edit changed a cluster selector. It
    defaults to the conservative answer, so a caller that knows nothing about
    the base revision — every existing caller — still forces the lane.
    """

    if is_docs_only(paths):
        return {key: key == "docs_only" for key in SCOPE_KEYS}
    selection = select_points(catalog, paths)
    check_ids = {
        check.id for check in selected_checks(catalog, selection, profile="ci")
    }
    point_ids = set(selection.point_ids)
    unknown_paths = tuple(
        path
        for path in paths
        if not any(matches(path, point.paths) for point in catalog.points)
    )
    scope = {
        "rust": needs_rust_gate(paths),
        "apple": any(matches(path, APPLE_PATHS) for path in paths),
        "android_jvm": any(matches(path, ANDROID_JVM_PATHS) for path in paths),
        "android_device": any(matches(path, ANDROID_DEVICE_PATHS) for path in paths),
        "web_layout": any(matches(path, WEB_LAYOUT_PATHS) for path in paths),
        "release_build": bool(unknown_paths)
        or any(matches(path, RELEASE_BUILD_PATHS) for path in paths),
        "container": any(matches(path, CONTAINER_PATHS) for path in paths),
        "mobile_version": "mobile-version" in check_ids,
        "hiqlite_spike": "core.media" in point_ids,
        # `cluster.membership` belongs here even though its own behavior is
        # unit-testable: the replicated Store, topology and daemon contracts
        # are the only place a membership change is exercised against real
        # voters, and `points.toml` already declares `cluster-auth` as its
        # evidence. Without this, a change to the request-admission fence
        # merges on the fast Rust gate alone while its regression record
        # claims three-voter coverage that never ran.
        "cluster_auth": bool(
            {"cluster.auth", "cluster.membership", "cluster.page-reads"} & point_ids
        ),
        "docs_only": False,
    }
    if any(matches(path, CI_ROUTING_PATHS) for path in paths):
        scope["rust"] = True
        # A catalog-only routing edit is the one case decided by content: it
        # forces the replicated Store lane only when it could hide it. Any
        # other routing path, alone or alongside the catalog, still forces it.
        if catalog_edit_hides_cluster_lanes or not catalog_is_the_only_routing_edit(
            paths
        ):
            scope["cluster_auth"] = True
    if any(matches(path, FFMPEG_ACTION_PATHS) for path in paths):
        scope["rust"] = True
        scope["cluster_auth"] = True
        scope["web_layout"] = True
    if any(matches(path, PLAYWRIGHT_ACTION_PATHS) for path in paths):
        scope["rust"] = True
        scope["web_layout"] = True
    return scope


def resolve_scope(event: str, base: str | None) -> dict[str, bool]:
    """Scope ordinary PRs; run everything for qualification and release events.

    `effort_qualification`, merge-group, push, and tag events all have non-PR
    names and fail open into `all_scope()`. A final effort-to-main candidate
    therefore proves every platform even while task PRs use effort-ci.yml's
    compile-only lane.
    """

    if event != "pull_request":
        return all_scope()
    if not base:
        print("ci-scope: no pull-request base; enabling every job", file=sys.stderr)
        return all_scope()

    try:
        paths = changed_paths(REPO_ROOT, "changed-from", base)
    except CatalogError as exc:
        print(f"ci-scope: cannot resolve diff ({exc}); enabling every job", file=sys.stderr)
        return all_scope()

    catalog = load_catalog()
    hides_cluster_lanes = True
    if catalog_is_the_only_routing_edit(paths):
        hides_cluster_lanes = catalog_edit_forces_cluster_lanes(
            catalog, base, repo_root=REPO_ROOT
        )
    return scope_for_paths(
        catalog,
        paths,
        catalog_edit_hides_cluster_lanes=hides_cluster_lanes,
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python3 -m validation.ci_scope",
        description="Emit GitHub Actions booleans for impact-selected CI jobs.",
    )
    parser.add_argument("--event", required=True)
    parser.add_argument("--base")
    args = parser.parse_args(argv)

    for key, enabled in resolve_scope(args.event, args.base).items():
        print(f"{key}={'true' if enabled else 'false'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
