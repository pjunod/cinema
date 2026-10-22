"""Closed registries and graph import. Importing a graph is inventory, not proof."""
from __future__ import annotations
import hashlib
import json
import re
from collections import Counter, deque
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = Path(__file__).resolve().parent
REPLIES = ('DomainRefused','DomainAcquired','BindRefused','BindAccepted','QueueLoss',
           'DomainLoss','Pressure','WorkFailed','WorkSucceeded','JoinedFailed',
           'JoinedSucceeded','ReleaseReturned','Done')
REQUESTS = {'AcquireDomain','Bind','PollWork','RevokeDomain','CancelWork','JoinWork',
            'DropPublisher','ReleaseRefused','ReleaseRetry','ReleaseNone',
            'ReleaseInterrupted','SettleYield','SettleRetry','RetireQueue','DropAdmission'}
ACTIONS = {'claim': {'job','node'}, 'renew': {'job','token'},
 'settle': {'job','token','disposition'}, 'bind': {'job','token','lease'},
 'complete': {'job','token','lease'}, 'acquire_domain': {'node'},
 'release_domain': {'lease'}, 'remove_owner': {'node'}, 'clock_advance': {'now'}}
RESULTS = {'claimed','refused','cancelled','expired_or_pruned','renewed','accepted',
           'acquired','held','removed','advanced'}
RESULTS_BY_OP = {'claim': {'claimed','refused','cancelled','expired_or_pruned'}, 'renew': {'renewed','refused'},
 'settle': {'accepted','refused'}, 'bind': {'accepted','refused'}, 'complete': {'accepted','refused'},
 'acquire_domain': {'acquired','held'}, 'release_domain': {'accepted','refused'},
 'remove_owner': {'removed'}, 'clock_advance': {'advanced'}}
DRIVER_SOURCES = ('crates/plurx-worker-control/src/library_driver.rs',
 'crates/plurx-worker-control/src/library_lifecycle.rs',
 'crates/plurx-worker-control/examples/library_driver_graph.rs',
 'crates/plurxd/src/library_work.rs','crates/plurxd/src/library_work/env.rs')
CONFIGS = {
 'safety': {'stable':False,'mutation':'none','invariants':['AllStoreSafety','TypeOK','NoTwoSourceReaders','ResultAtomicity',
   'LastRequestAtomicRetirement','ReservationConservation','NoStaleCommit','MonotoneFence','AdmissionUntilJoined','JoinBeforeRelease',
   'NoFictitiousRelease','GuardBeforeRevoke'],'properties':[],'expected':None},
 'cleanup': {'stable':False,'mutation':'none','invariants':['Safety'],
   'properties':['ConditionalAttemptCleanup'],'expected':None},
 'selected-success': {'stable':True,'mutation':'none','invariants':['Safety'],
   'properties':['ConditionalAttemptCleanup','BothSelectedCompletion'],'expected':None},
 'nonvacuity': {'stable':True,'mutation':'none','invariants':['NonvacuityWitness'],
   'properties':[],'expected':'NonvacuityWitness'},
 'control-weak-admission': {'stable':True,'mutation':'weak-admission','invariants':['Safety'],
   'properties':['BothSelectedCompletion'],'expected':'BothSelectedCompletion'},
 'control-no-claim-win': {'stable':True,'mutation':'no-claim-win','invariants':['Safety'],
   'properties':['BothSelectedCompletion'],'expected':'BothSelectedCompletion'},
 'control-withheld-join': {'stable':False,'mutation':'withheld-join','invariants':['Safety'],
   'properties':['ConditionalAttemptCleanup'],'expected':'ConditionalAttemptCleanup'},
 'control-old-poll-policy': {'stable':False,'mutation':'old-poll-policy','invariants':['Safety'],
   'properties':['ConditionalAttemptCleanup'],'expected':'ConditionalAttemptCleanup'},
 'control-local-revocation': {'model':'BoundaryControls','stable':False,'mutation':'local-revocation',
   'invariants':['NoStaleCommit','NoCommitAfterLocalRevocation'],'properties':[],'expected':'NoCommitAfterLocalRevocation'},
 'control-unknown-as-rollback': {'model':'BoundaryControls','stable':False,'mutation':'unknown-as-rollback',
   'invariants':['NoStaleCommit','ResultAtomicity'],'properties':[],'expected':'ResultAtomicity'},
 'control-drop-before-join': {'stable':False,'mutation':'drop-before-join',
   'invariants':['AdmissionUntilJoined'],'properties':[],'expected':'AdmissionUntilJoined'},
}

