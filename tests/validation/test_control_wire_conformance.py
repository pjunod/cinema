"""The playback-control request has four implementations. This pins them together.

The server defines `ControlRequestV1` and its nested structs with
`deny_unknown_fields`, so a client that misspells one field has every exchange
refused — and the refusal is a 400 the reporter reads as terminal, which means
that client silently stops reporting for the rest of a film. There is no
partial failure mode to notice in testing.

Four ports now speak this wire: the Rust server, the web client, Apple, and
Android. A per-platform unit test proves each one is self-consistent; nothing
proved they agreed with each other. This does.

It is deliberately a name-level contract rather than a schema. What drifts in
practice is a field name — a `snake_case` key spelled `camelCase` by a key
strategy, a `SerialName` forgotten on a new property, a JS object literal
using the Swift spelling. Types and bounds are each platform's own tests.
"""

from __future__ import annotations

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]

RUST = ROOT / "crates/plurxd/src/playback_control.rs"
WEB = ROOT / "crates/plurxd/src/web/playback-control.js"
APPLE = ROOT / "clients/apple/Sources/PlaybackControlReporter.swift"
ANDROID = ROOT / (
    "clients/android/app/src/main/java/tv/plurx/app/player/PlaybackControlReporter.kt"
)


def rust_struct_fields(source: str, name: str) -> set[str]:
    """Serde field names of one struct, honouring an explicit `rename`."""
    match = re.search(
        r"pub\(crate\) struct " + re.escape(name) + r"\s*\{(.*?)\n\}",
        source,
        re.DOTALL,
    )
    assert match, f"{name} is no longer a struct in {RUST.name}"
    fields = set()
    for line in match.group(1).splitlines():
        line = line.strip()
        if line.startswith("//") or line.startswith("#["):
            continue
        declaration = re.match(r"pub ([A-Za-z_][A-Za-z0-9_]*)\s*:", line)
        if declaration:
            fields.add(declaration.group(1))
    return fields


def swift_coding_keys(source: str, name: str) -> set[str]:
    """Wire names of one Swift type.

    The control types spell their keys in camelCase and let
    `.convertToSnakeCase` produce the wire form, so a bare case name maps to
    its snake_case spelling; an explicit raw value overrides that. This
    reproduces that rule rather than trusting either spelling.
    """
    match = re.search(
        r"struct " + re.escape(name) + r"\b.*?\n\}", source, re.DOTALL
    )
    assert match, f"{name} is no longer a struct in {APPLE.name}"
    body = match.group(0)
    keys_block = re.search(r"enum CodingKeys[^{]*\{(.*?)\n    \}", body, re.DOTALL)
    if keys_block:
        names: set[str] = set()
        for line in keys_block.group(1).splitlines():
            line = line.split("//")[0].strip()
            if not line.startswith("case "):
                continue
            for entry in line[len("case "):].split(","):
                entry = entry.strip()
                if not entry:
                    continue
                explicit = re.match(r"([A-Za-z0-9_]+)\s*=\s*\"([^\"]+)\"", entry)
                if explicit:
                    names.add(explicit.group(2))
                else:
                    names.add(snake(entry))
        return names
    # Stored properties only. A computed one (`var isValid: Bool { ... }`) is
    # not on the wire, and counting it would make this test fail for a reason
    # that has nothing to do with the contract.
    return {
        snake(match.group(1))
        for match in re.finditer(
            r"^\s*var ([A-Za-z0-9_]+)\s*:[^\n{]*$", body, re.MULTILINE
        )
    }


def kotlin_fields(source: str, name: str) -> set[str]:
    """Wire names of one Kotlin data class, honouring `@SerialName`."""
    match = re.search(
        r"data class " + re.escape(name) + r"\s*\((.*?)\n\)", source, re.DOTALL
    )
    assert match, f"{name} is no longer a data class in {ANDROID.name}"
    fields = set()
    for line in match.group(1).splitlines():
        line = line.split("//")[0].strip()
        if not line:
            continue
        serial = re.search(r'@SerialName\("([^"]+)"\)', line)
        declaration = re.search(r"\bval ([A-Za-z0-9_]+)\s*:", line)
        if serial:
            fields.add(serial.group(1))
        elif declaration:
            fields.add(declaration.group(1))
    return fields



def rust_enum_values(source: str, name: str) -> set[str]:
    """Variant names of a `rename_all = "snake_case"` enum, as sent."""
    match = re.search(
        r"pub\(crate\) enum " + re.escape(name) + r"\s*\{(.*?)\n\}", source, re.DOTALL
    )
    assert match, f"{name} is no longer an enum in {RUST.name}"
    return {
        snake(variant)
        for variant in re.findall(r"^\s*([A-Z][A-Za-z0-9]*)\s*,", match.group(1), re.MULTILINE)
    }


def swift_enum_values(source: str, name: str) -> set[str]:
    """Raw values of a Swift string enum, explicit or implied by the case name."""
    match = re.search(
        r"enum " + re.escape(name) + r"\s*:\s*String[^{]*\{(.*?)\n\}", source, re.DOTALL
    )
    assert match, f"{name} is no longer a String enum in {APPLE.name}"
    values = set()
    for line in match.group(1).splitlines():
        line = line.split("//")[0].strip()
        if not line.startswith("case "):
            continue
        for entry in line[len("case "):].split(","):
            entry = entry.strip()
            if not entry:
                continue
            explicit = re.match(r"[A-Za-z0-9_]+\s*=\s*\"([^\"]+)\"", entry)
            values.add(explicit.group(1) if explicit else entry)
    return values


