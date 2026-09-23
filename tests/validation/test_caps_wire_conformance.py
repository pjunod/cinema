"""The capabilities document has four implementations. This pins their names.

`test_control_wire_conformance.py` exists because a field name is what drifts
in practice — a snake_case key spelled camelCase by a key strategy, a
`@SerialName` forgotten on a new property, a JS object literal using the Swift
spelling. The capabilities document had no equivalent guard, and it is more
dangerous than the control wire in one specific way: `DeviceCaps` has no
`deny_unknown_fields` and every field is `#[serde(default)]`, so a misspelled
claim does not fail. It is silently dropped and the server reads "not claimed",
with nothing anywhere saying so.

What that costs, stated precisely, because an earlier wording here had it
backwards: a dropped claim is a **needless burn**, not an unplayable delivery.
The client is told the track needs burning in, the server burns it, and the
viewer sees subtitles — having spent a full re-encode, and on an HDR source the
grade along with it, to draw pictures the device could have drawn itself. The
delivery always plays. It is just expensive and quietly worse, which is exactly
the kind of failure that never generates a report.

So this asserts the opposite of the usual thing. It does not check that the
ports agree on a schema; it checks that a capability one of them claims is
spelled the way the server reads it, and — for the two ports whose claim is a
constant rather than a runtime answer — that the claim actually reaches the
wire.

**What this cannot catch, and what does.** It reads source text; it never
encodes a document. So it cannot see a key strategy changed in `PlurxAPI.swift`
(it asserts the property Apple's *current* strategy needs, and the strategy is
named in a comment below, not verified), a `@Serializable` dropped from
`CapsPolicy`, a caller that builds its own document instead of using these
types, or a renderer that stops working while the claim stays. The end-to-end
proof of any of that is a real PGS title played on a real device — §5.5 of the
RCA, not this file. What this file does catch is the silent-drop class, which
is the one that produces no error anywhere.
"""

from __future__ import annotations

import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]

SERVER = ROOT / "crates/plurx-core/src/playback/caps.rs"
APPLE = ROOT / "clients/apple/Sources/Caps.swift"
ANDROID = ROOT / "clients/android/app/src/main/java/tv/plurx/app/data/CapsPolicy.kt"
WEB = ROOT / "crates/plurxd/src/web/player/decode-tiers.js"

# The wire spelling. Every port either uses this exact string or produces it
# through a documented key strategy, and the per-port checks below say which.
FIELD = "subtitle_overlays"
PROTOCOL = "pgs-v1"


def read(path: pathlib.Path) -> str:
    assert path.is_file(), f"missing port source: {path}"
    return path.read_text(encoding="utf-8")


def strip_js_line_comments(source: str) -> tuple[str, str]:
    """Split JavaScript into (code, comments) on `//` line comments.

    Deliberately crude — it is not a parser and does not need to be. It skips a
    `//` that follows a colon so a `https://` in a string is not mistaken for a
    comment, which is the only false positive this file has ever had to care
    about. It is used to ask one question: does the web player *mention* the
    field in prose while *not* emitting it as a key. Both halves of that stay
    true through reformatting, reordering and renamed neighbours, which a
    fixed-offset window around a sibling key does not.
    """
    code_lines: list[str] = []
    comment_lines: list[str] = []
    for line in source.splitlines():
        cut = None
        for match in re.finditer(r"//", line):
            if match.start() > 0 and line[match.start() - 1] == ":":
                continue
            cut = match.start()
            break
        if cut is None:
            code_lines.append(line)
        else:
            code_lines.append(line[:cut])
            comment_lines.append(line[cut:])
    return "\n".join(code_lines), "\n".join(comment_lines)


