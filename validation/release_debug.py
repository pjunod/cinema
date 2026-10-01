"""Bounded ELF/DWARF checks for the GNU profile-C executable/DWP pair."""

from __future__ import annotations

from pathlib import Path
import struct


MAX_METADATA = 32 * 1024 * 1024
MAX_ROOT_METADATA = 64 * 1024
MAX_UNITS = 65_536
MAX_SLOTS = 262_144
INDEX_SECTIONS = {
    1: ".debug_info.dwo", 2: ".debug_types.dwo", 3: ".debug_abbrev.dwo",
    4: ".debug_line.dwo", 5: ".debug_loc.dwo", 6: ".debug_str_offsets.dwo",
    7: ".debug_macro.dwo", 8: ".debug_rnglists.dwo", 9: ".debug_loclists.dwo",
}


class Elf:
    def __init__(self, path: Path):
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"packed debug artifact must be a regular file: {path.name}")
        self.path = path
        self.size = path.stat().st_size
        header = self.read(0, 64)
        if header[:6] != b"\x7fELF\x02\x01":
            raise ValueError("packed debug requires little-endian ELF64")
        self.machine = struct.unpack_from("<H", header, 18)[0]
        offset = struct.unpack_from("<Q", header, 40)[0]
        entry_size, count, names_index = struct.unpack_from("<HHH", header, 58)
        if entry_size != 64 or not 0 < names_index < count:
            raise ValueError("invalid packed-debug ELF section table")
        table = self.read(offset, count * 64)
        sections = [struct.unpack_from("<IIQQQQIIQQ", table, i * 64) for i in range(count)]
        names = self.read(sections[names_index][4], sections[names_index][5])
        self.sections: dict[str, tuple[int, int]] = {}
        for section in sections[1:]:
            name_offset, kind, _flags, _address, start, length, *_ = section
            end = names.find(b"\0", name_offset)
            if end < 0 or end - name_offset > 512:
                raise ValueError("invalid packed-debug ELF section name")
            name = names[name_offset:end].decode("utf-8")
            if name in self.sections:
                raise ValueError("duplicate packed-debug ELF section")
            if kind != 8 and start + length > self.size:
                raise ValueError("packed-debug ELF section exceeds file")
            self.sections[name] = (start, length)

    def read(self, offset: int, length: int) -> bytes:
        if length > MAX_METADATA or offset + length > self.size:
            raise ValueError("packed-debug metadata exceeds bounded file range")
        with self.path.open("rb") as source:
            source.seek(offset)
            data = source.read(length)
        if len(data) != length:
            raise ValueError("truncated packed-debug metadata")
        return data

    def section(self, name: str) -> bytes:
        if name not in self.sections or not self.sections[name][1]:
            raise ValueError(f"packed debug is missing nonempty {name}")
        return self.read(*self.sections[name])


def _uleb(data: bytes, cursor: int) -> tuple[int, int]:
    result = 0
    for shift in range(0, 70, 7):
        if cursor >= len(data):
            break
        byte = data[cursor]; cursor += 1
        result |= (byte & 127) << shift
        if byte < 128:
            return result, cursor
    raise ValueError("invalid packed-debug LEB128")


def _root_attributes(abbrev: bytes, offset: int, wanted: int,
                     expected_tag: int | None = None) -> list[tuple[int, int]]:
    while offset < len(abbrev):
        code, offset = _uleb(abbrev, offset)
        if not code:
            break
        tag, offset = _uleb(abbrev, offset)
        if offset >= len(abbrev) or abbrev[offset] not in (0, 1):
            raise ValueError("invalid packed-debug abbreviation children flag")
        offset += 1  # has-children byte
        attributes = []
        while True:
            attribute, offset = _uleb(abbrev, offset)
            form, offset = _uleb(abbrev, offset)
            if not attribute and not form:
                break
            attributes.append((attribute, form))
            if len(attributes) > 256:
                raise ValueError("packed-debug root has too many attributes")
            if form == 0x21:  # DW_FORM_implicit_const: operand is in abbreviation.
                _, offset = _uleb(abbrev, offset)
        if code == wanted:
            if expected_tag is not None and tag != expected_tag:
                raise ValueError("DWP root is not a compilation unit")
            return attributes
    raise ValueError("packed-debug root abbreviation missing")


