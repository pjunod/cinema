"""Inventory ignored Rust tests and reject expired known-red entries."""

from __future__ import annotations

import argparse
from collections import Counter, deque
import dataclasses
import datetime as dt
import pathlib
import re
import subprocess
import sys
import tomllib


ROOT = pathlib.Path(__file__).resolve().parents[1]
RULES = ROOT / "validation/known-red.toml"
IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
OPTIONAL_CARGO_IDENTITIES = frozenset(
    {
        "crates/plurx-core/tests/store_contract.rs::hiqlite_activation_node_process",
        "crates/plurx-core/tests/store_contract.rs::hiqlite_contract_node_process",
        "crates/plurxd/src/live_tv/videotoolbox_tests.rs::"
        "live_tv::videotoolbox_tests::"
        "live_tv_videotoolbox_atsc1_publishes_decodable_segments",
    }
)


class KnownRedError(ValueError):
    """The known-red inventory does not satisfy its fail-closed contract."""


@dataclasses.dataclass(frozen=True)
class IgnoredTest:
    identity: str
    cargo_name: str
    reason: str
    path: str
    line: int


@dataclasses.dataclass(frozen=True)
class _Token:
    kind: str
    value: str
    line: int


@dataclasses.dataclass(frozen=True)
class _Attribute:
    tokens: tuple[_Token, ...]
    line: int
    inner: bool = False

    @property
    def path(self) -> tuple[str, ...]:
        parts: list[str] = []
        index = 0
        while index < len(self.tokens):
            token = self.tokens[index]
            if token.kind != "ident":
                break
            parts.append(token.value)
            index += 1
            if index + 1 >= len(self.tokens) or (
                self.tokens[index].value,
                self.tokens[index + 1].value,
            ) != (":", ":"):
                break
            index += 2
        return tuple(parts)


def _lex_rust(text: str, source: str) -> tuple[_Token, ...]:
    """Tokenize enough Rust to distinguish attributes from comments and strings."""

    tokens: list[_Token] = []
    index = 0
    line = 1
    length = len(text)

    def advance(end: int) -> None:
        nonlocal index, line
        line += text.count("\n", index, end)
        index = end

    while index < length:
        char = text[index]
        if char.isspace():
            advance(index + 1)
            continue
        if text.startswith("//", index):
            end = text.find("\n", index + 2)
            advance(length if end < 0 else end)
            continue
        if text.startswith("/*", index):
            start_line = line
            depth = 1
            end = index + 2
            while end < length and depth:
                if text.startswith("/*", end):
                    depth += 1
                    end += 2
                elif text.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            if depth:
                raise KnownRedError(f"{source}:{start_line}: unterminated block comment")
            advance(end)
            continue

        raw = re.match(
            r"(?P<prefix>br|cr|r)(?P<hashes>#{0,255})\"", text[index:]
        )
        if raw:
            start_line = line
            hashes = raw.group("hashes")
            content_start = index + raw.end()
            terminator = '"' + hashes
            end = text.find(terminator, content_start)
            if end < 0:
                raise KnownRedError(f"{source}:{start_line}: unterminated raw string")
            kind = "string" if raw.group("prefix") == "r" else "non_string"
            tokens.append(_Token(kind, text[content_start:end], start_line))
            advance(end + len(terminator))
            continue
        prefix_length = (
            1
            if char in {"b", "c"}
            and index + 1 < length
            and text[index + 1] == '"'
            else 0
        )
        if char == '"' or prefix_length:
            start_line = line
            start = index + prefix_length + 1
            end = start
            escaped = False
            while end < length:
                current = text[end]
                if not escaped and current == '"':
                    break
                if not escaped and current == "\\":
                    escaped = True
                else:
                    escaped = False
                end += 1
            if end >= length:
                raise KnownRedError(f"{source}:{start_line}: unterminated string")
            raw_value = text[start:end]
            kind = "string" if prefix_length == 0 else "non_string"
            tokens.append(_Token(kind, raw_value, start_line))
            advance(end + 1)
            continue
        if char == "'":
            # Consume a character literal, but leave a Rust lifetime such as
            # `'a` as punctuation plus an identifier.
            end = index + 1
            if end < length and text[end] == "\\":
                escape = text[end + 1] if end + 1 < length else ""
                end += 2
                if escape == "u" and end < length and text[end] == "{":
                    end = text.find("}", end + 1) + 1
                elif escape == "x":
                    end += 2
            else:
                end += 1
            if 0 < end < length and text[end] == "'":
                advance(end + 1)
            else:
                tokens.append(_Token("punct", char, line))
                advance(index + 1)
            continue
        identifier = IDENT_RE.match(text, index)
        if identifier:
            tokens.append(_Token("ident", identifier.group(0), line))
            advance(identifier.end())
            continue
        tokens.append(_Token("punct", char, line))
        advance(index + 1)
    return tuple(tokens)