class CapsWireConformance(unittest.TestCase):
    def test_the_server_reads_the_snake_case_name(self) -> None:
        source = read(SERVER)
        self.assertIn(
            f"pub {FIELD}: Vec<String>",
            source,
            f"the server's field is what defines the wire name {FIELD!r}",
        )
        self.assertRegex(
            source,
            rf"#\[serde\(default\)\]\s*\n\s*pub {FIELD}\b",
            "an absent claim must default rather than refuse: absent is never a claim",
        )

    def test_apple_declares_the_property_its_key_strategy_turns_into_the_wire_name(
        self,
    ) -> None:
        source = read(APPLE)
        # Apple encodes with `.convertToSnakeCase` (PlurxAPI.swift), so the
        # Swift property must be the camelCase of the wire name and nothing
        # else. `subtitleOverlays` -> `subtitle_overlays`.
        camel = re.sub(r"_([a-z])", lambda m: m.group(1).upper(), FIELD)
        declaration = re.search(
            rf"var\s+{camel}\s*:\s*\[String\]\s*=\s*(?P<default>.+)", source
        )
        self.assertIsNotNone(
            declaration,
            f"Apple must declare {camel!r} so convertToSnakeCase emits {FIELD!r}",
        )
        # The type alone proves nothing: `= []` compiles, encodes, and reads on
        # the server as "this client cannot draw a bitmap" — which would send
        # an Apple TV off to burn a whole film while `PGSOverlay.swift` sits
        # compiled in and unused. The claim is unconditional, so its default is
        # the claim, and the default is what this asserts.
        default = declaration.group("default")
        self.assertIn(
            "PGSOverlayPolicy.protocolName",
            default,
            "Apple's renderer is compiled in, so the property's DEFAULT must "
            f"carry the claim; found {default.strip()!r}",
        )

    def test_android_spells_the_wire_name_directly_and_always_encodes_it(self) -> None:
        source = read(ANDROID)
        # Kotlin uses the snake_case wire names directly in this file rather
        # than @SerialName, so the property name IS the wire name.
        self.assertRegex(
            source,
            rf"val\s+{FIELD}\s*:\s*List<String>",
            f"Android must declare {FIELD!r} verbatim — this file uses no @SerialName",
        )
        # kotlinx omits a property equal to its default, so a constant claim
        # with a default never reaches the wire without this annotation.
        #
        # Bound to the property rather than to a window of preceding text: a
        # neighbouring property that carries the annotation for its own reasons
        # would satisfy a window and prove nothing about this one. Comments and
        # blank lines may sit between the two, nothing else.
        self.assertRegex(
            source,
            r"@EncodeDefault\(EncodeDefault\.Mode\.ALWAYS\)"
            r"(?:\s|//[^\n]*)*"
            rf"val\s+{FIELD}\b",
            "EncodeDefault.ALWAYS must annotate this property specifically, or "
            "kotlinx drops the claim and the server reads it as 'cannot draw'",
        )

    def test_every_port_that_claims_the_protocol_names_the_same_string(self) -> None:
        # The server carries the literal; the clients name it once in a
        # constant and reference that — which is why this follows each constant
        # to its definition rather than grepping for the string: a port that
        # de-duplicates a magic string is doing the right thing and must not be
        # failed for it.
        self.assertIn(PROTOCOL, read(SERVER), "the server names the protocol it serves")

        android_constant = read(ANDROID.parent.parent / "player" / "PGSOverlay.kt")
        self.assertIn(
            f'PGS_OVERLAY_PROTOCOL = "{PROTOCOL}"',
            android_constant,
            "Android's constant must be the protocol the server serves",
        )
        self.assertIn(
            "PGS_OVERLAY_PROTOCOL",
            read(ANDROID),
            "Android's caps document must claim the protocol by its constant",
        )

        self.assertIn(
            f'protocolName = "{PROTOCOL}"',
            read(APPLE.parent / "PGSOverlay.swift"),
            "Apple's constant must be the protocol the server serves",
        )
        self.assertIn(
            "PGSOverlayPolicy.protocolName",
            read(APPLE),
            "Apple's caps document must claim the protocol by its constant",
        )

    def test_the_web_claims_nothing_and_says_why(self) -> None:
        code, comments = strip_js_line_comments(read(WEB))
        self.assertNotIn(
            FIELD,
            code,
            "the web player has no PGS renderer; claiming one would earn it a "
            "delivery it cannot paint. Delete this assertion the day "
            "`decode-tiers.js` gains a renderer — not before.",
        )
        self.assertIn(
            FIELD,
            comments,
            "the absence must be deliberate and commented, not an oversight — "
            f"name {FIELD!r} in a comment saying why it is not claimed",
        )


if __name__ == "__main__":
    unittest.main()
