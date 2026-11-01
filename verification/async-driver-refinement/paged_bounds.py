"""Source-bound admission derivation, never an observed-state maximum.

Only a registered complete, source-reviewed construction specification can
produce a receipt. Operational quotas and caller-provided widths are excluded.
The finite formula DAG is retained symbolically when exact expansion is huge.
"""
from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import sys

from paged_codec import Invalid, canonical, encoded, require, sha_bytes, strict_json

PACKAGE = Path(__file__).resolve().parent
ROOT = PACKAGE.parents[1]
SPEC_KEYS = {'schema_version', 'kind', 'algorithm_version', 'complete', 'finite_inventory_sha256',
             'record_schemas', 'init_class', 'init_class_rules', 'primitive_bindings', 'formulas', 'records',
             'limits', 'source_inputs', 'report_role', 'inventory_mapping'}
LIMIT_KEYS = {'maximum_nodes', 'maximum_depth', 'maximum_arguments', 'maximum_integer_digits', 'maximum_metadata_bytes'}
# Nonsemantic storage is charged to explicit PhysicalPolicy3 families/pools (typed_physical_caps_v1),
# never to an aggregate multiplied against logical record allowances. Each peak-workspace class
# names exactly one physical charge; nothing is a derived whole-backup or index multiple.
PHYSICAL_CHARGES = {
    'backup_and_chunks': 'snapshot_spool_bytes',
    'downloads_and_restores': 'restore_staging_bytes',
    'logs': 'retained_artifact_bytes',
    'source_archive': 'source_runtime_scratch_bytes',
    'sql_indexes': 'family_main_bytes',
    'sql_wal': 'family_journal_bytes',
}


def exact(value, keys, label):
    require(type(value) is dict and set(value) == set(keys), label + ': closed shape differs')


def decimal(value, label):
    require(type(value) is str and value and value.isascii() and value.isdecimal() and
            (value == '0' or not value.startswith('0')), label + ': canonical natural decimal required')
    return int(value)


def source_file(root, relative):
    path = Path(relative)
    require(not path.is_absolute() and all(part not in ('', '.', '..') for part in path.parts), 'unsafe source input')
    full = Path(root) / path
    require(full.is_file() and not full.is_symlink() and full.resolve().is_relative_to(Path(root).resolve()), 'source input missing/outside root')
    return full


class FormulaGraph:
    """Closed acyclic arithmetic with sound budget comparison and no eval."""
    def __init__(self, nodes, primitives, limits):
        self.nodes, self.depth = {}, {}
        self.primitives, self.limits = primitives, limits
        require(type(nodes) is list and len(nodes) <= limits['maximum_nodes'], 'formula count exceeds registration')
        for n in nodes:
            require(type(n) is dict and type(n.get('id')) is str and n['id'] and n['id'] not in self.nodes, 'duplicate/invalid formula ID')
            op = n.get('op')
            if op == 'literal':
                exact(n, {'id', 'op', 'value'}, 'literal')
                require(len(n['value']) <= limits['maximum_integer_digits'], 'literal integer length')
                decimal(n['value'], 'literal')
                deps = []
            elif op == 'ref':
                exact(n, {'id', 'op', 'primitive_id'}, 'primitive ref')
                require(n['primitive_id'] in primitives, 'unknown primitive binding')
                deps = []
            elif op in ('add', 'multiply', 'max'):
                exact(n, {'id', 'op', 'args'}, op)
                require(type(n['args']) is list and 1 <= len(n['args']) <= limits['maximum_arguments'], 'arithmetic argument bound')
                deps = n['args']
            elif op == 'power':
                exact(n, {'id', 'op', 'base', 'exponent'}, 'power')
                deps = [n['base'], n['exponent']]
            else:
                raise Invalid('unknown source-bound arithmetic operator')
            require(all(type(d) is str and d in self.nodes for d in deps), 'forward/unknown formula operand')
            depth = 1 + max((self.depth[d] for d in deps), default=0)
            require(depth <= limits['maximum_depth'], 'formula depth exceeds registration')
            self.nodes[n['id']], self.depth[n['id']] = n, depth

    def bounded_value(self, identity, ceiling):
        """Return min(exact_value, ceiling+1), with a proof-preserving sentinel.

        This saturates only metadata comparison; it never clips a source value
        or prints the sentinel as an exact cardinality.
        """
        require(type(ceiling) is int and ceiling >= 0 and identity in self.nodes, 'invalid formula comparison')
        values = {}
        for key, n in self.nodes.items():
            op = n['op']
            if op == 'literal':
                v = decimal(n['value'], 'literal')
            elif op == 'ref':
                v = self.primitives[n['primitive_id']]
            elif op == 'add':
                v = sum(values[x] for x in n['args'])
            elif op == 'multiply':
                v = 1
                for arg in n['args']:
                    v = min(ceiling + 1, v * values[arg])
            elif op == 'max':
                v = max(values[x] for x in n['args'])
            else:
                base, exponent = values[n['base']], values[n['exponent']]
                if exponent == 0 or base == 1:
                    v = 1
                elif base == 0:
                    v = 0
                else:
                    # Repeated squaring short-circuits only after proving >C.
                    v = 1
                    while exponent:
                        if exponent & 1:
                            v = min(ceiling + 1, v * base)
                        exponent >>= 1
                        if exponent:
                            base = min(ceiling + 1, base * base)
                        if v > ceiling:
                            break
            values[key] = min(ceiling + 1, v)
            if key == identity:
                return values[key]
        raise Invalid('missing formula')

    def exact_if_bounded(self, identity, maximum_digits=None):
        digits = maximum_digits or self.limits['maximum_integer_digits']
        limit = 10 ** digits - 1
        value = self.bounded_value(identity, limit)
        return str(value) if value <= limit else None