class Invalid(RuntimeError):
    pass

def require(ok, message):
    if not ok:
        raise Invalid(message)

def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()

def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',',':'), ensure_ascii=False)

def read_json(path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, f'duplicate JSON key: {key}')
            result[key] = value
        return result
    return json.loads(Path(path).read_text(), object_pairs_hook=unique)

def integer(value, low=None, high=None):
    return type(value) is int and (low is None or value >= low) and (high is None or value <= high)

def exact_keys(value, keys, context):
    require(isinstance(value,dict) and set(value) == set(keys), f'{context}: exact fields differ')

def graph_structure(graph, cap):
    require(graph.get('schema_version') == 1 and graph.get('initial') == 0, 'unsupported graph schema/initial')
    states, edges = graph.get('states'), graph.get('edges')
    require(isinstance(states,list) and 0 < len(states) <= cap, 'state inventory empty or cap exceeded')
    require([s.get('id') for s in states] == list(range(len(states))), 'state IDs not exact contiguous inventory')
    require(isinstance(edges,list) and edges, 'empty edge inventory')
    seen = set()
    adjacency = [[] for _ in states]
    for e in edges:
        require(integer(e.get('from'),0,len(states)-1) and integer(e.get('to'),0,len(states)-1), 'missing/outside edge target')
        key = canonical(e)
        require(key not in seen, 'duplicate graph edge')
        seen.add(key)
        adjacency[e['from']].append(e['to'])
    reachable, todo = {0}, deque([0])
    while todo:
        for target in adjacency[todo.popleft()]:
            if target not in reachable:
                reachable.add(target)
                todo.append(target)
    require(len(reachable) == len(states), 'unreachable declared graph state')
    return adjacency

def validate_driver(g):
    exact_keys(g, {'schema_version','scope','initial','domain','states','edges','rejected'}, 'driver root')
    adj = graph_structure(g, 1000)
    require(g['domain'].get('reply_classes') == list(REPLIES), 'unknown/missing driver reply registry')
    seen = set()
    for s in g['states']:
        exact_keys(s, {'id','phase','held','work','revoked','cancelled','pending','remaining','terminal'}, 'driver state')
        exact_keys(s['held'], {'admission','queue','domain','publisher'}, 'driver held')
        require(all(type(v) is bool for v in s['held'].values()), 'nonboolean held field')
        require(s['pending'] is None or s['pending'] in REQUESTS, 'unknown pending request')
        require(isinstance(s['remaining'],list) and all(x in REQUESTS for x in s['remaining']), 'unknown effect tail')
        require(type(s['terminal']) is bool and s['terminal'] == (s['pending'] is None), 'terminal inventory mismatch')
    for e in g['edges']:
        exact_keys(e, {'from','to','reply','request'}, 'driver edge')
        require(e['reply'] in REPLIES and e['request'] == g['states'][e['to']]['pending'], 'unknown/inconsistent driver result')
        pair = (e['from'],e['reply'])
        require(pair not in seen, 'duplicate driver input')
        seen.add(pair)
    for r in g['rejected']:
        exact_keys(r, {'state','reply','unchanged'}, 'driver rejection')
        require(integer(r['state'],0,len(g['states'])-1) and r['reply'] in REPLIES and r['unchanged'] is True,
                'invalid rejection or mutated rejected input')
        pair = (r['state'],r['reply'])
        require(pair not in seen, 'accepted/rejected reply overlap')
        seen.add(pair)
    require(seen == {(s['id'],r) for s in g['states'] for r in REPLIES}, 'missing driver input')
    # Every accepted transition strictly descends this rank. Effect tails alone
    # do not descend across phase changes; this rank is over the actual DAG.
    indegree = [0]*len(adj)
    for targets in adj:
        for t in targets: indegree[t] += 1
    queue = deque(i for i,d in enumerate(indegree) if d == 0)
    order = []
    while queue:
        n = queue.popleft(); order.append(n)
        for t in adj[n]:
            indegree[t] -= 1
            if indegree[t] == 0: queue.append(t)
    require(len(order) == len(adj), 'actual driver graph has an internal cycle; no DAG rank')
    rank = [0]*len(adj)
    for n in reversed(order): rank[n] = max((rank[t]+1 for t in adj[n]),default=0)
    return rank

