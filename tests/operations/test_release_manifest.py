"""The packaged plurxd.exe must embed exactly crates/plurxd/plurxd.manifest.

On 2026-10-10 the first Windows run of a packaged plurxd.exe failed to start
("its side-by-side configuration is incorrect"): lld-link had merged its own
default UAC block into the source manifest and written trustInfo as prefixed
asm.v1 elements with namespaced attributes. These tests hold the two halves of
the fix: the build asks the linker for no UAC block of its own, and packaging
refuses any executable whose embedded manifest is not the source manifest.
"""

from pathlib import Path
import struct
import tempfile
import unittest

from validation.release_manifest import embedded_manifest, verify_embedded_manifest


ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "crates/plurxd/plurxd.manifest"

# The manifest the broken 2026-10-10 build actually embedded, byte for byte.
LINKER_MERGED = b"""<?xml version="1.0" encoding="UTF-8"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <ms_asmv1:trustInfo xmlns="urn:schemas-microsoft-com:asm.v3" xmlns:ms_asmv1="urn:schemas-microsoft-com:asm.v1">
    <ms_asmv1:security>
      <ms_asmv1:requestedPrivileges>
        <ms_asmv1:requestedExecutionLevel ms_asmv1:level="asInvoker" ms_asmv1:uiAccess="false"/>
      </ms_asmv1:requestedPrivileges>
    </ms_asmv1:security>
  </ms_asmv1:trustInfo>
  <assemblyIdentity name="plurx.plurxd" processorArchitecture="amd64" type="win32" version="1.0.0.0"/>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>
"""


def resource_section(manifests: list[bytes], base: int) -> bytes:
    """A .rsrc section at RVA `base` holding these RT_MANIFEST resources."""
    names = len(manifests)
    root, kinds, leaves = 0, 0x18, 0x30
    data_entries = leaves + 0x18 * names
    payload = data_entries + 16 * names
    section = bytearray(payload + sum(len(m) for m in manifests))
    struct.pack_into("<HH", section, root + 12, 0, 1)
    struct.pack_into("<II", section, root + 16, 24, 0x80000000 | kinds)
    struct.pack_into("<HH", section, kinds + 12, 0, names)
    position = payload
    for index, manifest in enumerate(manifests):
        leaf = leaves + 0x18 * index
        struct.pack_into("<II", section, kinds + 16 + 8 * index, index + 1, 0x80000000 | leaf)
        struct.pack_into("<HH", section, leaf + 12, 0, 1)
        struct.pack_into("<II", section, leaf + 16, 0x409, data_entries + 16 * index)
        struct.pack_into("<II", section, data_entries + 16 * index, base + position, len(manifest))
        section[position:position + len(manifest)] = manifest
        position += len(manifest)
    return bytes(section)


def executable(manifests: list[bytes]) -> bytes:
    """A minimal PE32+ whose one section is that .rsrc."""
    base, raw = 0x1000, 0x200
    section = resource_section(manifests, base)
    image = bytearray(raw + len(section))
    image[:2] = b"MZ"
    struct.pack_into("<I", image, 60, 64)
    image[64:68] = b"PE\0\0"
    struct.pack_into("<HH", image, 68, 0x8664, 1)
    struct.pack_into("<H", image, 84, 240)
    struct.pack_into("<H", image, 88, 0x20B)
    struct.pack_into("<I", image, 88 + 108, 16)  # NumberOfRvaAndSizes
    struct.pack_into("<II", image, 88 + 112 + 16, base, len(section))
    struct.pack_into("<4I", image, 328 + 8, len(section), base, len(section), raw)
    image[raw:] = section
    return bytes(image)


class EmbeddedManifestCase(unittest.TestCase):
    def check(self, manifests: list[bytes]) -> None:
        with tempfile.TemporaryDirectory() as raw:
            path = Path(raw) / "plurxd.exe"
            path.write_bytes(executable(manifests))
            verify_embedded_manifest(path, SOURCE)

    def test_the_source_manifest_embedded_as_written_is_accepted(self):
        self.check([SOURCE.read_bytes()])
        with tempfile.TemporaryDirectory() as raw:
            path = Path(raw) / "plurxd.exe"
            path.write_bytes(executable([SOURCE.read_bytes()]))
            self.assertEqual(embedded_manifest(path), SOURCE.read_bytes())

    def test_a_linker_merged_trust_block_is_refused(self):
        with self.assertRaisesRegex(ValueError, "not the source manifest"):
            self.check([LINKER_MERGED])

    def test_whitespace_and_attribute_order_are_not_differences_but_element_order_is(self):
        compact = (
            b'<assembly manifestVersion="1.0" xmlns="urn:schemas-microsoft-com:asm.v1">'
            b'<assemblyIdentity version="1.0.0.0" type="win32" processorArchitecture="amd64" name="plurx.plurxd"/>'
            b'<trustInfo xmlns="urn:schemas-microsoft-com:asm.v3"><security><requestedPrivileges>'
            b'<requestedExecutionLevel uiAccess="false" level="asInvoker"/>'
            b'</requestedPrivileges></security></trustInfo>'
            b'<application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings>'
            b'<longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>'
            b'</windowsSettings></application>'
            b'</assembly>'
        )
        self.check([compact])
        reordered = (
            b'<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">'
            b'<application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings>'
            b'<longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>'
            b'</windowsSettings></application>'
            b'<trustInfo xmlns="urn:schemas-microsoft-com:asm.v3"><security><requestedPrivileges>'
            b'<requestedExecutionLevel uiAccess="false" level="asInvoker"/>'
            b'</requestedPrivileges></security></trustInfo>'
            b'<assemblyIdentity version="1.0.0.0" type="win32" processorArchitecture="amd64" name="plurx.plurxd"/>'
            b'</assembly>'
        )
        with self.assertRaisesRegex(ValueError, "not the source manifest"):
            self.check([reordered])

    def test_an_executable_needs_exactly_one_manifest_at_resource_id_1(self):
        with self.assertRaisesRegex(ValueError, "expected one embedded manifest"):
            self.check([])
        # A second manifest sits at ID 2, which the loader never reads.
        with self.assertRaisesRegex(ValueError, "not the one Windows reads"):
            self.check([SOURCE.read_bytes(), SOURCE.read_bytes()])

    def test_a_manifest_that_is_not_xml_is_refused_as_a_value_error(self):
        with self.assertRaisesRegex(ValueError, "not well-formed"):
            self.check([b"<assembly"])

    def test_the_windows_build_asks_the_linker_for_no_uac_block_of_its_own(self):
        build = (ROOT / "crates/plurxd/build.rs").read_text(encoding="utf-8")
        self.assertIn('"cargo:rustc-link-arg-bin=plurxd=/MANIFEST:EMBED"', build)
        self.assertIn('"cargo:rustc-link-arg-bin=plurxd=/MANIFESTUAC:NO"', build)
        self.assertIn("/MANIFESTINPUT:{}", build)
        self.assertIn("requestedExecutionLevel", SOURCE.read_text(encoding="utf-8"))
        packaging = (ROOT / "scripts/package-windows").read_text(encoding="utf-8")
        self.assertIn("python3 -m validation.release_manifest", packaging)
        self.assertIn('"$source_root/crates/plurxd/plurxd.manifest"', packaging)


if __name__ == "__main__":
    unittest.main()
