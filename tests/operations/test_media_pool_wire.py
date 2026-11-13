"""The media-pool wire changes only with a protocol bump and a rollout note.

Placement between nodes requires an exact `PROTOCOL_VERSION`
(`crates/plurxd/src/media_pool.rs`), and most of its envelopes are
`#[serde(deny_unknown_fields)]`. Two consequences follow, and neither is
visible in review of a single diff:

* a field added to, removed from or retyped in a strict envelope makes an
  older node refuse the message even under the same protocol number, so the
  change needs a `PROTOCOL_VERSION` bump (which excludes the old workers from
  placement) and an operator note on how to roll it out;
* a field added to a lenient envelope (`MediaNodeSnapshot`, the offer pair)
  must carry `#[serde(default)]`, or the new node cannot parse an old peer's
  advertisement — not even far enough to see that its protocol differs.

So the envelopes are parsed from source at the merge base and at HEAD and
compared. The rules:

1. strict envelope, wire field set (name, type, flatten) changed →
   HEAD's `PROTOCOL_VERSION` must exceed the base's, *and* `docs/OPERATIONS.md`
   "Rolling upgrades" must carry a dated line naming `protocol <N>`;
2. lenient envelope, existing field removed or retyped → the same;
3. lenient envelope, field added → it carries `#[serde(default)]`.

`#[serde(skip)]` fields are not on the wire and are ignored. The envelopes are
every deserialized struct and enum in the three files below (a `try_from`
proxy is checked through its `…Wire` struct), and every type their fields
name, followed to its in-repo definition wherever it lives — `SessionKind`,
`ReopenReason`, `Presentation`, `plurx_core::playback::audio::AudioDelivery`
and whatever those name in turn. A followed struct is strict or lenient by its
own `deny_unknown_fields`, under the same rules. An enum's variant set is a
strict field set: adding, removing or renaming a variant needs the bump
(an old node refuses a variant it does not know), unless the enum has a
`#[serde(other)]` catch-all, which makes added variants safe. A struct
variant's fields follow the enum's own strictness, and a change to the serde
container attributes that shape the wire (`tag`, `content`, `rename_all`,
`untagged`, `transparent`) needs the bump. A type that is new at HEAD has no
baseline and is not checked until it exists at the base; a type with no
in-repo `Deserialize` derive (std, serde_json, a hand-written impl) is opaque.

The base comes from `PLURX_VALIDATION_BASE` (set by `scripts/paperwork`) or
`git merge-base HEAD origin/main`. The rule functions are also exercised on
fixed sources, so the check is proven even where no base resolves.
"""

from __future__ import annotations

import os
import re
import subprocess
import unittest
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WIRE_FILES = (
    "crates/plurxd/src/media_pool.rs",
    "crates/plurxd/src/media_sessions.rs",
    "crates/plurxd/src/transcode/session_request.rs",
)
PROTOCOL_FILE = "crates/plurxd/src/media_pool.rs"
OPERATIONS = ROOT / "docs" / "OPERATIONS.md"
ROLLING_HEADING = "## Rolling upgrades"

