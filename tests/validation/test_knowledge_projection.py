"""Visible-corpus isolation: synthetic passages only, never sealed gold."""
from dataclasses import replace
import hashlib
from types import SimpleNamespace
import unittest

from validation.knowledge.evaluation_projection import project
from validation.knowledge.model import Anchor, Claim, Edge, KnowledgeError, Node, Provenance, canonical_json, content_digest, digest_bytes

# Paths below belong to disposable repositories, not this documentation tree.
FIXTURE_DOC_ROOT = "docs"

REVISION = 'a' * 40


def fixture(content, *, kind='section', selector=('Rule',), line=1, end=2):
    raw = content.encode()
    expected = digest_bytes(raw) if kind == 'section' else content_digest({f'{FIXTURE_DOC_ROOT}/rule.md': digest_bytes(raw)})
    aid = 'plurx:' + kind + ':fixture'
    anchor = Anchor(aid, f'{FIXTURE_DOC_ROOT}/rule.md', kind, selector, expected, expected,
                    f'{FIXTURE_DOC_ROOT}/rule.md', (f'{FIXTURE_DOC_ROOT}/rule.md',), 'valid', line, end)
    provenance = Provenance('reviewed', 'author', 'reviewer', aid, expected, REVISION)
    node = Node('plurx:contract:fixture', 'contract', 'Synthetic rule', ('src/code.py',),
                (aid,), {'behavior': 'Synthetic visible rule.'}, provenance)
    return SimpleNamespace(nodes=(node,), anchors=(anchor,), edges=(), claims=(),
                           coverage=(), conflicts=(), receipts=())


