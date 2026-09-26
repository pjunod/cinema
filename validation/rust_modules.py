"""Read a Rust module the way it read before S-14 split it into child files.

S-14 moved regions of `transcode.rs`, `http/hls.rs` and `vodserve.rs` into
`#[path]` child modules without changing a byte of them (see
`scripts/split-identity`). Each parent keeps a `// split: begin <name>` /
`// split: end <name>` block where a region used to be. Source inventories
that pin anchors or occurrence counts in "the transcode module" read the
parent through `module_source`, which puts every product child back in its
block, so an anchor that moved into a child is still found exactly once and
a count still counts the same text.

Test children (a path under a `tests` directory or named `tests.rs`) are not
expanded: those modules left their parents before these inventories were
re-pinned, and the inventories already scope them explicitly.

Expansion removes exactly the plumbing the split added: the child's
`use super::*;` header and the `pub(super) ` it gained for parent reach.
It also spells a parent's own `pub(super)` the way the parent did: a child
of `http/hls.rs` has to write that reach as `pub(in crate::http)`, or as
`pub(in crate::http::hls)` inside the inline `plan_derivation` module.
A block naming several children is one `impl` cut into chunks; the chunks'
shared `impl` line and closing brace are restored once.
"""

from __future__ import annotations

import pathlib
import re

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
CHILD_HEADER = "use super::*;\n\n"
_BLOCK = re.compile(r"// split: begin ([\w-]+)\n(.*?)// split: end \1\n", re.S)
_PATH = re.compile(r'#\[path = "([^"]+)"\]')
_RESTATED = {
    "crates/plurxd/src/http/hls.rs": ("pub(in crate::http) ", "pub(in crate::http::hls) "),
}
# A path the parent wrote relative to its own parent gains one `super::` in
# a child (hls.rs's `super::stream::` is `super::super::stream::` there).
_RELOCATED = {"crates/plurxd/src/http/hls.rs": ("super::super::", "super::")}


def _resolve(path: str | pathlib.Path) -> pathlib.Path:
    path = pathlib.Path(path)
    return path if path.is_absolute() else REPO_ROOT / path


def _is_test_child(relative: str) -> bool:
    parts = pathlib.PurePosixPath(relative).parts
    return "tests" in parts or parts[-1] == "tests.rs"


def _child_body(path: pathlib.Path, key: str) -> str:
    text = path.read_text(encoding="utf-8")
    if text.startswith(CHILD_HEADER):
        text = text[len(CHILD_HEADER) :]
    text = text.replace("pub(super) ", "")
    for spelling in _RESTATED.get(key, ()):
        text = text.replace(spelling, "pub(super) ")
    if key in _RELOCATED:
        text = text.replace(*_RELOCATED[key])
    return text


def _product_blocks(text: str):
    for block in _BLOCK.finditer(text):
        children = _PATH.findall(block.group(2))
        if children and not any(_is_test_child(child) for child in children):
            yield block, children


def module_source(path: str | pathlib.Path) -> str:
    """Return `path`'s text with its product split children expanded in place."""

    path = _resolve(path)
    text = path.read_text(encoding="utf-8")
    try:
        key = path.resolve().relative_to(REPO_ROOT.resolve()).as_posix()
    except ValueError:
        key = ""
    pieces: list[str] = []
    last = 0
    for block, children in _product_blocks(text):
        bodies = [_child_body(path.parent / child, key) for child in children]
        if len(bodies) == 1:
            expanded = bodies[0]
        else:
            header = bodies[0].split("\n", 1)[0] + "\n"
            inner = [body[len(header) : -len("}\n")] for body in bodies]
            expanded = header + "\n".join(inner) + "}\n"
        pieces += [text[last : block.start()], expanded]
        last = block.end()
    pieces.append(text[last:])
    return "".join(pieces)


def split_children(path: str | pathlib.Path) -> set[pathlib.Path]:
    """The product child files `module_source(path)` expands."""

    path = _resolve(path)
    children: set[pathlib.Path] = set()
    for _block, names in _product_blocks(path.read_text(encoding="utf-8")):
        children.update((path.parent / child).resolve() for child in names)
    return children