STRUCT_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)\b([^;{]*)\{\s*$")
TUPLE_STRUCT_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)\b[^;{(]*\((.*)\)\s*;\s*$")
ENUM_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)\b([^;{]*)\{\s*$")
VARIANT_RE = re.compile(r"^\s*([A-Z][A-Za-z0-9_]*)\s*(.*)$")
TYPE_NAME_RE = re.compile(r"((?:[A-Za-z_][A-Za-z0-9_]*::)*)([A-Z][A-Za-z0-9_]*)")
# The serde container attributes that change what the wire looks like.
SHAPE_ATTRS = ("tag", "content", "rename_all", "untagged", "transparent")
# Path roots that are never in-repo definitions.
EXTERNAL_ROOTS = {
    "std", "core", "alloc", "serde", "serde_json", "chrono", "uuid", "url", "bytes",
    "tokio", "hiqlite", "rusqlite", "time", "http", "axum", "smallvec",
}
# Bare names that are std, serde or a dependency, never followed.
EXTERNAL_NAMES = {
    "Option", "Vec", "Box", "Arc", "Rc", "String", "BTreeMap", "BTreeSet", "HashMap",
    "HashSet", "VecDeque", "Cow", "PathBuf", "Path", "Duration", "SystemTime", "Instant",
    "Uuid", "Value", "Map", "Ipv4Addr", "Ipv6Addr", "IpAddr", "SocketAddr", "Url", "Bytes",
    "NonZeroU8", "NonZeroU16", "NonZeroU32", "NonZeroU64", "NonZeroUsize", "Self", "T",
}
# The type may start on the next line (`name:\n    Option<…>,`), as rustfmt
# writes a long one.
FIELD_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?([a-z_][a-z0-9_]*)\s*:(?!:)\s*(.*?),?\s*$")
PROTOCOL_RE = re.compile(r"PROTOCOL_VERSION\s*:\s*i64\s*=\s*(\d+)\s*;")


@dataclass(frozen=True)
class Field:
    name: str
    type: str
    default: bool
    flatten: bool

    def signature(self) -> tuple[str, str, bool]:
        return (self.name, self.type, self.flatten)


@dataclass
class Variant:
    name: str
    #: Struct-variant fields (by wire name), or `None` for a unit/tuple variant.
    fields: dict[str, Field] | None = None
    #: Tuple-variant element types, whitespace-free.
    types: tuple[str, ...] = ()

    def signature(self) -> tuple:
        return (self.name, self.types, None if self.fields is None else "struct")


@dataclass
class Envelope:
    name: str
    strict: bool
    fields: dict[str, Field] = field(default_factory=dict)
    #: `None` for a struct; the variants by wire name for an enum.
    variants: dict[str, Variant] | None = None
    #: The wire-shaping container attributes, normalized.
    shape: tuple[str, ...] = ()
    #: An enum with a `#[serde(other)]` catch-all takes unknown variants.
    other: bool = False

    def types(self) -> list[str]:
        """Every field and variant type, for following."""
        found = [f.type for f in self.fields.values()]
        for variant in (self.variants or {}).values():
            found.extend(variant.types)
            found.extend(f.type for f in (variant.fields or {}).values())
        return found


def _serde_args(attrs: list[str]) -> str:
    return " ".join(a for a in attrs if a.startswith("#[serde("))


def _shape(serde: str) -> tuple[str, ...]:
    shape = []
    for attr in SHAPE_ATTRS:
        match = re.search(rf'\b{attr}\b(?:\s*=\s*"([^"]*)")?', serde)
        if match:
            shape.append(f"{attr}={match.group(1) or ''}")
    return tuple(shape)


def _split_top(text: str) -> list[str]:
    """Split on commas outside <>, (), []."""
    parts, depth, current = [], 0, ""
    for ch in text:
        if ch in "<([":
            depth += 1
        elif ch in ">)]":
            depth -= 1
        if ch == "," and depth == 0:
            parts.append(current)
            current = ""
        else:
            current += ch
    parts.append(current)
    return [re.sub(r"\s+", "", p) for p in parts if p.strip()]


def _parse_enum_body(envelope: Envelope, lines: list[str], i: int) -> int:
    """Read an enum body starting after its `{` line; returns the index past it."""
    variants: dict[str, Variant] = {}
    attrs: list[str] = []
    depth = 1
    current: Variant | None = None
    field_attrs: list[str] = []
    pending: str | None = None
    tuple_text: str | None = None
    while i < len(lines) and depth > 0:
        text = re.sub(r"\s//.*$", "", lines[i]).rstrip()
        stripped = text.strip()
        if stripped.startswith("//"):
            i += 1
            continue
        if tuple_text is not None:
            tuple_text += " " + stripped
            if _balanced(tuple_text):
                inner = tuple_text[tuple_text.index("(") + 1 : tuple_text.rindex(")")]
                current.types = tuple(_split_top(inner))
                tuple_text = None
        elif depth == 1 and stripped.startswith("#["):
            attrs.append(stripped)
        elif depth == 1 and stripped:
            match = VARIANT_RE.match(text)
            if match:
                serde = _serde_args(attrs)
                if re.search(r"\bother\b", serde):
                    envelope.other = True
                rename = re.search(r'rename\s*=\s*"([^"]+)"', serde)
                skipped = re.search(r"\bskip(?:_deserializing)?\b(?!_s)", serde)
                name = rename.group(1) if rename else match.group(1)
                current = Variant(name=name)
                rest = match.group(2).strip()
                if rest.startswith("("):
                    if _balanced(rest):
                        inner = rest[rest.index("(") + 1 : rest.rindex(")")]
                        current.types = tuple(_split_top(inner))
                    else:
                        tuple_text = rest
                elif rest.startswith("{"):
                    current.fields = {}
                    body = rest[1:]
                    if "}" in body:
                        holder = Envelope(name=name, strict=envelope.strict)
                        for part in _split_top(body[: body.rindex("}")]):
                            _add_field(holder, part.replace(":", ": ", 1), [])
                        current.fields = holder.fields
                if not skipped:
                    variants[name] = current
                attrs = []
        elif depth == 2 and current is not None and current.fields is not None:
            if stripped.startswith("#["):
                field_attrs.append(stripped)
            elif stripped and pending is None and FIELD_RE.match(text):
                pending = stripped
            elif stripped and pending is not None and not stripped.startswith("}"):
                pending += " " + stripped
            if pending is not None and (_complete(pending) or stripped.startswith("}")):
                holder = Envelope(name=current.name, strict=envelope.strict, fields=current.fields)
                _add_field(holder, pending, field_attrs)
                pending, field_attrs = None, []
        depth += text.count("{") - text.count("}")
        i += 1
    envelope.variants = variants
    return i


def _skip_body(lines: list[str], i: int) -> int:
    depth = 1
    while i < len(lines) and depth > 0:
        text = re.sub(r"\s//.*$", "", lines[i])
        depth += text.count("{") - text.count("}")
        i += 1
    return i


def parse_envelopes(source: str, proxies: bool = False) -> dict[str, Envelope]:
    """Every struct and enum in `source` that derives `Deserialize`, keyed by
    name; a repeated name gets `#2`, `#3` in order, since a function-local
    `Wire` is still an envelope. A `try_from`/`from` proxy is skipped (its
    `…Wire` struct is the envelope) unless `proxies`, when it is kept as a
    single `<from>` field so following reaches the proxy type. Skipping a
    non-wire struct skips its body too: its fields never match the struct
    pattern."""
    lines = source.splitlines()
    envelopes: dict[str, Envelope] = {}
    attrs: list[str] = []
    i = 0
    while i < len(lines):
        line = lines[i].strip()
        if line.startswith("#[") or line.startswith("///") or line.startswith("//"):
            if line.startswith("#["):
                attrs.append(line)
            i += 1
            continue
        match = STRUCT_RE.match(lines[i])
        enum = ENUM_RE.match(lines[i])
        tuple_struct = TUPLE_STRUCT_RE.match(lines[i])
        if not (match or enum or tuple_struct):
            attrs = []
            i += 1
            continue
        name = (match or enum or tuple_struct).group(1)
        derives = " ".join(a for a in attrs if a.startswith("#[derive("))
        serde = _serde_args(attrs)
        proxy = re.search(r'\b(?:try_from|from)\s*=\s*"([^"]+)"', serde)
        wire = "Deserialize" in derives
        if not wire or (proxy and not proxies):
            attrs = []
            i += 1
            continue
        key, n = name, 2
        while key in envelopes:
            key, n = f"{name}#{n}", n + 1
        envelope = Envelope(name=key, strict="deny_unknown_fields" in serde, shape=_shape(serde))
        attrs = []
        if proxy:
            # Followed only: the wire is the proxy type's.
            envelope.fields["<from>"] = Field("<from>", re.sub(r"\s+", "", proxy.group(1)), False, False)
            envelopes[key] = envelope
            if match or enum:
                i = _skip_body(lines, i + 1)
            else:
                i += 1
            continue
        if tuple_struct:
            for n_, type_ in enumerate(_split_top(tuple_struct.group(2))):
                type_ = re.sub(r"^pub(\([^)]*\))?", "", type_)
                envelope.fields[str(n_)] = Field(str(n_), type_, False, False)
            envelopes[key] = envelope
            i += 1
            continue
        if enum:
            i = _parse_enum_body(envelope, lines, i + 1)
            envelopes[key] = envelope
            continue
        depth = 1
        field_attrs: list[str] = []
        pending: str | None = None
        i += 1
        while i < len(lines) and depth > 0:
            text = re.sub(r"\s//.*$", "", lines[i]).rstrip()
            stripped = text.strip()
            if stripped.startswith("//"):
                i += 1
                continue
            if depth == 1 and stripped.startswith("#["):
                field_attrs.append(stripped)
            elif depth == 1 and pending is not None and stripped.startswith("}"):
                _add_field(envelope, pending, field_attrs)
                pending, field_attrs = None, []
            elif depth == 1 and stripped and pending is None:
                match_field = FIELD_RE.match(text)
                if match_field:
                    pending = stripped
                    if _complete(stripped):
                        _add_field(envelope, pending, field_attrs)
                        pending, field_attrs = None, []
            elif depth == 1 and pending is not None:
                pending += " " + stripped
                if _complete(pending):
                    _add_field(envelope, pending, field_attrs)
                    pending, field_attrs = None, []
            depth += text.count("{") - text.count("}")
            i += 1
        if pending is not None:
            _add_field(envelope, pending, field_attrs)
        envelopes[key] = envelope
    return envelopes


def _balanced(text: str) -> bool:
    return all(text.count(o) <= text.count(c) for o, c in ("<>", "()", "[]"))


def _complete(text: str) -> bool:
    """A field declaration is whole once its brackets close and it ends in
    the comma rustfmt always writes (the closing `}` flushes a last one)."""
    return _balanced(text) and text.rstrip().endswith(",")


def _add_field(envelope: Envelope, text: str, attrs: list[str]) -> None:
    match = FIELD_RE.match(text)
    if not match:
        return
    serde = _serde_args(attrs)
    if re.search(r"\bskip\b(?!_)", serde):
        return
    rename = re.search(r'rename\s*=\s*"([^"]+)"', serde)
    name = rename.group(1) if rename else match.group(1)
    type_ = re.sub(r"\s+", "", match.group(2).rstrip(","))
    envelope.fields[name] = Field(
        name=name,
        type=type_,
        default=bool(re.search(r"\bdefault\b", serde)),
        flatten="flatten" in serde,
    )


class Tree:
    """A source tree: read a file, and find the files defining a type."""

    def read(self, path: str) -> str | None:  # pragma: no cover - interface
        raise NotImplementedError

    def find(self, name: str) -> list[str]:  # pragma: no cover - interface
        raise NotImplementedError


class DictTree(Tree):
    """Fixed sources (tests), or overrides on top of another tree."""

    def __init__(self, files: dict[str, str], under: Tree | None = None):
        self.files, self.under = files, under

    def read(self, path: str) -> str | None:
        if path in self.files:
            return self.files[path]
        return self.under.read(path) if self.under else None

    def find(self, name: str) -> list[str]:
        pattern = _definition_re(name)
        found = {p for p, src in self.files.items() if pattern.search(src)}
        if self.under:
            found |= {p for p in self.under.find(name) if p not in self.files}
        return sorted(found)


class GitTree(Tree):
    """The working tree (`rev=None`) or a commit, through git."""

    def __init__(self, rev: str | None):
        self.rev, self._read, self._find = rev, {}, {}

    def read(self, path: str) -> str | None:
        if path not in self._read:
            if self.rev is None:
                file = ROOT / path
                self._read[path] = file.read_text(encoding="utf-8") if file.is_file() else None
            else:
                self._read[path] = _show(self.rev, path)
        return self._read[path]

    def find(self, name: str) -> list[str]:
        if name not in self._find:
            pattern = rf"^\s*(pub(\([^)]*\))?\s+)?(struct|enum)\s+{name}\b"
            args = ["grep", "-l", "-E", pattern] + ([self.rev] if self.rev else []) + ["--", "crates/*.rs"]
            try:
                out = _git(*args)
            except subprocess.CalledProcessError:
                out = ""
            prefix = f"{self.rev}:" if self.rev else ""
            self._find[name] = sorted(line[len(prefix):] for line in out.splitlines() if line)
        return self._find[name]


#: The working tree, shared so its reads and searches are cached once.
HEAD = GitTree(None)


def _definition_re(name: str) -> re.Pattern:
    return re.compile(rf"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum)\s+{name}\b", re.M)


def _crate_of(path: str) -> str | None:
    match = re.match(r"(crates/[^/]+)/src/", path)
    return match.group(1) if match else None


def _module_files(crate: str, segments: list[str]) -> list[str]:
    base = f"{crate}/src/" + "/".join(segments)
    return [f"{base}.rs", f"{base}/mod.rs"] if segments else [f"{crate}/src/lib.rs", f"{crate}/src/main.rs"]


def _module_of(path: str) -> list[str]:
    crate = _crate_of(path)
    rel = path[len(crate) + len("/src/") :] if crate else path
    parts = rel[: -len(".rs")].split("/")
    if parts[-1] in ("mod", "lib", "main"):
        parts = parts[:-1]
    return parts


def _path_candidates(path: str, segments: list[str]) -> list[str]:
    """The files a module path (before the type name) may name."""
    if not segments:
        return []
    crate = _crate_of(path)
    head = segments[0]
    if head == "crate" and crate:
        return _module_files(crate, segments[1:])
    if head in ("self", "super") and crate:
        module = _module_of(path)
        rest = segments
        while rest and rest[0] in ("self", "super"):
            if rest[0] == "super":
                module = module[:-1]
            rest = rest[1:]
        return _module_files(crate, module + rest)
    if head.startswith("plurx"):
        return _module_files(f"crates/{head.replace('_', '-')}", segments[1:])
    return []


def _use_path(source: str, name: str) -> list[str] | None:
    """The module path a `use` line imports `name` from, if any."""
    for match in re.finditer(r"^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+([^;]+);", source, re.M):
        text = re.sub(r"\s+", "", match.group(1))
        if re.search(rf"(?:^|[:{{,]){name}(?:$|[}},])", text) is None:
            continue
        if "{" in text:
            prefix = text[: text.index("{")].rstrip(":")
        else:
            prefix = text.rsplit("::", 1)[0] if "::" in text else ""
        return [segment for segment in prefix.split("::") if segment]
    return None


def resolve(tree: Tree, path: str, written: str) -> list[str]:
    """The files defining the type `written` (`a::b::Name` or `Name`) as named
    from `path`; empty when it is external or not found in the repo."""
    *segments, name = written.split("::")
    if name in EXTERNAL_NAMES and not segments:
        return []
    if segments and segments[0] in EXTERNAL_ROOTS:
        return []
    pattern = _definition_re(name)
    source = tree.read(path) or ""
    if not segments:
        if pattern.search(source):
            return [path]
        segments = _use_path(source, name) or []
        if segments and segments[0] in EXTERNAL_ROOTS:
            return []
    for candidate in _path_candidates(path, segments):
        text = tree.read(candidate)
        if text is not None and pattern.search(text):
            return [candidate]
    found = tree.find(name)
    crate = segments[0].replace("_", "-") if segments and segments[0].startswith("plurx") else None
    own = [f for f in found if _crate_of(f) == (f"crates/{crate}" if crate else _crate_of(path))]
    return own or found


def collect_wire(tree: Tree, roots: tuple[str, ...] = WIRE_FILES) -> dict[str, dict[str, Envelope]]:
    """The root envelopes of `roots`, and every in-repo type their fields name,
    followed to a fixpoint, keyed by defining file then type name."""
    wire: dict[str, dict[str, Envelope]] = {}
    queue: list[tuple[str, Envelope]] = []
    for path in roots:
        source = tree.read(path)
        if source is None:
            continue
        wire[path] = parse_envelopes(source)
        queue.extend((path, envelope) for envelope in wire[path].values())
    parsed: dict[str, dict[str, Envelope]] = {}
    seen: set[tuple[str, str]] = {(p, k) for p, envs in wire.items() for k in envs}
    while queue:
        path, envelope = queue.pop()
        for type_ in envelope.types():
            for prefix, name in TYPE_NAME_RE.findall(type_):
                written = prefix + name
                for defining in resolve(tree, path, written):
                    if (defining, name) in seen:
                        continue
                    seen.add((defining, name))
                    if defining not in parsed:
                        parsed[defining] = parse_envelopes(tree.read(defining) or "", proxies=True)
                    found = parsed[defining].get(name)
                    if found is None:
                        continue  # no Deserialize derive: opaque
                    wire.setdefault(defining, {})[name] = found
                    queue.append((defining, found))
    return wire


def protocol_version(source: str) -> int | None:
    match = PROTOCOL_RE.search(source)
    return int(match.group(1)) if match else None


def rollout_notes(operations: str) -> str:
    heading = re.search(r"(?m)^" + re.escape(ROLLING_HEADING) + r"[ \t]*$", operations)
    if heading is None:
        return ""
    start = heading.start()
    end = operations.find("\n## ", heading.end())
    return operations[start : end if end >= 0 else len(operations)]


def has_dated_note(notes: str, protocol: int) -> bool:
    return any(
        re.search(r"\b\d{4}-\d{2}-\d{2}\b", line) and re.search(rf"\bprotocol {protocol}\b", line)
        for line in notes.splitlines()
    )


def wire_findings(
    base: dict[str, dict[str, Envelope]],
    head: dict[str, dict[str, Envelope]],
    base_protocol: int | None,
    head_protocol: int | None,
    notes: str,
) -> list[str]:
    """Problems with HEAD's wire relative to the base; empty when compatible."""
    findings: list[str] = []
    needs_bump: list[str] = []
    for path, head_envelopes in head.items():
        for key, now in head_envelopes.items():
            before = base.get(path, {}).get(key)
            if before is None:
                continue
            if before.shape != now.shape:
                needs_bump.append(f"{path}::{key} (serde {', '.join(before.shape) or 'none'} -> {', '.join(now.shape) or 'none'})")
                continue
            if before.variants is not None or now.variants is not None:
                needs_bump.extend(_enum_changes(path, key, before, now, findings))
                continue
            if before.strict != now.strict:
                needs_bump.append(f"{path}::{key} (strict)")
                continue
            needs_bump.extend(_field_changes(f"{path}::{key}", now.strict, before.fields, now.fields, findings))
    if needs_bump:
        bumped = base_protocol is not None and head_protocol is not None and head_protocol > base_protocol
        if not bumped:
            findings.append(
                "media-pool wire changed without a PROTOCOL_VERSION bump "
                f"(base {base_protocol}, head {head_protocol}): " + "; ".join(needs_bump)
            )
        elif not has_dated_note(notes, head_protocol):
            findings.append(
                f"PROTOCOL_VERSION rose to {head_protocol}; add a dated line naming "
                f"`protocol {head_protocol}` under {ROLLING_HEADING!r} in docs/OPERATIONS.md "
                "saying what changed and how to roll it out"
            )
    return findings


def _field_changes(where: str, strict: bool, before: dict[str, Field], now: dict[str, Field], findings: list[str]) -> list[str]:
    """The struct rules on one field set: what needs a bump, and (into
    `findings`) a new lenient field without a default."""
    if strict:
        old = {f.signature() for f in before.values()}
        new = {f.signature() for f in now.values()}
        return [f"{where} (strict)"] if old != new else []
    bump = []
    removed_or_retyped = {
        name for name, f in before.items() if name not in now or now[name].signature() != f.signature()
    }
    if removed_or_retyped:
        bump.append(f"{where} ({', '.join(sorted(removed_or_retyped))} removed or retyped)")
    for name, f in now.items():
        if name not in before and not f.default:
            findings.append(
                f"{where}.{name} is a new field on a lenient envelope without "
                "#[serde(default)]: a new node could not parse an old peer's message"
            )
    return bump


def _enum_changes(path: str, key: str, before: Envelope, now: Envelope, findings: list[str]) -> list[str]:
    """An enum's variant set is a strict field set (an old node refuses a
    variant it does not know) unless a `#[serde(other)]` catch-all takes
    additions; a struct variant's fields follow the enum's strictness."""
    where = f"{path}::{key}"
    old, new = before.variants or {}, now.variants or {}
    bump = []
    added = sorted(set(new) - set(old))
    removed = sorted(set(old) - set(new))
    if removed or (added and not (before.other and now.other)) or before.strict != now.strict:
        bump.append(
            f"{where} (variants" + (f" +{','.join(added)}" if added else "")
            + (f" -{','.join(removed)}" if removed else "") + ")"
        )
    for name in sorted(set(old) & set(new)):
        a, b = old[name], new[name]
        if a.signature() != b.signature():
            bump.append(f"{where}::{name} (variant shape)")
        elif a.fields is not None:
            bump.extend(_field_changes(f"{where}::{name}", before.strict or now.strict, a.fields, b.fields or {}, findings))
    return bump


def _git(*args: str) -> str:
    return subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True, text=True).stdout


def resolve_base() -> str | None:
    explicit = os.environ.get("PLURX_VALIDATION_BASE")
    try:
        if explicit:
            return _git("rev-parse", "--verify", f"{explicit}^{{commit}}").strip()
        return _git("merge-base", "HEAD", "origin/main").strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


def _show(base: str, path: str) -> str | None:
    try:
        return _git("show", f"{base}:{path}")
    except subprocess.CalledProcessError:
        return None


STRICT_SNAPSHOT = """
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Strict {
    #[serde(skip)]
    pub local_only: Option<u8>,
    pub file_id: i64,
}
"""

LENIENT_SNAPSHOT = """
#[derive(Serialize, Deserialize)]
pub(crate) struct MediaNodeSnapshot {
    pub node_id: String,
    #[serde(default)]
    pub io: MediaIoObservation,
}
"""


class RuleTests(unittest.TestCase):
    """The rules on fixed sources — these run without any git base."""

    def both(self, base_src: str, head_src: str, base_p=9, head_p=9, notes=""):
        return wire_findings(
            {"f.rs": parse_envelopes(base_src)},
            {"f.rs": parse_envelopes(head_src)},
            base_p,
            head_p,
            notes,
        )

    def test_parser_reads_strictness_fields_and_skips(self):
        env = parse_envelopes(STRICT_SNAPSHOT)["Strict"]
        self.assertTrue(env.strict)
        self.assertEqual(set(env.fields), {"file_id"})

    def test_a_scratch_field_without_default_on_the_snapshot_fails(self):
        head = LENIENT_SNAPSHOT.replace("pub node_id: String,", "pub node_id: String,\n    pub scratch: bool,")
        findings = self.both(LENIENT_SNAPSHOT, head)
        self.assertEqual(len(findings), 1)
        self.assertIn("MediaNodeSnapshot.scratch", findings[0])

    def test_a_defaulted_scratch_field_on_the_snapshot_passes(self):
        head = LENIENT_SNAPSHOT.replace(
            "pub node_id: String,", "pub node_id: String,\n    #[serde(default)]\n    pub scratch: bool,"
        )
        self.assertEqual(self.both(LENIENT_SNAPSHOT, head), [])

    def test_rollout_notes_ignore_a_router_mention_before_the_heading(self):
        document = 'See `## Rolling upgrades`.\n\n## Another section\nReference.\n\n## Rolling upgrades\n- 2026-11-13: protocol 10 adds a pinned digest.\n\n## Configuration\n'
        notes = rollout_notes(document)
        self.assertTrue(has_dated_note(notes, 10))
        self.assertNotIn('Configuration', notes)

    def test_a_strict_field_needs_a_bump_and_a_dated_note(self):
        head = STRICT_SNAPSHOT.replace(
            "pub file_id: i64,",
            "pub file_id: i64,\n    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n    pub extra: Option<u8>,",
        )
        unbumped = self.both(STRICT_SNAPSHOT, head)
        self.assertTrue(unbumped and "without a PROTOCOL_VERSION bump" in unbumped[0])
        unnoted = self.both(STRICT_SNAPSHOT, head, 9, 10)
        self.assertTrue(unnoted and "dated line" in unnoted[0])
        noted = self.both(STRICT_SNAPSHOT, head, 9, 10, "## Rolling upgrades\n- 2026-11-07: protocol 10 adds `extra`.\n")
        self.assertEqual(noted, [])

    def test_a_skip_field_is_not_on_the_wire(self):
        head = STRICT_SNAPSHOT.replace("pub local_only: Option<u8>,", "pub local_only: Option<u16>,")
        self.assertEqual(self.both(STRICT_SNAPSHOT, head), [])

    def test_removing_a_lenient_field_needs_a_bump(self):
        head = LENIENT_SNAPSHOT.replace("    #[serde(default)]\n    pub io: MediaIoObservation,\n", "")
        findings = self.both(LENIENT_SNAPSHOT, head)
        self.assertTrue(findings and "io removed or retyped" in findings[0])

    def test_multi_line_types_parse_whole(self):
        src = "#[derive(Deserialize)]\n#[serde(deny_unknown_fields)]\nstruct W {\n    pub a: BTreeMap<\n        String,\n        i64,\n    >,\n    pub b: u8,\n}\n"
        env = parse_envelopes(src)["W"]
        self.assertEqual(set(env.fields), {"a", "b"})
        self.assertEqual(env.fields["a"].type, "BTreeMap<String,i64,>")


STRICT_ENUM = """
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Kind {
    Transcode {
        height: i64,
    },
    /// Copy.
    Copy {
        aac: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        dual: bool,
    },
}
"""

OPEN_ENUM = """
#[derive(Serialize, Deserialize)]
pub enum Telemetry {
    #[serde(rename = "film-v1")]
    FilmV1,
    #[serde(other)]
    Unknown,
}
"""

ROOT_SRC = """
use crate::kinds::Kind;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub kind: Kind,
    pub audio: Option<plurx_core::audio::Delivery>,
}
"""

AUDIO_SRC = """
#[derive(Serialize, Deserialize)]
pub struct Delivery {
    pub action: Action,
    pub reason: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Action {
    None,
    Encode { codec: String },
}
"""


class EnumAndFollowingTests(unittest.TestCase):
    """Enums are strict field sets, and field types are followed to their
    in-repo definitions — fixed sources, no git."""

    def enum(self, base_src: str, head_src: str):
        return wire_findings(
            {"f.rs": parse_envelopes(base_src)}, {"f.rs": parse_envelopes(head_src)}, 9, 9, ""
        )

    def test_the_enum_parser_reads_variants_tag_and_fields(self):
        env = parse_envelopes(STRICT_ENUM)["Kind"]
        self.assertTrue(env.strict)
        self.assertEqual(set(env.variants), {"Transcode", "Copy"})
        self.assertEqual(set(env.variants["Copy"].fields), {"aac", "dual"})
        self.assertIn("tag=kind", env.shape)
        open_ = parse_envelopes(OPEN_ENUM)["Telemetry"]
        self.assertTrue(open_.other)
        self.assertEqual(set(open_.variants), {"film-v1", "Unknown"})

    def test_a_new_variant_needs_a_bump(self):
        head = STRICT_ENUM.replace("    /// Copy.\n", "    Probe,\n    /// Copy.\n")
        findings = self.enum(STRICT_ENUM, head)
        self.assertTrue(findings and "Kind (variants +Probe)" in findings[0], findings)

    def test_a_new_variant_under_a_catch_all_passes_and_a_removal_does_not(self):
        added = OPEN_ENUM.replace("    FilmV1,\n", "    FilmV1,\n    #[serde(rename = \"film-v2\")]\n    FilmV2,\n")
        self.assertEqual(self.enum(OPEN_ENUM, added), [])
        removed = OPEN_ENUM.replace("    #[serde(rename = \"film-v1\")]\n    FilmV1,\n", "")
        self.assertTrue(self.enum(OPEN_ENUM, removed))

    def test_a_field_on_a_strict_struct_variant_needs_a_bump_even_defaulted(self):
        head = STRICT_ENUM.replace(
            "        aac: bool,\n", "        aac: bool,\n        #[serde(default)]\n        extra: bool,\n"
        )
        findings = self.enum(STRICT_ENUM, head)
        self.assertTrue(findings and "Kind::Copy (strict)" in findings[0], findings)

    def test_a_changed_tag_needs_a_bump(self):
        head = STRICT_ENUM.replace('tag = "kind"', 'tag = "type"')
        self.assertTrue(self.enum(STRICT_ENUM, head))

    KINDS, AUDIO = "crates/plurxd/src/kinds.rs", "crates/plurx-core/src/audio.rs"

    def tree(self, overrides: dict[str, str] | None = None):
        files = {"crates/plurxd/src/root.rs": ROOT_SRC, self.KINDS: STRICT_ENUM, self.AUDIO: AUDIO_SRC}
        files.update(overrides or {})
        return DictTree(files)

    def follow(self, head: DictTree):
        roots = ("crates/plurxd/src/root.rs",)
        return wire_findings(collect_wire(self.tree(), roots), collect_wire(head, roots), 9, 9, "")

    def test_following_reaches_a_use_import_and_a_plurx_core_path(self):
        wire = collect_wire(self.tree(), ("crates/plurxd/src/root.rs",))
        self.assertIn("Kind", wire["crates/plurxd/src/kinds.rs"])
        self.assertEqual(set(wire["crates/plurx-core/src/audio.rs"]), {"Delivery", "Action"})

    def test_a_variant_added_in_another_file_needs_a_bump(self):
        head = STRICT_ENUM.replace("    /// Copy.\n", "    Probe,\n    /// Copy.\n")
        findings = self.follow(self.tree({self.KINDS: head}))
        self.assertTrue(any("kinds.rs::Kind (variants +Probe)" in f for f in findings), findings)

    def test_a_bare_field_on_a_followed_lenient_struct_fails(self):
        head = AUDIO_SRC.replace("    pub reason: String,\n", "    pub reason: String,\n    pub scratch: bool,\n")
        findings = self.follow(self.tree({self.AUDIO: head}))
        self.assertTrue(any("audio.rs::Delivery.scratch" in f for f in findings), findings)
        defaulted = AUDIO_SRC.replace(
            "    pub reason: String,\n", "    pub reason: String,\n    #[serde(default)]\n    pub scratch: bool,\n"
        )
        self.assertEqual(self.follow(self.tree({self.AUDIO: defaulted})), [])

    def test_a_variant_on_a_type_two_hops_away_needs_a_bump(self):
        head = AUDIO_SRC.replace("    None,\n", "    None,\n    Passthrough,\n")
        findings = self.follow(self.tree({self.AUDIO: head}))
        self.assertTrue(any("audio.rs::Action (variants +Passthrough)" in f for f in findings), findings)


class TreeTests(unittest.TestCase):
    """The rules on the real envelopes, against the merge base."""

    def test_the_real_envelopes_parse(self):
        source = (ROOT / PROTOCOL_FILE).read_text(encoding="utf-8")
        envelopes = parse_envelopes(source)
        self.assertIsNotNone(protocol_version(source))
        self.assertTrue(envelopes["QualityCatalogRequest"].strict)
        self.assertFalse(envelopes["MediaNodeSnapshot"].strict)
        self.assertIn("protocol_version", envelopes["MediaNodeSnapshot"].fields)

    def test_a_scratch_field_on_the_real_snapshot_and_response(self):
        """The contract's counterfactual, on the shipped envelopes: a bare
        field on `MediaNodeSnapshot` fails; with `#[serde(default)]` it
        passes; any field on the strict `RemoteStartResponse` needs a bump."""
        pool = (ROOT / PROTOCOL_FILE).read_text(encoding="utf-8")
        sessions = (ROOT / WIRE_FILES[1]).read_text(encoding="utf-8")
        version = protocol_version(pool)
        anchor = "pub(crate) struct MediaNodeSnapshot {\n"
        self.assertIn(anchor, pool)

        def check(pool_head: str, sessions_head: str, head_version=version, notes=""):
            return wire_findings(
                {"p": parse_envelopes(pool), "s": parse_envelopes(sessions)},
                {"p": parse_envelopes(pool_head), "s": parse_envelopes(sessions_head)},
                version,
                head_version,
                notes,
            )

        bare = pool.replace(anchor, anchor + "    pub scratch: bool,\n", 1)
        self.assertTrue(any("MediaNodeSnapshot.scratch" in f for f in check(bare, sessions)))
        defaulted = pool.replace(anchor, anchor + "    #[serde(default)]\n    pub scratch: bool,\n", 1)
        self.assertEqual(check(defaulted, sessions), [])
        response = "pub(crate) struct RemoteStartResponse {\n"
        strict = sessions.replace(response, response + "    pub scratch: bool,\n", 1)
        self.assertTrue(any("RemoteStartResponse (strict)" in f for f in check(pool, strict)))
        note = f"## Rolling upgrades\n- 2026-11-07: protocol {version + 1} adds scratch.\n"
        self.assertEqual(check(pool, strict, version + 1, note), [])

    def test_the_real_nested_types_are_followed(self):
        """`SessionRequest` reaches its enums in the same file and the
        plurx-core audio types through their written path."""
        wire = collect_wire(HEAD)
        session = wire[WIRE_FILES[2]]
        for name in ("SessionKind", "ReopenReason", "Presentation", "SourceTelemetry"):
            self.assertIn(name, session)
        self.assertTrue(session["SessionKind"].strict)
        self.assertTrue(session["SourceTelemetry"].other)
        audio = wire.get("crates/plurx-core/src/playback/audio.rs", {})
        self.assertIn("AudioDelivery", audio)
        self.assertIn("AudioClaim", audio)

    def test_a_new_session_kind_variant_needs_a_bump(self):
        """The review's counterfactual: a variant on the strict, tagged
        `SessionKind` changes what an old worker can parse."""
        head = HEAD
        source = head.read(WIRE_FILES[2])
        anchor = "pub enum SessionKind {\n"
        self.assertIn(anchor, source)
        changed = DictTree({WIRE_FILES[2]: source.replace(anchor, anchor + "    Probe,\n", 1)}, head)
        findings = wire_findings(collect_wire(head), collect_wire(changed), 9, 9, "")
        self.assertTrue(any("SessionKind (variants +probe)" in f or "SessionKind (variants +Probe)" in f for f in findings), findings)

    def test_a_bare_field_on_audio_delivery_fails_and_a_defaulted_one_passes(self):
        head = HEAD
        audio = "crates/plurx-core/src/playback/audio.rs"
        source = head.read(audio)
        anchor = "pub struct AudioDelivery {\n"
        self.assertIn(anchor, source)

        def check(text: str) -> list[str]:
            changed = DictTree({audio: source.replace(anchor, anchor + text, 1)}, head)
            return wire_findings(collect_wire(head), collect_wire(changed), 9, 9, "")

        bare = check("    pub scratch: bool,\n")
        self.assertTrue(any("AudioDelivery.scratch" in f for f in bare), bare)
        self.assertEqual(check("    #[serde(default)]\n    pub scratch: bool,\n"), [])

    def test_head_wire_is_compatible_with_the_base(self):
        base = resolve_base()
        if base is None:
            self.skipTest("no merge base with origin/main; scripts/paperwork supplies one")
        base_tree, head_tree = GitTree(base), HEAD
        findings = wire_findings(
            collect_wire(base_tree),
            collect_wire(head_tree),
            protocol_version(base_tree.read(PROTOCOL_FILE) or ""),
            protocol_version(head_tree.read(PROTOCOL_FILE) or ""),
            rollout_notes(OPERATIONS.read_text(encoding="utf-8")),
        )
        self.assertEqual(findings, [], "\n".join(findings))


if __name__ == "__main__":
    unittest.main()
