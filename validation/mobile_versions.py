"""Enforce mobile release versions and the derived build-number wiring."""

from __future__ import annotations

import dataclasses
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from collections.abc import Callable


REPO_ROOT = Path(__file__).resolve().parents[1]
SEMANTIC_VERSION = re.compile(
    r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)"
)

APPLE_RELEASE_PATHS = (
    "clients/apple/Sources/",
    "clients/apple/Resources/",
)
APPLE_RELEASE_FILES = frozenset({"clients/apple/project.yml"})

ANDROID_RELEASE_PATHS = ("clients/android/app/src/main/",)
ANDROID_RELEASE_FILES = frozenset(
    {
        "clients/android/app/build.gradle.kts",
        "clients/android/app/proguard-rules.pro",
        "clients/android/build.gradle.kts",
        "clients/android/gradle.properties",
        "clients/android/gradle/libs.versions.toml",
        "clients/android/settings.gradle.kts",
    }
)


# The fixed development placeholder project.yml carries for
# CURRENT_PROJECT_VERSION. The shipped number is derived at build time by
# scripts/build-number and stamped into each app's Info.plist, so the stored
# value is never bumped; a change to it would reintroduce the per-pull-request
# counter whose conflicts the derived number removed.
APPLE_PLACEHOLDER_BUILD = 1
# build.gradle.kts assigns versionCode from this value, which it computes by
# running scripts/build-number (or takes from -PplurxVersionCode).
ANDROID_DERIVED_VERSION_CODE = "plurxVersionCode"
BUILD_NUMBER_SCRIPT = "scripts/build-number"
APPLE_STAMP = re.compile(
    r'^\s*/usr/libexec/PlistBuddy -c "Set :CFBundleVersion \$\{number\}" '
    r'"\$\{TARGET_BUILD_DIR\}/\$\{INFOPLIST_PATH\}"\s*$',
    re.MULTILINE,
)


class MobileVersionError(ValueError):
    """A mobile version declaration is invalid."""


@dataclasses.dataclass(frozen=True)
class MobileVersions:
    workspace: str
    android_name: str
    android_version_code: str
    android_runs_build_number: bool
    apple_name: str
    apple_build: int
    apple_marketing_references: int
    apple_build_references: int
    apple_stamp_phases: int
    apple_runs_build_number: int


def _required_value(contents: str, pattern: str, label: str) -> str:
    matches = re.findall(pattern, contents, flags=re.MULTILINE)
    if len(matches) != 1:
        raise MobileVersionError(
            f"expected exactly one {label}, found {len(matches)}"
        )
    return matches[0]


def _positive_integer(value: str, label: str) -> int:
    if not re.fullmatch(r"[1-9]\d*", value):
        raise MobileVersionError(f"{label} must be a positive integer; found {value!r}")
    return int(value)


def read_versions(read: Callable[[str], str]) -> MobileVersions:
    try:
        workspace = tomllib.loads(read("Cargo.toml"))["workspace"]["package"][
            "version"
        ]
    except (KeyError, TypeError, tomllib.TOMLDecodeError) as exc:
        raise MobileVersionError(
            "Cargo.toml must declare workspace.package.version"
        ) from exc
    if not isinstance(workspace, str):
        raise MobileVersionError("workspace.package.version must be a string")

    android = read("clients/android/app/build.gradle.kts")
    apple = read("clients/apple/project.yml")
    android_name = _required_value(
        android,
        r'^\s*versionName\s*=\s*"([^"]+)"\s*$',
        "Android versionName",
    )
    android_version_code = _required_value(
        android,
        r"^\s*versionCode\s*=\s*(\S+)\s*$",
        "Android versionCode",
    )
    apple_name = _required_value(
        apple,
        r'^\s*MARKETING_VERSION:\s*"([^"]+)"\s*$',
        "Apple MARKETING_VERSION",
    )
    apple_build = _positive_integer(
        _required_value(
            apple,
            r'^\s*CURRENT_PROJECT_VERSION:\s*"?([^"\s]+)"?\s*$',
            "Apple CURRENT_PROJECT_VERSION",
        ),
        "Apple CURRENT_PROJECT_VERSION",
    )

    return MobileVersions(
        workspace=workspace,
        android_name=android_name,
        android_version_code=android_version_code,
        android_runs_build_number=BUILD_NUMBER_SCRIPT in android,
        apple_name=apple_name,
        apple_build=apple_build,
        apple_marketing_references=len(
            re.findall(
                r'^\s*CFBundleShortVersionString:\s*"\$\(MARKETING_VERSION\)"\s*$',
                apple,
                re.MULTILINE,
            )
        ),
        apple_build_references=len(
            re.findall(
                r'^\s*CFBundleVersion:\s*"\$\(CURRENT_PROJECT_VERSION\)"\s*$',
                apple,
                re.MULTILINE,
            )
        ),
        apple_stamp_phases=len(APPLE_STAMP.findall(apple)),
        apple_runs_build_number=len(
            re.findall(r'"\$\{SRCROOT\}/\.\./\.\./scripts/build-number"', apple)
        ),
    )


