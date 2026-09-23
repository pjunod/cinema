"""The capabilities document has four implementations. This pins their names.

`test_control_wire_conformance.py` exists because a field name is what drifts
in practice — a snake_case key spelled camelCase by a key strategy, a
`@SerialName` forgotten on a new property, a JS object literal using the Swift
spelling. The capabilities document had no equivalent guard, and it is more
dangerous than the control wire in one specific way: `DeviceCaps` has no
`deny_unknown_fields` and every field is `#[serde(default)]`, so a misspelled
claim does not fail. It is silently dropped, the server reads "not claimed",
and the client gets a delivery it cannot play with nothing anywhere saying so.

So this asserts the opposite of the usual thing. It does not check that the
ports agree on a schema; it checks that a capability one of them claims is
spelled the way the server reads it.
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


class CapsWireConformance(unittest.TestCase):
    def test_the_server_reads_the_snake_case_name(self) -> None:
        source = read(SERVER)
        self.assertIn(
            f"pub {FIELD}: Vec<String>",
            source,
            f"the server's field is what defines the wire name {FIELD!r}",
        )
        self.assertIn(
            "#[serde(default)]\n    pub subtitle_overlays",
            source,
            "an absent claim must default rather than refuse: absent is never a claim",
        )

    def test_apple_spells_it_so_the_key_strategy_produces_the_wire_name(self) -> None:
        source = read(APPLE)
        # Apple encodes with `.convertToSnakeCase` (PlurxAPI.swift), so the
        # Swift property must be the camelCase of the wire name and nothing
        # else. `subtitleOverlays` -> `subtitle_overlays`.
        camel = re.sub(r"_([a-z])", lambda m: m.group(1).upper(), FIELD)
        self.assertRegex(
            source,
            rf"var\s+{camel}\s*:\s*\[String\]",
            f"Apple must declare {camel!r} so convertToSnakeCase emits {FIELD!r}",
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
        # kotlinx omits a property equal to its default. A constant claim with
        # a default would therefore never reach the wire.
        index = source.index(f"val {FIELD}")
        preceding = source[max(0, index - 400):index]
        self.assertIn(
            "@EncodeDefault(EncodeDefault.Mode.ALWAYS)",
            preceding,
            "a constant claim needs EncodeDefault.ALWAYS or kotlinx drops it",
        )

    def test_every_port_that_claims_the_protocol_names_the_same_string(self) -> None:
        # The server and Apple carry the literal. Android names it once in a
        # constant and references that — which is why this follows the
        # constant to its definition rather than grepping for the string: a
        # port that de-duplicates a magic string is doing the right thing and
        # must not be failed for it.
        self.assertIn(PROTOCOL, read(SERVER), "the server names the protocol it serves")
        self.assertIn(
            f'protocolName = "{PROTOCOL}"',
            read(ANDROID.parent.parent / "player" / "PGSOverlay.kt").replace(
                f'PGS_OVERLAY_PROTOCOL = "{PROTOCOL}"', f'protocolName = "{PROTOCOL}"'
            ),
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
        source = read(WEB)
        builder = source[source.index("transports:[\"progressive\",\"hls\"]"):][:1200]
        self.assertNotIn(
            f"{FIELD}:",
            builder,
            "the web player has no PGS renderer; claiming one would earn it "
            "a delivery it cannot paint",
        )
        self.assertIn(
            FIELD,
            builder,
            "the absence must be deliberate and commented, not an oversight — "
            "name the field in a comment saying why it is not claimed",
        )


if __name__ == "__main__":
    unittest.main()