def expected_actions(s):
    """Enumerate declared D inputs only; never predict a SQL result/guard."""
    out = []
    def add(op, **kw): out.append(canonical({'op':op,**kw}))
    for j,tokens in enumerate(s['captured_tokens']):
        if s['budgets']['claims'][j] < 2:
            for n in range(2): add('claim',job=j,node=n)
        for i,t in enumerate(tokens):
            if any(later['claim_id'] == t['claim_id'] for later in tokens[i+1:]): continue
            if t['claim_id'] not in s['budgets']['renewed_claims']: add('renew',job=j,token=i)
            for d in range(3): add('settle',job=j,token=i,disposition=d)
            for k,l in enumerate(s['captured_leases']):
                if l['owner_node_id'] == t['node_id']:
                    add('bind',job=j,token=i,lease=k); add('complete',job=j,token=i,lease=k)
    for n,owner in enumerate(('node-a','node-b','foreign-holder')):
        owns = n == 2 or any(j['state'] == 'running' and j['owner'] == owner for j in s['jobs'])
        if owns and not s['budgets']['domain_acquired'][n]: add('acquire_domain',node=n)
    for i in range(len(s['captured_leases'])): add('release_domain',lease=i)
    for n in range(2):
        if not s['budgets']['removed'][n]: add('remove_owner',node=n)
    boundaries = [j['expiry'] for j in s['jobs'] if j['expiry'] is not None]
    boundaries += [t['lease_expires_ms'] for ts in s['captured_tokens'] for t in ts]
    boundaries += [l['expires_at_unix_ms'] for l in s['captured_leases']]
    if s['domain'] is not None: boundaries.append(s['domain']['expiry'])
    later = [t+d for t in boundaries for d in (-1,0,1) if -(2**63) <= t+d < 2**63 and t+d > s['now']]
    if later: add('clock_advance',now=min(later))
    return set(out)

