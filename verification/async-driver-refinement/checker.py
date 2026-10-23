"""Closed registries and graph import. Importing a graph is inventory, not proof."""
from __future__ import annotations
import copy
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
WORK_CLASSES = {
 'Idle':'idle','Running':'running',
 'Completed { ok: false }':'completed_failed','Completed { ok: true }':'completed_succeeded',
 'Joined { ok: false }':'joined_failed','Joined { ok: true }':'joined_succeeded',
}

REQUESTS = {'AcquireDomain','Bind','PollWork','RevokeDomain','CancelWork','JoinWork',
            'DropPublisher','ReleaseRefused','ReleaseRetry','ReleaseNone',
            'ReleaseInterrupted','SettleYield','SettleRetry','RetireQueue','DropAdmission'}
ACTIONS = {'claim': {'job','node'}, 'renew': {'job','token'},
 'settle': {'job','token','disposition'}, 'bind': {'job','token','lease'},
 'complete': {'job','token','lease'}, 'acquire_domain': {'node'},
 'release_domain': {'lease'}, 'remove_owner': {'node'}, 'clock_advance': {'now'},
 'resolve': {'job','token'}}
RESULTS = {'claimed','refused','cancelled','expired_or_pruned','renewed','accepted',
           'acquired','held','removed','advanced','running','cancel_requested','settled','lost_ownership'}
RESULTS_BY_OP = {'claim': {'claimed','refused','cancelled','expired_or_pruned'}, 'renew': {'renewed','refused'},
 'settle': {'accepted','refused'}, 'bind': {'accepted','refused'}, 'complete': {'accepted','refused'},
 'acquire_domain': {'acquired','held'}, 'release_domain': {'accepted','refused'},
 'remove_owner': {'removed'}, 'clock_advance': {'advanced'},
 'resolve': {'running','cancel_requested','settled','lost_ownership','expired_or_pruned'}}
STORE_BOUNDARIES = {'retirement_inputs': 'disposition 3 uses Some(worker_retired), None checkpoint and not_before_ms=current '
                      'episode.now for every captured original token; ordinary dispositions 0/1/2 retain '
                      'latest-revision inputs',
 'resolution_read_boundary': 'resolve_claim is one logical actual-API pure-read action; its two '
                             'authoritative QueueSql awaits and internal interleavings are a named '
                             'conformance boundary, not closed read-interleaving coverage',
 'primitive_clock_boundary': "each generated primitive request uses current episode.now; the daemon's three "
                             'retirement wall samples and dispatch/application races are reviewed '
                             'adapter/conformance boundaries, not source-derived clock-race coverage',
 'reservation_conservation_boundary': 'conservation is checked for unexpired queue authority only; expired '
                                      'Running rows and residue remain represented, with no independent '
                                      'complete-residue conservation guarantee until reclaim/prune',
 'domain_reacquisition_boundary': 'each owner can acquire at most once; release or expiry does not reset its '
                                  'budget, so same-owner release-to-reacquire cycles are excluded',
 'foreign_holder_boundary': 'foreign-holder is a third adversarial domain-lease owner, not either worker '
                            'node, with one independent acquisition budget',
 'retirement_sequence_boundary': 'the graph supplies separate actual settle and logical resolve primitives; '
                                 'daemon drain/local-token/first-settle/resolve/optional-second-settle '
                                 'sequencing is a reviewed source and daemon-conformance boundary composed '
                                 'by the temporal model',
 'cleanup_boundary': 'local cleanup needs bounded retirement calls to return, not eventual successful '
                     'acknowledgement; errors are logged and finish returns while durable authority may '
                     'remain',
 'dispatch_time_noninterference': 'for retirement fence Some(F), resolve_claim reads dispatched_at_ms only '
                                  'in its skipped fence-is-none window branch; neither SQL query nor later '
                                  'classification reads it, so a representative dispatch sample preserves '
                                  'results at fixed now/read outcomes; this is a reviewed source lemma, not '
                                  'a machine proof',
 'queue_sql_boundary': 'all generated queue primitives use QueueSql::queue_sql, not queue_transaction; '
                       'ReplyBoundary passes queue_transaction through without fault injection, so '
                       'multi-statement transaction faults are outside this slice',
 'schema_snapshot_boundary': 'canonical durable rows cover exactly the nine snapshot_tables; all installed '
                             'schema objects are hash-bound for identity, not complete row snapshots or '
                             'proofs of unrelated tables',
 'savepoint_boundary': 'no closure transition invokes queue_transaction or nested explicit transactions; '
                       'test savepoints bracket sequential primitives on the production writer and any '
                       'savepoint/SQL failure aborts closure',
 'refusal_observation': 'the extracted oracle retains the original full-canonical refused-edge equality guard before memoization; logical resolve retains durable read-only observation; no pre-existing closure gap is claimed',
 'OrdinarySettlementPayloadTransfer': 'unclosed: ordinary Yield/Retry use declared representative payloads (Yield error_code=None and checkpoint=None; Retry error_code=transient; not_before_ms=application now); production capture-time deadlines, retry delay and failed-attempt snapshot, error text, local dispatch/loss/deadline guards and future claim eligibility correspondence are not closed; no payload noninterference lemma or true transfer assumption is claimed'}
DRIVER_SOURCES = ('crates/plurx-worker-control/src/library_driver.rs',
 'crates/plurx-worker-control/src/library_lifecycle.rs',
 'crates/plurx-worker-control/examples/library_driver_graph.rs',
 'crates/plurxd/src/library_work.rs','crates/plurxd/src/library_work/env.rs')
