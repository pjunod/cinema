"""Real tar fixtures for the CI media transport; no binary or Docker build needed."""
import io
import os
from pathlib import Path
import stat
import tarfile
import tempfile
import unittest
from unittest import mock

from validation.media_export import unpack


def entry(name, kind=tarfile.REGTYPE, data=b'payload', mode=0o644, link=''):
    item = tarfile.TarInfo(name)
    item.type, item.mode, item.linkname = kind, mode, link
    item.size = len(data) if kind == tarfile.REGTYPE else 0
    return item, data


def archive(path, extra=()):
    rows = [entry('package', tarfile.DIRTYPE, mode=0o755),
            entry('package/bin', tarfile.DIRTYPE, mode=0o755),
            entry('package/bin/ffmpeg', data=b'\x7fELF\x00real-bytes\xff', mode=0o755)]
    with tarfile.open(path, 'w') as output:
        for item, data in rows + list(extra):
            output.addfile(item, io.BytesIO(data) if item.isfile() else None)


class MediaExportCase(unittest.TestCase):
    def test_real_tar_preserves_contents_modes_symlink_and_hardlink(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, output = root/'media.tar', root/'export'
            archive(source, [entry('package/readonly', tarfile.DIRTYPE, mode=0o555),
                             entry('package/readonly/child', data=b'child', mode=0o444),
                             entry('package/notice', data=b'license\n', mode=0o444),
                             entry('package/ffmpeg', tarfile.SYMTYPE, mode=0o777, link='bin/ffmpeg'),
                             entry('package/bin/ffmpeg-hard', tarfile.LNKTYPE, mode=0o755,
                                   link='package/bin/ffmpeg')])
            self.assertEqual(unpack(source, output), 8)
            self.assertEqual((output/'package/bin/ffmpeg').read_bytes(), b'\x7fELF\x00real-bytes\xff')
            self.assertEqual((output/'package/notice').read_bytes(), b'license\n')
            self.assertEqual(stat.S_IMODE((output/'package/bin/ffmpeg').stat().st_mode), 0o755)
            self.assertEqual(stat.S_IMODE((output/'package/notice').stat().st_mode), 0o444)
            self.assertEqual(stat.S_IMODE((output/'package/readonly').stat().st_mode), 0o555)
            self.assertEqual((output/'package/readonly/child').read_bytes(), b'child')
            self.assertEqual(os.readlink(output/'package/ffmpeg'), 'bin/ffmpeg')
            self.assertEqual((output/'package/bin/ffmpeg-hard').stat().st_ino,
                             (output/'package/bin/ffmpeg').stat().st_ino)

    def test_late_unsafe_headers_refuse_before_any_output_write(self):
        hazards = [entry('../escape'), entry('/absolute'), entry('package/../escape'),
                   entry('package//ambiguous'), entry('./package/ambiguous'),
                   entry('package/bin/ffmpeg'), entry('package/bin', tarfile.DIRTYPE),
                   entry('package/fifo', tarfile.FIFOTYPE), entry('package/dev', tarfile.CHRTYPE),
                   entry('package/setuid', mode=0o4755),
                   entry('package/escape', tarfile.SYMTYPE, link='../../escape'),
                   entry('package/absolute', tarfile.SYMTYPE, link='/outside'),
                   entry('package/link-mode', tarfile.SYMTYPE, mode=0o600, link='bin/ffmpeg'),
                   entry('package/broken', tarfile.SYMTYPE, link='missing'),
                   entry('package/cycle', tarfile.SYMTYPE, link='cycle'),
                   entry('package/hard-cycle', tarfile.LNKTYPE, link='package/hard-cycle'),
                   entry('package/bad-hard', tarfile.LNKTYPE, mode=0o600, link='package/bin/ffmpeg')]
        for hazard in hazards:
            with self.subTest(hazard=hazard[0].name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory).resolve()
                source, output = root/'media.tar', root/'export'
                archive(source, [hazard])
                with self.assertRaises((ValueError, tarfile.TarError)):
                    unpack(source, output)
                self.assertFalse(output.exists())
                self.assertEqual({p.name for p in root.iterdir()}, {'media.tar'})

    def test_link_or_file_ancestors_cannot_redirect_writes(self):
        for parent in [entry('package/alias', tarfile.SYMTYPE, link='bin'), entry('package/alias')]:
            with self.subTest(kind=parent[0].type), tempfile.TemporaryDirectory() as directory:
                root = Path(directory).resolve()
                source, output = root/'media.tar', root/'export'
                archive(source, [parent, entry('package/alias/injected')])
                with self.assertRaises(ValueError):
                    unpack(source, output)
                self.assertFalse(output.exists())

    def test_truncation_or_appended_headers_refuse_before_output_write(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, output = root/'media.tar', root/'export'
            archive(source)
            original = source.read_bytes()
            with tarfile.open(source) as parsed:
                last = parsed.getmembers()[-1]
            for bad in [original[:last.offset_data + last.size - 1],
                        b'x' + original[1:],
                        original + tarfile.TarInfo('package/hidden').tobuf()]:
                source.write_bytes(bad)
                with self.assertRaises((ValueError, tarfile.TarError)):
                    unpack(source, output)
                self.assertFalse(output.exists())

    def test_existing_destination_is_never_modified(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, output = root/'media.tar', root/'export'
            archive(source)
            output.mkdir()
            (output/'keep').write_bytes(b'untouched')
            with self.assertRaises(ValueError):
                unpack(source, output)
            self.assertEqual((output/'keep').read_bytes(), b'untouched')

    def test_copy_failure_cleans_private_staging_without_publishing_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, output = root/'media.tar', root/'export'
            archive(source)
            with mock.patch('validation.media_export.shutil.copyfileobj', side_effect=OSError('disk full')):
                with self.assertRaises(OSError):
                    unpack(source, output)
            self.assertFalse(output.exists())
            self.assertEqual({p.name for p in root.iterdir()}, {'media.tar'})


if __name__ == '__main__':
    unittest.main()
