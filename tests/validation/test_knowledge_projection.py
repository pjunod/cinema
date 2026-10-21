"""Visible-corpus isolation: synthetic passages only, never sealed gold."""
import hashlib
from types import SimpleNamespace
import unittest

from validation.knowledge.evaluation_projection import project
from validation.knowledge.model import Anchor, KnowledgeError, Node, Provenance, content_digest, digest_bytes

REVISION = 'a' * 40


def fixture(content, *, kind='section', selector=('Rule',), line=1, end=2):
    raw = content.encode()
    expected = digest_bytes(raw) if kind == 'section' else content_digest({'docs/rule.md': digest_bytes(raw)})
    aid = 'plurx:' + kind + ':fixture'
    anchor = Anchor(aid, 'docs/rule.md', kind, selector, expected, expected,
                    'docs/rule.md', ('docs/rule.md',), 'valid', line, end)
    provenance = Provenance('reviewed', 'author', 'reviewer', aid, expected, REVISION)
    node = Node('plurx:contract:fixture', 'contract', 'Synthetic rule', ('src/code.py',),
                (aid,), {'behavior': 'Synthetic visible rule.'}, provenance)
    return SimpleNamespace(nodes=(node,), anchors=(anchor,), edges=(), claims=(),
                           coverage=(), conflicts=(), receipts=())


def views(content, ranges):
    return {'docs/rule.md': {'content': content, 'mode': 33188}}, {
        'docs/rule.md': {'line_ranges': ranges,
                         'content_sha256': hashlib.sha256(content.encode()).hexdigest()}}


class KnowledgeProjectionTests(unittest.TestCase):
    def test_whole_reviewed_section_survives_but_invisible_section_does_not(self):
        original = '# Rule\nOnly a synthetic constraint.\n'
        graph = fixture(original)
        whole = project(graph, *views(original, [[1, 2]]))
        self.assertEqual(len(whole.nodes), 1)
        self.assertEqual(whole.anchors[0].integrity, 'valid')
        partial = project(graph, *views('# Rule\n\n', [[1, 1]]))
        self.assertEqual(partial.nodes, ())
        self.assertEqual(partial.anchors, ())
        self.assertEqual(partial.excluded_nodes, 1)

    def test_mutated_visible_section_is_never_marked_as_pristine(self):
        graph = fixture('# Rule\nOriginal synthetic wording.\n')
        modified = '# Rule\nChanged synthetic wording.\n'
        result = project(graph, *views(modified, [[1, 2]]))
        self.assertEqual(result.anchors[0].integrity, 'changed')
        self.assertNotEqual(result.anchors[0].current_digest, result.anchors[0].digest)

    def test_partial_symbol_cannot_attest_unseen_physical_source(self):
        content = 'def rule():\n    return 1\n'
        graph = fixture(content, kind='symbol', selector='def rule():', end=1)
        result = project(graph, *views('def rule():\n\n', [[1, 1]]))
        self.assertEqual(result.anchors[0].integrity, 'changed')
        self.assertEqual(result.anchors[0].excerpt, '')

    def test_hidden_source_and_digest_tampering_refuse(self):
        graph = fixture('# Rule\nSynthetic.\n')
        files, visibility = views('# Rule\nHidden text.\n', [[1, 1]])
        with self.assertRaises(KnowledgeError):
            project(graph, files, visibility)
        files, visibility = views('# Rule\nSynthetic.\n', [[1, 2]])
        visibility['docs/rule.md']['content_sha256'] = '0' * 64
        with self.assertRaises(KnowledgeError):
            project(graph, files, visibility)

    def test_hidden_origin_prunes_record_even_when_its_anchor_is_visible(self):
        original = '# Rule\nSynthetic.\n'
        graph = fixture(original)
        node = graph.nodes[0]
        hidden = Provenance('reviewed', 'author', 'reviewer', 'docs/private-rule.md',
                            digest_bytes(b'hidden'), REVISION)
        graph.nodes = (Node(node.id, node.kind, node.title, node.scope, node.anchors,
                            node.attributes, hidden),)
        self.assertEqual(project(graph, *views(original, [[1, 2]])).nodes, ())


if __name__ == '__main__':
    unittest.main()