def validate_store(g, verify_sources=True):
    exact_keys(g, {'schema_version','scope','initial','source_bindings','schema_bindings','domain','coverage','states','edges'}, 'Store root')
    graph_structure(g, 20000)
    domain = g['domain']
    require(domain.get('numeric_quotient') == 'none: every i64/u64 and captured token remains exact; opaque job UUIDs alone are renamed', 'numeric quotient is not proved')
    require(domain.get('cap') == 20000, 'Store cap differs from declared D')
    require(domain.get('jobs') == 2 and domain.get('nodes') == ['node-a','node-b'], 'requires exact two-job/two-node D')
    require(domain.get('attempts_per_job') == 2 and domain.get('renewals_per_attempt') == 1, 'attempt/renewal domain drift')
    require(domain.get('rearrival') in (0,False,'none'), 'finite episode does not permit demand re-arrival')
    require(domain.get('physical_capacity_per_node') == 1 and domain.get('domain_acquisitions_per_owner') == 1,
            'physical/domain acquisition budget drift')
    require(domain.get('domain_owners') == ['node-a','node-b','foreign-holder'] and
      domain.get('selected_attempt_ids') == 4 and domain.get('reserved_boundary_ids') == 2, 'identity-domain drift')
    require(domain.get('settlement_dispositions') == ['yield','retry','cancel'], 'settlement input domain drift')
    require(domain.get('ordinary_token_inputs') == 'latest captured revision per claim; queue-token Mutex adapter contract; historical revisions remain captured and conformance-tested', 'ordinary token input-domain drift')
    require(domain.get('binding_completion_inputs') == 'captured domain lease owner equals captured queue-token node; wrong-owner inputs are separate conformance', 'lease-owner input-domain drift')
    require(domain.get('node_domain_acquisition_inputs') == 'node has selected running durable queue ownership, including expired retained ownership for dispatch/application races; foreign holder is independent', 'node-domain input-domain drift')
    bindings = g['source_bindings']
    expected_sources = {'store/'+p for p in ('background_jobs.rs','background_jobs_domain.rs',
      'background_jobs_library.rs','background_jobs_schema.sql','background_jobs_resources.sql',
      'background_jobs_library.sql','background_jobs_domain.sql','subtitle_managed_schema.sql',
      'sqlite/mod.rs','sqlite/background_jobs.rs','sqlite/coordination.rs','async_driver_refinement.rs')}
    expected_sources.add('cluster/coordination.rs')
    require(isinstance(bindings,dict) and set(bindings) == expected_sources, 'SQL/schema/harness source registry differs')
    for path, digest in bindings.items():
        require(isinstance(path,str) and not Path(path).is_absolute() and '..' not in Path(path).parts,
                'unsafe source binding path')
        require(re.fullmatch('[0-9a-f]{64}',digest) is not None, 'bad source digest')
        require(not verify_sources or sha(ROOT/'crates/plurx-core/src'/path) == digest, f'Store source hash mismatch: {path}')
    schema = g['schema_bindings']
    exact_keys(schema, {'compiled_sqlite_migrations','installed_sqlite_schema_sha256','schema_inventory'}, 'schema bindings')
    migrations = schema['compiled_sqlite_migrations']
    require(isinstance(migrations,list) and migrations, 'missing compiled migration inventory')
    for i,m in enumerate(migrations):
        exact_keys(m, {'version','sha256'}, 'compiled migration')
        require(m['version'] == i+1 and re.fullmatch('[0-9a-f]{64}',m['sha256']), 'bad migration hash inventory')
    require(isinstance(schema['schema_inventory'],list) and schema['schema_inventory'], 'empty installed schema')
    # SQLite json_group_array/json_array and compact JSON use the same scalar
    # encoding for this schema inventory (all fields UTF8 SQL text or null).
    require(hashlib.sha256(canonical(schema['schema_inventory']).encode()).hexdigest() == schema['installed_sqlite_schema_sha256'],
            'installed schema inventory hash mismatch')
    counts, results, cutoffs = Counter(), Counter(), Counter()
    outgoing = [set() for _ in g['states']]
    hashes = set()
    for s in g['states']:
        exact_keys(s, {'id','now','jobs','domain','captured_tokens','captured_leases','budgets','cutoffs','canonical_sha256'}, 'Store state')
        require(s['canonical_sha256'] not in hashes, 'duplicate canonical state identity')
        hashes.add(s['canonical_sha256'])
        require(integer(s.get('now'),-(2**63),2**63-1), 'non-i64 concrete time')
        require(re.fullmatch('[0-9a-f]{64}',s.get('canonical_sha256','')) is not None, 'missing canonical state hash')
        require(isinstance(s.get('jobs'),list) and len(s['jobs']) == 2, 'two raw job projections required')
        require(isinstance(s.get('captured_tokens'),list) and len(s['captured_tokens']) == 2, 'missing captured token inventory')
        require(isinstance(s.get('captured_leases'),list), 'missing captured lease inventory')
        require(isinstance(s.get('cutoffs'),list), 'missing visible input cutoffs')
        for cutoff in s['cutoffs']:
            require(re.fullmatch(r'attempt_budget_j[01]|renewal_budget_.+|domain_acquire_budget|no_future_boundary',cutoff) is not None,
                    f'unknown cutoff class: {cutoff}')
            cutoffs[cutoff] += 1
        exact_keys(s['budgets'], {'claims','renewed_claims','domain_acquired','removed'}, 'episode budgets')
        require(len(s['budgets']['claims']) == 2 and all(integer(x,0,2) for x in s['budgets']['claims']), 'invalid claim budgets')
        require(len(s['budgets']['domain_acquired']) == 3 and all(type(x) is bool for x in s['budgets']['domain_acquired']), 'invalid domain acquisition flags')
        require(len(s['budgets']['removed']) == 2 and all(type(x) is bool for x in s['budgets']['removed']), 'invalid removal flags')
        if s['domain'] is not None: exact_keys(s['domain'], {'resource','owner','fence','revision','expiry'}, 'domain row')
        for j, job in enumerate(s['jobs']):
            exact_keys(job, {'state','owner','fence','revision','expiry','failed_attempts','active','request_done','request_state','reservation_count'}, 'raw job projection')
            require(job.get('state') in {'queued','running','succeeded','failed','cancelled'}, 'unknown job state')
            require(job.get('request_state') in {'pending','succeeded','failed','cancelled'}, 'unknown request state')
            require(type(job.get('active')) is bool and type(job.get('request_done')) is bool, 'missing raw activity/request flags')
            require(job.get('owner') in {None,'node-a','node-b'}, 'unknown queue owner')
            require(integer(job.get('reservation_count'),0,6), 'invalid raw reservation count')
            for token in s['captured_tokens'][j]:
                exact_keys(token, {'job_id','node_id','boot_id','claim_id','fence','revision','lease_expires_ms'}, 'captured JobToken')
                for f in ('fence','revision','lease_expires_ms'):
                    require(integer(token[f],-(2**63),2**63-1), 'non-i64 token payload')
        for lease in s['captured_leases']:
            require(lease.get('owner_node_id') in {'node-a','node-b','foreign-holder'}, 'unknown opaque lease owner')
            for field in ('fence','revision'): require(integer(lease.get(field),0,2**64-1), 'invalid u64 domain field')
            require(integer(lease.get('expires_at_unix_ms'),-(2**63),2**63-1), 'invalid i64 domain expiry')
            exact_keys(lease, {'resource','owner_node_id','fence','revision','expires_at_unix_ms'}, 'captured lease')
    for e in g['edges']:
        exact_keys(e, {'from','to','action','result'}, 'Store edge')
        a = e['action']; op = a.get('op')
        encoded = canonical(a)
        require(encoded not in outgoing[e['from']], 'duplicate/nondeterministic Store input')
        outgoing[e['from']].add(encoded)
        require(op in ACTIONS, f'unknown Store action: {op}')
        exact_keys(a, ACTIONS[op] | {'op'}, f'Store action {op}')
        require(e['result'] in RESULTS_BY_OP[op], f'unknown result for {op}: {e["result"]}')
        counts[op] += 1; results[e['result']] += 1
        before, after = g['states'][e['from']], g['states'][e['to']]
        require(e['result'] not in {'refused','cancelled','expired_or_pruned','held'} or e['from'] == e['to'],
                'FailedCasAtomicity: refused operation changed canonical state')
        for field in ACTIONS[op]:
            require(integer(a[field]), f'noninteger Store action {field}')
        if 'job' in a: require(a['job'] in (0,1), 'invalid job index')
        if 'node' in a: require(a['node'] in ((0,1,2) if op == 'acquire_domain' else (0,1)), 'invalid node index')
        if 'token' in a: require(0 <= a['token'] < len(before['captured_tokens'][a['job']]), 'missing token target')
        if 'lease' in a: require(0 <= a['lease'] < len(before['captured_leases']), 'missing lease target')
        if 'disposition' in a: require(a['disposition'] in range(3), 'unknown disposition')
        for j in range(2):
            old, new = before['captured_tokens'][j], after['captured_tokens'][j]
            require(new[:len(old)] == old, 'captured token identity is not append-only')
        require(after['captured_leases'][:len(before['captured_leases'])] == before['captured_leases'],
                'captured lease identity is not append-only')
    for s,actual in zip(g['states'],outgoing):
        require(actual == expected_actions(s), f'Store input-domain closure incomplete at state {s["id"]}')
    coverage = g['coverage']
    exact_keys(coverage, {'closed','no_skipped_edges','state_count','edge_count','by_op','by_result','cutoff_counts'}, 'coverage')
    require(coverage['closed'] is True and coverage['no_skipped_edges'] is True, 'SQL closure incomplete/skipped')
    require(coverage['state_count'] == len(g['states']) and coverage['edge_count'] == len(g['edges']), 'coverage count mismatch')
    require(coverage['by_op'] == dict(counts) and set(counts) == set(ACTIONS), 'operation coverage incomplete')
    require(coverage['by_result'] == dict(results) and set(results) <= RESULTS, 'result coverage incomplete')
    require(coverage['cutoff_counts'] == dict(cutoffs), 'cutoff coverage mismatch')
    require({'attempt_budget_j0','attempt_budget_j1','domain_acquire_budget','no_future_boundary'} <= set(cutoffs),
            'required visible cutoff class not reached')
    return {'states':len(g['states']),'edges':len(g['edges']),'coverage':coverage,'domain':domain,
      'unobserved_known_results':sorted(RESULTS-set(results))}

