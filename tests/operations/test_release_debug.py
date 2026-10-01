"""Real-format, bounded packed-debug pairing and release-retention contracts."""

from pathlib import Path
import struct
import tempfile
import tomllib
import unittest

from validation.release_artifact import create, verify
from validation.release_debug import Elf, MAX_METADATA, verify_packed_pair
from validation.release_dockerfile import required_debug_binaries, render, render_binary_export


ROOT = Path(__file__).resolve().parents[2]


def elf(path: Path, sections: dict[str, bytes], machine: int = 62) -> None:
    names = b"\0"
    offsets = {}
    for name in (*sections, ".shstrtab"):
        offsets[name] = len(names); names += name.encode() + b"\0"
    sections = {**sections, ".shstrtab": names}
    header = bytearray(64); header[:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<HHI", header, 16, 3, machine, 1)
    body = bytearray(header); rows = [bytes(64)]
    for name, data in sections.items():
        rows.append(struct.pack("<IIQQQQIIQQ", offsets[name], 3 if name == ".shstrtab" else 1,
                                0, 0, len(body), len(data), 0, 0, 1, 0))
        body.extend(data)
    struct.pack_into("<Q", body, 40, len(body))
    struct.pack_into("<HHH", body, 58, 64, len(rows), len(rows) - 1)
    body.extend(b"".join(rows)); path.write_bytes(body)


def packed_pair(directory: Path, name: str = "plurxd", identity: int = 0x12345678) -> None:
    # DWARF4 root: DW_AT_GNU_dwo_id (ULEB2131) with DW_FORM_data8.
    abbrev = bytes((1, 0x11, 0, 0xB1, 0x42, 7, 0, 0, 0))
    unit = struct.pack("<HIBBQ", 4, 0, 8, 1, identity)
    info = struct.pack("<I", len(unit)) + unit
    elf(directory / name, {".debug_info": info, ".debug_abbrev": abbrev,
                           ".debug_line": b"line", ".symtab": bytes(24)})
    index = struct.pack("<4IQI2I4I", 2, 2, 1, 1, identity, 1, 1, 3, 0, 0, len(info), len(abbrev))
    elf(directory / f"{name}.dwp", {".debug_info.dwo": info, ".debug_abbrev.dwo": abbrev,
                                   ".debug_cu_index": index})


class PackedDebugCase(unittest.TestCase):
    def test_selected_profile_c_preserves_unwind_thin_and_default_codegen_policy(self):
        profile = tomllib.loads((ROOT / "Cargo.toml").read_text())["profile"]["release"]
        self.assertEqual((profile["debug"], profile["strip"], profile["split-debuginfo"]),
                         ("line-tables-only", "none", "packed"))
        self.assertEqual(profile["lto"], "thin")
        self.assertEqual(profile.get("codegen-units", 16), 16)
        self.assertEqual(profile.get("panic", "unwind"), "unwind")
        self.assertFalse(profile.get("overflow-checks", False))

    def test_packed_debug_covers_the_executables_actual_dwo_identity(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); packed_pair(root)
            self.assertEqual(verify_packed_pair(root / "plurxd", root / "plurxd.dwp"), 1)
            packed_pair(root, "other", 0xDEADBEEF)
            with self.assertRaisesRegex(ValueError, "does not cover"):
                verify_packed_pair(root / "plurxd", root / "other.dwp")

    def test_missing_symlink_and_torn_debug_artifacts_are_rejected(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); packed_pair(root)
            (root / "alias.dwp").symlink_to(root / "plurxd.dwp")
            for name in ("absent.dwp", "alias.dwp"):
                with self.subTest(name=name), self.assertRaises(ValueError):
                    verify_packed_pair(root / "plurxd", root / name)
            (root / "plurxd.dwp").write_bytes((root / "plurxd.dwp").read_bytes()[:-1])
            with self.assertRaises(ValueError):
                verify_packed_pair(root / "plurxd", root / "plurxd.dwp")

    def test_hostile_debug_section_sizes_do_not_allocate_unbounded_memory(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); packed_pair(root)
            path = root / "plurxd.dwp"; data = bytearray(path.read_bytes())
            table = struct.unpack_from("<Q", data, 40)[0]
            struct.pack_into("<Q", data, table + 64 + 32, MAX_METADATA + 1)
            path.write_bytes(data)
            with self.assertRaises(ValueError):
                verify_packed_pair(root / "plurxd", path)

    def test_line_symbol_and_contribution_requirements_cannot_be_silently_dropped(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            for section in (".debug_line", ".symtab"):
                packed_pair(root)
                path = root / "plurxd"; data = bytearray(path.read_bytes())
                start, length = Elf(path).sections[section]
                table = struct.unpack_from("<Q", data, 40)[0]
                count = struct.unpack_from("<H", data, 60)[0]
                for row in range(1, count):
                    position = table + row * 64
                    if struct.unpack_from("<QQ", data, position + 24) == (start, length):
                        struct.pack_into("<Q", data, position + 32, 0); break
                path.write_bytes(data)
                with self.assertRaisesRegex(ValueError, "missing"):
                    verify_packed_pair(path, root / "plurxd.dwp")

    def test_manifest_retains_and_verifies_the_paired_debug_and_pinned_compiler(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); packed_pair(root)
            common = dict(git_tree="1" * 40, git_commit="2" * 40, build_ref="fixture",
                          target="x86_64-unknown-linux-gnu", binary_names=("plurxd",),
                          packed_debug=True, rustc_release="1.97.1")
            with self.assertRaisesRegex(ValueError, "compiler"):
                create(root, rustc="rustc 1.98.0 (wrong)", **common)
            manifest = create(root, rustc="rustc 1.97.1 (fixture)", **common)
            self.assertEqual(verify(root, **common), manifest)
            self.assertEqual(manifest["schema"], 2)
            self.assertEqual(manifest["debug_artifacts"]["plurxd.dwp"]["dwo_units"], 1)
            path = root / "plurxd.dwp"
            path.write_bytes(path.read_bytes() + b"tampered")
            with self.assertRaisesRegex(ValueError, "digest"):
                verify(root, **common)

    def test_export_and_runtime_keep_debug_beside_every_binary(self):
        source = (ROOT / "Dockerfile").read_text()
        names = ("plurxd", "plurx-cluster-check")
        self.assertEqual(required_debug_binaries(source), names)
        exported, runtime = render_binary_export(source), render(source)
        for name in names:
            self.assertIn(f"COPY --from=build /{name}.dwp /{name}.dwp", exported)
            self.assertIn(f"COPY --chmod=0644 release-bin/{name}.dwp /usr/local/bin/{name}.dwp", runtime)
        with self.assertRaisesRegex(ValueError, "every binary"):
            required_debug_binaries(source.replace("COPY --from=build /plurxd.dwp /usr/local/bin/plurxd.dwp", ""))

    def test_historical_runtime_does_not_invent_a_debug_requirement(self):
        source = "FROM rust:1-bookworm AS build\nWORKDIR /src\nRUN true\nFROM debian:bookworm-slim\nCOPY --from=build /plurxd /usr/local/bin/plurxd\n"
        self.assertEqual(required_debug_binaries(source), ())
        self.assertNotIn(".dwp", render_binary_export(source))


if __name__ == "__main__":
    unittest.main()
