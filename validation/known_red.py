"""Inventory ignored Rust tests and reject expired known-red entries."""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import pathlib
import re
import subprocess
import sys
import tomllib


ROOT = pathlib.Path(__file__).resolve().parents[1]
RULES = ROOT / "validation/known-red.toml"
IGNORE_RE = re.compile(
    r'#\[ignore\s*=\s*"(?P<reason>(?:[^"\\]|\\.)*)"\]'
    r"(?:(?!\bfn\s).)*?\bfn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)",
    re.DOTALL,
)


class KnownRedError(ValueError):
    """The known-red inventory does not satisfy its fail-closed contract."""


@dataclasses.dataclass(frozen=True)
class IgnoredTest:
    name: str
    reason: str
    path: str
    line: int


def ignored_tests(root: pathlib.Path = ROOT) -> tuple[IgnoredTest, ...]:
    found: list[IgnoredTest] = []
    for path in sorted((root / "crates").rglob("*.rs")):
        text = path.read_text(encoding="utf-8")
        for match in IGNORE_RE.finditer(text):
            found.append(
                IgnoredTest(
                    name=match.group("name"),
                    reason=bytes(match.group("reason"), "utf-8").decode("unicode_escape"),
                    path=path.relative_to(root).as_posix(),
                    line=text.count("\n", 0, match.start()) + 1,
                )
            )
    return tuple(found)


def listed_rust_tests(root: pathlib.Path = ROOT) -> tuple[str, ...]:
    process = subprocess.run(
        [
            "cargo",
            "test",
            "--workspace",
            "--exclude",
            "plurx-cluster-check",
            "--",
            "--list",
            "--format",
            "terse",
        ],
        cwd=root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    if process.returncode:
        raise KnownRedError(f"cargo test listing failed:\n{process.stdout}")
    return tuple(
        line.removesuffix(": test")
        for line in process.stdout.splitlines()
        if line.endswith(": test")
    )


def load_entries(path: pathlib.Path = RULES) -> tuple[dict[str, object], ...]:
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise KnownRedError(f"cannot read {path}: {exc}") from exc
    if data.get("version") != 1 or not isinstance(data.get("entries"), list):
        raise KnownRedError("known-red.toml must have version = 1 and an entries array")
    return tuple(data["entries"])


def validate_entries(
    entries: tuple[dict[str, object], ...],
    ignored: tuple[IgnoredTest, ...],
    today: dt.date | None = None,
) -> None:
    today = today or dt.datetime.now(dt.timezone.utc).date()
    ignored_names = {item.name for item in ignored}
    required = {"suite", "test", "owner", "reason", "expires"}
    for index, entry in enumerate(entries):
        where = f"entries[{index}]"
        if not isinstance(entry, dict):
            raise KnownRedError(f"{where} must be a table")
        unknown = set(entry) - required
        missing = required - set(entry)
        if unknown or missing:
            raise KnownRedError(
                f"{where} keys differ: missing={sorted(missing)} unknown={sorted(unknown)}"
            )
        for field in ("suite", "test", "owner", "reason"):
            if not isinstance(entry[field], str) or not entry[field].strip():
                raise KnownRedError(f"{where}.{field} must be non-empty text")
        if entry["suite"] not in {"rust", "node", "python"}:
            raise KnownRedError(f"{where}.suite must be rust, node, or python")
        # Today only Rust has checked-in ignore reasons. Node/Python entries
        # therefore fail closed until their source has an equivalent scanner.
        if entry["suite"] != "rust" or entry["test"] not in ignored_names:
            raise KnownRedError(f"{where}.test does not resolve to an ignored test")
        expires = entry["expires"]
        if not isinstance(expires, dt.date):
            raise KnownRedError(f"{where}.expires must be a TOML date")
        if expires <= today:
            raise KnownRedError(f"{where}.expires is not in the future")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--no-cargo-list", action="store_true")
    args = parser.parse_args(argv)
    ignored = ignored_tests()
    if not args.no_cargo_list:
        listed = listed_rust_tests()
        missing = [
            item for item in ignored if not any(name.endswith(f"::{item.name}") or name == item.name for name in listed)
        ]
        # Platform/feature-gated ignored tests may be absent from this host's
        # cargo listing. Source remains authoritative for their reason, while
        # every listed ignored function must resolve unambiguously.
        for item in missing:
            if not ("videotoolbox" in item.path or "store_contract" in item.path):
                raise KnownRedError(f"ignored test is absent from cargo --list: {item.name}")
    entries = load_entries()
    validate_entries(entries, ignored)
    print("test\treason\tsource")
    for item in ignored:
        print(f"{item.name}\t{item.reason}\t{item.path}:{item.line}")
    print(f"known-red: {len(entries)} entries")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KnownRedError as exc:
        print(f"known-red: {exc}", file=sys.stderr)
        raise SystemExit(1)