def rejection_controls(driver,store):
    import copy
    mutations = []
    broken = copy.deepcopy(driver); broken['edges'][0]['to'] = len(driver['states'])
    mutations.append(('missing-driver-target',lambda: validate_driver(broken),'missing/outside edge target'))
    bad_store = copy.deepcopy(store); first = next(iter(bad_store['source_bindings']))
    bad_store['source_bindings'][first] = '0'*64
    mutations.append(('source-hash-drift',lambda: validate_store(bad_store),'Store source hash mismatch'))
    missing = copy.deepcopy(store)
    index = next(i for i,e in enumerate(missing['edges']) if e['from'] == e['to'])
    missing['edges'].pop(index)
    mutations.append(('missing-store-input',lambda: validate_store(missing),'Store input-domain closure incomplete'))
    outcomes = {}
    for name,action,expected in mutations:
        try: action()
        except Invalid as exc:
            require(expected in str(exc),f'control {name} failed for unrelated reason: {exc}')
            outcomes[name] = {'result_class':'strict_rejection_control','named_failure':expected}
        else: raise Invalid(f'control {name} did not reject')
    return outcomes

def tla(value):
    if value is None: return '""'
    if type(value) is bool: return 'TRUE' if value else 'FALSE'
    if type(value) is int: return str(value) if value >= 0 else f'({value})'
    if isinstance(value,str): return json.dumps(value,ensure_ascii=True)
    if isinstance(value,list): return '<<' + ','.join(tla(v) for v in value) + '>>'
    if isinstance(value,dict): return '[' + ','.join(f'{k} |-> {tla(v)}' for k,v in value.items()) + ']'
    raise Invalid('unrepresentable TLA value')