def validate_versions(current: MobileVersions) -> tuple[str, ...]:
    """The static release-version rules one tree must satisfy.

    Build numbers are derived at build time (scripts/build-number), so there is
    no counter to compare against a baseline: what is checked is that both
    apps still take the derived number and store none of their own.
    """
    errors: list[str] = []

    if not SEMANTIC_VERSION.fullmatch(current.workspace):
        errors.append(
            "workspace version must be MAJOR.MINOR.PATCH; "
            f"found {current.workspace!r}"
        )
    if current.android_name != current.workspace:
        errors.append(
            "Android versionName must match the workspace version "
            f"({current.workspace}); found {current.android_name}"
        )
    if current.apple_name != current.workspace:
        errors.append(
            "Apple MARKETING_VERSION must match the workspace version "
            f"({current.workspace}); found {current.apple_name}"
        )
    if current.android_version_code != ANDROID_DERIVED_VERSION_CODE:
        errors.append(
            f"Android versionCode must be `{ANDROID_DERIVED_VERSION_CODE}`, the value "
            f"build.gradle.kts derives from {BUILD_NUMBER_SCRIPT}; found "
            f"{current.android_version_code!r}. Build numbers are never stored in source."
        )
    if not current.android_runs_build_number:
        errors.append(
            f"clients/android/app/build.gradle.kts must derive versionCode by running "
            f"{BUILD_NUMBER_SCRIPT}"
        )
    if current.apple_build != APPLE_PLACEHOLDER_BUILD:
        errors.append(
            f"Apple CURRENT_PROJECT_VERSION is a fixed development placeholder and must "
            f"stay {APPLE_PLACEHOLDER_BUILD}; found {current.apple_build}. The shipped "
            f"build number is derived by {BUILD_NUMBER_SCRIPT}; do not bump it."
        )
    if current.apple_marketing_references != 2:
        errors.append(
            "the iOS and tvOS targets must both stamp MARKETING_VERSION "
            f"(found {current.apple_marketing_references} references)"
        )
    if current.apple_build_references != 2:
        errors.append(
            "the iOS and tvOS targets must both stamp CURRENT_PROJECT_VERSION "
            f"(found {current.apple_build_references} references)"
        )
    if current.apple_stamp_phases != 2 or current.apple_runs_build_number != 2:
        errors.append(
            "the iOS and tvOS targets must each carry one build phase that sets "
            f"CFBundleVersion from {BUILD_NUMBER_SCRIPT} (found "
            f"{current.apple_stamp_phases} PlistBuddy stamps and "
            f"{current.apple_runs_build_number} {BUILD_NUMBER_SCRIPT} runs)"
        )
    return tuple(errors)


def _git(root: Path, *arguments: str) -> str:
    try:
        return subprocess.run(
            ["git", *arguments],
            cwd=root,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        ).stdout
    except (OSError, subprocess.CalledProcessError) as exc:
        detail = getattr(exc, "stderr", "")
        raise MobileVersionError(
            f"git {' '.join(arguments)} failed: {detail or exc}"
        ) from exc


def _git_reader(root: Path, ref: str) -> Callable[[str], str]:
    return lambda path: _git(root, "show", f"{ref}:{path}")


def _filesystem_reader(root: Path) -> Callable[[str], str]:
    return lambda path: (root / path).read_text(encoding="utf-8")


def check_repository(root: Path = REPO_ROOT, *, mode: str = "all") -> tuple[str, ...]:
    """Validate the tree the validation mode names.

    ``staged`` reads the index, ``changed-from`` the committed HEAD, and every
    other mode the working tree. No mode compares against a baseline any more:
    the build numbers a baseline comparison used to police are derived.
    """
    if mode == "staged":
        read = _git_reader(root, "")
    elif mode == "changed-from":
        read = _git_reader(root, "HEAD")
    elif mode in {"all", "paths", "point"}:
        read = _filesystem_reader(root)
    else:
        raise MobileVersionError(f"unsupported validation mode: {mode}")
    return validate_versions(read_versions(read))


def main() -> int:
    mode = os.environ.get("PLURX_VALIDATION_MODE", "all")
    try:
        errors = check_repository(mode=mode)
    except (MobileVersionError, OSError) as exc:
        print(f"mobile version validation failed: {exc}", file=sys.stderr)
        return 1

    if errors:
        print("mobile version validation failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    print("mobile versions are valid and both build numbers are derived")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
