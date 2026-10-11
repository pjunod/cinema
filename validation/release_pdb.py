"""Match an MSVC executable's CodeView GUID/age to its retained MSF7 PDB."""

from pathlib import Path
import argparse
import math
import re
import struct


MAX_DIRECTORY = 32 * 1024 * 1024
MSF7 = b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0"


def _read(path: Path, offset: int, size: int) -> bytes:
    if path.is_symlink() or not path.is_file() or size > MAX_DIRECTORY or offset + size > path.stat().st_size:
        raise ValueError("invalid bounded PDB/PE file range")
    with path.open("rb") as source:
        source.seek(offset); data = source.read(size)
    if len(data) != size:
        raise ValueError("truncated PDB/PE file")
    return data


def _codeview(executable: Path) -> tuple[bytes, int, str]:
    dos = _read(executable, 0, 64)
    if dos[:2] != b"MZ":
        raise ValueError("missing executable DOS header")
    pe = struct.unpack_from("<I", dos, 60)[0]
    header = _read(executable, pe, 24)
    if header[:4] != b"PE\0\0":
        raise ValueError("missing executable PE header")
    if struct.unpack_from("<H", header, 4)[0] != 0x8664:
        raise ValueError("expected the Windows x86_64 release machine")
    count, optional_size = struct.unpack_from("<H", header, 6)[0], struct.unpack_from("<H", header, 20)[0]
    optional = _read(executable, pe + 24, optional_size)
    if len(optional) < 168 or struct.unpack_from("<H", optional)[0] != 0x20B:
        raise ValueError("expected Windows PE32+ executable")
    rva, length = struct.unpack_from("<II", optional, 112 + 6 * 8)
    table = _read(executable, pe + 24 + optional_size, count * 40)
    for position in range(0, len(table), 40):
        virtual_size, address, raw_size, raw = struct.unpack_from("<4I", table, position + 8)
        if address <= rva and rva + length <= address + min(virtual_size, raw_size):
            directory = _read(executable, raw + rva - address, length)
            break
    else:
        raise ValueError("executable debug directory is not file-backed")
    if not length or length % 28:
        raise ValueError("invalid executable debug directory")
    pairs = []
    for position in range(0, length, 28):
        kind, size, _address, raw = struct.unpack_from("<4I", directory, position + 12)
        if kind != 2:
            continue
        if size > 65_536:
            raise ValueError("CodeView identity exceeds bounded record size")
        data = _read(executable, raw, size)
        if len(data) < 25 or data[:4] != b"RSDS" or b"\0" not in data[24:]:
            raise ValueError("missing executable RSDS CodeView identity")
        name = data[24:].split(b"\0", 1)[0].decode("utf-8").replace("\\", "/").rsplit("/", 1)[-1]
        pattern = re.escape(executable.stem) + r"(?:-[0-9a-f]+)?\.pdb"
        if re.fullmatch(pattern, name, flags=re.IGNORECASE) is None:
            raise ValueError("executable names an unsafe or unrelated PDB")
        pairs.append((data[4:20], struct.unpack_from("<I", data, 20)[0], name))
    if len(pairs) != 1:
        raise ValueError("expected one executable CodeView identity")
    return pairs[0]


def _pdb_identity(pdb: Path) -> tuple[bytes, int]:
    header = _read(pdb, 0, 56)
    if header[:32] != MSF7:
        raise ValueError("expected an MSF7 PDB")
    block_size, _free, blocks, size, _reserved, map_block = struct.unpack_from("<6I", header, 32)
    if block_size not in (512, 1024, 2048, 4096) or blocks * block_size != pdb.stat().st_size:
        raise ValueError("invalid PDB block geometry")
    if size < 12 or size > MAX_DIRECTORY:
        raise ValueError("invalid bounded PDB directory size")
    count = math.ceil(size / block_size)
    if count * 4 > block_size or map_block >= blocks:
        raise ValueError("unsupported PDB directory block map")
    numbers = struct.unpack(f"<{count}I", _read(pdb, map_block * block_size, count * 4))
    if any(number >= blocks for number in numbers):
        raise ValueError("PDB directory block exceeds file")
    directory = b"".join(_read(pdb, number * block_size, block_size) for number in numbers)[:size]
    streams = struct.unpack_from("<I", directory)[0]
    if not 2 <= streams <= 65_536 or 4 + streams * 4 > len(directory):
        raise ValueError("invalid PDB stream directory")
    sizes = struct.unpack_from(f"<{streams}I", directory, 4)
    position = 4 + streams * 4
    stream_one = None
    for index, stream_size in enumerate(sizes):
        number_count = 0 if stream_size == 0xFFFFFFFF else math.ceil(stream_size / block_size)
        if position + number_count * 4 > len(directory):
            raise ValueError("truncated PDB stream block map")
        if any(struct.unpack_from("<I", directory, position + block * 4)[0] >= blocks
               for block in range(number_count)):
            raise ValueError("PDB stream block exceeds file")
        if index == 1:
            if stream_size < 28 or number_count < 1:
                raise ValueError("PDB has no identity stream")
            number = struct.unpack_from("<I", directory, position)[0]
            stream_one = _read(pdb, number * block_size, 28)
        position += number_count * 4
    return stream_one[12:28], struct.unpack_from("<I", stream_one, 8)[0]


def verify_pdb_pair(executable: Path, pdb: Path) -> str:
    guid, age, name = _codeview(executable)
    if (guid, age) != _pdb_identity(pdb):
        raise ValueError("executable/PDB GUID or age mismatch")
    return name


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=Path)
    parser.add_argument("pdb", type=Path)
    args = parser.parse_args()
    print(verify_pdb_pair(args.executable, args.pdb))


if __name__ == "__main__":
    main()