def edge_fresh(e, store):
    a = e['action']; op = a['op']
    if op not in {'renew','settle','bind','complete'} or e['result'] not in {'accepted','renewed'}:
        return True
    s = store['states'][e['from']]; j = s['jobs'][a['job']]
    t = s['captured_tokens'][a['job']][a['token']]
    owner = {'node-a':0,'node-b':1}.get(t['node_id'])
    fresh = (j['state'] == 'running' and j['owner'] == t['node_id']
      and j['fence'] == t['fence'] and j['revision'] == t['revision']
      and j['expiry'] == t['lease_expires_ms'] and t['lease_expires_ms'] > s['now']
      and owner is not None and not s['budgets']['removed'][owner])
    if op in {'bind','complete'}:
        l = s['captured_leases'][a['lease']]; d = s['domain']
        fresh = fresh and d is not None and d['resource'] == l['resource'] and d['owner'] == l['owner_node_id']
        fresh = fresh and d['fence'] == l['fence'] and d['revision'] == l['revision']
        fresh = fresh and d['expiry'] == l['expires_at_unix_ms'] and d['expiry'] > s['now']
        fresh = fresh and l['owner_node_id'] == t['node_id']
    return fresh

def edge_monotone(e,store):
    if e['action']['op'] != 'claim' or e['result'] != 'claimed': return True
    j = e['action']['job']
    return store['states'][e['to']]['jobs'][j]['fence'] > store['states'][e['from']]['jobs'][j]['fence']