def validate_specification(specification, *, root=ROOT, expected_sha256=None):
    raw = Path(specification).read_bytes()
    require(expected_sha256 is not None and sha_bytes(raw) == expected_sha256, 'unregistered source-bound specification')
    spec = strict_json(raw)
    exact(spec, SPEC_KEYS, 'source-bound construction specification')
    require(spec['schema_version'] == spec['algorithm_version'] == 1 and spec['kind'] == 'source_bound_specification', 'construction specification version')
    require(spec['report_role']=='source_global_symbolic','source report role differs')
    require(spec['complete'] is True, 'source-bound per-field/row/opaque/Init derivation incomplete; admission refused')
    exact(spec['limits'], LIMIT_KEYS, 'registered metadata limits')
    require(all(type(v) is int and v > 0 for v in spec['limits'].values()) and len(raw) <= spec['limits']['maximum_metadata_bytes'], 'metadata registration bounds')
    require(type(spec['source_inputs']) is list and spec['source_inputs'] == sorted(spec['source_inputs'], key=lambda r: r['path']), 'source input order')
    seen = set()
    sources = {}
    for r in spec['source_inputs']:
        exact(r, {'path', 'sha256'}, 'source input')
        require(r['path'] not in seen, 'duplicate source input')
        seen.add(r['path'])
        path = source_file(root, r['path'])
        require(sha_bytes(path.read_bytes()) == r['sha256'], 'source-bound premise source drift: ' + r['path'])
        sources[r['path']] = path
    require(spec['record_schemas'] == sorted(spec['record_schemas'], key=lambda r: r['path']), 'record schema order')
    for r in spec['record_schemas']:
        exact(r, {'path', 'sha256'}, 'record schema binding')
        require(r['path'] in sources and sha_bytes(sources[r['path']].read_bytes()) == r['sha256'], 'record schema premise drift')
    finite = 'verification/async-driver-refinement/finite-inventory.json'
    require(finite in sources and sha_bytes(sources[finite].read_bytes()) == spec['finite_inventory_sha256'], 'finite inventory identity')
    primitive_ids, primitives = set(), {}
    require(spec['primitive_bindings'] == sorted(spec['primitive_bindings'], key=lambda r: r['id']), 'primitive binding order')
    for primitive in spec['primitive_bindings']:
        exact(primitive, {'id', 'rule_id', 'source_path', 'source_sha256', 'anchor', 'value'}, 'primitive binding')
        require(primitive['id'] not in primitive_ids and primitive['rule_id'], 'duplicate/unreviewed primitive')
        primitive_ids.add(primitive['id'])
        require(primitive['source_path'] in sources and primitive['source_sha256'] == sha_bytes(sources[primitive['source_path']].read_bytes()), 'primitive source hash')
        require(type(primitive['anchor']) is str and primitive['anchor'] and primitive['anchor'] in sources[primitive['source_path']].read_text(), 'primitive source anchor missing')
        require(len(primitive['value']) <= spec['limits']['maximum_integer_digits'], 'primitive numeric metadata length')
        primitives[primitive['id']] = decimal(primitive['value'], 'primitive value')
    rules = spec['init_class_rules']
    require(type(rules) is list and rules == sorted(rules, key=lambda r: r['id']) and len({r['id'] for r in rules}) == len(rules), 'Init class rules/order')
    for rule in rules:
        exact(rule, {'id', 'rule_id', 'source_path', 'source_sha256', 'anchor', 'operands'}, 'Init-class source rule')
        require(rule['source_path'] in sources and rule['source_sha256'] == sha_bytes(sources[rule['source_path']].read_bytes()) and
                rule['anchor'] and rule['anchor'] in sources[rule['source_path']].read_text(), 'Init class source premise drift')
        require(type(rule['operands']) is list, 'Init class operands')
        for operand in rule['operands']:
            decimal(operand, 'Init-class operand')
    exact(spec['init_class'], {'specification_id', 'specification_sha256', 'predicates'}, 'Init class')
    require(spec['init_class']['specification_sha256'] == sha_bytes(encoded(rules)) and
            spec['init_class']['predicates'] == [r['id'] for r in rules], 'Init class body/predicate binding')
    graph = FormulaGraph(spec['formulas'], primitives, spec['limits'])
    records = spec['records']
    require(type(records) is list and records == sorted(records, key=lambda r: r['record_kind']) and
            len({r['record_kind'] for r in records}) == len(records), 'complete record-kind order')
    for r in records:
        exact(r, {'record_kind', 'encoded_width_formula', 'finite_cardinality_formula', 'premise_ids'}, 'record bound')
        require(r['encoded_width_formula'] in graph.nodes and r['finite_cardinality_formula'] in graph.nodes, 'record formula missing')
        require(r['premise_ids'] and r['premise_ids'] == sorted(set(r['premise_ids'])) and set(r['premise_ids']) <= primitive_ids, 'record premise coverage')
    # Complete kind inventory is read from the pinned schemas, never supplied by
    # an admission caller. SourceBoundReceipt itself is construction evidence.
    expected=set()
    def declared(rule,document):
        if '$ref' in rule:
            reference=rule['$ref']
            require(reference.startswith('#/$defs/'),'top-level inventory ref must use its pinned local definition')
            yield from declared(document['$defs'][reference[8:]],document)
            return
        kind=rule.get('properties',{}).get('kind',{}).get('const')
        if kind is not None:yield kind
        for name in ('oneOf','allOf','anyOf'):
            for child in rule.get(name,[]):yield from declared(child,document)
    for binding in spec['record_schemas']:
        document=strict_json(sources[binding['path']].read_bytes())
        expected.update(declared(document,document))
    mapping=spec['inventory_mapping']
    require(type(mapping) is list and mapping==sorted(mapping,key=lambda r:r['kind']) and
            len({r['kind'] for r in mapping})==len(mapping),'registered semantic/operational mapping order')
    source_kinds=set()
    operational=set()
    charged=set()
    for item in mapping:
        exact(item,{'kind','role','lifetime','source_record_kind','overhead_field'},'source/operational inventory mapping')
        if item['role']=='source_global':
            require(item['lifetime']=='session_unique_global_record' and item['source_record_kind']==item['kind'] and item['overhead_field'] is None,'source-global inventory meaning')
            source_kinds.add(item['kind'])
        elif item['role']=='operational_only':
            require(item['lifetime']=='session_retained_unit_member' and item['source_record_kind'] is None and item['overhead_field'] is None,'operational-only record meaning')
            operational.add(item['kind'])
        elif item['role']=='operational_overhead':
            require(item['source_record_kind'] is None,'operational overhead meaning')
            if item['lifetime']=='peak_workspace':
                require(PHYSICAL_CHARGES.get(item['kind'])==item['overhead_field'],'peak workspace class is not charged to its physical policy pool/family')
                charged.add(item['kind'])
            else:
                require(item['lifetime']=='resident_working_set' and item['overhead_field'] is None,'operational overhead meaning')
        else:raise Invalid('unknown accounting inventory role')
    require(charged==set(PHYSICAL_CHARGES),'physical policy charge inventory is incomplete')
    require(source_kinds|operational==expected and not source_kinds&operational,'missing/extra registered source/control/auxiliary kind')
    require({r['record_kind'] for r in records}==source_kinds,'source theorem missing a semantic kind or inventing Nk for operational artifacts')
    return spec, graph


