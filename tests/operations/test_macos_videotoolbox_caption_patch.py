"""Compile exact patched SEI parser/append logic against original byte vectors.

The CoreMedia transport and unchanged SEI writer are isolated here. Actual
encoder caption round-trip acceptance is a separate package experiment.
"""
import ctypes
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PATCH = ROOT / "scripts/macos-video-ffmpeg-patches/0003-videotoolbox-caption-sei.patch"


def updated_hunks():
    return "\n".join(line[1:] for line in PATCH.read_text().splitlines()
                     if line.startswith(("+", " ")) and not line.startswith("+++"))


def block(text, marker):
    start = text.index(marker)
    brace = text.index("{", start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[start:end]


def escaped(data):
    result = bytearray()
    zeros = 0
    for byte in data:
        if zeros == 2 and byte <= 3:
            result.append(3)
            zeros = 0
        result.append(byte)
        zeros = zeros + 1 if byte == 0 else 0
    return bytes(result)


def extended(value):
    return b"\xff" * (value // 255) + bytes([value % 255])


def nal(messages):
    return b"\x06" + escaped(b"".join(extended(kind) + extended(len(payload)) + payload
                                     for kind, payload in messages) + b"\x80")


def c_source(limit=None):
    text = updated_hunks()
    parser = block(text, "static int read_sei_byte(") + "\n" + block(text, "static int find_sei_end(")
    append = block(text, "if (sei && !wrote_sei && nal_type == H264_NAL_SEI)")
    headers = """
#include <stdint.h>
#include <stddef.h>
#include <limits.h>
#include <string.h>
typedef struct AVCodecContext { int unused; } AVCodecContext;
#define AVERROR_INVALIDDATA (-1094995529)
#define AVERROR_BUFFER_TOO_SMALL (-1397118274)
#define AV_LOG_ERROR 16
#define H264_NAL_SEI 6
#define SEI_TYPE_USER_DATA_REGISTERED_ITU_T_T35 4
#define av_log(ctx, level, ...) ((void)(ctx))
"""
    if limit is not None:
        headers += f"#undef SIZE_MAX\n#define SIZE_MAX ((size_t){limit})\n"
    wrappers = """
int parse_sei(uint8_t *data, size_t size, size_t *offset) {
    uint8_t *end = NULL;
    int result = find_sei_end(NULL, data, size, &end);
    *offset = end ? (size_t)(end - data) : SIZE_MAX;
    return result;
}
typedef struct ExtraSEI { const uint8_t *data; size_t size; } ExtraSEI;
static size_t writer_capacity;
/* Deterministic encoded message isolates the patched caller's capacity. */
static int write_sei(const ExtraSEI *sei, int type, uint8_t *dst, size_t capacity) {
    (void)type;
    writer_capacity = capacity;
    if (sei->size > capacity) return AVERROR_BUFFER_TOO_SMALL;
    memcpy(dst, sei->data, sei->size);
    return (int)sei->size;
}
size_t observed_writer_capacity(void) { return writer_capacity; }
int append_sei(uint8_t *data, size_t capacity, size_t size,
               const uint8_t *message, size_t message_size) {
    AVCodecContext context;
    AVCodecContext *avctx = &context;
    ExtraSEI item = { message, message_size };
    ExtraSEI *sei = &item;
    int wrote_sei = 0, nal_type = H264_NAL_SEI;
    uint8_t *dst_data = data, *dst_box = data + 4;
    size_t remaining_dst_size = capacity, box_len = size;
    if (capacity < size + 4) return AVERROR_BUFFER_TOO_SMALL;
APPEND_BLOCK
    return (int)(size + 4 + capacity - remaining_dst_size);
}
""".replace("APPEND_BLOCK", append)
    return headers + parser + wrappers


class VideoToolboxCaptionPatch(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.compiler = shutil.which("cc")
        if not cls.compiler:
            raise RuntimeError("C compiler required for the exact caption parser regression")
        cls.library = cls.compile_library("normal", None)
        # A reduced limit exercises the identical overflow guard without a
        # multi-gigabyte malicious header allocation.
        cls.limited = cls.compile_library("limited", 255)

    @classmethod
    def tearDownClass(cls):
        cls.directory.cleanup()

    @classmethod
    def compile_library(cls, name, limit):
        root = Path(cls.directory.name)
        source = root / (name + ".c")
        library = root / (name + ".so")
        source.write_text(c_source(limit))
        subprocess.run([cls.compiler, "-std=c11", "-shared", "-fPIC", "-Wall", "-Wextra",
                        "-Werror", str(source), "-o", str(library)], check=True,
                       capture_output=True, timeout=30)
        result = ctypes.CDLL(str(library))
        result.parse_sei.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
        result.parse_sei.restype = ctypes.c_int
        result.append_sei.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_size_t,
                                     ctypes.c_void_p, ctypes.c_size_t]
        result.append_sei.restype = ctypes.c_int
        result.observed_writer_capacity.restype = ctypes.c_size_t
        return result

    def parse(self, data, library=None):
        buffer = ctypes.create_string_buffer(data)
        offset = ctypes.c_size_t()
        result = (library or self.library).parse_sei(buffer, len(data), ctypes.byref(offset))
        self.assertEqual(buffer.raw[:len(data)], data, "existing encoded SEI must remain unchanged")
        return result, offset.value

    def test_parser_counts_rbsp_and_retains_encoded_offsets(self):
        for messages in [[], [(0, b"")], [(5, b"\0\0\0\0\1\0\0\2\0\0\3")],
                         [(0, b""), (5, b"\0\0\1"), (4, b"original captions")],
                         [(510, b"payload"), (5, b"x" * 510)],
                         [(5, b"\0\0"), (0, b"\0\0\3")]]:
            with self.subTest(messages=messages):
                data = nal(messages)
                self.assertEqual(self.parse(data), (len(data), len(data) - 1))

    def test_parser_rejects_malformed_sizes_escapes_trailers_and_overflow(self):
        for data in [b"\x06", b"\x06\xff\xff", b"\x06\x05\xff",
                     b"\x06\x05\x03\x01\x80", b"\x06\x05\x01\x44",
                     b"\x06\x05\x03\0\0\x03\x80",
                     b"\x06\x05\x03\0\0\x03\x04\x80",
                     b"\x06\x05\x03\0\0\x01\x80",
                     b"\x06\x05\x01\x44\x80\0"]:
            with self.subTest(data=data):
                self.assertLess(self.parse(data)[0], 0)
        large = nal([(5, b"x" * 510)])
        self.assertGreater(self.parse(large)[0], 0)
        self.assertLess(self.parse(large, self.limited)[0], 0)

    def test_append_capacity_reserves_start_code_and_exact_trailer(self):
        original = nal([(5, b"original\0\0\1escaped payload")])
        message = extended(4) + extended(8) + b"captions"
        initial = b"\0\0\0\1" + original
        expected = initial[:-1] + message + b"\x80"
        for capacity, success in [(len(expected), True), (len(expected) - 1, False),
                                  (len(initial), False)]:
            with self.subTest(capacity=capacity):
                buffer = ctypes.create_string_buffer(initial + b"\xa5" * 128)
                payload = ctypes.create_string_buffer(message)
                result = self.library.append_sei(buffer, capacity, len(original), payload, len(message))
                self.assertEqual(buffer.raw[capacity:], (initial + b"\xa5" * 128 + b"\0")[capacity:])
                if success:
                    self.assertEqual(result, len(expected))
                    self.assertEqual(buffer.raw[:result], expected)
                    self.assertEqual(self.library.observed_writer_capacity(), len(message))
                else:
                    self.assertLess(result, 0)


if __name__ == "__main__":
    unittest.main()