def tables(driver,store):
    ranks = validate_driver(driver)
    ds = []
    for s,r in zip(driver['states'],ranks):
        ds.append({'pending':s['pending'],'terminal':s['terminal'],'work':s['work'],
          **s['held'],'revoked':s['revoked'],'cancelled':s['cancelled'],'rank':r})
    de = [{'src':e['from'],'dst':e['to'],'reply':e['reply']} for e in driver['edges']]
    ss = []
    owners = {None:-1,'node-a':0,'node-b':1,'foreign':2,'foreign-holder':2}
    for s in store['states']:
        ss.append({'jobs':[{'state':j['state'],'active':j['active'],'done':j['request_done'],
            'owner':owners[j['owner']],'reservations':j['reservation_count']} for j in s['jobs']],
           'tokens':s['captured_tokens'],
           'leases':[{'owner':owners[l['owner_node_id']]} for l in s['captured_leases']]})
    se = [{'fresh':edge_fresh(e,store), 'monotone':edge_monotone(e,store), 'src':e['from'],'dst':e['to'],'op':e['action']['op'],'result':e['result'],
       **{key:e['action'].get(key,-1) for key in ('job','node','token','lease','disposition')}} for e in store['edges']]
    # Tokens' concrete payloads are opaque to the product. Length and identity
    # are checked above, and SQL membership comes solely from the source graph.
    for output,original in zip(ss,store['states']):
        output['tokens'] = [[t['claim_id'] for t in tokens] for tokens in original['captured_tokens']]
    claim_ids = sorted({t['claim_id'] for state in store['states'] for tokens in state['captured_tokens'] for t in tokens})
    return ('--------------------------- MODULE SourceTables ---------------------------\n'
      'EXTENDS Integers, Sequences\nDriverInitial == 0\nStoreInitial == 0\n'
      + 'DriverStates == '+tla(ds)+'\nDriverEdges == {'+','.join(tla(e) for e in de)+'}\n'
      + 'StoreStates == '+tla(ss)+'\nStoreEdges == {'+','.join(tla(e) for e in se)+'}\n'
      + 'ClaimIds == {'+','.join(tla(a) for a in claim_ids)+'}\n'
      + '=============================================================================\n')

def config(name):
    c = CONFIGS[name]
    out = ['SPECIFICATION Spec','CHECK_DEADLOCK FALSE','CONSTANTS',' Mutation = '+tla(c['mutation'])]
    if c.get('model') != 'BoundaryControls': out += [' StableSuccess = '+tla(c['stable']), ' Witness = FALSE']
    if c['invariants']: out += ['INVARIANTS'] + [' '+x for x in c['invariants']]
    if c['properties']: out += ['PROPERTIES'] + [' '+x for x in c['properties']]
    return '\n'.join(out)+'\n'

def verdict(output, code, expected):
    require(not re.search(r'(?i)(outofmemory|java heap|parse error|semantic error|cannot find|stack overflow)',output),
            'tool/compiler/resource failure is not semantic evidence')
    generated = re.search(r'([\d,]+) states generated, ([\d,]+) distinct states found, ([\d,]+) states left on queue',output)
    require(generated is not None, 'missing per-configuration state inventory')
    counts = [int(v.replace(',','')) for v in generated.groups()]
    require(counts[0] > 0 and counts[1] > 0, 'zero-state run')
    safety = re.findall(r'Invariant ([A-Za-z][A-Za-z0-9_]*) is violated\.',output)
    temporal = 'Temporal properties were violated.' in output
    if expected:
        require(code in (12,13) and ('State 1:' in output or 'State 1: <' in output), 'missing semantic counterexample/exit')
        require(safety == [expected] or (not safety and temporal and 'Back to state' in output), 'wrong named counterexample')
        return {'result_class':'reachability_witness' if expected == 'NonvacuityWitness' else 'semantic_control',
                'expected_property':expected,'states':counts,'complete':False,'counterexample':True}
    require(code == 0 and not safety and not temporal and counts[2] == 0, 'incomplete/failed positive TLC run')
    require('Model checking completed. No error has been found.' in output, 'missing explicit TLC success')
    return {'result_class':'conditional_finite_model_theorem','states':counts,'complete':True,'counterexample':False}