def _form(data: bytes, cursor: int, form: int, width: int, address: int) -> tuple[int, int]:
    sizes = {1: address, 5: 2, 6: 4, 7: 8, 0x0B: 1, 0x0C: 1, 0x0E: width,
             0x10: width, 0x11: 1, 0x12: 2, 0x13: 4, 0x14: 8, 0x17: width,
             0x1C: 4, 0x1D: width, 0x1E: 16, 0x1F: width, 0x20: 8, 0x24: 8,
             0x25: 1, 0x26: 2, 0x27: 3, 0x28: 4, 0x29: 1, 0x2A: 2, 0x2B: 3, 0x2C: 4}
    if form in sizes:
        end = cursor + sizes[form]
        if end > len(data):
            raise ValueError("truncated packed-debug attribute")
        return int.from_bytes(data[cursor:end], "little"), end
    if form in (0x0D, 0x0F, 0x15, 0x1A, 0x1B, 0x22, 0x23, 0x1F01, 0x1F02):
        return _uleb(data, cursor)
    if form in (0x19, 0x21):
        return 0, cursor
    if form == 8:
        end = data.find(b"\0", cursor)
        if end < 0:
            raise ValueError("unterminated packed-debug attribute")
        return 0, end + 1
    if form == 0x16:
        actual, cursor = _uleb(data, cursor)
        if actual == 0x16:
            raise ValueError("recursive packed-debug indirect form")
        return _form(data, cursor, actual, width, address)
    if form in (3, 4, 9, 0x0A, 0x18):
        if form in (9, 0x18):
            size, cursor = _uleb(data, cursor)
        else:
            size, cursor = _form(data, cursor, {3: 5, 4: 6, 0x0A: 0x0B}[form], width, address)
        if cursor + size > len(data):
            raise ValueError("packed-debug block exceeds unit")
        return 0, cursor + size
    raise ValueError(f"unsupported packed-debug DWARF form {form:#x}")


def _binary_ids(binary: Elf) -> set[int]:
    data = binary.section(".debug_info")
    abbrev = binary.section(".debug_abbrev")
    ids = set()
    cursor = 0
    units = 0
    while cursor < len(data):
        units += 1
        if units > MAX_UNITS:
            raise ValueError("packed-debug compilation-unit count exceeds bound")
        length, cursor = _form(data, cursor, 6, 4, 8)
        width = 4
        if length == 0xFFFFFFFF:
            length, cursor = _form(data, cursor, 7, 8, 8); width = 8
        end = cursor + length
        if not length or end > len(data):
            raise ValueError("invalid packed-debug compilation-unit length")
        unit = data[cursor:end]; position = 0
        version, position = _form(unit, position, 5, width, 8)
        if version == 5:
            kind, position = _form(unit, position, 0x0B, width, 8)
            address, position = _form(unit, position, 0x0B, width, 8)
            offset, position = _form(unit, position, 6 if width == 4 else 7, width, address)
            if kind in (4, 5):
                identity, position = _form(unit, position, 7, width, address)
                ids.add(identity); cursor = end; continue
        elif version == 4:
            offset, position = _form(unit, position, 6 if width == 4 else 7, width, 8)
            address, position = _form(unit, position, 0x0B, width, 8)
        else:
            raise ValueError("packed-debug requires DWARF4 or DWARF5")
        code, position = _uleb(unit, position)
        for attribute, form in _root_attributes(abbrev, offset, code):
            value, position = _form(unit, position, form, width, address)
            if attribute == 0x2131:  # DW_AT_GNU_dwo_id
                ids.add(value)
        cursor = end
    if not ids or 0 in ids:
        raise ValueError("executable has no nonzero split-debug identities")
    return ids