def _closing(
    tokens: tuple[_Token, ...], start: int, opening: str, closing: str
) -> int:
    depth = 0
    for index in range(start, len(tokens)):
        token = tokens[index]
        if token.kind != "punct":
            continue
        if token.value == opening:
            depth += 1
        elif token.value == closing:
            depth -= 1
            if depth == 0:
                return index
    return -1


def _attribute(
    tokens: tuple[_Token, ...], start: int
) -> tuple[_Attribute, int] | None:
    if start >= len(tokens) or tokens[start].value != "#":
        return None
    bracket = start + 1
    inner = False
    if bracket < len(tokens) and tokens[bracket].value == "!":
        inner = True
        bracket += 1
    if bracket >= len(tokens) or tokens[bracket].value != "[":
        return None
    end = _closing(tokens, bracket, "[", "]")
    if end < 0:
        raise KnownRedError(f"line {tokens[start].line}: unterminated Rust attribute")
    return _Attribute(tokens[bracket + 1 : end], tokens[start].line, inner), end + 1


def _direct_ignore_reason(attribute: _Attribute, source: str) -> str:
    if attribute.inner:
        raise KnownRedError(
            f"{source}:{attribute.line}: inner #[ignore] cannot attach to a test"
        )
    tokens = attribute.tokens
    if len(tokens) != 3 or tokens[1].value != "=" or tokens[2].kind != "string":
        raise KnownRedError(
            f"{source}:{attribute.line}: #[ignore] must use #[ignore = \"reason\"]"
        )
    reason = tokens[2].value.strip()
    if not reason:
        raise KnownRedError(f"{source}:{attribute.line}: #[ignore] reason is empty")
    return reason


def _split_meta(tokens: tuple[_Token, ...]) -> tuple[tuple[_Token, ...], ...]:
    parts: list[tuple[_Token, ...]] = []
    start = 0
    depths = {"(": 0, "[": 0, "{": 0}
    pairs = {")": "(", "]": "[", "}": "{"}
    for index, token in enumerate(tokens):
        if token.kind != "punct":
            continue
        if token.value in depths:
            depths[token.value] += 1
        elif token.value in pairs:
            depths[pairs[token.value]] -= 1
        elif token.value == "," and not any(depths.values()):
            parts.append(tokens[start:index])
            start = index + 1
    parts.append(tokens[start:])
    return tuple(parts)


def _ignore_reasons(attribute: _Attribute, source: str) -> tuple[str, ...]:
    if attribute.path == ("ignore",):
        return (_direct_ignore_reason(attribute, source),)
    if attribute.path != ("cfg_attr",):
        return ()
    try:
        opening = next(
            index for index, token in enumerate(attribute.tokens) if token.value == "("
        )
    except StopIteration:
        return ()
    closing = _closing(attribute.tokens, opening, "(", ")")
    if closing < 0:
        raise KnownRedError(f"{source}:{attribute.line}: unterminated cfg_attr")
    parts = _split_meta(attribute.tokens[opening + 1 : closing])
    reasons: list[str] = []
    for meta in parts[1:]:
        nested = _Attribute(meta, attribute.line)
        reasons.extend(_ignore_reasons(nested, source))
    if attribute.inner and reasons:
        raise KnownRedError(
            f"{source}:{attribute.line}: inner cfg_attr cannot attach #[ignore] to a test"
        )
    return tuple(reasons)


