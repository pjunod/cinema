"""Prove a Windows executable embeds exactly its source application manifest.

The linker writes plurxd.exe's RT_MANIFEST resource, and given /MANIFESTINPUT
it merges crates/plurxd/plurxd.manifest with whatever it generates itself.
lld-link (the cargo-xwin lane) merged its own default UAC block into ours and
rewrote `trustInfo` as prefixed asm.v1 elements with namespaced attributes
(`ms_asmv1:level`). Windows' side-by-side loader rejects that manifest, so the
packaged plurxd.exe could not start at all ("the application has failed to
start because its side-by-side configuration is incorrect"); the GitHub
release smoke found it on 2026-10-10. The build now tells the linker to add no
UAC block of its own (/MANIFESTUAC:NO), so the source file is the whole
manifest. This check is what keeps it so: packaging refuses an executable
whose embedded manifest is not the source manifest, element for element,
whatever a linker does next.
"""

from pathlib import Path
import argparse
import struct
import xml.etree.ElementTree as ElementTree


RT_MANIFEST = 24
# The loader reads an executable's manifest from resource ID 1 only.
CREATEPROCESS_MANIFEST_RESOURCE_ID = 1
MAX_MANIFEST = 64 * 1024
MAX_ENTRIES = 64


def embedded_manifest(executable: Path) -> bytes:
    """The one RT_MANIFEST resource of a Windows x86_64 PE32+ executable."""
    if executable.is_symlink() or not executable.is_file():
        raise ValueError("executable must be a regular file")
    with executable.open("rb") as source:
        def read(offset: int, size: int) -> bytes:
            source.seek(offset)
            data = source.read(size)
            if len(data) != size:
                raise ValueError("truncated executable")
            return data

        dos = read(0, 64)
        if dos[:2] != b"MZ":
            raise ValueError("missing executable DOS header")
        pe = struct.unpack_from("<I", dos, 60)[0]
        header = read(pe, 24)
        if header[:4] != b"PE\0\0" or struct.unpack_from("<H", header, 4)[0] != 0x8664:
            raise ValueError("expected a Windows x86_64 PE executable")
        count, optional_size = struct.unpack_from("<H", header, 6)[0], struct.unpack_from("<H", header, 20)[0]
        optional = read(pe + 24, optional_size)
        if (len(optional) < 136 or struct.unpack_from("<H", optional)[0] != 0x20B
                or struct.unpack_from("<I", optional, 108)[0] < 3):
            raise ValueError("expected a PE32+ executable with a resource directory entry")
        root, length = struct.unpack_from("<II", optional, 112 + 2 * 8)
        if not root or not length:
            raise ValueError("executable has no resources")
        table = read(pe + 24 + optional_size, count * 40)
        sections = []
        for position in range(0, len(table), 40):
            virtual_size, address, raw_size, raw = struct.unpack_from("<4I", table, position + 8)
            sections.append((address, min(virtual_size, raw_size), raw))

        def at(rva: int, size: int) -> bytes:
            for address, backed, raw in sections:
                if address <= rva and rva + size <= address + backed:
                    return read(raw + rva - address, size)
            raise ValueError("resource data is not file-backed")

        def directory(rva: int) -> list[tuple[int, int]]:
            named, ids = struct.unpack_from("<HH", at(rva, 16), 12)
            if named + ids > MAX_ENTRIES:
                raise ValueError("resource directory is implausibly large")
            entries = at(rva + 16, (named + ids) * 8)
            return [struct.unpack_from("<II", entries, index * 8) for index in range(named + ids)]

        def subdirectory(offset: int) -> int:
            if not offset & 0x80000000:
                raise ValueError("resource tree is malformed")
            return root + (offset & 0x7FFFFFFF)

        manifests = []
        for kind, offset in directory(root):
            if kind != RT_MANIFEST:
                continue
            for name, named in directory(subdirectory(offset)):
                if name != CREATEPROCESS_MANIFEST_RESOURCE_ID:
                    raise ValueError(f"manifest resource {name} is not the one Windows reads (1)")
                for _language, leaf in directory(subdirectory(named)):
                    if leaf & 0x80000000:
                        raise ValueError("resource tree is malformed")
                    data, size = struct.unpack_from("<II", at(root + leaf, 16))
                    if size > MAX_MANIFEST:
                        raise ValueError("embedded manifest is implausibly large")
                    manifests.append(at(data, size))
    if len(manifests) != 1:
        raise ValueError(f"expected one embedded manifest, found {len(manifests)}")
    return manifests[0]


def _canonical(element: ElementTree.Element) -> tuple:
    # Namespaces resolve to their URIs, so a prefix rewrite that moves an
    # element or attribute into another namespace is a difference, and so is
    # element order (assemblyIdentity must lead): the source file is the
    # authority, written the way Windows reads it. Only whitespace and the
    # order of attributes are not.
    return (
        element.tag,
        tuple(sorted(element.attrib.items())),
        (element.text or "").strip(),
        tuple(_canonical(child) for child in element),
    )


def _parse(data: bytes, what: str) -> ElementTree.Element:
    try:
        return ElementTree.fromstring(data)
    except ElementTree.ParseError as error:
        raise ValueError(f"{what} is not well-formed XML: {error}") from error


def verify_embedded_manifest(executable: Path, source: Path) -> None:
    embedded = _parse(embedded_manifest(executable), "the embedded manifest")
    expected = _parse(source.read_bytes(), "the source manifest")
    if _canonical(embedded) != _canonical(expected):
        raise ValueError(
            "the executable's embedded manifest is not the source manifest; "
            "Windows rejects a linker-merged one:\n"
            + ElementTree.tostring(embedded, encoding="unicode")
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=Path)
    parser.add_argument("manifest", type=Path)
    args = parser.parse_args()
    verify_embedded_manifest(args.executable, args.manifest)
    print(f"{args.executable.name} embeds {args.manifest.name} exactly")


if __name__ == "__main__":
    main()