def views(content, ranges):
    return {f'{FIXTURE_DOC_ROOT}/rule.md': {'content': content, 'mode': 33188}}, {
        f'{FIXTURE_DOC_ROOT}/rule.md': {'line_ranges': ranges,
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

    def test_changed_sentence_outside_unchanged_excerpt_retracts_pristine_semantics(self):
        original = '# Rule\nUnchanged excerpt.\nPristine governing sentence.\n'
        graph = fixture(original, end=3)
        anchor = replace(graph.anchors[0], excerpt='Unchanged excerpt.')
        provenance = graph.nodes[0].provenance
        contract = replace(graph.nodes[0], title='PRISTINE_ONLY_TITLE', attributes={'behavior': 'PRISTINE_ONLY_BEHAVIOR'})
        obligation = Node('plurx:obligation:fixture', 'obligation', 'Obligation', contract.scope,
                          contract.anchors, {'action': 'PRISTINE_ONLY_ACTION'}, provenance)
        decision = Node('plurx:decision:fixture', 'decision', 'Decision', contract.scope,
                        contract.anchors, {'rationale': 'PRISTINE_ONLY_RATIONALE'}, provenance)
        section = Node(anchor.id, 'section', 'Rule', (f'{FIXTURE_DOC_ROOT}/rule.md',), (anchor.id,),
                       {'path': f'{FIXTURE_DOC_ROOT}/rule.md', 'integrity': 'valid'}, provenance)
        artifact = Node('plurx:artifact:fixture', 'artifact', 'Artifact', (f'{FIXTURE_DOC_ROOT}/rule.md',), (),
                        {'path': f'{FIXTURE_DOC_ROOT}/rule.md', 'digest': digest_bytes(original.encode()), 'role': 'reference'},
                        replace(provenance, method='parsed', origin=f'{FIXTURE_DOC_ROOT}/rule.md', source_digest=digest_bytes(original.encode())))
        graph.nodes = (contract, obligation, decision, section, artifact)
        graph.anchors = (anchor,)
        graph.claims = (Claim(contract.id, 'authority', 'PRISTINE_ONLY_CLAIM', provenance),)
        graph.edges = (Edge(contract.id, 'constrained-by', decision.id, provenance),)
        modified = '# Rule\nUnchanged excerpt.\nDifferent governing sentence.\n'
        result = project(graph, *views(modified, [[1, 3]]))
        serialized = canonical_json(result)
        self.assertNotIn('PRISTINE_ONLY', serialized)
        self.assertEqual(result.claims, ())
        self.assertEqual(result.edges, ())
        rebuilt = {node.id: node for node in result.nodes}
        self.assertEqual(rebuilt[artifact.id].attributes['digest'], digest_bytes(modified.encode()))
        self.assertNotEqual(rebuilt[artifact.id].attributes['digest'], artifact.attributes['digest'])
        self.assertEqual(rebuilt[section.id].attributes['integrity'], 'changed')

    def test_visible_signature_cannot_reveal_invisible_body_summary(self):
        original = 'def rule():\n    return "hidden original body"\n'
        graph = fixture(original, kind='symbol', selector='def rule():', end=1)
        graph.nodes = (replace(graph.nodes[0], attributes={'behavior': 'PRISTINE_ONLY_BODY_ACTION'}),)
        result = project(graph, *views('def rule():\n\n', [[1, 1]]))
        self.assertEqual(result.nodes, ())
        self.assertNotIn('PRISTINE_ONLY_BODY_ACTION', canonical_json(result))
        self.assertEqual(result.anchors[0].integrity, 'changed')

    def test_unchanged_visible_policy_survives_changed_implementation(self):
        policy = '# Rule\nAn independently visible policy still applies.\n'
        graph = fixture(policy)
        code = 'def rule():\n    return 1\n'
        code_digest = content_digest({'src/code.py': digest_bytes(code.encode())})
        code_anchor = Anchor('plurx:symbol:implementation', 'src/code.py', 'symbol', 'def rule():',
                             code_digest, code_digest, 'src/code.py', ('src/code.py',), 'valid', 1, 1)
        graph.anchors += (code_anchor,)
        graph.nodes = (replace(graph.nodes[0], anchors=graph.nodes[0].anchors + (code_anchor.id,),
                               attributes={'behavior': 'The independently visible policy still applies.'}),)
        files, visibility = views(policy, [[1, 2]])
        changed_code = 'def rule():\n    return 2\n'
        files['src/code.py'] = {'content': changed_code, 'mode': 33188}
        visibility['src/code.py'] = {'line_ranges': [[1, 2]], 'content_sha256': hashlib.sha256(changed_code.encode()).hexdigest()}
        result = project(graph, files, visibility)
        self.assertEqual(len(result.nodes), 1)
        self.assertEqual(result.nodes[0].attributes['behavior'], 'The independently visible policy still applies.')
        self.assertEqual(result.nodes[0].anchors, (graph.anchors[0].id,))
        self.assertEqual(next(anchor for anchor in result.anchors if anchor.id == code_anchor.id).integrity, 'changed')

    def test_hidden_source_and_digest_tampering_refuse(self):
        graph = fixture('# Rule\nSynthetic.\n')
        files, visibility = views('# Rule\nHidden text.\n', [[1, 1]])
        with self.assertRaises(KnowledgeError):
            project(graph, files, visibility)
        files, visibility = views('# Rule\nSynthetic.\n', [[1, 2]])
        visibility[f'{FIXTURE_DOC_ROOT}/rule.md']['content_sha256'] = '0' * 64
        with self.assertRaises(KnowledgeError):
            project(graph, files, visibility)

    def test_sealed_permission_modes_are_regular_and_symlink_modes_refuse(self):
        original = '# Rule\nSynthetic.\n'
        graph = fixture(original)
        files, visibility = views(original, [[1, 2]])
        files[f'{FIXTURE_DOC_ROOT}/rule.md']['mode'] = 0o644
        self.assertEqual(len(project(graph, files, visibility).nodes), 1)
        files[f'{FIXTURE_DOC_ROOT}/rule.md']['mode'] = 0o120000
        with self.assertRaises(KnowledgeError):
            project(graph, files, visibility)

    def test_hidden_origin_prunes_record_even_when_its_anchor_is_visible(self):
        original = '# Rule\nSynthetic.\n'
        graph = fixture(original)
        node = graph.nodes[0]
        hidden = Provenance('reviewed', 'author', 'reviewer', f'{FIXTURE_DOC_ROOT}/private-rule.md',
                            digest_bytes(b'hidden'), REVISION)
        graph.nodes = (Node(node.id, node.kind, node.title, node.scope, node.anchors,
                            node.attributes, hidden),)
        self.assertEqual(project(graph, *views(original, [[1, 2]])).nodes, ())


if __name__ == '__main__':
    unittest.main()