def validate_initial_class(database, fixture):
    """Check the actual persisted fresh constructor class without rewriting it."""
    import uuid
    exact(fixture,{'jobs','library_id','omitted','initial_projection','initial_inputs'},'raw fresh fixture')
    require(fixture['library_id']==1 and type(fixture['jobs']) is list and len(fixture['jobs'])==2 and
            len(set(fixture['jobs']))==2,'fresh selected library/job inventory differs')
    def uuid4(value):
        require(type(value) is str and len(value)==36,'fresh UUID representation differs')
        try:parsed=uuid.UUID(value)
        except (ValueError,AttributeError):raise Invalid('fresh UUID is invalid') from None
        require(str(parsed)==value and parsed.version==4 and parsed.variant==uuid.RFC_4122,'fresh UUID source class differs')
    for job in fixture['jobs']:uuid4(job)
    for table in ('background_jobs','background_library_requests','background_job_waiters'):
        require(database.execute('SELECT count(*) FROM '+table).fetchone()[0]==2,'fresh selected initial row inventory differs: '+table)
    for table in ('background_job_attempts','background_job_reservations','background_job_domain_leases','background_job_commands','job_leases'):
        require(database.execute('SELECT count(*) FROM '+table).fetchone()[0]==0,'fresh initial table is not empty: '+table)

    settings=list(database.execute('SELECT key,value,updated_at FROM settings ORDER BY key'))
    require(len(settings)==2 and [r[0] for r in settings]==['hdr_format_backfilled_v1','instance.id'] and
            settings[0][1]=='1','fresh startup settings inventory differs')
    uuid4(settings[1][1])
    for _,_,sample in settings:
        require(type(sample) is int and -(2**63)<=sample<2**63,'fixed Init startup unixepoch is not exact signed i64')
    # These fixed startup i64 samples are not Episode clocks or transition
    # horizons. Their exact values remain in every canonical raw snapshot.
    libraries=list(database.execute('SELECT id,name,kind,paths,anime,created_at FROM libraries ORDER BY id'))
    require(len(libraries)==1 and libraries[0][:5]==(1,'async-store-domain','movies','["/async-store-root"]',0),
            'fresh library constructor inventory differs')
    require(type(libraries[0][5]) is int and -(2**63)<=libraries[0][5]<2**63,'fixed library startup timestamp differs')
    require(list(database.execute('SELECT library_id,root_path,domain_id FROM background_storage_domains ORDER BY library_id,root_path'))==
            [(1,'/async-store-root','shared-root')],'fresh storage domain constructor inventory differs')
    return True


