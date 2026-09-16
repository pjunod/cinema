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
        # It accepts an intent envelope no client sends yet. The field is
        # optional precisely so every deployed client keeps working without it,
        # and teaching the two ports to build one is §4's client-adapter work,
        # not something to fake here by declaring a field neither of them can
        # populate.
        #
        # This discard is therefore temporary in a way the one above is not,
        # and it is the only thing standing between the ports and a real
        # divergence — so it comes out in the same change that adds `intent` to
        # the Swift and Kotlin requests, and the test starts comparing it
        # again.
        rust.discard("intent")
        # `acknowledgement` was discarded per-port while only Apple consumed
        # `prepare` and Android had nothing to acknowledge. Android consumes it
        # now, so the last discard comes out here — as the comment that stood
        # in this place said it would — and both ports are compared against the
        # same server struct with nothing set aside but `intent`.
        self.assertEqual(
            swift_coding_keys(self.apple, "ControlRequest"),
            rust,
            "ControlRequestV1: Apple and the server disagree "
            f"(apple-only {sorted(swift_coding_keys(self.apple, 'ControlRequest') - rust)}, "
            f"server-only {sorted(rust - swift_coding_keys(self.apple, 'ControlRequest'))})",
        )
        self.assertEqual(
            kotlin_fields(self.android, "ControlRequest"),
            rust,
            "ControlRequestV1: Android and the server disagree "
            f"(android-only {sorted(kotlin_fields(self.android, 'ControlRequest') - rust)}, "
            f"server-only {sorted(rust - kotlin_fields(self.android, 'ControlRequest'))})",
        )

    def test_the_acknowledgement(self) -> None:
        """The one request field a port gains only when it can act on one.

        This was a premise rather than a parity check while no client modelled
        `ActionAcknowledgement`: `test_the_request_itself` discarded the field,
        so the struct sat outside every comparison and the server could have
        added, renamed or retyped one silently. Apple models it now, so the
        premise is promoted to the check it was standing in for.

        It compares whichever ports have the type rather than waiting for all
        of them. A port that is late is not a reason to stop checking the one
        that is early, and the field that would break first is the one with no
        other test to catch it: the server requires
        `committed_media_origin_ms` *and* `first_frame_unix_ms` on a commit,
        compares the origin against the staged successor's own, and refuses a
        mismatch — so a port that spells either differently has every commit
        refused at validation with nothing to say why.
        """
        rust = rust_struct_fields(self.rust, "ActionAcknowledgement")
        self.assertIn("committed_media_origin_ms", rust)
        self.assertIn("first_frame_unix_ms", rust)
        modelled = 0
        if re.search(r"struct\s+ActionAcknowledgement\b", self.apple):
            modelled += 1
            swift = swift_coding_keys(self.apple, "ActionAcknowledgement")
            self.assertEqual(
                swift,
                rust,
                "ActionAcknowledgement: Apple and the server disagree "
                f"(apple-only {sorted(swift - rust)}, server-only {sorted(rust - swift)})",
            )
        if re.search(r"(data\s+)?class\s+ActionAcknowledgement\b", self.android):
            modelled += 1
            kotlin = kotlin_fields(self.android, "ActionAcknowledgement")
            self.assertEqual(
                kotlin,
                rust,
                "ActionAcknowledgement: Android and the server disagree "
                f"(android-only {sorted(kotlin - rust)}, server-only {sorted(rust - kotlin)})",
            )
        self.assertGreater(
            modelled,
            0,
            "no port models ActionAcknowledgement any more, so the request "
            "comparison's discard is unguarded again — restore the premise "
            "test rather than deleting this one",
        )

    def test_a_port_that_prepares_spells_the_declared_name_the_server_matches(self) -> None:
        """`prepare_replacement` is declared; `prepare` is what arrives.

        Every other action's declared name equals its wire tag. This one does
        not, and both ways of confusing them fail silently: a port that
        declares `prepare` is never offered anything, because `accepts()` is a
        literal comparison, and a port that switches on `prepare_replacement`
        never fires. So a port that declares the action at all must spell it
        the server's way, and must not declare the tag.
        """
        declared = re.search(
            r'PREPARE_REPLACEMENT_ACTION[^=]*=\s*"([^"]+)"', self.rust
        )
        self.assertIsNotNone(declared, "the server no longer names the prepared action")
        name = declared.group(1)
        self.assertEqual(name, "prepare_replacement")
        tag = re.search(
            r"fn vocabulary_name\(&self\).*?Self::Prepare \{ \.\. \} => Some\(([A-Z_]+)\)",
            self.rust,
            re.DOTALL,
        )
        self.assertIsNotNone(tag, "the server no longer maps Prepare to a declared name")
        self.assertEqual(tag.group(1), "PREPARE_REPLACEMENT_ACTION")
        for label, source, pattern in (
            ("web", self.web, r"SUPPORTED_ACTIONS\s*=\s*Object\.freeze\(\[([^\]]*)\]"),
            ("apple", self.apple, r"supportedActions\s*=\s*\[([^\]]*)\]"),
            ("android", self.android, r"SUPPORTED_ACTIONS\s*=\s*listOf\(([^)]*)\)"),
        ):
            found = re.search(pattern, source)
            self.assertIsNotNone(found, f"{label} no longer declares an action vocabulary")
            body = found.group(1)
            declared_names = set(re.findall(r'"([^"]+)"', body))
            self.assertNotIn(
                "prepare",
                declared_names,
                f"{label} declares the wire tag instead of the action name, which "
                "is never matched and fails completely silently",
            )
            if name in declared_names or "prepareReplacementAction" in body:
                # A port that prepares must consume the tag, not the name.
                self.assertRegex(
                    source,
                    r'"prepare"',
                    f"{label} declares {name} but never handles the `prepare` tag it "
                    "actually receives",
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

    def test_every_port_declares_the_action_it_accepts(self) -> None:
        """The server sends only actions the client named, so the names must match.

        A misspelling here has no loud failure mode. The server simply never
        sends that client the action, and the client goes quietly unmanaged for
        the life of every session — which is the drift this file exists for.
        """
        server = set(
            re.findall(
                r'(?:HOLD|TERMINAL|RETRY_RESOURCE)_ACTION[^=]*=\s*"([^"]+)"', self.rust
            )
        )
        self.assertEqual(
            server,
            {"hold", "terminal", "retry_resource"},
            "the server's action names changed; every client must move with them",
        )
        for label, source, pattern in (
            ("web", self.web, r"SUPPORTED_ACTIONS\s*=\s*Object\.freeze\(\[([^\]]*)\]"),
            ("apple", self.apple, r"supportedActions\s*=\s*\[([^\]]*)\]"),
            ("android", self.android, r"SUPPORTED_ACTIONS\s*=\s*listOf\(([^)]*)\)"),
        ):
            found = re.search(pattern, source)
            self.assertIsNotNone(found, f"{label} no longer declares an action vocabulary")
            declared = set(re.findall(r'"([^"]+)"', found.group(1)))
            missing = sorted(server - declared)
            self.assertFalse(
                missing,
                f"{label} does not declare {missing}, so the server would never send them "
                f"and that client would go quietly unmanaged",
            )

    def test_the_producer_decision_is_bounded_by_one_list(self) -> None:
        """The relay check and the enum must agree on every name.

        A variant added to `ProducerDecisionReason` but forgotten in `ALL`
        would be accepted on the wire under no name the relay could bound, and
        the permanence split that decides whether a client gives up would have
        a hole in it.
        """
        listed = re.search(
            r"pub\(crate\) const ALL: \[Self; (\d+)\] = \[(.*?)\];",
            self.rust,
            re.DOTALL,
        )
        self.assertIsNotNone(listed, "the decision vocabulary is no longer enumerated")
        count = int(listed.group(1))
        entries = re.findall(r"Self::([A-Za-z]+),", listed.group(2))
        self.assertEqual(len(entries), count)
        declared = re.search(
            r"pub\(crate\) enum ProducerDecisionReason \{(.*?)\n\}",
            self.rust,
            re.DOTALL,
        )
        self.assertIsNotNone(declared, "ProducerDecisionReason is no longer an enum")
        variants = re.findall(r"^\s+([A-Za-z]+),", declared.group(1), re.MULTILINE)
        self.assertEqual(
            sorted(entries),
            sorted(variants),
            "ProducerDecisionReason::ALL and the enum disagree",
        )

    def test_the_hold_reason_is_one_vocabulary(self) -> None:
        """The action's reason and `DeliveryView.hold_reason` are the same fact."""
        mapping = re.search(
            r"fn from_delivery\(reason: &str\) -> Option<Self> \{(.*?)\n    \}",
            self.rust,
            re.DOTALL,
        )
        self.assertIsNotNone(mapping, "the server no longer maps delivery hold reasons")
        reasons = set(re.findall(r'"([a-z_]+)" =>', mapping.group(1)))
        self.assertEqual(
            reasons,
            {"demand", "time", "bytes", "global", "ahead", "working_set", "no_room"},
        )
        bound = re.search(
            r"hold_reason\s*\.as_deref\(\)\s*\.is_none_or\(\|value\| \{(.*?)\}\)",
            self.rust,
            re.DOTALL,
        )
        self.assertIsNotNone(bound, "the delivery view no longer bounds its hold reason")
        self.assertEqual(
            set(re.findall(r'"([a-z_]+)"', bound.group(1))),
            reasons,
            "the action and the delivery view disagree about the hold vocabulary",
        )

    def test_the_preparation_state_is_one_vocabulary(self) -> None:
        """`delivery.preparation` means the same three things on every port.

        It is read as a decision, not as a label: `none` ends a client's wait
        and reopens the stream. A port that spelled one of these differently
        would not fail loudly — it would fall through to its own "unknown"
        branch and wait out the twelve-second bound on every quality change,
        which looks exactly like a slow server. So the names are pinned here
        rather than left to each port's own tests.

        Absence is deliberately not in the vocabulary. An older relay peer
        emits no field at all, and every port must read that as "not evaluated
        here", never as `none`.
        """
        bound = re.search(
            r"preparation\s*\.as_deref\(\)\s*\.is_none_or\(\|value\| "
            r"matches!\(value, (.*?)\)\)",
            self.rust,
            re.DOTALL,
        )
        self.assertIsNotNone(
            bound, "the delivery view no longer bounds its preparation state"
        )
        states = set(re.findall(r'"([a-z]+)"', bound.group(1)))
        self.assertEqual(states, {"staging", "offered", "none"})

        # Zero, one, two or three ports may declare it: the field is additive
        # and the clients adopt it on their own milestones. A port that does
        # declare it has to agree.
        for label, source in (
            ("web", self.web),
            ("apple", self.apple),
            ("android", self.android),
        ):
            if "preparation" not in source:
                continue
            declared = set(re.findall(r'"(staging|offered|none)"', source))
            self.assertTrue(
                declared <= states,
                f"{label} names a preparation state the server cannot emit: "
                f"{sorted(declared - states)}",
            )

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
