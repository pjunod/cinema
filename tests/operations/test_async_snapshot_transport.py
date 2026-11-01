"""Defined-only mutation fixtures for private snapshot byte primitives."""
from pathlib import Path
import tempfile
import unittest

from validation import async_snapshot_transport as transport

ROOT = Path(__file__).resolve().parents[2]


class SnapshotPrimitiveTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / '.agent-proof-fixtures' / 'snapshot-transport'
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.cas = self.root / 'cas'
        self.cas.mkdir(mode=0o700)
        self.charged = 0
        self.lease_valid = True

    def lease(self):
        if not self.lease_valid:
            raise transport.SnapshotTransportError('expired fixture lease')

    def reserve(self, size):
        self.charged += size

    def release(self, size):
        self.charged -= size

    def owned(self):
        return transport.OwnedDirectory(self.cas, self.root, self.lease)

    def test_fixed_chunks_preserve_exact_tail_and_empty_image(self):
        snapshot = self.cas / 'snapshot'
        snapshot.write_bytes(b'a' * transport.CHUNK_BYTES + b'tail')
        snapshot.chmod(0o600)
        with self.owned() as owned:
            chunks = list(transport.iter_chunks(owned, 'snapshot'))
            self.assertEqual([len(row[1]) for row in chunks], [transport.CHUNK_BYTES, 4])
            self.assertEqual([row[0] for row in chunks], [0, 1])
            snapshot.write_bytes(b'')
            self.assertEqual(list(transport.iter_chunks(owned, 'snapshot')), [])

    def test_reuse_rehashes_bytes_and_never_charges_twice(self):
        with self.owned() as owned:
            digest, size, fresh = transport.store_chunk(owned, b'exact bytes', self.reserve, self.release)
            self.assertTrue(fresh)
            self.assertEqual(self.charged, size)
            self.assertEqual(transport.store_chunk(owned, b'exact bytes', self.reserve, self.release),
                             (digest, size, False))
            self.assertEqual(self.charged, size)
            (self.cas / digest).write_bytes(b'wrong bytes')
            with self.assertRaises(transport.SnapshotTransportError):
                transport.store_chunk(owned, b'exact bytes', self.reserve, self.release)

    def test_symlink_and_external_hardlink_chunks_are_rejected(self):
        outside = self.root / 'outside'
        outside.write_bytes(b'bytes')
        outside.chmod(0o600)
        with self.owned() as owned:
            digest, size, _ = transport.store_chunk(owned, b'bytes', self.reserve, self.release)
            member = self.cas / digest
            member.unlink()
            member.symlink_to(outside)
            with self.assertRaises(OSError):
                transport.verify_chunk(owned, digest, size)
            member.unlink()
            member.hardlink_to(outside)
            with self.assertRaises(transport.SnapshotTransportError):
                transport.verify_chunk(owned, digest, size)

    def test_expired_lease_and_replaced_directory_prevent_writes(self):
        with self.owned() as owned:
            self.lease_valid = False
            with self.assertRaises(transport.SnapshotTransportError):
                transport.store_chunk(owned, b'bytes', self.reserve, self.release)
            self.lease_valid = True
            self.cas.rename(self.root / 'previous')
            self.cas.mkdir(mode=0o700)
            with self.assertRaises(transport.SnapshotTransportError):
                transport.store_chunk(owned, b'bytes', self.reserve, self.release)
        self.assertEqual(self.charged, 0)

    def test_mutation_between_streamed_chunks_is_rejected(self):
        snapshot = self.cas / 'snapshot'
        snapshot.write_bytes(b'a' * (transport.CHUNK_BYTES + 1))
        snapshot.chmod(0o600)
        with self.owned() as owned:
            chunks = transport.iter_chunks(owned, 'snapshot')
            next(chunks)
            snapshot.write_bytes(b'short')
            with self.assertRaises(transport.SnapshotTransportError):
                list(chunks)

    def test_budget_refusal_creates_no_cas_or_temporary_member(self):
        def refuse(_size):
            raise transport.SnapshotTransportError('admitted pool exhausted')

        with self.owned() as owned:
            with self.assertRaises(transport.SnapshotTransportError):
                transport.store_chunk(owned, b'bytes', refuse, self.release)
        self.assertEqual(list(self.cas.iterdir()), [])