def derive(specification, source, *, expected_sha256, root=ROOT):
    spec, graph = validate_specification(specification, root=root, expected_sha256=expected_sha256)
    require(source['finite_inventory_sha256'] == spec['finite_inventory_sha256'], 'receipt source finite inventory differs')
    return {'schema_version': 1, 'kind': 'source_bound_receipt', 'source': source,
            'specification_sha256': expected_sha256, 'finite_inventory_sha256': spec['finite_inventory_sha256'],
            'record_schemas': spec['record_schemas'], 'algorithm_version': 1, 'report_role':'source_global_symbolic',
            'premise_class': 'reviewed_source_lemmas', 'machine_sqlite_rust_proof': False,
            'init_class': spec['init_class'], 'primitive_bindings': spec['primitive_bindings'],
            'formulas': spec['formulas'], 'records': spec['records'],
            'complete_kind_inventory_sha256': sha_bytes(encoded([r['record_kind'] for r in spec['records']])), 'acceptance': False}


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--specification', type=Path, required=True)
    p.add_argument('--specification-sha256', required=True)
    p.add_argument('--source', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    args = p.parse_args(argv)
    value = derive(args.specification, strict_json(args.source.read_bytes()), expected_sha256=args.specification_sha256)
    args.output.write_bytes(encoded(value))
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Invalid as error:
        print('source-bound admission refused: ' + str(error), file=sys.stderr)
        sys.exit(2)