def _contribution_id(data: bytes, abbrev: bytes, contribution_size: int) -> int:
    """Inspect one bounded unit header/root, not its potentially large DIE tree."""
    length, position = _form(data, 0, 6, 4, 8)
    width = 4
    if length == 0xFFFFFFFF:
        length, position = _form(data, position, 7, 8, 8); width = 8
    if not length or position + length != contribution_size:
        raise ValueError("invalid DWP compilation-unit length")
    version, position = _form(data, position, 5, width, 8)
    identity = None
    if version == 5:
        kind, position = _form(data, position, 0x0B, width, 8)
        address, position = _form(data, position, 0x0B, width, 8)
        offset, position = _form(data, position, 6 if width == 4 else 7, width, address)
        if kind != 5:
            raise ValueError("DWP requires a split compilation unit")
        identity, position = _form(data, position, 7, width, address)
    elif version == 4:
        offset, position = _form(data, position, 6 if width == 4 else 7, width, 8)
        address, position = _form(data, position, 0x0B, width, 8)
    else:
        raise ValueError("DWP requires DWARF4 or DWARF5")
    if address not in (1, 2, 4, 8):
        raise ValueError("invalid DWP address size")
    code, position = _uleb(data, position)
    for attribute, form in _root_attributes(abbrev, offset, code, expected_tag=0x11):
        value, position = _form(data, position, form, width, address)
        if attribute == 0x2131:
            if identity is not None and identity != value:
                raise ValueError("DWP unit has conflicting identities")
            identity = value
    if not identity:
        raise ValueError("DWP unit has no nonzero split-debug identity")
    return identity


def verify_packed_pair(binary_path: Path, debug_path: Path) -> int:
    """Require line/symbol sections and DWP coverage of every skeleton DWO ID."""
    binary, debug = Elf(binary_path), Elf(debug_path)
    if debug.machine != binary.machine:
        raise ValueError("packed-debug machine mismatch")
    for name in (".debug_line", ".symtab"):
        if name not in binary.sections or not binary.sections[name][1]:
            raise ValueError(f"profile-C executable missing {name}")
    data = debug.section(".debug_cu_index")
    if len(data) < 16:
        raise ValueError("truncated DWP compilation-unit index")
    version, columns, units, slots = struct.unpack_from("<4I", data)
    expected = 16 + slots * 12 + columns * 4 + units * columns * 8
    if version not in (2, 5) or not 0 < columns <= 16 or not 0 < units <= MAX_UNITS or not units <= slots <= MAX_SLOTS or slots & (slots - 1) or expected != len(data):
        raise ValueError("invalid DWP compilation-unit index dimensions")
    identities = struct.unpack_from(f"<{slots}Q", data, 16)
    rows = struct.unpack_from(f"<{slots}I", data, 16 + slots * 8)
    present = {identity for identity, row in zip(identities, rows) if identity and row}
    used = [row for identity, row in zip(identities, rows) if identity and row]
    if len(present) != units or sorted(used) != list(range(1, units + 1)) or any(bool(i) != bool(r) for i, r in zip(identities, rows)):
        raise ValueError("invalid DWP identity/row mapping")
    cursor = 16 + slots * 12
    section_ids = struct.unpack_from(f"<{columns}I", data, cursor); cursor += columns * 4
    if len(set(section_ids)) != columns or not {1, 3} <= set(section_ids):
        raise ValueError("DWP lacks unique info/abbreviation columns")
    offsets = struct.unpack_from(f"<{units * columns}I", data, cursor)
    sizes = struct.unpack_from(f"<{units * columns}I", data, cursor + units * columns * 4)
    for index, (start, size) in enumerate(zip(offsets, sizes)):
        section = INDEX_SECTIONS.get(section_ids[index % columns])
        if section_ids[index % columns] in (1, 3) and not size:
            raise ValueError("DWP has an empty info/abbreviation contribution")
        if section not in debug.sections or start + size > debug.sections[section][1]:
            raise ValueError("DWP contribution exceeds its debug section")
    required = _binary_ids(binary)
    row_ids = {row: identity for identity, row in zip(identities, rows) if row}
    info_column, abbrev_column = section_ids.index(1), section_ids.index(3)
    for row in range(units):
        contributions = []
        for column, section in ((info_column, ".debug_info.dwo"),
                                (abbrev_column, ".debug_abbrev.dwo")):
            index = row * columns + column
            contributions.append(debug.read(debug.sections[section][0] + offsets[index],
                                            min(sizes[index], MAX_ROOT_METADATA)))
        actual = _contribution_id(*contributions, sizes[row * columns + info_column])
        if actual != row_ids[row + 1]:
            raise ValueError("DWP index identity disagrees with its compilation unit")
    if not required <= present:
        raise ValueError("DWP does not cover executable split-debug identities")
    return len(required)
