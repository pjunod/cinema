from pathlib import Path
import tempfile
import unittest

from validation.jellyfin_identity_guard import replacement_tables, rust_literals, violations


class JellyfinIdentityGuardTest(unittest.TestCase):
    def test_mapped_entity_replacement_guard_rejects_injected_sql(self):
        samples = [
            'INSERT OR REPLACE INTO users VALUES (1)',
            'replace into "main"."files" VALUES (1)',
            'INSERT /* gap */ OR\nREPLACE INTO [items] VALUES (1)',
            'REPLACE INTO `libraries` VALUES (1)',
        ]
        for sample in samples:
            with self.subTest(sample=sample):
                self.assertTrue(replacement_tables(sample))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'crates/example/src/migration.rs'
            source.parent.mkdir(parents=True)
            injected = 'const SQL: &str = "INSERT OR \\\n REPLACE INTO \\"users\\" VALUES (1)";'
            # Build one actual Rust escaped quote, not an escaped backslash
            # which would terminate the literal before the table name.
            injected = injected.replace('\\\\"', '\\"')
            source.write_text(injected)
            bodies = list(rust_literals(injected))
            self.assertEqual(len(bodies), 1)
            self.assertIn('"users"', bodies[0][1])
            self.assertEqual(len(violations(root)), 1)
            source.write_text('const SQL: &str = r###"REPLACE INTO [files] VALUES (1)"###;')
            self.assertEqual(len(violations(root)), 1)
            source.write_text(
                '// "REPLACE INTO users VALUES (1)"\n'
                '/* outer /* "REPLACE INTO files VALUES (1)" */ comment */\n'
                'const SQL: &str = "INSERT INTO users VALUES (1) '
                'ON CONFLICT(id) DO UPDATE SET username = excluded.username";'
            )
            self.assertEqual(violations(root), [])

    def test_unmapped_replacement_and_regular_upsert_are_permitted(self):
        for sql in [
            'INSERT OR REPLACE INTO settings VALUES (1)',
            'REPLACE INTO user_settings VALUES (1)',
            'INSERT INTO files VALUES (1) ON CONFLICT(path) DO UPDATE SET size=1',
        ]:
            self.assertEqual(replacement_tables(sql), [])

    def test_repository_mapped_entities_do_not_use_replacement_writes(self):
        root = Path(__file__).resolve().parents[2]
        self.assertEqual(violations(root), [])
