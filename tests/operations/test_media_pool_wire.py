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
every deserialized struct in the three files below (a `try_from` proxy is
checked through its `…Wire` struct); a struct that is new at HEAD has no
baseline and is not checked until it exists at the base.

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
FIELD_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?([a-z_][a-z0-9_]*)\s*:\s*(.+?),?\s*$")
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
class Envelope:
    name: str
    strict: bool
    fields: dict[str, Field] = field(default_factory=dict)


def _serde_args(attrs: list[str]) -> str:
    return " ".join(a for a in attrs if a.startswith("#[serde("))


def parse_envelopes(source: str) -> dict[str, Envelope]:
    """Every braced struct in `source` that is deserialized field by field
    (derives `Deserialize`, no `try_from`/`from` proxy), keyed by name; a
    repeated name gets `#2`, `#3` in order, since a function-local `Wire` is
    still an envelope. Skipping a non-wire struct skips its body too: its
    fields never match the struct pattern."""
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
        if not match:
            attrs = []
            i += 1
            continue
        name = match.group(1)
        derives = " ".join(a for a in attrs if a.startswith("#[derive("))
        serde = _serde_args(attrs)
        wire = "Deserialize" in derives and not re.search(r"\b(try_from|from)\s*=", serde)
        if not wire:
            attrs = []
            i += 1
            continue
        key, n = name, 2
        while key in envelopes:
            key, n = f"{name}#{n}", n + 1
        envelope = Envelope(name=key, strict="deny_unknown_fields" in serde)
        attrs = []
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
            elif depth == 1 and stripped and pending is None:
                match_field = FIELD_RE.match(text)
                if match_field:
                    pending = stripped
                    if _balanced(stripped):
                        _add_field(envelope, pending, field_attrs)
                        pending, field_attrs = None, []
            elif depth == 1 and pending is not None:
                pending += " " + stripped
                if _balanced(pending):
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


def protocol_version(source: str) -> int | None:
    match = PROTOCOL_RE.search(source)
    return int(match.group(1)) if match else None


def rollout_notes(operations: str) -> str:
    start = operations.find(ROLLING_HEADING)
    if start < 0:
        return ""
    end = operations.find("\n## ", start + len(ROLLING_HEADING))
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
            old = {f.signature() for f in before.fields.values()}
            new = {f.signature() for f in now.fields.values()}
            if before.strict or now.strict:
                if old != new or before.strict != now.strict:
                    needs_bump.append(f"{path}::{key} (strict)")
                continue
            removed_or_retyped = {
                name
                for name, f in before.fields.items()
                if name not in now.fields or now.fields[name].signature() != f.signature()
            }
            if removed_or_retyped:
                needs_bump.append(f"{path}::{key} ({', '.join(sorted(removed_or_retyped))} removed or retyped)")
            for name, f in now.fields.items():
                if name not in before.fields and not f.default:
                    findings.append(
                        f"{path}::{key}.{name} is a new field on a lenient envelope without "
                        "#[serde(default)]: a new node could not parse an old peer's message"
                    )
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

    def test_head_wire_is_compatible_with_the_base(self):
        base = resolve_base()
        if base is None:
            self.skipTest("no merge base with origin/main; scripts/paperwork supplies one")
        base_sources = {path: _show(base, path) for path in WIRE_FILES}
        head_sources = {path: (ROOT / path).read_text(encoding="utf-8") for path in WIRE_FILES}
        findings = wire_findings(
            {p: parse_envelopes(s) for p, s in base_sources.items() if s is not None},
            {p: parse_envelopes(s) for p, s in head_sources.items()},
            protocol_version(base_sources[PROTOCOL_FILE] or ""),
            protocol_version(head_sources[PROTOCOL_FILE]),
            rollout_notes(OPERATIONS.read_text(encoding="utf-8")),
        )
        self.assertEqual(findings, [], "\n".join(findings))


if __name__ == "__main__":
    unittest.main()