CONFIGS = {
 'safety': {'stable':False,'mutation':'none','invariants':['ImportedOracleSafety','TypeOK','NoTwoSourceReaders','ResultAtomicity',
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

COVERAGE_INVARIANTS = ('DemandCovered','CapacityAgrees','CertifiedReachable',
 'CertificateNextAgreement','FairPairAgreement','FairEnablednessAgreement')
for _cfg in CONFIGS.values():
    if _cfg.get('model') != 'BoundaryControls':
        _cfg['invariants'] = list(COVERAGE_INVARIANTS) + _cfg['invariants']

# Wire vocabulary is shared; semantic enumeration below is independently authored.
IDLE = dict(stage='idle',job=-1,attempt='',token=-1,lease=-1,ctrl=-1,call='ready',reply='',
    accepted=False,localToken=False,retire='off',inner='ready',original=-1,resolved=-1,
    outcome='',sample=-1,firstSample=-1,resolveSample=-1,afterSample=-1,started=False,
    dropped=False,joined=False,workStarted=False,faultUsed=False,admission=False,readLock=False)
CONTEXTS = {
 'base':(False,'none'), 'stable':(True,'none'), 'weak_admission':(True,'weak-admission'),
 'no_claim_win':(True,'no-claim-win'), 'withheld_join':(False,'withheld-join'),
 'old_poll_policy':(False,'old-poll-policy'), 'drop_before_join':(False,'drop-before-join')}
BOUNDARY_CONTEXTS = {'boundary_local_revocation':'local-revocation',
                     'boundary_unknown_rollback':'unknown-as-rollback'}
CONFIG_CONTEXT = {'safety':'base','cleanup':'base','selected-success':'stable','nonvacuity':'stable',
 'control-weak-admission':'weak_admission','control-no-claim-win':'no_claim_win',
 'control-withheld-join':'withheld_join','control-old-poll-policy':'old_poll_policy',
 'control-drop-before-join':'drop_before_join','control-local-revocation':'boundary_local_revocation',
 'control-unknown-as-rollback':'boundary_unknown_rollback'}
FAIR_FAMILIES = ('ClaimSend','ClaimApply','ClaimReply','ClaimReturn','Dispatch','StoreApply',
 'SimpleApply','Reply','DriverReturn','RetireLocal','RetireCallReturn','ExternalReturn',
 'JoinReturn','RetireDrain','MakeAvailable','Admission','ClaimWin')
RETIRE_LOCAL = {'RetireBegin','RetireTake','RetireSample','RetireSend','RetireReply','RetireObserve','RetireFinish'}
SQL_PENDING = {'AcquireDomain','Bind','SettleYield','SettleRetry','ReleaseRefused',
               'ReleaseRetry','ReleaseNone','ReleaseInterrupted'}
RELEASE_PENDING = {'ReleaseRefused','ReleaseRetry','ReleaseNone','ReleaseInterrupted'}
BRANCH_CHOICES = {name:set() for name in ('ClaimSend','ClaimApply','ClaimReturn','Dispatch','StoreApply',
 'AmbiguousWithoutApply','SimpleApply','WorkSuccess','DriverReturn','Recycle','RetireBegin',
 'RetireDrain','RetireTake','RetireSend','RetireApply','RetireError','RetireObserve','RetireFinish',
 'MakeAvailable','RemoveAvailability')}
BRANCH_CHOICES.update(Admission={'job'},ClaimReply={'lost'},Reply={'lost'},RetireReply={'lost'},
                     WorkOther={'reply'},JoinReturn={'reply'},RetireSample={'clock_error'},Environment={'input','ack'})
SEMANTIC_PATHS = {'verification/async-driver-refinement/'+name for name in
 ('checker.py','product-schema.json','AsyncDriverRefinement.tla','BoundaryControls.tla')}
SEMANTIC_PATHS.update('verification/async-driver-refinement/'+name+'.cfg' for name in CONFIGS)
SNAPSHOT_TABLES = {'background_jobs','background_job_waiters','background_job_attempts',
 'background_job_reservations','background_job_domain_leases','background_library_requests',
 'background_job_commands','job_leases','settings'}

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

def driver_work_class(value):
    require(isinstance(value,str) and value in WORK_CLASSES,
            'unknown driver Work Debug class; exporter/importer contract must be reviewed')
    return WORK_CLASSES[value]

def validate_driver(g):
    exact_keys(g, {'schema_version','scope','initial','domain','states','edges','rejected'}, 'driver root')
    adj = graph_structure(g, 1000)
    require(g['domain'].get('reply_classes') == list(REPLIES), 'unknown/missing driver reply registry')
    seen = set()
    for s in g['states']:
        exact_keys(s, {'id','phase','held','work','revoked','cancelled','pending','remaining','terminal'}, 'driver state')
        exact_keys(s['held'], {'admission','queue','domain','publisher'}, 'driver held')
        require(all(type(v) is bool for v in s['held'].values()), 'nonboolean held field')
        driver_work_class(s['work'])
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
            add('settle',job=j,token=i,disposition=3)
            add('resolve',job=j,token=i)
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

def validate_job_projection(s):
    """Independent arithmetic/atomicity checks of exported durable fields."""
    for job in s['jobs']:
        expiry = job['expiry']
        require(expiry is None or integer(expiry,-(2**63),2**63-1), 'non-i64 queue expiry')
        active = job['state'] == 'running' and expiry is not None and expiry > s['now']
        require(job['active'] is active, 'activity projection disagrees with concrete queue expiry')
        require(not job['request_done'] or
                (job['request_state'] == 'succeeded' and job['state'] == 'succeeded' and
                 job['owner'] is None and job['reservation_count'] == 0),
                'LastRequestAtomicRetirement: malformed completed request projection')
        require(job['owner'] is not None or job['reservation_count'] == 0,
                'ReservationConservation: ownerless reservation projection')
        require(not active or job['reservation_count'] == 3,
                'ReservationConservation: unexpired owner lacks all three reservations')
    require(sum(job['active'] for job in s['jobs']) <= 1, 'NoTwoSourceReaders: multiple unexpired authorities')

def validate_resolution(e, before, after):
    """Validate returned identity and read-only projections, not resolve guards."""
    if e['action']['op'] != 'resolve':
        require(e['resolved_token'] is None, 'non-resolve returned a resolution token')
        return
    for field in ('now','jobs','domain','captured_leases','budgets'):
        require(before[field] == after[field], f'resolve changed read-only projection: {field}')
    j = e['action']['job']
    returned = e['resolved_token']
    if e['result'] in {'running','cancel_requested'}:
        require(integer(returned,0,len(after['captured_tokens'][j])-1), 'missing resolved token index')
        original = before['captured_tokens'][j][e['action']['token']]
        token = after['captured_tokens'][j][returned]
        require(all(original[k] == token[k] for k in ('job_id','node_id','boot_id','claim_id','fence')),
                'resolve returned another immutable attempt identity')
        for k in range(2):
            old, new = before['captured_tokens'][k], after['captured_tokens'][k]
            expected = old + ([token] if k == j and token not in old else [])
            require(new == expected, 'resolve appended an unrelated token')
    else:
        require(returned is None, 'non-running resolution returned a token')
        require(before['captured_tokens'] == after['captured_tokens'], 'non-running resolve captured a token')

def validate_raw_store(s):
    require(isinstance(s['canonical_json'],str), 'missing exact canonical JSON bytes')
    require(hashlib.sha256(s['canonical_json'].encode()).hexdigest() == s['canonical_sha256'],
            'canonical Store preimage hash mismatch')
    def unique(items):
        out = {}
        for key,value in items:
            require(key not in out, 'duplicate canonical JSON key')
            out[key] = value
        return out
    parsed = json.loads(s['canonical_json'],object_pairs_hook=unique)
    require(parsed == s['raw'], 'raw Store view disagrees with canonical preimage')
    exact_keys(parsed, {'rows','episode'}, 'Store canonical payload')
    exact_keys(parsed['rows'], SNAPSHOT_TABLES, 'nine-table snapshot')
    require(all(isinstance(rows,list) and all(isinstance(row,list) for row in rows)
                for rows in parsed['rows'].values()), 'malformed full-row snapshot')
    ep = parsed['episode']
    exact_keys(ep, {'now','claims','tokens','renewed_claims','leases','domain_acquired','removed'}, 'canonical episode')
    require(ep == {'now':s['now'],'claims':s['budgets']['claims'],'tokens':s['captured_tokens'],
        'renewed_claims':s['budgets']['renewed_claims'],'leases':s['captured_leases'],
        'domain_acquired':s['budgets']['domain_acquired'],'removed':s['budgets']['removed']},
        'canonical episode/projection disagreement')
    expiries = {t['lease_expires_ms'] for ts in s['captured_tokens'] for t in ts}
    require(all(j['expiry'] is None or j['expiry'] in expiries for j in s['jobs']),
            'durable expiry/capture boundary disagreement')


def product_payload(p):
    return {key:p[key] for key in ('context','sql','w','available','completed')}


def classify_domain(s, action, *, missing=None, sample=None):
    """Classify caller input domain; never predict a production SQL guard/result."""
    if missing is not None: return {'kind':'capture','reason':missing}
    if sample is not None and sample != s['now']:
        return {'kind':'sample','reason':'retirement_sample'}
    if canonical(action) in expected_actions(s): return None
    op = action['op']; reason = None
    if op == 'claim' and s['budgets']['claims'][action['job']] >= 2: reason = 'attempt_budget'
    if op in {'renew','settle','bind','complete'}:
        j,i = action['job'],action['token']; token = s['captured_tokens'][j][i]
        ordinary = op != 'settle' or action['disposition'] != 3
        if ordinary and any(t['claim_id'] == token['claim_id'] for t in s['captured_tokens'][j][i+1:]):
            reason = 'ordinary_revision'
        elif op == 'renew' and token['claim_id'] in s['budgets']['renewed_claims']: reason = 'renewal_budget'
        elif op in {'bind','complete'} and s['captured_leases'][action['lease']]['owner_node_id'] != token['node_id']:
            reason = 'lease_owner'
    if op == 'acquire_domain':
        n = action['node']
        if s['budgets']['domain_acquired'][n]: reason = 'domain_budget'
        elif n != 2 and not any(j['state'] == 'running' and j['owner'] == ('node-a','node-b')[n] for j in s['jobs']):
            reason = 'domain_owner'
    if op == 'remove_owner' and s['budgets']['removed'][action['node']]: reason = 'owner_removed'
    if op == 'clock_advance': reason = 'no_clock_boundary'
    require(reason is not None, 'unclassified structural input outside D')
    return {'kind':'domain','reason':reason}


def same_claim(p,s,n,action):
    w = p['w'][n]
    return (w['stage'] in {'claim','driver'} and w['job'] == action['job'] and w['token'] >= 0 and
      s['captured_tokens'][action['job']][w['token']]['claim_id'] ==
      s['captured_tokens'][action['job']][action['token']]['claim_id'])


def environment_inputs(p,s):
    for encoded in sorted(expected_actions(s)):
        a = json.loads(encoded); op = a['op']
        if op not in {'clock_advance','remove_owner','acquire_domain','release_domain','renew'}: continue
        if op == 'acquire_domain' and a['node'] != 2: continue
        if op == 'release_domain' and s['captured_leases'][a['lease']]['owner_node_id'] != 'foreign-holder': continue
        if op == 'renew' and any(same_claim(p,s,n,a) and not
            (p['w'][n]['localToken'] and (p['w'][n]['call'] == 'ready' or p['w'][n]['retire'] == 'drain'))
            for n in range(2)): continue
        yield a


def expected_product_demands(p, driver, store):
    s = store['states'][p['sql']]; stable,_ = CONTEXTS[p['context']]; out = []
    def add(family,n,role,op,**fields):
        w = p['w'][n] if n is not None else None; missing = None; sample = None
        if 'token' in fields and fields['token'] == -1: missing = 'missing_token'
        elif 'lease' in fields and fields['lease'] == -1: missing = 'missing_lease'
        a = None if missing else {'op':op,**fields}
        if family == 'RetireApply' or (role == 'capacity' and w['retire'] in {'first','resolve','second'}):
            sample = w['sample']
        out.append({'context':p['context'],'from':p['id'],'branch':{'family':family,'worker':n},
            'role':role,'input':a,'exclusion':classify_domain(s,a,missing=missing,sample=sample)})
    def call(family,n,role,pending):
        w = p['w'][n]; j,t,l = w['job'],w['token'],w['lease']
        if pending == 'AcquireDomain': add(family,n,role,'acquire_domain',node=n)
        elif pending == 'Bind': add(family,n,role,'bind',job=j,token=t,lease=l)
        elif pending in {'SettleYield','SettleRetry'}:
            add(family,n,role,'settle',job=j,token=t,disposition=0 if pending == 'SettleYield' else 1)
        elif pending in RELEASE_PENDING: add(family,n,role,'release_domain',lease=l)
        elif pending == 'PollWork': add(family,n,role,'complete',job=j,token=t,lease=l)
        elif pending == 'RetireQueue':
            if w['retire'] == 'resolve': add(family,n,role,'resolve',job=j,token=w['original'])
            else: add(family,n,role,'settle',job=j,
                token=w['original'] if w['retire'] == 'first' else w['resolved'],
                disposition=2 if w['retire'] == 'second' and w['outcome'] == 'cancel_requested' else 3)
    for n,w in enumerate(p['w']):
        if w['stage'] == 'claim' and w['call'] == 'sent': add('ClaimApply',n,'apply','claim',job=w['job'],node=n)
        if w['stage'] != 'driver' or w['call'] != 'sent': continue
        pending = driver['states'][w['ctrl']]['pending']
        if pending in SQL_PENDING:
            call('StoreApply',n,'apply',pending); call('Capacity',n,'capacity',pending)
        if pending == 'PollWork': call('WorkSuccess',n,'apply',pending)
        if pending == 'RetireQueue' and w['retire'] in {'first','resolve','second'}:
            if w['inner'] in {'ready','sent'}: call('Capacity',n,'capacity',pending)
            if w['inner'] == 'sent': call('RetireApply',n,'apply',pending)
    if not stable:
        for a in environment_inputs(p,s): add('Environment',None,'apply',a['op'],**{k:v for k,v in a.items() if k != 'op'})
    require(len({canonical(d) for d in out}) == len(out), 'duplicate independently derived demand')
    return out


def expected_product_successors(p,driver,store,oracle):
    """Full donor Next, independently transcribed. No fairness prefix pruning."""
    stable,mutation = CONTEXTS[p['context']]; s = store['states'][p['sql']]; out = []
    demands = expected_product_demands(p,driver,store)
    def lookup(family,n,action=None):
        matches = [d for d in demands if d['role'] == 'apply' and d['branch'] == {'family':family,'worker':n}
                   and (action is None or d['input'] == action)]
        require(len(matches) == 1, 'structural apply demand is not unique')
        d = matches[0]
        if d['exclusion'] is not None: return None
        key = (p['sql'],canonical(d['input']))
        require(key in oracle, 'MissingProductOracleEdge: eligible apply demand')
        i = oracle[key]; return i,store['edges'][i]
    def reply_allowed(n,tag):
        return any(e['from'] == p['w'][n]['ctrl'] and e['reply'] == tag for e in driver['edges'])
    def put(family,n,choice,change,edge=None):
        q = copy.deepcopy(product_payload(p))
        change(q)
        members = set()
        if family in FAIR_FAMILIES: members.add(family+'/'+str(n))
        if family == 'ClaimApply' and edge is not None and store['edges'][edge]['result'] == 'claimed':
            members.add('ClaimWin/'+str(n))
        if family in {'WorkSuccess','WorkOther'}: members.add('ExternalReturn/'+str(n))
        if family in RETIRE_LOCAL: members.add('RetireLocal/'+str(n))
        if family in {'RetireApply','RetireError'}: members.add('RetireCallReturn/'+str(n))
        out.append(({'family':family,'worker':n,'choice':choice},q,edge,members))
    def sql_update(q,e):
        q['sql'] = e['to']
        q['completed'] = sorted(set(q['completed']) | {j for j,job in enumerate(store['states'][e['to']]['jobs']) if job['request_done']})
    def local(q,n,tag): q['w'][n].update(call='applied',reply=tag,accepted=False)
    def commit(family,n,tag,pair):
        if pair is None or not reply_allowed(n,tag): return
        i,e = pair
        def change(q):
            sql_update(q,e); w = q['w'][n]
            w.update(call='applied',reply=tag,accepted=e['action']['op'] == 'settle' and e['result'] == 'accepted')
            if e['action']['op'] == 'acquire_domain' and e['result'] == 'acquired':
                w['lease'] = len(store['states'][e['to']]['captured_leases'])-1
        put(family,n,{},change,i)
    def local_branch(family,n,tag,choice=None):
        if reply_allowed(n,tag): put(family,n,choice or {},lambda q:local(q,n,tag))
    for n,w in enumerate(p['w']):
        def update(q, **fields): q['w'][n].update(fields)
        if w['stage'] == 'idle' and p['available'][n]:
            for j,job in enumerate(s['jobs']):
                if job['state'] == 'queued' and not job['request_done'] and (not stable or j == n):
                    def admit(q,j=j): q['w'][n] = {**IDLE,'stage':'claim','job':j,'admission':True}
                    put('Admission',n,{'job':j},admit)
        if w['stage'] == 'claim':
            if w['call'] == 'ready': put('ClaimSend',n,{},lambda q:update(q,call='sent'))
            if w['call'] == 'sent':
                pair = lookup('ClaimApply',n)
                if pair is not None:
                    i,e = pair; success = e['result'] == 'claimed'
                    def claimed(q):
                        q['sql'] = e['to']; tokens = store['states'][e['to']]['captured_tokens'][w['job']]
                        update(q,call='applied',reply='Claimed' if success else 'Refused',
                            token=len(tokens)-1 if success else -1,attempt=tokens[-1]['claim_id'] if success else '')
                    put('ClaimApply',n,{},claimed,i)
            if w['call'] == 'applied':
                for lost in ([False] if stable else [False,True]):
                    put('ClaimReply',n,{'lost':lost},lambda q,lost=lost:update(q,call='replied',
                        reply='Unknown' if lost else w['reply'],faultUsed=w['faultUsed'] or lost))
            if w['call'] == 'replied':
                def claim_return(q):
                    if w['reply'] == 'Claimed': update(q,stage='driver',ctrl=0,call='ready',started=True,localToken=True)
                    else: q['w'][n] = dict(IDLE)
                put('ClaimReturn',n,{},claim_return)
        if w['stage'] != 'driver':
            # Availability is independent of the local stage and stable premise.
            family = 'RemoveAvailability' if p['available'][n] else 'MakeAvailable'
            put(family,n,{},lambda q:q['available'].__setitem__(n,not p['available'][n]))
            continue
        d = driver['states'][w['ctrl']]; pending = d['pending']
        if not d['terminal'] and w['call'] == 'ready':
            put('Dispatch',n,{},lambda q:update(q,call='sent',readLock=pending == 'PollWork',accepted=False))
        if w['call'] == 'sent':
            if pending in SQL_PENDING:
                pair = lookup('StoreApply',n)
                if pair is not None:
                    i,e = pair
                    if pending == 'AcquireDomain' and (not stable or e['result'] == 'acquired'):
                        commit('StoreApply',n,'DomainAcquired' if e['result'] == 'acquired' else 'DomainRefused',pair)
                    elif pending == 'Bind' and (not stable or e['result'] == 'accepted'):
                        commit('StoreApply',n,'BindAccepted' if e['result'] == 'accepted' else 'BindRefused',pair)
                    elif pending in {'SettleYield','SettleRetry'}: commit('StoreApply',n,'Done',pair)
                    elif pending in RELEASE_PENDING: commit('StoreApply',n,'ReleaseReturned',pair)
                if not stable and not w['faultUsed']:
                    tag = ('DomainRefused' if pending == 'AcquireDomain' else 'BindRefused' if pending == 'Bind'
                           else 'Done' if pending in {'SettleYield','SettleRetry'} else 'ReleaseReturned')
                    if reply_allowed(n,tag):
                        def ambiguous(q): local(q,n,tag);q['w'][n]['faultUsed'] = True
                        put('AmbiguousWithoutApply',n,{},ambiguous)
            if pending in {'RevokeDomain','CancelWork','DropPublisher','DropAdmission'} and not w['readLock']:
                local_branch('SimpleApply',n,'Done')
            if pending == 'PollWork':
                pair = lookup('WorkSuccess',n)
                if pair is not None and pair[1]['result'] == 'accepted': commit('WorkSuccess',n,'WorkSucceeded',pair)
                if not stable:
                    for tag in ('QueueLoss','DomainLoss','Pressure','WorkFailed'):
                        local_branch('WorkOther',n,tag,{'reply':tag})
            if pending == 'JoinWork' and mutation != 'withheld-join':
                work = driver_work_class(d['work'])
                tags = ['JoinedSucceeded'] if work == 'completed_succeeded' else ['JoinedFailed'] if work == 'completed_failed' else ['JoinedFailed','JoinedSucceeded']
                for tag in tags: local_branch('JoinReturn',n,tag,{'reply':tag})
            if pending == 'RetireQueue':
                r,inner = w['retire'],w['inner']
                if r == 'off' and not w['readLock']: put('RetireBegin',n,{},lambda q:update(q,retire='drain'))
                if r == 'drain': put('RetireDrain',n,{},lambda q:update(q,retire='take'))
                if r == 'take': put('RetireTake',n,{},lambda q:update(q,
                    retire='sample-first' if w['localToken'] else 'done',original=w['token'],localToken=False))
                if r in {'sample-first','sample-resolve','sample-after'}:
                    for error in ([False] if w['faultUsed'] else [False,True]):
                        def sample(q,error=error):
                            value = -1 if error else s['now']; slot = {'sample-first':'firstSample','sample-resolve':'resolveSample','sample-after':'afterSample'}[r]
                            stage = ('warn' if error else 'first' if r == 'sample-first' else 'resolve' if r == 'sample-resolve'
                                     else 'second' if w['outcome'] in {'running','cancel_requested'} else 'done')
                            update(q,**{slot:value},sample=value,inner='ready',faultUsed=w['faultUsed'] or error,retire=stage)
                        put('RetireSample',n,{'clock_error':error},sample)
                if r in {'first','resolve','second'}:
                    if inner == 'ready': put('RetireSend',n,{},lambda q:update(q,inner='sent'))
                    if inner == 'sent':
                        pair = lookup('RetireApply',n)
                        if pair is not None:
                            i,e = pair
                            def apply_retire(q):
                                sql_update(q,e); update(q,inner='applied',outcome=e['result'],
                                    resolved=e['resolved_token'] if e['action']['op'] == 'resolve' and e['resolved_token'] is not None else -1 if e['action']['op'] == 'resolve' else w['resolved'])
                            put('RetireApply',n,{},apply_retire,i)
                        if not w['faultUsed']: put('RetireError',n,{},lambda q:update(q,inner='applied',outcome='error',faultUsed=True))
                    if inner == 'applied':
                        for lost in ([False] if w['faultUsed'] else [False,True]):
                            put('RetireReply',n,{'lost':lost},lambda q,lost=lost:update(q,inner='replied',
                                faultUsed=w['faultUsed'] or lost,outcome='error' if lost else w['outcome']))
                    if inner == 'replied':
                        stage = ('done' if w['outcome'] == 'accepted' else 'sample-resolve') if r == 'first' else (
                            'warn' if w['outcome'] == 'error' else 'sample-after') if r == 'resolve' else (
                            'warn' if w['outcome'] == 'error' else 'done')
                        put('RetireObserve',n,{},lambda q:update(q,retire=stage))
                if r in {'done','warn'}: local_branch('RetireFinish',n,'Done')
        if w['call'] == 'applied':
            lose = (not w['faultUsed'] and pending in SQL_PENDING | {'PollWork'} and
                    (not stable or pending in RELEASE_PENDING | {'PollWork'}))
            for lost in ([False,True] if lose else [False]):
                def reply(q,lost=lost):
                    tag = {'WorkSucceeded':'WorkFailed','DomainAcquired':'DomainRefused','BindAccepted':'BindRefused'}.get(w['reply'],w['reply']) if lost else w['reply']
                    update(q,call='replied',readLock=False,reply=tag,faultUsed=w['faultUsed'] or lost,
                        localToken=False if not lost and w['accepted'] and pending in {'SettleYield','SettleRetry'} else w['localToken'])
                put('Reply',n,{'lost':lost},reply)
        if w['call'] == 'replied':
            for edge in driver['edges']:
                if edge['from'] != w['ctrl'] or edge['reply'] != w['reply']: continue
                def returned(q,edge=edge): update(q,ctrl=edge['to'],call='ready',
                    joined=w['joined'] or pending == 'JoinWork',
                    workStarted=w['workStarted'] or pending == 'Bind' and w['reply'] == 'BindAccepted',
                    dropped=w['dropped'] or pending == 'DropAdmission',
                    admission=False if pending == 'DropAdmission' or mutation == 'drop-before-join' and pending == 'CancelWork' else w['admission'])
                put('DriverReturn',n,{},returned)
        if d['terminal'] and not stable: put('Recycle',n,{},lambda q:q['w'].__setitem__(n,dict(IDLE)))
        family = 'RemoveAvailability' if p['available'][n] else 'MakeAvailable'
        put(family,n,{},lambda q:q['available'].__setitem__(n,not p['available'][n]))
    if not stable:
        for a in environment_inputs(p,s):
            i,e = lookup('Environment',None,a)
            matches = [n for n in range(2) if a['op'] == 'renew' and same_claim(p,s,n,a)]
            ambiguous = e['result'] == 'renewed' and any(not p['w'][n]['faultUsed'] for n in matches)
            for ack in ([False,True] if ambiguous else [True]):
                def environment(q,a=a,e=e,ack=ack,matches=matches):
                    sql_update(q,e)
                    if a['op'] == 'renew' and e['result'] == 'renewed':
                        for n in matches:
                            if ack: q['w'][n]['token'] = len(store['states'][e['to']]['captured_tokens'][a['job']])-1
                            q['w'][n]['faultUsed'] = p['w'][n]['faultUsed'] or not ack
                put('Environment',None,{'input':a,'ack':ack},environment,i)
    return out


def validate_store(g, verify_sources=True, *, product=False):
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
    require(domain.get('settlement_dispositions') == ['yield','retry','cancel','retirement_yield'], 'settlement input domain drift')
    require(domain.get('ordinary_token_inputs') == 'latest captured revision per claim; queue-token Mutex adapter contract; historical revisions remain captured and conformance-tested', 'ordinary token input-domain drift')
    require(domain.get('binding_completion_inputs') == 'captured domain lease owner equals captured queue-token node; wrong-owner inputs are separate conformance', 'lease-owner input-domain drift')
    require(domain.get('node_domain_acquisition_inputs') == 'node has selected running durable queue ownership, including expired retained ownership for dispatch/application races; foreign holder is independent', 'node-domain input-domain drift')
    for key, value in STORE_BOUNDARIES.items():
        require(domain.get(key) == value, f'Store boundary drift: {key}')
    bindings = g['source_bindings']
    expected_sources = {'store/'+p for p in ('background_jobs.rs','background_jobs_domain.rs',
      'background_jobs_library.rs','background_jobs_schema.sql','background_jobs_resources.sql',
      'background_jobs_library.sql','background_jobs_domain.sql','subtitle_managed_schema.sql',
      'sqlite/mod.rs','sqlite/background_jobs.rs','sqlite/coordination.rs','async_driver_refinement.rs')}
    expected_sources.add('cluster/coordination.rs')
    if product: expected_sources.update({'store/async_driver_product.rs','store/mod.rs'})
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
        fields = {'id','now','jobs','domain','captured_tokens','captured_leases','budgets','cutoffs','canonical_sha256'}
        exact_keys(s, fields | ({'raw','canonical_json'} if product else set()), 'Store state')
        if product: validate_raw_store(s)
        require(s['canonical_sha256'] not in hashes, 'duplicate canonical state identity')
        hashes.add(s['canonical_sha256'])
        require(integer(s.get('now'),-(2**63),2**63-1), 'non-i64 concrete time')
        require(re.fullmatch('[0-9a-f]{64}',s.get('canonical_sha256','')) is not None, 'missing canonical state hash')
        require(isinstance(s.get('jobs'),list) and len(s['jobs']) == 2, 'two raw job projections required')
        require(isinstance(s.get('captured_tokens'),list) and len(s['captured_tokens']) == 2, 'missing captured token inventory')
        require(isinstance(s.get('captured_leases'),list), 'missing captured lease inventory')
        require(isinstance(s.get('cutoffs'),list), 'missing visible input cutoffs')
        for cutoff in s['cutoffs']:
            require(re.fullmatch(r'attempt_budget_j[01]|renewal_budget_.+|domain_acquire_budget(?:_node-a|_node-b|_foreign-holder)?|no_future_boundary',cutoff) is not None,
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
        validate_job_projection(s)
        for lease in s['captured_leases']:
            require(lease.get('owner_node_id') in {'node-a','node-b','foreign-holder'}, 'unknown opaque lease owner')
            for field in ('fence','revision'): require(integer(lease.get(field),0,2**64-1), 'invalid u64 domain field')
            require(integer(lease.get('expires_at_unix_ms'),-(2**63),2**63-1), 'invalid i64 domain expiry')
            exact_keys(lease, {'resource','owner_node_id','fence','revision','expires_at_unix_ms'}, 'captured lease')
    for e in g['edges']:
        exact_keys(e, {'from','to','action','result','resolved_token'}, 'Store edge')
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
        if 'disposition' in a: require(a['disposition'] in range(4), 'unknown disposition')
        validate_resolution(e,before,after)
        for j in range(2):
            old, new = before['captured_tokens'][j], after['captured_tokens'][j]
            require(new[:len(old)] == old, 'captured token identity is not append-only')
        require(after['captured_leases'][:len(before['captured_leases'])] == before['captured_leases'],
                'captured lease identity is not append-only')
    for s,actual in zip(g['states'],outgoing):
        expected = expected_actions(s)
        require(actual <= expected if product else actual == expected,
                f'Store input-domain {"membership" if product else "closure"} incomplete at state {s["id"]}')
    if product:
        return {'store_states':len(g['states']),'store_edges':len(g['edges']),
                'states':len(g['states']),'edges':len(g['edges']),'domain':domain,
                'unobserved_known_results':sorted(RESULTS-set(results))}
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

SOURCE_BOUNDARIES = {'OrdinarySettlementPayloadTransfer':'unclosed'}
PRODUCT_KEYS = {'schema_version','scope','driver_graph_sha256','source_bindings','semantic_bindings','source_boundaries',
 'schema_bindings','domain','store','contexts','product_states','product_edges','demands','enabledness','coverage'}
CORE_SOURCES = {'store/'+name for name in ('background_jobs.rs','background_jobs_domain.rs',
 'background_jobs_library.rs','background_jobs_schema.sql','background_jobs_resources.sql',
 'background_jobs_library.sql','background_jobs_domain.sql','subtitle_managed_schema.sql',
 'sqlite/mod.rs','sqlite/background_jobs.rs','sqlite/coordination.rs','async_driver_refinement.rs',
 'async_driver_product.rs','mod.rs')} | {'cluster/coordination.rs'}


def product_source_bindings():
    return {path:sha(ROOT/'crates/plurx-core/src'/path) for path in sorted(CORE_SOURCES)}


def product_semantic_bindings():
    return {path:sha(ROOT/path) for path in sorted(SEMANTIC_PATHS)}


def validate_schema_bindings(schema):
    exact_keys(schema, {'compiled_sqlite_migrations','installed_sqlite_schema_sha256','schema_inventory'}, 'schema bindings')
    require(isinstance(schema['compiled_sqlite_migrations'],list) and schema['compiled_sqlite_migrations'], 'missing compiled migrations')
    for i,m in enumerate(schema['compiled_sqlite_migrations']):
        exact_keys(m, {'version','sha256'}, 'compiled migration')
        require(m['version'] == i+1 and isinstance(m['sha256'],str) and re.fullmatch('[0-9a-f]{64}',m['sha256']), 'invalid compiled migration')
    require(isinstance(schema['schema_inventory'],list) and schema['schema_inventory'], 'missing schema inventory')
    require(hashlib.sha256(canonical(schema['schema_inventory']).encode()).hexdigest() == schema['installed_sqlite_schema_sha256'],
            'installed schema inventory hash mismatch')
    return schema


def validate_worker(w,s,driver):
    exact_keys(w, set(IDLE), 'product worker')
    for key,initial in IDLE.items():
        if type(initial) is bool: require(type(w[key]) is bool, 'worker Boolean type drift: '+key)
        elif type(initial) is int: require(integer(w[key],-(2**63),2**63-1), 'worker integer type drift: '+key)
        else: require(isinstance(w[key],str), 'worker string type drift: '+key)
    require(w['stage'] in {'idle','claim','driver'} and w['call'] in {'ready','sent','applied','replied'}, 'unknown worker stage/call')
    require(w['retire'] in {'off','drain','take','sample-first','first','sample-resolve','resolve','sample-after','second','done','warn'}, 'unknown retire phase')
    require(w['inner'] in {'ready','sent','applied','replied'}, 'unknown retirement inner phase')
    require(w['reply'] in {'','Claimed','Refused','Unknown'} | set(REPLIES), 'unknown worker reply')
    require(w['outcome'] in {'' ,'error'} | RESULTS, 'unknown inner outcome')
    if w['stage'] == 'idle': require(w == IDLE, 'idle worker is not fully reset')
    else: require(w['job'] in (0,1), 'worker missing selected job')
    if w['stage'] == 'driver': require(integer(w['ctrl'],0,len(driver['states'])-1), 'invalid actual driver control')
    else: require(w['ctrl'] == -1, 'non-driver has a driver control')
    for key in ('token','original','resolved'):
        require(w[key] == -1 or w['job'] in (0,1) and integer(w[key],0,len(s['captured_tokens'][w['job']])-1), 'invalid captured worker index: '+key)
    require(w['lease'] == -1 or integer(w['lease'],0,len(s['captured_leases'])-1), 'invalid captured worker lease')
    if w['token'] >= 0:
        require(w['attempt'] == s['captured_tokens'][w['job']][w['token']]['claim_id'], 'mutable worker attempt identity')
    if w['stage'] == 'driver': require(w['token'] >= 0 and bool(w['attempt']), 'driver without claimed token history')


def validate_semantic_bindings(bindings,verify_sources=True):
    require(isinstance(bindings,dict) and set(bindings) == SEMANTIC_PATHS, 'semantic source registry differs')
    for path,digest in bindings.items():
        require(isinstance(digest,str) and re.fullmatch('[0-9a-f]{64}',digest), 'invalid semantic source digest')
        require(not verify_sources or sha(ROOT/path) == digest, 'semantic source hash mismatch: '+path)


def validate_source_boundaries(boundaries):
    require(isinstance(boundaries,dict) and boundaries == SOURCE_BOUNDARIES,
            'OrdinarySettlementPayloadTransfer boundary must be exactly unclosed')


def validate_product_schema(schema):
    exact_keys(schema, {'schema_version','scope','kind','root_fields','main_contexts',
        'boundary_contexts','configuration_contexts','fair_families','worker_fields',
        'product_state_fields','product_edge_fields','demand_fields','demand_roles',
        'store_state_additions','source_policy','resource_policy','source_boundaries'}, 'product schema')
    require(schema.get('kind') == 'closed_wire_vocabulary_not_acceptance', 'product schema kind drift')
    require(schema.get('schema_version') == 2 and schema.get('scope') == 'selected_two_worker_product', 'product schema scope drift')
    require(schema.get('root_fields') == sorted(PRODUCT_KEYS), 'product schema root registry drift')
    validate_source_boundaries(schema['source_boundaries'])
    require(schema.get('main_contexts') == {name:{'stable_success':v[0],'mutation':v[1]} for name,v in CONTEXTS.items()}, 'product schema context drift')
    require(schema.get('boundary_contexts') == BOUNDARY_CONTEXTS and schema.get('configuration_contexts') == CONFIG_CONTEXT, 'product schema configuration drift')
    require(schema.get('fair_families') == list(FAIR_FAMILIES) and set(schema.get('worker_fields',[])) == set(IDLE), 'product schema family/worker drift')
    for key,values in {
        'product_state_fields':['id','context','sql','w','available','completed','canonical_sha256'],
        'product_edge_fields':['from','to','branch','oracle_edge','fair_actions'],
        'demand_fields':['context','from','branch','role','input','oracle_edge','exclusion'],
        'demand_roles':['apply','capacity'],'store_state_additions':['raw','canonical_json']}.items():
        require(schema[key] == values, 'product schema '+key+' drift')
    require(schema['source_policy'] == 'exact core and semantic registries; raw driver input digest; full Store canonical identity; independent semantics',
            'product schema source policy drift')
    require(schema.get('resource_policy') == {'store_identity_cap':20000,'product_node_cap':None,
        'bundle_max_bytes':268435456,'artifact_max_bytes':536870912}, 'product schema resource drift')


def require_exact_inventory(actual,expected,label):
    require(isinstance(actual,list),label+' is not an array')
    a,b = [canonical(v) for v in actual],[canonical(v) for v in expected]
    require(len(a) == len(set(a)),label+' has duplicates')
    require(set(a) == set(b),label+' differs from complete independent enumeration')


def complete_product_demands(p,driver,store,oracle):
    complete = []; used = set()
    for d in expected_product_demands(p,driver,store):
        index = None
        if d['exclusion'] is None:
            key = (p['sql'],canonical(d['input']))
            require(key in oracle, 'MissingProductOracleEdge: eligible apply/capacity demand')
            index = oracle[key]; used.add(index)
        complete.append({**d,'oracle_edge':index})
    return complete,used


def product_pair_obligations(p,candidates,by_payload):
    pair_members = {}
    for branch,q,index,members in candidates:
        pair_members.setdefault(canonical(q),set()).update(members)
    edges = []; targets = []; enabled = set()
    for branch,q,index,members in candidates:
        encoded = canonical(q)
        require(encoded in by_payload, 'MissingProductSuccessor: original Next target absent')
        to = by_payload[encoded]; targets.append(to); all_members = pair_members[encoded]
        if encoded != canonical(product_payload(p)): enabled.update(all_members)
        edges.append({'from':p['id'],'to':to,'branch':branch,'oracle_edge':index,'fair_actions':sorted(all_members)})
    return edges,{'state':p['id'],'actions':sorted(enabled)},targets


def validate_context_reachability(initial,ids,adjacency):
    reachable = {initial}; todo = deque(reachable)
    while todo:
        for to in adjacency[todo.popleft()]:
            if to not in reachable: reachable.add(to);todo.append(to)
    require(reachable == set(ids), 'orphan/unreachable product certificate')


def validate_product(bundle,driver,verify_sources=True,driver_sha256=None):
    try: return _validate_product(bundle,driver,verify_sources,driver_sha256)
    except (KeyError,TypeError,IndexError,ValueError) as exc:
        raise Invalid('MalformedProductBundle: '+str(exc)) from exc


def _validate_product(g,driver,verify_sources,driver_sha256):
    exact_keys(g,PRODUCT_KEYS,'product bundle')
    require(g['schema_version'] == 2 and g['scope'] == 'selected_two_worker_product', 'unsupported product schema/scope')
    validate_source_boundaries(g['source_boundaries'])
    require(len(canonical(g).encode()) <= 256*1024*1024, 'product bundle byte bound exceeded')
    validate_driver(driver)
    require(isinstance(g['driver_graph_sha256'],str) and re.fullmatch('[0-9a-f]{64}',g['driver_graph_sha256']), 'invalid driver graph digest')
    require(not verify_sources or driver_sha256 is not None, 'raw driver graph digest required for source validation')
    require(driver_sha256 is None or g['driver_graph_sha256'] == driver_sha256, 'driver graph input hash mismatch')
    validate_product_schema(read_json(PACKAGE/'product-schema.json'))
    validate_semantic_bindings(g['semantic_bindings'],verify_sources)
    exact_keys(g['store'], {'initial','states','edges'}, 'imported oracle relation')
    store = g['store']
    oracle_graph = {'schema_version':1,'scope':'imported_oracle','initial':store['initial'],
       'states':store['states'],'edges':store['edges'],'domain':g['domain'],'source_bindings':g['source_bindings'],
       'schema_bindings':g['schema_bindings'],'coverage':None}
    facts = validate_store(oracle_graph,verify_sources,product=True)
    require(isinstance(g['contexts'],list), 'missing context inventory')
    contexts = {}
    for context in g['contexts']:
        exact_keys(context, {'id','model','stable_success','mutation','witness','initial','states'}, 'product context')
        name = context['id']; require(name not in contexts, 'duplicate product context')
        require(type(context['stable_success']) is bool and context['witness'] is False, 'context constant type drift')
        contexts[name] = context
    require(set(contexts) == set(CONTEXTS) | set(BOUNDARY_CONTEXTS), 'missing/unknown context')
    for name,(stable,mutation) in CONTEXTS.items():
        require(contexts[name]['model'] == 'AsyncDriverRefinement' and contexts[name]['stable_success'] == stable and
                contexts[name]['mutation'] == mutation, 'main context constant drift')
    for name,mutation in BOUNDARY_CONTEXTS.items():
        require(contexts[name] == {'id':name,'model':'BoundaryControls','stable_success':False,'mutation':mutation,
                 'witness':False,'initial':None,'states':[]}, 'boundary descriptor is not existential-only')
    states = g['product_states']; require(isinstance(states,list) and states, 'empty product certificate')
    require([p.get('id') for p in states] == list(range(len(states))), 'product IDs are not contiguous')
    by_payload = {}; per_context = {name:[] for name in CONTEXTS}
    for p in states:
        exact_keys(p, {'id','context','sql','w','available','completed','canonical_sha256'}, 'product state')
        require(p['context'] in CONTEXTS, 'non-main product state')
        require(integer(p['sql'],0,len(store['states'])-1), 'invalid product Store reference')
        require(isinstance(p['w'],list) and len(p['w']) == 2, 'product requires exactly two workers')
        require(isinstance(p['available'],list) and len(p['available']) == 2 and all(type(v) is bool for v in p['available']), 'invalid physical availability')
        require(isinstance(p['completed'],list) and all(integer(v,0,1) for v in p['completed']) and p['completed'] == sorted(set(p['completed'])), 'invalid completion history')
        for w in p['w']: validate_worker(w,store['states'][p['sql']],driver)
        encoded = canonical(product_payload(p))
        require(p['canonical_sha256'] == hashlib.sha256(encoded.encode()).hexdigest(), 'product canonical preimage mismatch')
        require(encoded not in by_payload, 'duplicate exact product valuation')
        by_payload[encoded] = p['id']; per_context[p['context']].append(p['id'])
    for name,ids in per_context.items():
        context = contexts[name]
        require(context['states'] == ids, 'context state partition differs')
        initial = {'context':name,'sql':store['initial'],'w':[dict(IDLE),dict(IDLE)],'available':[True,True],
            'completed':[j for j,job in enumerate(store['states'][store['initial']]['jobs']) if job['request_done']]}
        require(canonical(initial) in by_payload and context['initial'] == by_payload[canonical(initial)], 'product Init mismatch')
    oracle = {(e['from'],canonical(e['action'])):i for i,e in enumerate(store['edges'])}
    require(len(oracle) == len(store['edges']), 'nondeterministic oracle input')
    expected_demands = []; used_oracle = set(); expected_edges = []; adjacency = [[] for _ in states]
    expected_enabled = []
    for p in states:
        demands,used = complete_product_demands(p,driver,store,oracle)
        expected_demands.extend(demands);used_oracle.update(used)
        candidates = expected_product_successors(p,driver,store,oracle)
        edges,enabled,targets = product_pair_obligations(p,candidates,by_payload)
        expected_edges.extend(edges);expected_enabled.append(enabled);adjacency[p['id']] = targets
    require_exact_inventory(g['demands'],expected_demands,'product demand coverage')
    require_exact_inventory(g['product_edges'],expected_edges,'product successor/state-pair membership')
    require_exact_inventory(g['enabledness'],expected_enabled,'fair nonstuttering enabledness')
    require(used_oracle == set(range(len(store['edges']))), 'undemanded oracle edge in product bundle')
    for name,ids in per_context.items():
        validate_context_reachability(contexts[name]['initial'],ids,adjacency)
    coverage = g['coverage']
    exact_keys(coverage, {'complete','no_skipped_demands','store_state_count','store_edge_count',
        'product_state_count','product_edge_count','demand_count','by_context'}, 'product coverage')
    require(coverage['complete'] is True and coverage['no_skipped_demands'] is True, 'partial product frontier')
    for key,actual in (('store_state_count',len(store['states'])),('store_edge_count',len(store['edges'])),
      ('product_state_count',len(states)),('product_edge_count',len(expected_edges)),('demand_count',len(expected_demands))):
        require(type(coverage[key]) is int and coverage[key] == actual, 'product coverage count mismatch: '+key)
    edge_counts = Counter(states[e['from']]['context'] for e in expected_edges)
    demand_counts = Counter(d['context'] for d in expected_demands)
    expected_counts = {name:{'state_count':len(ids),'edge_count':edge_counts[name],
        'demand_count':demand_counts[name],'frontier_count':0} for name,ids in per_context.items()}
    require(coverage['by_context'] == expected_counts, 'context frontier/count coverage mismatch')
    return {**facts,'product_states':len(states),'product_edges':len(expected_edges),'coverage':coverage,
            'result_class':'independent_product_inventory','acceptance':False,'scope':g['scope'],
            'source_boundaries':dict(g['source_boundaries'])}


def rejection_controls(driver,store):
    import copy
    mutations = []
    broken = copy.deepcopy(driver); broken['edges'][0]['to'] = len(driver['states'])
    mutations.append(('missing-driver-target',lambda: validate_driver(broken),'missing/outside edge target'))
    require(isinstance(store.get('source_bindings'),dict) and store['source_bindings'],
            'source-hash-drift control requires nonempty source bindings')
    bad_store = dict(store); bad_store['source_bindings'] = dict(store['source_bindings'])
    first = next(iter(bad_store['source_bindings']))
    bad_store['source_bindings'][first] = '0'*64
    if store.get('scope') == 'selected_two_worker_product':
        mutations.append(('source-hash-drift',lambda: validate_product(bad_store,driver,
            driver_sha256=store['driver_graph_sha256']),'Store source hash mismatch'))
        oracle = {(e['from'],canonical(e['action'])):i for i,e in enumerate(store['store']['edges'])}
        selected = next(((p,d) for p in store['product_states']
            for d in expected_product_demands(p,driver,store['store']) if d['exclusion'] is None),None)
        require(selected is not None, 'missing-store-input control requires an eligible demand')
        predecessor,demand = selected
        missing_oracle = dict(oracle)
        key = (predecessor['sql'],canonical(demand['input']))
        require(key in missing_oracle, 'missing-store-input control requires a present oracle edge')
        del missing_oracle[key]
        mutations.append(('missing-store-input',lambda: complete_product_demands(
            predecessor,driver,store['store'],missing_oracle),'MissingProductOracleEdge'))
    else:
        mutations.append(('source-hash-drift',lambda: validate_store(bad_store),'Store source hash mismatch'))
        missing = copy.deepcopy(store)
        require(any(e['from'] == e['to'] for e in missing['edges']),
                'missing-store-input control requires a self edge')
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
    bundle = store if store.get('scope') == 'selected_two_worker_product' else None
    if bundle is not None: store = bundle['store']
    ranks = validate_driver(driver)
    ds = []
    for s,r in zip(driver['states'],ranks):
        ds.append({'pending':s['pending'],'terminal':s['terminal'],'work':driver_work_class(s['work']),
          **s['held'],'revoked':s['revoked'],'cancelled':s['cancelled'],'rank':r})
    de = [{'src':e['from'],'dst':e['to'],'reply':e['reply']} for e in driver['edges']]
    ss = []
    owners = {None:-1,'node-a':0,'node-b':1,'foreign':2,'foreign-holder':2}
    for s in store['states']:
        ss.append({'input_domain':{key:s[key] for key in ('now','jobs','domain','captured_tokens','captured_leases','budgets')},
           'now':s['now'],'jobs':[{'state':j['state'],'active':j['active'],'done':j['request_done'],
            'owner':owners[j['owner']],'reservations':j['reservation_count']} for j in s['jobs']],
           'tokens':s['captured_tokens'],
           'leases':[{'owner':owners[l['owner_node_id']]} for l in s['captured_leases']]})
    se = [{'fresh':edge_fresh(e,store), 'monotone':edge_monotone(e,store), 'src':e['from'],'dst':e['to'],'op':e['action']['op'],'result':e['result'],
       'resolved':e['resolved_token'] if e['resolved_token'] is not None else -1,'input':e['action'],
       **{key:e['action'].get(key,-1) for key in ('job','node','token','lease','disposition')}} for e in store['edges']]
    # Tokens' concrete payloads are opaque to the product. Length and identity
    # are checked above, and SQL membership comes solely from the source graph.
    for output,original in zip(ss,store['states']):
        output['tokens'] = [[t['claim_id'] for t in tokens] for tokens in original['captured_tokens']]
    claim_ids = sorted({t['claim_id'] for state in store['states'] for tokens in state['captured_tokens'] for t in tokens})
    product_tables = ''
    if bundle is not None:
        ps = [{**{k:p[k] for k in ('id','context','sql','available','completed')},'workers':p['w']} for p in bundle['product_states']]
        pe = [{'src':e['from'],'dst':e['to'],'fair':e['fair_actions']} for e in bundle['product_edges']]
        outgoing = [dict() for _ in ps]
        for e in pe: outgoing[e['src']][e['dst']] = {'dst':e['dst'],'fair':e['fair']}
        groups = [[targets[to] for to in sorted(targets)] for targets in outgoing]
        entries = []
        for p in bundle['product_states']:
            key = [p['context'],p['sql'],p['w'],p['available'],p['completed']]
            entries.append('('+tla(key)+' :> '+str(p['id'])+')')
        def merge(items):
            if not items: return r'[x \in {} |-> x]'
            if len(items) == 1: return items[0]
            mid = len(items)//2
            return '('+merge(items[:mid])+' @@ '+merge(items[mid:])+')'
        index = merge(entries)
        product_tables = ('ProductStates == '+tla(ps)+'\nProductEdges == {'+','.join(tla(e) for e in pe)+'}\n'
          +'ProductEnabled == '+tla(sorted(bundle['enabledness'],key=lambda r:r['state']))+'\n'
          +'ProductOutgoing == '+tla(groups)+'\nProductIndex == '+index+'\n')
    return ('--------------------------- MODULE SourceTables ---------------------------\n'
      'EXTENDS Integers, Sequences, TLC\nDriverInitial == 0\nStoreInitial == 0\n'
      + 'DriverStates == '+tla(ds)+'\nDriverEdges == {'+','.join(tla(e) for e in de)+'}\n'
      + 'StoreStates == '+tla(ss)+'\nStoreEdges == {'+','.join(tla(e) for e in se)+'}\n'
      + 'ClaimIds == {'+','.join(tla(a) for a in claim_ids)+'}\n' + product_tables
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
        kinds = {'temporal' if expected in cfg['properties'] else 'invariant'
                 for cfg in CONFIGS.values() if cfg['expected'] == expected}
        require(len(kinds) == 1, 'unregistered or ambiguous expected counterexample kind')
        if kinds == {'invariant'}:
            require(code == 12 and safety == [expected] and not temporal, 'wrong named invariant counterexample')
        else:
            require(code == 13 and not safety and temporal and 'Back to state' in output, 'wrong temporal counterexample')
        return {'result_class':'reachability_witness' if expected == 'NonvacuityWitness' else 'semantic_control',
                'expected_property':expected,'states':counts,'complete':False,'counterexample':True}
    require(code == 0 and not safety and not temporal and counts[2] == 0, 'incomplete/failed positive TLC run')
    require('Model checking completed. No error has been found.' in output, 'missing explicit TLC success')
    return {'result_class':'conditional_finite_model_theorem','states':counts,'complete':True,'counterexample':False}
