"""Patches carried on a vendored crate, one file per patch.

`vendor/hiqlite/PLURX-PATCH.md` and `vendor/hiqlite-wal/PLURX-PATCH.md` list
each patch as a numbered table row plus a bullet, and stated the total in
words. Every pull request that added a patch took "the next number", extended
the same table tail and rewrote the same count, so any two of them conflicted
(four were open at once on 2026-11-15).

The numbered rows stay as the record of what was carried on that day and gain
no more. A later patch is a file under the crate's `patches.d/`, keyed by its
file name rather than by a number:

    patch = "Lease idle Raft client connections"
    kind = "generic bug"            # or "plurx policy", "dependency-only"
    upstream = "pending M6"         # a public URL, or "—" where none applies
    drop = "Upstream release stops opening a connection per new_client."
    description = '''
    What the patch does and why Plurx needs it.
    Files: `src/network/raft_client.rs`.
    '''

The count of patches is derived: numbered rows plus files. Each file obeys
the rules a row obeys, checked by tests/operations/test_hiqlite_patch_ledger.py.
"""

from __future__ import annotations

import dataclasses
from pathlib import Path
import re
import sys
import tomllib

REPO_ROOT = Path(__file__).resolve().parents[1]
CRATES = ("vendor/hiqlite", "vendor/hiqlite-wal")
LEDGER = "PLURX-PATCH.md"
DIRECTORY = "patches.d"
KINDS = ("generic bug", "plurx policy", "dependency-only")
_ROW = re.compile(r"(?m)^\| (\d+) \|")
_KEYS = {"patch", "kind", "upstream", "drop", "description"}


class VendorPatchError(ValueError):
    """A per-patch file is malformed."""


@dataclasses.dataclass(frozen=True)
class Patch:
    key: str
    patch: str
    kind: str
    upstream: str
    drop: str
    description: str


def numbered_rows(crate: Path) -> int:
    """How many numbered rows the crate's ledger table holds."""

    return len(_ROW.findall((crate / LEDGER).read_text(encoding="utf-8")))


def load(crate: Path) -> tuple[Patch, ...]:
    """The crate's per-patch files, in file-name order."""

    directory = crate / DIRECTORY
    if not directory.is_dir():
        return ()
    patches = []
    for path in sorted(directory.glob("*.toml")):
        where = f"{crate.name}/{DIRECTORY}/{path.name}"
        try:
            raw = tomllib.loads(path.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as exc:
            raise VendorPatchError(f"cannot read {where}: {exc}") from exc
        if set(raw) != _KEYS or not all(isinstance(raw[key], str) and raw[key].strip() for key in _KEYS):
            raise VendorPatchError(
                f"{where} must contain exactly the nonempty strings {', '.join(sorted(_KEYS))}"
            )
        if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", path.stem):
            raise VendorPatchError(f"{where}: name a patch file in lower-case letters, digits and '-'")
        kind, upstream, drop = raw["kind"], raw["upstream"].strip(), raw["drop"].strip()
        if kind not in KINDS:
            raise VendorPatchError(f"{where}: kind must be one of {', '.join(KINDS)}")
        if kind == "generic bug":
            if not (upstream == "pending M6" or upstream.startswith(("http://", "https://"))):
                raise VendorPatchError(
                    f"{where}: a generic bug needs `pending M6` or a public upstream URL"
                )
        elif upstream != "—":
            raise VendorPatchError(f"{where}: upstream is `—` for a {kind} patch")
        if kind == "plurx policy" and not drop.startswith("Never;"):
            raise VendorPatchError(f"{where}: a plurx policy's drop condition starts `Never;`")
        if kind == "dependency-only" and drop.startswith("Never;"):
            raise VendorPatchError(
                f"{where}: a dependency-only patch names the upstream release that retires it"
            )
        patches.append(Patch(path.stem, raw["patch"].strip(), kind, upstream, drop, raw["description"].strip()))
    return tuple(patches)


def count(crate: Path) -> int:
    """Every patch the crate carries: its numbered rows plus its per-patch files."""

    return numbered_rows(crate) + len(load(crate))


def main() -> int:
    try:
        for name in CRATES:
            crate = REPO_ROOT / name
            files = load(crate)
            kinds = ", ".join(f"{sum(p.kind == kind for p in files)} {kind}" for kind in KINDS)
            print(f"{name}: {count(crate)} patches ({numbered_rows(crate)} numbered rows, "
                  f"{len(files)} files: {kinds})")
    except VendorPatchError as exc:
        print(f"vendor patches: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