def kotlin_enum_values(source: str, name: str) -> set[str]:
    """`@SerialName` values of a Kotlin enum — the only thing that reaches the wire."""
    match = re.search(
        r"enum class " + re.escape(name) + r"\s*\{(.*?)\n\}", source, re.DOTALL
    )
    assert match, f"{name} is no longer an enum in {ANDROID.name}"
    return set(re.findall(r'@SerialName\("([^"]+)"\)', match.group(1)))


def snake(name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


@unittest.skipUnless(
    APPLE.is_file() and ANDROID.is_file(),
    "the Apple and Android ports are not both present yet; this activates itself "
    "when they land, which is exactly when it starts being able to catch drift",
)
class ControlRequestWireCase(unittest.TestCase):
    """Every port spells the request the same way."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.rust = RUST.read_text(encoding="utf-8")
        cls.web = WEB.read_text(encoding="utf-8")
        cls.apple = APPLE.read_text(encoding="utf-8")
        cls.android = ANDROID.read_text(encoding="utf-8")

    def assertSameWire(self, label: str, rust: set[str], swift: set[str], kotlin: set[str]) -> None:
        self.assertEqual(
            swift,
            rust,
            f"{label}: Apple and the server disagree "
            f"(apple-only {sorted(swift - rust)}, server-only {sorted(rust - swift)})",
        )
        self.assertEqual(
            kotlin,
            rust,
            f"{label}: Android and the server disagree "
            f"(android-only {sorted(kotlin - rust)}, server-only {sorted(rust - kotlin)})",
        )

    def test_the_request_itself(self) -> None:
        rust = rust_struct_fields(self.rust, "ControlRequestV1")
        # The server accepts an acknowledgement M2 clients never send: they
        # consume no action, so they have nothing to acknowledge.
        rust.discard("acknowledgement")
        # `supported_actions` is the client's action vocabulary, and the server
        # deploys before the clients do. An absent list means passive, which is
        # exactly what an M2 client is, so a server that has the field and
        # clients that do not is the correct intermediate state rather than a
        # drift. This discard comes out in the client PR that adds it.
        rust.discard("supported_actions")
        self.assertSameWire(
            "ControlRequestV1",
            rust,
            swift_coding_keys(self.apple, "ControlRequest"),
            kotlin_fields(self.android, "ControlRequest"),
        )

    def test_the_selection(self) -> None:
        self.assertSameWire(
            "ClientSelection",
            rust_struct_fields(self.rust, "ClientSelection"),
            swift_coding_keys(self.apple, "ClientSelection"),
            kotlin_fields(self.android, "ClientSelection"),
        )

    def test_the_capabilities(self) -> None:
        self.assertSameWire(
            "DynamicCapabilities",
            rust_struct_fields(self.rust, "DynamicCapabilities"),
            swift_coding_keys(self.apple, "DynamicCapabilities"),
            kotlin_fields(self.android, "DynamicCapabilities"),
        )

    def test_the_observation(self) -> None:
        self.assertSameWire(
            "ClientObservation",
            rust_struct_fields(self.rust, "ClientObservation"),
            swift_coding_keys(self.apple, "ClientObservation"),
            kotlin_fields(self.android, "ClientObservation"),
        )

    def test_the_subtitle_selection(self) -> None:
        self.assertSameWire(
            "SubtitleSelection",
            rust_struct_fields(self.rust, "SubtitleSelection"),
            swift_coding_keys(self.apple, "SubtitleSelection"),
            kotlin_fields(self.android, "SubtitleSelection"),
        )

    def test_the_bootstrap(self) -> None:
        self.assertSameWire(
            "ControlBootstrap",
            rust_struct_fields(self.rust, "ControlBootstrap"),
            swift_coding_keys(self.apple, "ControlBootstrap"),
            kotlin_fields(self.android, "ControlBootstrap"),
        )

    def test_every_port_agrees_on_the_protocol_string(self) -> None:
        server = re.search(r'PROTOCOL_V1[^=]*=\s*"([^"]+)"', self.rust)
        self.assertIsNotNone(server, "the server no longer names its protocol version")
        expected = server.group(1)
        for label, source, pattern in (
            ("web", self.web, r'PROTOCOL\s*=\s*"([^"]+)"'),
            ("apple", self.apple, r'protocolName\s*=\s*"([^"]+)"'),
            ("android", self.android, r'PROTOCOL\s*=\s*"([^"]+)"'),
        ):
            found = re.search(pattern, source)
            self.assertIsNotNone(found, f"{label} no longer names its protocol version")
            self.assertEqual(found.group(1), expected, f"{label} speaks a different version")

    def test_every_port_agrees_on_the_enum_vocabularies(self) -> None:
        """A value the server does not know is refused exactly like a bad name."""
        for label, rust_name, swift_name, kotlin_name in (
            ("demand", "PlaybackDemand", "PlaybackDemand", "PlaybackDemand"),
            ("render state", "RenderState", "RenderState", "RenderState"),
            ("codec", "CodecPolicy", "CodecPolicy", "CodecPolicy"),
            ("dynamic range", "DynamicRangePolicy", "DynamicRangePolicy", "DynamicRangePolicy"),
            ("subtitle mode", "SubtitleMode", "SubtitleMode", "SubtitleMode"),
            ("decoder state", "DecoderState", "DecoderState", "DecoderState"),
            ("error code", "ClientErrorCode", "ClientErrorCode", "ClientErrorCode"),
        ):
            server = rust_enum_values(self.rust, rust_name)
            self.assertTrue(server, f"{label}: no variants found for {rust_name}")
            self.assertEqual(
                swift_enum_values(self.apple, swift_name),
                server,
                f"{label}: Apple speaks a different vocabulary",
            )
            self.assertEqual(
                kotlin_enum_values(self.android, kotlin_name),
                server,
                f"{label}: Android speaks a different vocabulary",
            )


if __name__ == "__main__":
    unittest.main()