def _is_test_attribute(attribute: _Attribute) -> bool:
    if attribute.path in {("test",), ("tokio", "test")}:
        return True
    if attribute.path != ("cfg_attr",):
        return False
    try:
        opening = next(
            index for index, token in enumerate(attribute.tokens) if token.value == "("
        )
    except StopIteration:
        return False
    closing = _closing(attribute.tokens, opening, "(", ")")
    if closing < 0:
        return False
    return any(
        _is_test_attribute(_Attribute(meta, attribute.line))
        for meta in _split_meta(attribute.tokens[opening + 1 : closing])[1:]
    )


def _item_start(tokens: tuple[_Token, ...], start: int) -> int:
    index = start
    if index < len(tokens) and tokens[index].value == "pub":
        index += 1
        if index < len(tokens) and tokens[index].value == "(":
            end = _closing(tokens, index, "(", ")")
            index = len(tokens) if end < 0 else end + 1
    while index < len(tokens) and tokens[index].value in {
        "async",
        "const",
        "default",
        "unsafe",
    }:
        index += 1
    if index < len(tokens) and tokens[index].value == "extern":
        index += 1
        if index < len(tokens) and tokens[index].kind == "string":
            index += 1
    return index


def _target_roots(root: pathlib.Path) -> tuple[pathlib.Path, ...]:
    targets: set[pathlib.Path] = set()
    crates = root / "crates"
    if not crates.is_dir():
        return ()
    for manifest in crates.glob("*/Cargo.toml"):
        crate = manifest.parent
        try:
            cargo = tomllib.loads(manifest.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as exc:
            raise KnownRedError(f"cannot read {manifest}: {exc}") from exc
        lib = cargo.get("lib")
        if isinstance(lib, dict) and isinstance(lib.get("path"), str):
            targets.add(crate / lib["path"])
        elif (crate / "src/lib.rs").is_file():
            targets.add(crate / "src/lib.rs")
        bins = cargo.get("bin", [])
        if isinstance(bins, list):
            for binary in bins:
                if isinstance(binary, dict) and isinstance(binary.get("path"), str):
                    targets.add(crate / binary["path"])
        if (crate / "src/main.rs").is_file():
            targets.add(crate / "src/main.rs")
        targets.update((crate / "src/bin").glob("*.rs"))
        declared_tests = cargo.get("test", [])
        if isinstance(declared_tests, list):
            for test in declared_tests:
                if isinstance(test, dict) and isinstance(test.get("path"), str):
                    targets.add(crate / test["path"])
        targets.update((crate / "tests").glob("*.rs"))
    return tuple(sorted(path for path in targets if path.is_file()))


def _external_module_path(
    source: pathlib.Path,
    external_dir: pathlib.Path,
    name: str,
    attributes: tuple[_Attribute, ...],
) -> pathlib.Path:
    for attribute in attributes:
        if attribute.path == ("path",):
            tokens = attribute.tokens
            if len(tokens) == 3 and tokens[1].value == "=" and tokens[2].kind == "string":
                return source.parent / tokens[2].value
            raise KnownRedError(
                f"{source}:{attribute.line}: module #[path] must be one string"
            )
    direct = external_dir / f"{name}.rs"
    nested = external_dir / name / "mod.rs"
    if direct.is_file() and nested.is_file():
        raise KnownRedError(f"ambiguous Rust module {name}: {direct} and {nested}")
    return direct if direct.is_file() else nested


def _scan_context(
    root: pathlib.Path,
    source: pathlib.Path,
    module_prefix: tuple[str, ...],
    external_dir: pathlib.Path,
    queue: deque[tuple[pathlib.Path, tuple[str, ...], pathlib.Path]],
) -> tuple[IgnoredTest, ...]:
    relative = source.relative_to(root).as_posix()
    tokens = _lex_rust(source.read_text(encoding="utf-8"), relative)
    found: list[IgnoredTest] = []

    def walk(
        start: int, end: int, modules: tuple[str, ...], child_dir: pathlib.Path
    ) -> None:
        index = start
        while index < end:
            attributes: list[_Attribute] = []
            while index < end:
                parsed = _attribute(tokens, index)
                if parsed is None:
                    break
                attribute, index = parsed
                attributes.append(attribute)
            item = _item_start(tokens, index)
            ignore_attributes = [
                (attribute, reason)
                for attribute in attributes
                for reason in _ignore_reasons(attribute, relative)
            ]
            if len(ignore_attributes) > 1:
                raise KnownRedError(
                    f"{relative}:{ignore_attributes[1][0].line}: duplicate #[ignore] attribute"
                )

            if item < end and tokens[item].value == "fn":
                if item + 1 >= end or tokens[item + 1].kind != "ident":
                    if ignore_attributes:
                        raise KnownRedError(
                            f"{relative}:{tokens[item].line}: unnamed ignored test function"
                        )
                    index = item + 1
                    continue
                if ignore_attributes:
                    if not any(_is_test_attribute(attribute) for attribute in attributes):
                        raise KnownRedError(
                            f"{relative}:{ignore_attributes[0][0].line}: #[ignore] is not attached to a test"
                        )
                    name = tokens[item + 1].value
                    cargo_name = "::".join((*modules, name))
                    found.append(
                        IgnoredTest(
                            identity=f"{relative}::{cargo_name}",
                            cargo_name=cargo_name,
                            reason=ignore_attributes[0][1] or "",
                            path=relative,
                            line=ignore_attributes[0][0].line,
                        )
                    )
                index = item + 2
                continue

            if ignore_attributes:
                line = ignore_attributes[0][0].line
                raise KnownRedError(
                    f"{relative}:{line}: #[ignore] is detached from a test function"
                )

            if item < end and tokens[item].value == "mod":
                if item + 1 >= end or tokens[item + 1].kind != "ident":
                    index = item + 1
                    continue
                name = tokens[item + 1].value
                boundary = item + 2
                if boundary < end and tokens[boundary].value == "{":
                    close = _closing(tokens, boundary, "{", "}")
                    if close < 0 or close > end:
                        raise KnownRedError(
                            f"{relative}:{tokens[boundary].line}: unterminated module"
                        )
                    walk(boundary + 1, close, (*modules, name), child_dir / name)
                    index = close + 1
                    continue
                if boundary < end and tokens[boundary].value == ";":
                    child = _external_module_path(
                        source, child_dir, name, tuple(attributes)
                    )
                    if not child.is_file():
                        raise KnownRedError(
                            f"{relative}:{tokens[item].line}: missing Rust module {name}"
                        )
                    next_dir = (
                        child.parent
                        if child.name == "mod.rs"
                        else child.parent / child.stem
                    )
                    queue.append((child, (*modules, name), next_dir))
                    index = boundary + 1
                    continue

            if (
                item + 3 < end
                and tokens[item].value == "include"
                and tokens[item + 1].value == "!"
                and tokens[item + 2].value == "("
                and tokens[item + 3].kind == "string"
            ):
                included = source.parent / tokens[item + 3].value
                if not included.is_file():
                    raise KnownRedError(
                        f"{relative}:{tokens[item].line}: missing included Rust source {included}"
                    )
                queue.append((included, modules, included.parent))
            index = max(index + 1, item + 1)

    walk(0, len(tokens), module_prefix, external_dir)
    return tuple(found)


def ignored_tests(root: pathlib.Path = ROOT) -> tuple[IgnoredTest, ...]:
    root = root.resolve()
    queue: deque[tuple[pathlib.Path, tuple[str, ...], pathlib.Path]] = deque()
    for target in _target_roots(root):
        if target.name in {"lib.rs", "main.rs", "mod.rs"}:
            external_dir = target.parent
        else:
            external_dir = target.parent / target.stem
        queue.append((target, (), external_dir))

    found: list[IgnoredTest] = []
    visited: set[tuple[pathlib.Path, tuple[str, ...]]] = set()
    scanned_paths: set[pathlib.Path] = set()
    while queue:
        source, module_prefix, external_dir = queue.popleft()
        source = source.resolve()
        context = (source, module_prefix)
        if context in visited:
            continue
        visited.add(context)
        scanned_paths.add(source)
        found.extend(_scan_context(root, source, module_prefix, external_dir, queue))

    # An ignore in source outside the reachable module graph cannot be tied to
    # a cargo test identity. Detect it instead of silently dropping it.
    for source in sorted((root / "crates").rglob("*.rs")):
        resolved = source.resolve()
        if resolved in scanned_paths:
            continue
        relative = source.relative_to(root).as_posix()
        tokens = _lex_rust(source.read_text(encoding="utf-8"), relative)
        for index in range(len(tokens)):
            parsed = _attribute(tokens, index)
            if parsed is not None and _ignore_reasons(parsed[0], relative):
                raise KnownRedError(
                    f"{relative}: ignored test source is not reachable from a Rust test target"
                )

    identities = Counter(item.identity for item in found)
    duplicates = sorted(identity for identity, count in identities.items() if count != 1)
    if duplicates:
        raise KnownRedError(f"duplicate ignored test identities: {', '.join(duplicates)}")
    return tuple(sorted(found, key=lambda item: item.identity))


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


def validate_listed_tests(
    ignored: tuple[IgnoredTest, ...], listed: tuple[str, ...]
) -> None:
    counts = Counter(listed)
    for item in ignored:
        count = counts[item.cargo_name]
        if count == 1:
            continue
        # These exact platform/feature-gated identities may be absent from this
        # host's cargo listing. New sources do not inherit a path substring
        # exemption; adding one requires naming the complete identity here.
        if count == 0 and item.identity in OPTIONAL_CARGO_IDENTITIES:
            continue
        if count == 0:
            raise KnownRedError(
                f"ignored test is absent from cargo --list: {item.identity}"
            )
        raise KnownRedError(
            f"ignored test is ambiguous in cargo --list ({count} matches): {item.identity}"
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
    identities = Counter(item.identity for item in ignored)
    duplicate_ignored = sorted(name for name, count in identities.items() if count != 1)
    if duplicate_ignored:
        raise KnownRedError(
            f"duplicate ignored test identities: {', '.join(duplicate_ignored)}"
        )
    seen_entries: set[str] = set()
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
        identity = str(entry["test"])
        if identity in seen_entries:
            raise KnownRedError(f"{where}.test duplicates {identity}")
        seen_entries.add(identity)
        # Today only Rust has checked-in ignore reasons. Node/Python entries
        # therefore fail closed until their source has an equivalent scanner.
        if entry["suite"] != "rust" or identities[identity] != 1:
            raise KnownRedError(
                f"{where}.test does not resolve to one ignored test identity"
            )
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
        validate_listed_tests(ignored, listed_rust_tests())
    entries = load_entries()
    validate_entries(entries, ignored)
    print("identity\treason\tsource")
    for item in ignored:
        print(f"{item.identity}\t{item.reason}\t{item.path}:{item.line}")
    print(f"known-red: {len(entries)} entries")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KnownRedError as exc:
        print(f"known-red: {exc}", file=sys.stderr)
        raise SystemExit(1)
