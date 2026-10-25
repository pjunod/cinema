"""Closed registries and graph import. Importing a graph is inventory, not proof."""
from __future__ import annotations
import hashlib
import json
import io
import re
from array import array
from bisect import bisect_right
from collections import Counter, deque
from collections.abc import Sequence
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
 'resolve': {'job','token'}, 'resolve_initial_claim': {'job','request'}, 'read_claim_job': {'job'},
 'ordinary_settle': {'job','token','kind','not_before_ms','request_now_ms','error_code','checkpoint'}}
RESULTS = {'claimed','refused','cancelled','expired_or_pruned','renewed','accepted',
           'acquired','held','removed','advanced','running','cancel_requested','settled','lost_ownership','validation_error_yield_time','validation_error_retry_time','present','missing'}
RESULTS_BY_OP = {'claim': {'claimed','refused','cancelled','expired_or_pruned'}, 'renew': {'renewed','refused'},
 'settle': {'accepted','refused'}, 'bind': {'accepted','refused'}, 'complete': {'accepted','refused'},
 'acquire_domain': {'acquired','held'}, 'release_domain': {'accepted','refused'},
 'remove_owner': {'removed'}, 'clock_advance': {'advanced'},
 'resolve': {'running','cancel_requested','settled','lost_ownership','expired_or_pruned'},
 'ordinary_settle': {'accepted','refused','validation_error_yield_time','validation_error_retry_time'},
 'resolve_initial_claim': {'running','cancel_requested','settled','lost_ownership','expired_or_pruned'}, 'read_claim_job': {'present','missing'}}
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
 'ordinary_settlement_payload_transfer': 'selected actual ordinary settlement payload/capture/request/local-service correspondence; evidence pending qualification; normal wall/mono input domain only'}
STORE_BOUNDARIES['omitted_dv_state_boundary'] = 'selected library_scan/metadata_refresh inputs preserve empty files and DV demand, marker [(1,1)] and exact initialized dv_title_jobs_demand sqlite_sequence rows; omitted-state drift aborts the whole run without changing nine-table canonical identity'
STORE_BOUNDARIES.update(clock='wall and mono independently advance to the smallest strict greater nonnegative representable neighbour of retained exact authority/payload/queued-eligibility or source local-deadline anchors; no horizon clipping',
 retirement_inputs='disposition 3 uses Some(worker_retired), None checkpoint and not_before_ms=current episode.now for every captured original token; cancellation uses disposition 2; ordinary service uses contextual ordinary_settle payloads',
 primitive_clock_boundary='intrinsic primitives use current episode.now; ordinary_settle preserves its separately captured request_now_ms; retirement wall samples/internal dispatch races and zero-delay claim/renew-resolution envelopes remain named source/conformance boundaries',
 renewal_resolution_boundary='selected zero-delay renewal plus actual logical resolve has no inner interleaving; both actual outcomes and capture references are certified; resolution errors consume the remaining per-attempt fault allowance')
STORE_BOUNDARIES['initial_claim_recovery_boundary'] = 'lost initial claims use actual fence-None resolution with original dispatch identity, conditional actual row reread or Cancel, and no inner query interleaving; at most eight persistent job/node/claim loss keys; inconclusive outcomes service exact 250ms source wakes until the fixed 30000ms local deadline; the separate Store request window is 120000ms'
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
ORDINARY = dict(phase='off',payload=None,captured_token=None,lost_seen=None,deadline_seen=None,
    guard_mono_ms=None,request_now_ms=None,api_outcome=None,observed_outcome=None)
IDLE.update(claim_snapshot=None,claim_issued_mono_ms=None,claim_request_now_ms=None,
    local_deadline_mono_ms=None,queue_lost=False,cancelling=False,driver_settlement=None,adapter_reply=None,ordinary=ORDINARY,claim_request=None,claim_recovery={'phase':'off','wake_mono_ms':None})
PRODUCT_VALUE_FIELDS = ('context','sql','w','available','completed','mono_now_ms','wall_anchors','mono_anchors','claim_loss_keys')
I64_MAX = 2**63-1
TLC_INT_MIN, TLC_INT_MAX = -(2**31), 2**31-1
JOB_LEASE_MS = 30000
ORDINARY_PENDING = {'SettleYield','SettleRetry'}
ORDINARY_LOCKED = {'check_lost','check_deadline','take_token','sample','sent','applied','finish'}
VALIDATION_ERRORS = {'validation_error_yield_time':'invalid background checkpoint or yield time',
                     'validation_error_retry_time':'invalid background failure retry'}
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
 'JoinReturn','RetireDrain','MakeAvailable','Admission','ClaimWin','AdapterSample','OrdinaryLock',
 'OrdinaryLocal','OrdinaryCallReturn','OrdinaryObserve','ClaimRecover')
GLOBAL_FAIR = ('EligibilityAdvance/global','ClaimRecoveryWake/global')
RETIRE_LOCAL = {'RetireBegin','RetireTake','RetireSample','RetireSend','RetireReply','RetireObserve','RetireFinish'}
SQL_PENDING = {'AcquireDomain','Bind','ReleaseRefused',
               'ReleaseRetry','ReleaseNone','ReleaseInterrupted'}
RELEASE_PENDING = {'ReleaseRefused','ReleaseRetry','ReleaseNone','ReleaseInterrupted'}
BRANCH_CHOICES = {name:set() for name in ('ClaimSend','ClaimApply','ClaimReturn','Dispatch','StoreApply',
 'AmbiguousWithoutApply','SimpleApply','WorkSuccess','DriverReturn','Recycle','RetireBegin',
 'RetireDrain','RetireTake','RetireSend','RetireApply','RetireError','RetireObserve','RetireFinish',
 'MakeAvailable','RemoveAvailability')}
BRANCH_CHOICES.update(Admission={'job'},ClaimReply={'lost'},Reply={'lost'},RetireReply={'lost'},
                     WorkOther={'reply'},JoinReturn={'reply'},RetireSample={'clock_error'},Environment={'input','ack','resolve_error'})
BRANCH_CHOICES.update(AdapterSample=set(),OrdinaryLock=set(),OrdinaryLocal={'phase'},
 OrdinaryCallReturn={'kind'},OrdinaryObserve={'lost'},WallAdvance={'now'},MonoAdvance={'now'},QueueLossSignal=set(),ClaimRecover={'kind'})
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
    return json.dumps(value, sort_keys=True, separators=(',',':'), ensure_ascii=False, allow_nan=False)

def logical_copy(value):
    """Copy logical JSON occurrences without deepcopy's alias-preserving memo.

    Decoder pools are borrowed read-only values. A semantic transition owns a
    separate mutable tree at every occurrence, even when equal workers, nested
    payloads, or arrays refer to the same pooled object in its predecessor.
    Only transient predecessors/assigned fields are copied; pools stay shared.
    """
    if isinstance(value,dict): return {key:logical_copy(item) for key,item in value.items()}
    if isinstance(value,list): return [logical_copy(item) for item in value]
    if isinstance(value,tuple): return tuple(logical_copy(item) for item in value)
    return value


def canonical_measure(value):
    """Exact canonical byte length/hash without duplicating a potentially 256MiB bundle."""
    digest = hashlib.sha256();size = 0
    encoder = json.JSONEncoder(sort_keys=True,separators=(',',':'),ensure_ascii=False,allow_nan=False)
    for chunk in encoder.iterencode(value):
        raw = chunk.encode('utf-8');size += len(raw);digest.update(raw)
    return size,digest.hexdigest()


def strict_json(text):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, f'duplicate JSON key: {key}')
            result[key] = value
        return result
    def invalid_constant(value):
        raise Invalid('non-finite JSON scalar: '+value)
    return json.loads(text, object_pairs_hook=unique, parse_constant=invalid_constant)

def read_json(path):
    return strict_json(Path(path).read_text())

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
            add('settle',job=j,token=i,disposition=2)
            add('resolve',job=j,token=i)
            if any(later['claim_id'] == t['claim_id'] for later in tokens[i+1:]): continue
            if t['claim_id'] not in s['budgets']['renewed_claims']: add('renew',job=j,token=i)
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
    if e['action']['op'] not in {'resolve','resolve_initial_claim'}:
        require(e['resolved_token'] is None, 'non-resolve returned a resolution token')
        return
    for field in ('now','jobs','domain','captured_leases','budgets'):
        require(before[field] == after[field], f'resolve changed read-only projection: {field}')
    if 'raw' in before:
        require(before['raw']['rows'] == after['raw']['rows'], 'resolve changed authoritative durable rows')
        require(before['raw']['episode']['claim_snapshots'] == after['raw']['episode']['claim_snapshots'], 'resolve invented a claimed snapshot')
    j = e['action']['job']
    returned = e['resolved_token']
    if e['result'] in {'running','cancel_requested'}:
        require(integer(returned,0,len(after['captured_tokens'][j])-1), 'missing resolved token index')
        initial = e['action']['op'] == 'resolve_initial_claim'
        original = e['action']['request'] if initial else before['captured_tokens'][j][e['action']['token']]
        token = after['captured_tokens'][j][returned]
        require(all(original[k] == token[k] for k in (('job_id','node_id','boot_id','claim_id') if initial else ('job_id','node_id','boot_id','claim_id','fence'))),
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
    exact_keys(ep, {'now','claims','tokens','renewed_claims','leases','domain_acquired','removed','claim_snapshots'}, 'canonical episode')
    require(ep == {'now':s['now'],'claims':s['budgets']['claims'],'tokens':s['captured_tokens'],
        'renewed_claims':s['budgets']['renewed_claims'],'leases':s['captured_leases'],
        'domain_acquired':s['budgets']['domain_acquired'],'removed':s['budgets']['removed'],
        'claim_snapshots':ep['claim_snapshots']},
        'canonical episode/projection disagreement')
    expiries = {t['lease_expires_ms'] for ts in s['captured_tokens'] for t in ts}
    require(all(j['expiry'] is None or j['expiry'] in expiries for j in s['jobs']),
            'durable expiry/capture boundary disagreement')


def product_payload(p):
    return {key:p[key] for key in PRODUCT_VALUE_FIELDS}


def retry_delay_ms(raw_job_id,failed_attempts):
    require(isinstance(raw_job_id,str) and integer(failed_attempts), 'invalid retry snapshot input')
    base = {0:5000,1:30000,2:120000}.get(failed_attempts,600000)
    value = failed_attempts & (2**64-1)
    for byte in raw_job_id.encode('utf-8'): value = (value*31+byte) & (2**64-1)
    return base + value % (base//5)


def settlement_after_reply(previous,pending,adapter_reply):
    tag = adapter_reply['tag']
    if tag != 'DomainRefused' and pending not in {'ReleaseRefused','ReleaseInterrupted','ReleaseRetry'}:
        return logical_copy(previous) if tag == 'Done' else None
    kind = 'retry' if pending == 'ReleaseRetry' else 'yield'
    delay = adapter_reply['retry_delay_ms'] if kind == 'retry' else 1000 if pending == 'ReleaseInterrupted' else 5000
    return {'kind':kind,'not_before_ms':sat_add(adapter_reply['now_ms'],delay)}


def fixture_jobs(fixture):
    exact_keys(fixture, {'jobs'}, 'fixture identity')
    require(isinstance(fixture['jobs'],list) and len(fixture['jobs']) == 2, 'missing raw fixture IDs')
    ids = []
    for j,entry in enumerate(fixture['jobs']):
        exact_keys(entry, {'job','raw_job_id'}, 'fixture job identity')
        require(entry['job'] == j and isinstance(entry['raw_job_id'],str) and re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}',entry['raw_job_id']), 'invalid raw fixture UUID')
        ids.append(entry['raw_job_id'])
    require(len(set(ids)) == 2, 'raw fixture IDs are not distinct')
    return ids


def validate_claim_snapshot_identity(snapshot,raw_ids):
    exact_keys(snapshot, {'job','raw_job_id','failed_attempts','claim_id','token_capture'}, 'actual claimed snapshot')
    require(integer(snapshot['job'],0,1) and snapshot['raw_job_id'] == raw_ids[snapshot['job']], 'snapshot raw fixture identity mismatch')
    require(integer(snapshot['failed_attempts'],0,I64_MAX), 'invalid immutable failed-attempt snapshot')
    token = snapshot['token_capture']
    exact_keys(token, {'job_id','node_id','boot_id','claim_id','fence','revision','lease_expires_ms'}, 'claimed snapshot JobToken')
    require(token['job_id'] == snapshot['raw_job_id'] and token['claim_id'] == snapshot['claim_id'], 'snapshot immutable token mismatch')
    require(all(isinstance(token[k],str) and token[k] for k in ('boot_id','claim_id')), 'invalid claimed opaque identity')
    require(token['node_id'] in {'node-a','node-b'} and all(integer(token[k],1,I64_MAX) for k in ('fence','revision','lease_expires_ms')), 'invalid claimed token fields')

def sat_add(value,delay): return max(-(2**63),min(I64_MAX,value+delay))


def certify_tlc_numbers(value,label='model input'):
    """Pinned TLC uses signed-int values. Reject globally; never truncate D."""
    if type(value) is int:
        require(TLC_INT_MIN <= value <= TLC_INT_MAX, 'UnsupportedTlcNumericDomain: '+label)
    elif type(value) is float:
        raise Invalid('UnsupportedTlcNumericDomain: '+label+' (non-integer JSON number)')
    elif isinstance(value,dict):
        for key,item in value.items(): certify_tlc_numbers(item,label+'.'+str(key))
    elif isinstance(value,(list,tuple)):
        for item in value: certify_tlc_numbers(item,label+'[]')


def certify_tlc_arithmetic(p,store):
    """Check every arithmetic operand/result used by original Next at this state.

    Source helpers retain full i64 behavior. This finite artifact certificate
    establishes that saturation is unreachable and TLA ordinary addition is
    exact; an incompatible artifact fails before any solver invocation.
    """
    s = store['states'][p['sql']]
    for anchor in p['wall_anchors']+p['mono_anchors']:
        certify_tlc_numbers((anchor-1,anchor+1),'boundary neighbours')
    certify_tlc_numbers((len(store['states'])+1,len(store['edges'])+1,len(p['claim_loss_keys'])+1),'index/count arithmetic')
    for w in p['w']:
        if w['claim_recovery']['phase'] != 'off': certify_tlc_numbers(p['mono_now_ms']+250,'source retry wake addition')
        issued = w['claim_issued_mono_ms']
        if issued is not None: certify_tlc_numbers(issued+JOB_LEASE_MS,'initial claim deadline')
        # All actual returned captures cover direct claim/renew and resolution.
        for token in (t for tokens in s['captured_tokens'] for t in tokens):
            for request in (s['now'],w['claim_request_now_ms']):
                if request is None: continue
                delta = token['lease_expires_ms']-request
                certify_tlc_numbers(delta,'expiry minus request sample')
                certify_tlc_numbers(p['mono_now_ms']+max(0,min(JOB_LEASE_MS,delta)), 'source local deadline')
        reply = w['adapter_reply']
        if reply is not None and reply['now_ms'] is not None:
            for delay in (1000,5000,reply['retry_delay_ms']):
                result = reply['now_ms']+delay
                certify_tlc_numbers(result,'saved settlement deadline addition')
                require(sat_add(reply['now_ms'],delay) == result, 'UnsupportedTlcNumericDomain: source i64 saturation differs')


def mono_deadline(issued,expiry,request_now):
    result = issued + max(0,min(JOB_LEASE_MS,expiry-request_now))
    require(0 <= result <= I64_MAX, 'monotonic deadline outside selected i64 domain')
    return result


def next_landmark(now,anchors):
    later = [a+d for a in anchors for d in (-1,0,1) if now < a+d <= I64_MAX and a+d >= 0]
    return min(later) if later else None


def anchor_payload(q,store):
    s = store['states'][q['sql']]; wall = set(q['wall_anchors']); mono = set(q['mono_anchors'])
    wall.update(j['expiry'] for j in s['jobs'] if j['expiry'] is not None)
    wall.update(j['not_before_ms'] for j in s['jobs'] if j['state'] == 'queued')
    wall.update(t['lease_expires_ms'] for ts in s['captured_tokens'] for t in ts)
    wall.update(l['expires_at_unix_ms'] for l in s['captured_leases'])
    if s['domain'] is not None: wall.add(s['domain']['expiry'])
    for w in q['w']:
        for payload in (w['driver_settlement'],w['ordinary']['payload']):
            if payload is not None: wall.add(payload['not_before_ms'])
        if w['claim_issued_mono_ms'] is not None and (w['token'] >= 0 or w['claim_recovery']['phase'] != 'off'):
            mono.add(w['claim_issued_mono_ms']+JOB_LEASE_MS)
        if w['claim_recovery']['wake_mono_ms'] is not None: mono.add(w['claim_recovery']['wake_mono_ms'])
        if w['local_deadline_mono_ms'] is not None: mono.add(w['local_deadline_mono_ms'])
    require(all(integer(v,0,I64_MAX) for v in wall|mono), 'anchor outside normal finite clock domain')
    q['wall_anchors'] = sorted(wall);q['mono_anchors'] = sorted(mono)
    return q


def holds_selected_domain(w,driver):
    if w['stage'] != 'driver' or w['lease'] < 0: return False
    d = driver['states'][w['ctrl']]; pending = d['pending']
    if pending in RELEASE_PENDING and w['call'] in {'applied','replied'}: return False
    return d['held']['domain'] or pending == 'AcquireDomain'


def stable_wall_allowed(p,s,target,driver,*,force=False):
    if not force and not CONTEXTS[p['context']][0]: return True
    for w in p['w']:
        live = w['localToken'] or w['stage'] == 'claim' and w['token'] >= 0
        if live and target >= s['captured_tokens'][w['job']][w['token']]['lease_expires_ms']: return False
        if holds_selected_domain(w,driver) and target >= s['captured_leases'][w['lease']]['expires_at_unix_ms']: return False
        if w['ordinary']['phase'] in {'waiting_lock','check_lost','check_deadline','take_token','sample'}:
            if target > w['ordinary']['payload']['not_before_ms']: return False
    return True


def stable_mono_allowed(p,target):
    if not CONTEXTS[p['context']][0]: return True
    for w in p['w']:
        if w['stage'] == 'driver' and w['started'] and not w['dropped']:
            if target >= w['local_deadline_mono_ms']: return False
        if w['stage'] == 'claim' and w['token'] >= 0:
            if target >= min(w['claim_issued_mono_ms']+JOB_LEASE_MS,w['local_deadline_mono_ms']): return False
    return True


def ordinary_input(w):
    o = w['ordinary']
    return {'op':'ordinary_settle','job':w['job'],'token':o['captured_token'],
        **o['payload'],'request_now_ms':o['request_now_ms']}


CLAIM_REQUEST_FIELDS = {'job_id','expected_revision','node_id','boot_id','claim_id','kind','payload_version','now_ms','dispatched_at_ms'}
JOB_READ_FIELDS = {'id','payload_version','payload','dedupe_key','priority','state','token','fence','revision',
 'failed_attempts','attempt_limit','retry_deadline_ms','attempt_errors','index_diagnostic_json','yield_count',
 'abandoned_count','not_before_ms','checkpoint','result_ref','last_error_code','created_at_ms','updated_at_ms'}


def claim_recovery_enabled(p,w):
    r = w['claim_recovery']
    return (w['stage'] == 'claim' and w['reply'] == 'Unknown' and
      (r['phase'] == 'ready' or r['phase'] == 'waiting' and p['mono_now_ms'] >= r['wake_mono_ms']) and
      p['mono_now_ms'] < w['claim_issued_mono_ms']+JOB_LEASE_MS)


def initial_resolution_input(w,s):
    request = w['claim_request']
    return {'op':'resolve_initial_claim','job':w['job'],'request':{
      **{k:request[k] for k in ('job_id','node_id','boot_id','claim_id','dispatched_at_ms')},
      'fence':None,'now_ms':s['now']}}


def claim_loss_key(w):
    return {'job':w['job'],'node':0 if w['claim_request']['node_id'] == 'node-a' else 1,'claim_id':w['claim_request']['claim_id']}


def supported_recovery_read(job,request,token):
    if job is None or job['token'] != token or job['payload_version'] != 1: return False
    payload = job['payload']
    return (isinstance(payload,dict) and set(payload) == {'kind','library_id','generation'} and
      payload['kind'] == request['kind'] and payload['kind'] in {'library_scan','metadata_refresh'} and
      integer(payload['library_id'],1,I64_MAX) and isinstance(payload['generation'],str) and
      re.fullmatch(r'[A-Za-z0-9._:-]{1,256}',payload['generation']) is not None)


def recovered_snapshot(job,index):
    return {'job':index,'raw_job_id':job['id'],'failed_attempts':job['failed_attempts'],
            'claim_id':job['token']['claim_id'],'token_capture':logical_copy(job['token'])}


def classify_product_input(p,driver,store,action):
    s = store['states'][p['sql']]
    if action['op'] == 'resolve_initial_claim':
        require(any(claim_recovery_enabled(p,w) and initial_resolution_input(w,s) == action for w in p['w']), 'initial resolve is not a source recovery request')
        return None
    if action['op'] == 'ordinary_settle':
        require(any(w['stage'] == 'driver' and w['ordinary']['phase'] == 'sent' and
                    ordinary_input(w) == action for w in p['w']), 'ordinary input is not a saved sent request')
        return None
    if action['op'] == 'clock_advance':
        require(action['now'] == next_landmark(s['now'],p['wall_anchors']), 'contextual clock input is not next landmark')
        return None if stable_wall_allowed(p,s,action['now'],driver) else {'kind':'domain','reason':'stable_wall_window'}
    return classify_domain(s,action)


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
        ordinary = op != 'settle'
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
        if op not in {'remove_owner','acquire_domain','release_domain','renew'}: continue
        if op == 'acquire_domain' and a['node'] != 2: continue
        if op == 'release_domain' and s['captured_leases'][a['lease']]['owner_node_id'] != 'foreign-holder': continue
        if op == 'renew':
            matches = [w for n,w in enumerate(p['w']) if same_claim(p,s,n,a)]
            if any(not w['localToken'] or w['queue_lost'] or w['token'] != a['token'] or
                   w['ordinary']['phase'] in ORDINARY_LOCKED or
                   not (w['call'] == 'ready' or w['retire'] == 'drain' or w['ordinary']['phase'] in {'waiting_lock','done'}) for w in matches): continue
        yield a


def expected_product_demands(p, driver, store):
    """All structural primary demands, before consulting any oracle outcome."""
    s = store['states'][p['sql']]; stable,_ = CONTEXTS[p['context']]; out = []
    def add(family,n,role,op,**fields):
        w = p['w'][n] if n is not None else None; missing = None; sample = None
        if fields.get('token') == -1: missing = 'missing_token'
        elif fields.get('lease') == -1: missing = 'missing_lease'
        action = None if missing else {'op':op,**fields}
        if family == 'RetireApply' or role == 'capacity' and w is not None and w['retire'] in {'first','resolve','second'}:
            sample = w['sample']
        exclusion = classify_domain(s,action,missing=missing,sample=sample) if missing or sample is not None else classify_product_input(p,driver,store,action)
        out.append({'context':p['context'],'from':p['id'],'oracle_from':p['sql'],
            'branch':{'family':family,'worker':n},'role':role,'input':action,'exclusion':exclusion})
    def call(family,n,role,pending):
        w = p['w'][n]; j,t,l = w['job'],w['token'],w['lease']
        if pending == 'AcquireDomain': add(family,n,role,'acquire_domain',node=n)
        elif pending == 'Bind': add(family,n,role,'bind',job=j,token=t,lease=l)
        elif pending in RELEASE_PENDING: add(family,n,role,'release_domain',lease=l)
        elif pending == 'PollWork': add(family,n,role,'complete',job=j,token=t,lease=l)
        elif pending == 'RetireQueue':
            if w['retire'] == 'resolve': add(family,n,role,'resolve',job=j,token=w['original'])
            else: add(family,n,role,'settle',job=j,
                token=w['original'] if w['retire'] == 'first' else w['resolved'],
                disposition=2 if w['cancelling'] or w['retire'] == 'second' and w['outcome'] == 'cancel_requested' else 3)
    for n,w in enumerate(p['w']):
        if claim_recovery_enabled(p,w):
            action = initial_resolution_input(w,s)
            add('ClaimRecover',n,'apply','resolve_initial_claim',job=w['job'],request=action['request'])
        if w['stage'] == 'claim' and w['call'] == 'sent': add('ClaimApply',n,'apply','claim',job=w['job'],node=n)
        if w['stage'] != 'driver' or w['call'] != 'sent': continue
        pending = driver['states'][w['ctrl']]['pending']
        if pending in SQL_PENDING:
            call('StoreApply',n,'apply',pending); call('Capacity',n,'capacity',pending)
        if pending in ORDINARY_PENDING and w['ordinary']['phase'] == 'sent':
            fields = ordinary_input(w); fields.pop('op')
            add('OrdinaryCallReturn',n,'apply','ordinary_settle',**fields)
            add('Capacity',n,'capacity','ordinary_settle',**fields)
        if pending == 'PollWork': call('WorkSuccess',n,'apply',pending)
        if pending == 'RetireQueue' and w['retire'] in {'first','resolve','second'}:
            if w['inner'] in {'ready','sent'}: call('Capacity',n,'capacity',pending)
            if w['inner'] == 'sent': call('RetireApply',n,'apply',pending)
    if not stable:
        for action in environment_inputs(p,s): add('Environment',None,'apply',action['op'],**{k:v for k,v in action.items() if k != 'op'})
    target = next_landmark(s['now'],p['wall_anchors'])
    if target is not None: add('WallAdvance',None,'apply','clock_advance',now=target)
    require(len({canonical(d) for d in out}) == len(out), 'duplicate independently derived demand')
    return out


def ordinary_successors(p,s,store,n,w,put,lookup):
    o = w['ordinary']; phase = o['phase']
    def change(q,**fields): q['w'][n]['ordinary'].update(logical_copy(fields))
    if phase == 'waiting_lock':
        put('OrdinaryLock',n,{},lambda q:change(q,phase='check_lost'))
    elif phase in {'check_lost','check_deadline','take_token','sample','finish'}:
        def local(q,error=False):
            current = q['w'][n]; fields = {}
            if phase == 'check_lost':
                fields = {'lost_seen':w['queue_lost'],'phase':'finish' if w['queue_lost'] else 'check_deadline',
                          'observed_outcome':'local_lost' if w['queue_lost'] else None}
            elif phase == 'check_deadline':
                expired = p['mono_now_ms'] >= w['local_deadline_mono_ms']
                fields = {'guard_mono_ms':p['mono_now_ms'],'deadline_seen':w['local_deadline_mono_ms'],
                    'phase':'finish' if expired else 'take_token','observed_outcome':'local_expired' if expired else None}
            elif phase == 'take_token':
                fields = {'captured_token':w['token'] if w['localToken'] else None,
                    'phase':'sample' if w['localToken'] else 'finish','observed_outcome':None if w['localToken'] else 'local_no_token'}
            elif phase == 'sample':
                fields = {'phase':'finish' if error else 'sent','request_now_ms':None if error else s['now'],
                          'observed_outcome':'clock_error' if error else None}
                current['faultUsed'] = current['faultUsed'] or error
            else:
                if o['observed_outcome'] == 'accepted': current['localToken'] = False
                current.update(call='replied',reply='Done',adapter_reply={'tag':'Done','now_ms':None,'retry_delay_ms':None})
                fields = {'phase':'done'}
            change(q,**fields)
        for error in ([False,True] if phase == 'sample' and not w['faultUsed'] else [False]):
            choice = {'phase':phase,**({'clock_error':error} if phase == 'sample' else {})}
            put('OrdinaryLocal',n,choice,lambda q,error=error:local(q,error))
    elif phase == 'sent':
        pair = lookup('OrdinaryCallReturn',n)
        if pair is not None:
            i,e = pair
            def apply(q):
                q['sql'] = e['to'];q['completed'] = sorted(set(q['completed'])|{j for j,x in enumerate(store['states'][e['to']]['jobs']) if x['request_done']})
                change(q,phase='applied',api_outcome=e['result'])
            put('OrdinaryCallReturn',n,{'kind':'apply'},apply,i)
        if not w['faultUsed']:
            def fail(q):
                q['w'][n]['faultUsed'] = True;change(q,phase='finish',observed_outcome='transport_error')
            put('OrdinaryCallReturn',n,{'kind':'error_before_apply'},fail)
    elif phase == 'applied':
        for lost in ([False] if w['faultUsed'] else [False,True]):
            def observe(q,lost=lost):
                q['w'][n]['faultUsed'] = w['faultUsed'] or lost
                change(q,phase='finish',observed_outcome='transport_error' if lost else o['api_outcome'])
            put('OrdinaryObserve',n,{'lost':lost},observe)


def expected_product_successors(p,driver,store,oracle):
    """Full donor Next, independently transcribed. No fairness prefix pruning."""
    stable,mutation = CONTEXTS[p['context']]; s = store['states'][p['sql']]; out = []
    demands = expected_product_demands(p,driver,store)
    primary = list(demands)
    demands += dependent_resolve_demands(p,driver,store,oracle,primary) + dependent_claim_demands(p,store,oracle,primary)
    def lookup(family,n,action=None,source=None):
        matches = [d for d in demands if d['role'] == 'apply' and d['branch'] == {'family':family,'worker':n}
                   and (action is None or d['input'] == action)]
        require(len(matches) == 1, 'structural apply demand is not unique')
        d = matches[0]
        if d['exclusion'] is not None: return None
        key = (d['oracle_from'],canonical(d['input']))
        require(source is None or d['oracle_from'] == source, 'dependent oracle predecessor mismatch')
        require(key in oracle, 'MissingProductOracleEdge: eligible apply demand')
        i = oracle[key]; return i,store['edges'][i]
    def reply_allowed(n,tag):
        return any(e['from'] == p['w'][n]['ctrl'] and e['reply'] == tag for e in driver['edges'])
    def put(family,n,choice,change,edge=None,resolution=None,read=None,cleanup=None):
        q = logical_copy(product_payload(p))
        change(q)
        anchor_payload(q,store)
        members = set()
        if family in FAIR_FAMILIES: members.add(family+'/'+str(n))
        if family == 'ClaimApply' and edge is not None and store['edges'][edge]['result'] == 'claimed':
            members.add('ClaimWin/'+str(n))
        if family in {'WorkSuccess','WorkOther'}: members.add('ExternalReturn/'+str(n))
        if family in RETIRE_LOCAL: members.add('RetireLocal/'+str(n))
        if family in {'RetireApply','RetireError'}: members.add('RetireCallReturn/'+str(n))
        if family == 'WallAdvance' and stable and any(job['state'] == 'queued' and not job['request_done'] and job['not_before_ms'] > s['now'] for job in s['jobs']):
            members.add('EligibilityAdvance/global')
        if family == 'MonoAdvance' and any(x['claim_recovery']['phase'] == 'waiting' and p['mono_now_ms'] < x['claim_recovery']['wake_mono_ms'] for x in p['w']):
            members.add('ClaimRecoveryWake/global')
        out.append(({'family':family,'worker':n,'choice':choice},q,edge,members,resolution,read,cleanup))
    def sql_update(q,e):
        q['sql'] = e['to']
        q['completed'] = sorted(set(q['completed']) | {j for j,job in enumerate(store['states'][e['to']]['jobs']) if job['request_done']})
    def local(q,n,tag):
        q['w'][n].update(call='applied',reply=tag,accepted=False)
        if tag == 'QueueLoss': q['w'][n]['queue_lost'] = True
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
        def update(q, **fields): q['w'][n].update(logical_copy(fields))
        if w['stage'] == 'idle' and p['available'][n]:
            for j,job in enumerate(s['jobs']):
                if job['state'] == 'queued' and not job['request_done'] and job['not_before_ms'] <= s['now'] and (not stable or j == n):
                    def admit(q,j=j): q['w'][n] = {**logical_copy(IDLE),'stage':'claim','job':j,'admission':True}
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
                            token=len(tokens)-1 if success else -1,attempt=tokens[-1]['claim_id'] if success else '',
                            claim_snapshot=logical_copy(e['claim_snapshot']) if success else None,
                            claim_request=logical_copy(e['claim_request']),claim_issued_mono_ms=p['mono_now_ms'],
                            claim_request_now_ms=s['now'],
                            local_deadline_mono_ms=mono_deadline(p['mono_now_ms'],tokens[-1]['lease_expires_ms'],s['now']) if success else None)
                    put('ClaimApply',n,{},claimed,i)
            if w['call'] == 'applied':
                key = claim_loss_key(w)
                can_lose = not stable and not w['faultUsed'] and key not in p['claim_loss_keys']
                for lost in ([False,True] if can_lose else [False]):
                    def claim_reply(q,lost=lost):
                        timely = w['token'] < 0 or p['mono_now_ms'] < min(w['claim_issued_mono_ms']+JOB_LEASE_MS,w['local_deadline_mono_ms'])
                        update(q,call='replied',reply='Unknown' if lost else w['reply'] if timely else 'Refused',
                            faultUsed=w['faultUsed'] or lost,
                            claim_snapshot=None if lost or not timely or w['reply'] != 'Claimed' else w['claim_snapshot'],
                            local_deadline_mono_ms=None if lost or not timely or w['reply'] != 'Claimed' else w['local_deadline_mono_ms'],
                            claim_recovery={'phase':'ready' if lost else 'off','wake_mono_ms':None})
                        if not lost and (not timely or w['reply'] != 'Claimed'):
                            update(q,claim_request=None,claim_issued_mono_ms=None,claim_request_now_ms=None)
                        if lost: q['claim_loss_keys'] = sorted(q['claim_loss_keys']+[key],key=lambda x:(x['job'],x['node'],x['claim_id']))
                    put('ClaimReply',n,{'lost':lost},claim_reply)
            if w['reply'] == 'Unknown' and w['claim_recovery']['phase'] != 'off':
                if p['mono_now_ms'] >= w['claim_issued_mono_ms']+JOB_LEASE_MS:
                    put('ClaimRecover',n,{'kind':'timeout'},lambda q:update(q,reply='Refused',token=-1,attempt='',claim_snapshot=None,local_deadline_mono_ms=None,claim_recovery={'phase':'off','wake_mono_ms':None}))
                elif claim_recovery_enabled(p,w):
                    i,e = lookup('ClaimRecover',n);read=cleanup=None
                    if e['result'] == 'running': read = lookup('ClaimRecoverRead',n,source=e['to'])
                    if e['result'] == 'cancel_requested': cleanup = lookup('ClaimRecoverCancel',n,source=e['to'])
                    def recover(q,e=e,read=read,cleanup=cleanup):
                        sql_update(q,e)
                        returned = None if e['resolved_token'] is None else store['states'][e['to']]['captured_tokens'][w['job']][e['resolved_token']]
                        job = read[1]['job_read'] if read is not None else None
                        if read is not None: sql_update(q,read[1])
                        if cleanup is not None: sql_update(q,cleanup[1])
                        if e['result'] == 'running' and supported_recovery_read(job,w['claim_request'],returned):
                            deadline = mono_deadline(w['claim_issued_mono_ms'],returned['lease_expires_ms'],w['claim_request']['now_ms'])
                            update(q,reply='Claimed' if p['mono_now_ms'] < deadline else 'Refused',
                                token=e['resolved_token'] if p['mono_now_ms'] < deadline else -1,attempt=returned['claim_id'] if p['mono_now_ms'] < deadline else '',claim_snapshot=recovered_snapshot(job,w['job']) if p['mono_now_ms'] < deadline else None,
                                local_deadline_mono_ms=deadline if p['mono_now_ms'] < deadline else None,claim_recovery={'phase':'off','wake_mono_ms':None})
                        elif e['result'] in {'cancel_requested','settled','lost_ownership'}:
                            update(q,reply='Refused',token=-1,attempt='',claim_snapshot=None,local_deadline_mono_ms=None,claim_recovery={'phase':'off','wake_mono_ms':None})
                        else:
                            update(q,claim_recovery={'phase':'waiting','wake_mono_ms':min(p['mono_now_ms']+250,w['claim_issued_mono_ms']+JOB_LEASE_MS)})
                    put('ClaimRecover',n,{'kind':'resolve'},recover,i,read=read[0] if read else None,cleanup=cleanup[0] if cleanup else None)
            if w['call'] == 'replied' and w['reply'] != 'Unknown':
                def claim_return(q):
                    if w['reply'] == 'Claimed' and p['mono_now_ms'] < w['local_deadline_mono_ms']:
                        update(q,stage='driver',ctrl=0,call='ready',started=True,localToken=True)
                    else: q['w'][n] = logical_copy(IDLE)
                put('ClaimReturn',n,{},claim_return)
        if w['stage'] != 'driver':
            # Availability is independent of the local stage and stable premise.
            family = 'RemoveAvailability' if p['available'][n] else 'MakeAvailable'
            put(family,n,{},lambda q:q['available'].__setitem__(n,not p['available'][n]))
            continue
        d = driver['states'][w['ctrl']]; pending = d['pending']
        if not d['terminal'] and w['call'] == 'ready':
            def dispatch(q):
                update(q,call='sent',readLock=pending == 'PollWork',accepted=False,adapter_reply=None)
                if pending in ORDINARY_PENDING:
                    require(w['driver_settlement'] is not None, 'ordinary dispatch without saved Driver payload')
                    payload = {**logical_copy(w['driver_settlement']),'error_code':'library_execution_failed' if pending == 'SettleRetry' else None,'checkpoint':None}
                    update(q,ordinary={**logical_copy(ORDINARY),'phase':'waiting_lock','payload':payload})
            put('Dispatch',n,{},dispatch)
        if w['call'] == 'sent':
            if pending in SQL_PENDING:
                pair = lookup('StoreApply',n)
                if pair is not None:
                    i,e = pair
                    if pending == 'AcquireDomain' and (not stable or e['result'] == 'acquired'):
                        commit('StoreApply',n,'DomainAcquired' if e['result'] == 'acquired' else 'DomainRefused',pair)
                    elif pending == 'Bind' and (not stable or e['result'] == 'accepted'):
                        commit('StoreApply',n,'BindAccepted' if e['result'] == 'accepted' else 'BindRefused',pair)
                    elif pending in RELEASE_PENDING: commit('StoreApply',n,'ReleaseReturned',pair)
                if not stable and not w['faultUsed']:
                    tag = ('DomainRefused' if pending == 'AcquireDomain' else 'BindRefused' if pending == 'Bind'
                           else 'ReleaseReturned')
                    if reply_allowed(n,tag):
                        def ambiguous(q): local(q,n,tag);q['w'][n]['faultUsed'] = True
                        put('AmbiguousWithoutApply',n,{},ambiguous)
            if pending in {'RevokeDomain','CancelWork','DropPublisher','DropAdmission'} and not w['readLock']:
                local_branch('SimpleApply',n,'Done')
            if pending == 'PollWork':
                pair = lookup('WorkSuccess',n)
                if not w['queue_lost'] and pair is not None and pair[1]['result'] == 'accepted': commit('WorkSuccess',n,'WorkSucceeded',pair)
                if not stable:
                    for tag in ('QueueLoss','DomainLoss','Pressure','WorkFailed'):
                        if (tag == 'QueueLoss') != w['queue_lost']: continue
                        local_branch('WorkOther',n,tag,{'reply':tag})
            if pending == 'JoinWork' and mutation != 'withheld-join':
                work = driver_work_class(d['work'])
                tags = ['JoinedSucceeded'] if work == 'completed_succeeded' else ['JoinedFailed'] if work == 'completed_failed' else ['JoinedFailed','JoinedSucceeded']
                for tag in tags: local_branch('JoinReturn',n,tag,{'reply':tag})
            if pending == 'RetireQueue':
                r,inner = w['retire'],w['inner']
                if r == 'off' and not w['readLock']: put('RetireBegin',n,{},lambda q:update(q,retire='drain',queue_lost=True))
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
        if w['call'] == 'sent' and pending in ORDINARY_PENDING:
            ordinary_successors(p,s,store,n,w,put,lookup)
        if w['call'] == 'applied' and pending not in ORDINARY_PENDING:
            lose = (not w['faultUsed'] and pending in SQL_PENDING | {'PollWork'} and
                    (not stable or pending in RELEASE_PENDING | {'PollWork'}))
            for lost in ([False,True] if lose else [False]):
                def reply(q,lost=lost):
                    tag = {'WorkSucceeded':'WorkFailed','DomainAcquired':'DomainRefused','BindAccepted':'BindRefused'}.get(w['reply'],w['reply']) if lost else w['reply']
                    update(q,call='replied',readLock=False,reply=tag,faultUsed=w['faultUsed'] or lost,
                        adapter_reply=None if tag in {'DomainRefused','DomainAcquired','ReleaseReturned'} else {'tag':tag,'now_ms':None,'retry_delay_ms':None})
                put('Reply',n,{'lost':lost},reply)
        if w['call'] == 'replied':
            if w['adapter_reply'] is None:
                def adapter_sample(q):
                    now = 0 if w['reply'] == 'DomainAcquired' or pending == 'ReleaseNone' else s['now']
                    delay = retry_delay_ms(w['claim_snapshot']['raw_job_id'],w['claim_snapshot']['failed_attempts']) if pending == 'ReleaseRetry' else 0
                    update(q,adapter_reply={'tag':w['reply'],'now_ms':now,'retry_delay_ms':delay})
                put('AdapterSample',n,{},adapter_sample)
            else:
                for edge in driver['edges']:
                    if edge['from'] != w['ctrl'] or edge['reply'] != w['reply']: continue
                    def returned(q,edge=edge):
                        payload = settlement_after_reply(w['driver_settlement'],pending,w['adapter_reply'])
                        update(q,ctrl=edge['to'],call='ready',driver_settlement=payload,adapter_reply=None,
                            joined=w['joined'] or pending == 'JoinWork',
                            workStarted=w['workStarted'] or pending == 'Bind' and w['reply'] == 'BindAccepted',
                            dropped=w['dropped'] or pending == 'DropAdmission',
                            admission=False if pending == 'DropAdmission' or mutation == 'drop-before-join' and pending == 'CancelWork' else w['admission'])
                    put('DriverReturn',n,{},returned)
        can_recycle = not stable or w['dropped'] and not s['jobs'][w['job']]['request_done'] and s['budgets']['claims'][w['job']] < 2
        if d['terminal'] and can_recycle: put('Recycle',n,{},lambda q:q['w'].__setitem__(n,logical_copy(IDLE)))
        if not stable and w['started'] and not w['dropped'] and not w['queue_lost']:
            put('QueueLossSignal',n,{},lambda q:update(q,queue_lost=True))
        family = 'RemoveAvailability' if p['available'][n] else 'MakeAvailable'
        put(family,n,{},lambda q:q['available'].__setitem__(n,not p['available'][n]))
    if not stable:
        for a in environment_inputs(p,s):
            i,e = lookup('Environment',None,a)
            matches = [n for n in range(2) if a['op'] == 'renew' and same_claim(p,s,n,a)]
            ambiguous = e['result'] == 'renewed' and any(not p['w'][n]['faultUsed'] for n in matches)
            for ack in ([False,True] if ambiguous else [True]):
                needs_resolve = bool(matches) and (e['result'] != 'renewed' or not ack)
                resolution = lookup('EnvironmentResolve',None,{'op':'resolve','job':a['job'],'token':a['token']},e['to']) if needs_resolve else None
                can_error = needs_resolve and ack and all(not p['w'][n]['faultUsed'] for n in matches)
                for resolve_error in ([False,True] if can_error else [False]):
                    def environment(q,a=a,e=e,ack=ack,matches=matches,resolution=resolution,resolve_error=resolve_error):
                        sql_update(q,e)
                        if not matches: return
                        if resolution is not None and not resolve_error: sql_update(q,resolution[1])
                        for n in matches:
                            current = q['w'][n]; old = p['w'][n]
                            current['faultUsed'] = old['faultUsed'] or not ack or resolve_error
                            if resolve_error:
                                current['queue_lost'] = True;continue
                            outcome = 'running' if resolution is None else resolution[1]['result']
                            if outcome in {'running','cancel_requested'}:
                                token_index = len(store['states'][e['to']]['captured_tokens'][a['job']])-1 if resolution is None else resolution[1]['resolved_token']
                                require(token_index is not None, 'resolution omitted source returned token')
                                current['token'] = token_index;current['localToken'] = True
                                if outcome == 'cancel_requested':
                                    current.update(cancelling=True,queue_lost=True);continue
                                if old['queue_lost'] or p['mono_now_ms'] >= old['local_deadline_mono_ms']:
                                    current['queue_lost'] = True
                                else:
                                    returned = store['states'][q['sql']]['captured_tokens'][a['job']][token_index]
                                    current['local_deadline_mono_ms'] = mono_deadline(p['mono_now_ms'],returned['lease_expires_ms'],s['now'])
                                    if p['mono_now_ms'] >= current['local_deadline_mono_ms']: current['queue_lost'] = True
                            else:
                                current.update(localToken=False,queue_lost=True)
                    put('Environment',None,{'input':a,'ack':ack,'resolve_error':resolve_error},environment,i,
                        resolution[0] if resolution is not None and not resolve_error else None)
    wall = next_landmark(s['now'],p['wall_anchors'])
    if wall is not None and stable_wall_allowed(p,s,wall,driver):
        i,e = lookup('WallAdvance',None)
        put('WallAdvance',None,{'now':wall},lambda q:sql_update(q,e),i)
    mono = next_landmark(p['mono_now_ms'],p['mono_anchors'])
    if mono is not None and stable_mono_allowed(p,mono):
        put('MonoAdvance',None,{'now':mono},lambda q:q.__setitem__('mono_now_ms',mono))
    return out


def validate_store(g, verify_sources=True, *, product=False):
    exact_keys(g, {'schema_version','scope','initial','source_bindings','schema_bindings','domain','coverage','states','edges'}, 'Store root')
    graph_structure(g, 20000)
    domain = g['domain']
    require(domain.get('numeric_quotient') == 'none: every i64/u64 and raw production job ID remains exact', 'numeric quotient is not proved')
    require(domain.get('cap') == 20000, 'Store cap differs from declared D')
    require(domain.get('jobs') == 2 and domain.get('nodes') == ['node-a','node-b'], 'requires exact two-job/two-node D')
    require(domain.get('attempts_per_job') == 2 and domain.get('renewals_per_attempt') == 1, 'attempt/renewal domain drift')
    require(domain.get('rearrival') in (0,False,'none'), 'finite episode does not permit demand re-arrival')
    require(domain.get('physical_capacity_per_node') == 1 and domain.get('domain_acquisitions_per_owner') == 1,
            'physical/domain acquisition budget drift')
    require(domain.get('domain_owners') == ['node-a','node-b','foreign-holder'] and
      domain.get('selected_attempt_ids') == 4 and domain.get('reserved_boundary_ids') == 2, 'identity-domain drift')
    require(domain.get('settlement_dispositions') == ['cancel','retirement_yield'], 'settlement input domain drift')
    require(domain.get('ordinary_token_inputs') == 'latest captured revision per claim for intrinsic renew/bind/complete; ordinary saved settlement captures are contextual and may be historical', 'ordinary token input-domain drift')
    require(domain.get('binding_completion_inputs') == 'captured domain lease owner equals captured queue-token node; wrong-owner inputs are separate conformance', 'lease-owner input-domain drift')
    require(domain.get('node_domain_acquisition_inputs') == 'node has selected running durable queue ownership, including expired retained ownership for dispatch/application races; foreign holder is independent', 'node-domain input-domain drift')
    for key, value in STORE_BOUNDARIES.items():
        require(domain.get(key) == value, f'Store boundary drift: {key}')
    bindings = g['source_bindings']
    expected_sources = {'store/'+p for p in ('background_jobs.rs','background_jobs_domain.rs',
      'background_jobs_library.rs','background_jobs_schema.sql','background_jobs_resources.sql',
      'background_jobs_library.sql','background_jobs_domain.sql','subtitle_managed_schema.sql',
 'dv_title_jobs.rs','dv_title_jobs.sql','dv_title_jobs_schema.rs',
      'sqlite/mod.rs','sqlite/background_jobs.rs','sqlite/coordination.rs','async_driver_refinement.rs')}
    expected_sources.add('cluster/coordination.rs')
    if product: expected_sources = set(CORE_SOURCES) | set(EXTERNAL_SOURCES)
    require(isinstance(bindings,dict) and set(bindings) == expected_sources, 'SQL/schema/harness source registry differs')
    for path, digest in bindings.items():
        require(isinstance(path,str) and not Path(path).is_absolute() and '..' not in Path(path).parts,
                'unsafe source binding path')
        require(re.fullmatch('[0-9a-f]{64}',digest) is not None, 'bad source digest')
        require(not verify_sources or sha(binding_path(path)) == digest, f'Store source hash mismatch: {path}')
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
            require(re.fullmatch(r'attempt_budget_j[01]|renewal_budget_.+|domain_acquire_budget(?:_node-a|_node-b|_foreign-holder)?|'+('no_intrinsic_expiry_boundary' if product else 'no_future_boundary'),cutoff) is not None,
                    f'unknown cutoff class: {cutoff}')
            cutoffs[cutoff] += 1
        exact_keys(s['budgets'], {'claims','renewed_claims','domain_acquired','removed'}, 'episode budgets')
        require(len(s['budgets']['claims']) == 2 and all(integer(x,0,2) for x in s['budgets']['claims']), 'invalid claim budgets')
        require(len(s['budgets']['domain_acquired']) == 3 and all(type(x) is bool for x in s['budgets']['domain_acquired']), 'invalid domain acquisition flags')
        require(len(s['budgets']['removed']) == 2 and all(type(x) is bool for x in s['budgets']['removed']), 'invalid removal flags')
        if s['domain'] is not None: exact_keys(s['domain'], {'resource','owner','fence','revision','expiry'}, 'domain row')
        for j, job in enumerate(s['jobs']):
            exact_keys(job, {'state','owner','fence','revision','expiry','failed_attempts','active','request_done','request_state','reservation_count','not_before_ms'}, 'raw job projection')
            require(integer(job['not_before_ms'],0,I64_MAX) and integer(job['failed_attempts'],0,I64_MAX), 'invalid queued eligibility/failed count')
            require(job.get('state') in {'queued','running','succeeded','failed','cancelled'}, 'unknown job state')
            require(job.get('request_state') in {'pending','succeeded','failed','cancelled'}, 'unknown request state')
            require(type(job.get('active')) is bool and type(job.get('request_done')) is bool, 'missing raw activity/request flags')
            require(job.get('owner') in {None,'node-a','node-b'}, 'unknown queue owner')
            require(integer(job.get('reservation_count'),0,6), 'invalid raw reservation count')
            for token in s['captured_tokens'][j]:
                exact_keys(token, {'job_id','node_id','boot_id','claim_id','fence','revision','lease_expires_ms'}, 'captured JobToken')
                for f in ('fence','revision','lease_expires_ms'):
                    require(integer(token[f],1,I64_MAX), 'invalid positive source token payload')
        validate_job_projection(s)
        for lease in s['captured_leases']:
            require(lease.get('owner_node_id') in {'node-a','node-b','foreign-holder'}, 'unknown opaque lease owner')
            for field in ('fence','revision'): require(integer(lease.get(field),0,2**64-1), 'invalid u64 domain field')
            require(integer(lease.get('expires_at_unix_ms'),-(2**63),2**63-1), 'invalid i64 domain expiry')
            exact_keys(lease, {'resource','owner_node_id','fence','revision','expires_at_unix_ms'}, 'captured lease')
    for e in g['edges']:
        exact_keys(e, {'from','to','action','result','resolved_token','claim_snapshot','error','claim_request','job_read'}, 'Store edge')
        a = e['action']; op = a.get('op')
        encoded = canonical(a)
        require(encoded not in outgoing[e['from']], 'duplicate/nondeterministic Store input')
        outgoing[e['from']].add(encoded)
        require(op in ACTIONS, f'unknown Store action: {op}')
        exact_keys(a, ACTIONS[op] | {'op'}, f'Store action {op}')
        require(e['result'] in RESULTS_BY_OP[op], f'unknown result for {op}: {e["result"]}')
        counts[op] += 1; results[e['result']] += 1
        before, after = g['states'][e['from']], g['states'][e['to']]
        require(e['result'] not in {'refused','cancelled','expired_or_pruned','held','validation_error_yield_time','validation_error_retry_time'} or e['from'] == e['to'],
                'FailedCasAtomicity: refused operation changed canonical state')
        for field in ACTIONS[op] - {'kind','error_code','checkpoint','request'}:
            require(integer(a[field]), f'noninteger Store action {field}')
        if 'job' in a: require(a['job'] in (0,1), 'invalid job index')
        if 'node' in a: require(a['node'] in ((0,1,2) if op == 'acquire_domain' else (0,1)), 'invalid node index')
        if 'token' in a: require(0 <= a['token'] < len(before['captured_tokens'][a['job']]), 'missing token target')
        if 'lease' in a: require(0 <= a['lease'] < len(before['captured_leases']), 'missing lease target')
        if 'disposition' in a: require(a['disposition'] in range(4), 'unknown disposition')
        if op == 'clock_advance':
            require(a['now'] > before['now'] and integer(a['now'],0,I64_MAX) and after['now'] == a['now'], 'clock input/projection mismatch')
            expected_raw = logical_copy(before['raw']);expected_raw['episode']['now'] = a['now']
            require(after['raw'] == expected_raw, 'clock changed non-clock canonical data')
        if op == 'ordinary_settle':
            validate_payload({key:a[key] for key in ('kind','not_before_ms','error_code','checkpoint')})
            require(integer(a['request_now_ms'],0,I64_MAX), 'invalid captured SettleJob request time')
            expected_error = 'validation_error_yield_time' if a['kind'] == 'yield' else 'validation_error_retry_time'
            if a['not_before_ms'] < a['request_now_ms']:
                require(e['result'] == expected_error and e['error'] == VALIDATION_ERRORS[expected_error], 'ordinary stale deadline validation outcome mismatch')
            else: require(e['result'] not in VALIDATION_ERRORS and e['error'] is None, 'spurious ordinary validation error')
        else: require(e['error'] is None, 'non-ordinary edge has API validation error')
        before_snapshots,after_snapshots = before['raw']['episode']['claim_snapshots'],after['raw']['episode']['claim_snapshots']
        if op == 'claim' and e['result'] == 'claimed':
            require(e['claim_snapshot']['token_capture']['node_id'] == ('node-a','node-b')[a['node']], 'claim returned another node token')
            validate_snapshot(e['claim_snapshot'],after,a['job'])
            require(e['claim_snapshot']['failed_attempts'] == after['jobs'][a['job']]['failed_attempts'] and
                    e['claim_snapshot']['token_capture'] == after['captured_tokens'][a['job']][-1], 'claim snapshot not actual committed response')
            require(after_snapshots == before_snapshots+[e['claim_snapshot']], 'claim snapshot history drift')
        else: require(e['claim_snapshot'] is None and after_snapshots == before_snapshots, 'non-claim captured another snapshot')
        if op == 'claim':
            request = e['claim_request'];exact_keys(request,CLAIM_REQUEST_FIELDS,'actual original ClaimJob')
            expected_claim = f"00000000-0000-4000-8000-{10+a['job']*4+before['budgets']['claims'][a['job']]:012}"
            require(request['node_id'] == ('node-a','node-b')[a['node']] and
                request['boot_id'] == f"00000000-0000-4000-8000-{a['node']+1:012}" and
                request['claim_id'] == expected_claim and request['expected_revision'] == before['jobs'][a['job']]['revision'] and
                request['kind'] == ('library_scan','metadata_refresh')[a['job']] and request['payload_version'] == 1 and
                request['now_ms'] == request['dispatched_at_ms'] == before['now'], 'actual ClaimJob input/source mismatch')
        else: require(e['claim_request'] is None, 'non-claim supplied an original request')
        if op == 'resolve_initial_claim':
            exact_keys(a['request'],{'job_id','node_id','boot_id','claim_id','fence','dispatched_at_ms','now_ms'},'initial ResolveClaim')
            require(a['request']['fence'] is None and a['request']['now_ms'] == before['now'], 'initial resolve lost original no-fence/current-sample semantics')
        if op == 'read_claim_job':
            require(e['from'] == e['to'], 'actual job reread changed full canonical identity')
            if e['result'] == 'present':
                job = e['job_read'];exact_keys(job,JOB_READ_FIELDS,'actual recovered BackgroundJob')
                for field in ('payload_version','fence','revision','failed_attempts','attempt_limit','retry_deadline_ms','yield_count','abandoned_count','not_before_ms','created_at_ms','updated_at_ms'):
                    require(integer(job[field],-(2**63),I64_MAX), 'invalid actual reread i64: '+field)
                require(integer(job['priority'],0,255) and all(isinstance(job[field],str) for field in ('id','dedupe_key','attempt_errors','index_diagnostic_json')), 'invalid actual reread scalar types')
                require(all(job[field] is None or isinstance(job[field],str) for field in ('result_ref','last_error_code')), 'invalid nullable reread string')
                projection = before['jobs'][a['job']]
                require(all(job[field] == projection[field] for field in ('state','fence','revision','failed_attempts','not_before_ms')),
                    'actual reread differs from durable projection')
                require(job['token'] is None or job['token'] in before['captured_tokens'][a['job']], 'actual reread returned an uncaptured token')
            else: require(e['job_read'] is None,'missing reread carried a row')
        else: require(e['job_read'] is None,'non-reread supplied a recovered row')
        validate_resolution(e,before,after)
        for j in range(2):
            old, new = before['captured_tokens'][j], after['captured_tokens'][j]
            require(new[:len(old)] == old, 'captured token identity is not append-only')
        require(after['captured_leases'][:len(before['captured_leases'])] == before['captured_leases'],
                'captured lease identity is not append-only')
    for s,actual in zip(g['states'],outgoing):
        expected = expected_actions(s)
        intrinsic_actual = {encoded for encoded in actual if json.loads(encoded)['op'] not in {'ordinary_settle','clock_advance','resolve_initial_claim','read_claim_job'}}
        require(intrinsic_actual <= expected if product else actual == expected,
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

SOURCE_BOUNDARIES = {'ordinary_settlement_payload_transfer':{'status':'pending','domain':'selected_two_worker_normal_clock'}}
PRODUCT_KEYS = {'schema_version','scope','driver_graph_sha256','source_bindings','semantic_bindings','source_boundaries',
 'schema_bindings','domain','store','contexts','product_states','product_edges','demands','enabledness','coverage','fixture_identity',
 'value_pool','worker_pool','edge_descriptors','demand_descriptors','product_digest_sequence_sha256'}
# Fixed wire columns are deliberately independent of any producer implementation.
WORKER_COLUMNS = tuple(sorted(IDLE))
WORKER_VALUE_REFS = frozenset({'claim_snapshot','claim_request','claim_recovery',
    'driver_settlement','adapter_reply','ordinary'})
WIRE_COLUMNS = {
    'worker':list(WORKER_COLUMNS),
    'product_state':['context_index','sql','worker0_ref','worker1_ref','available_ref',
        'completed_ref','mono_now_ms','wall_anchors_ref','mono_anchors_ref','claim_loss_keys_ref'],
    'product_edge':['to','edge_descriptor_ref'],
    'edge_descriptor':['branch_ref','oracle_edge','resolution_edge','read_edge','cleanup_edge','fair_actions_ref'],
    'demand':['demand_descriptor_ref'],
    'demand_descriptor':['oracle_from','branch_ref','role','input_ref','oracle_edge','exclusion_ref'],
    'enabledness':['actions_ref'],
    'store_state':['now','jobs_ref','domain_ref','captured_tokens_ref','captured_leases_ref',
        'budgets_ref','cutoffs_ref','canonical_sha256','canonical_fragment_ref']}
WIRE_VALUE_REF_FIELDS = {
    'worker':sorted(WORKER_VALUE_REFS),
    'product_state':['available_ref','completed_ref','wall_anchors_ref','mono_anchors_ref','claim_loss_keys_ref'],
    'product_edge':[], 'edge_descriptor':['branch_ref','fair_actions_ref'],
    'demand':[], 'demand_descriptor':['branch_ref','input_ref','exclusion_ref'], 'enabledness':['actions_ref'],
    'store_state':['jobs_ref','domain_ref','captured_tokens_ref','captured_leases_ref','budgets_ref','cutoffs_ref']}
WIRE_REFERENCE_POOLS = {
    'worker':{field:'value_pool' for field in sorted(WORKER_VALUE_REFS)},
    'product_state':{'context_index':'contexts','sql':'store.states','worker0_ref':'worker_pool',
        'worker1_ref':'worker_pool',**{field:'value_pool' for field in WIRE_VALUE_REF_FIELDS['product_state']}},
    'product_edge':{'to':'product_states','edge_descriptor_ref':'edge_descriptors'},
    'edge_descriptor':{'branch_ref':'value_pool',
        **{field:'store.edges' for field in ('oracle_edge','resolution_edge','read_edge','cleanup_edge')},
        'fair_actions_ref':'value_pool'},
    'demand':{'demand_descriptor_ref':'demand_descriptors'},
    'demand_descriptor':{'oracle_from':'store.states','branch_ref':'value_pool','input_ref':'value_pool',
        'oracle_edge':'store.edges','exclusion_ref':'value_pool'},
    'enabledness':{'actions_ref':'value_pool'},
    'store_state':{**{field:'value_pool' for field in WIRE_VALUE_REF_FIELDS['store_state']},
        'canonical_fragment_ref':'store.canonical_fragments'}}
WIRE_NULLABLE_REFERENCE_FIELDS = {kind:[] for kind in WIRE_COLUMNS}
WIRE_NULLABLE_REFERENCE_FIELDS.update(edge_descriptor=['oracle_edge','resolution_edge','read_edge','cleanup_edge'],
    demand_descriptor=['oracle_edge'])
WIRE_REFERENCE_COLUMNS = {kind:tuple((WIRE_COLUMNS[kind].index(field),field,target,
    field in WIRE_NULLABLE_REFERENCE_FIELDS[kind]) for field,target in targets.items())
    for kind,targets in WIRE_REFERENCE_POOLS.items()}
WIRE_CONTEXT_FIELDS = ['id','model','stable_success','mutation','witness','initial']
WIRE_FRAGMENT_NODES = {'leaf':['leaf','utf8_json_token_text'], 'concat':['concat','earlier_fragment_refs']}
WIRE_DIGEST_SEQUENCE = 'sha256-concatenated-32-raw-product-digests-in-state-id-order'
BUNDLE_MAX_BYTES = 256*1024*1024
ARTIFACT_MAX_BYTES = 512*1024*1024


class LogicalRows(Sequence):
    """Read-only indexed views: no expanded product rows are retained."""
    def __init__(self, rows, decode):
        self.rows, self.decode = rows, decode
    def __len__(self): return len(self.rows)
    def __iter__(self):
        for i,row in enumerate(self.rows): yield self.decode(i,row)
    def __getitem__(self, index):
        if isinstance(index,slice): return [self[i] for i in range(*index.indices(len(self)))]
        if index < 0: index += len(self)
        if not 0 <= index < len(self): raise IndexError(index)
        return self.decode(index,self.rows[index])


class ExactPayloadIndex:
    """Digest buckets accelerate lookup; full canonical bytes decide equality."""
    def __init__(self, states):
        self.states, self.buckets = states, {}
    def find(self, encoded):
        digest = hashlib.sha256(encoded.encode()).digest()
        for identity in self.buckets.get(digest,()):
            if canonical(product_payload(self.states[identity])) == encoded: return identity
        return None
    def __contains__(self, encoded): return self.find(encoded) is not None
    def get(self, encoded, default=None):
        identity = self.find(encoded)
        return default if identity is None else identity
    def __getitem__(self, encoded):
        identity = self.find(encoded)
        if identity is None: raise KeyError(encoded)
        return identity
    def __setitem__(self, encoded, identity):
        self.buckets.setdefault(hashlib.sha256(encoded.encode()).digest(),[]).append(identity)


class GroupedLogicalRows(LogicalRows):
    """Flat positional IDs over per-source occurrences without expanded rows."""
    def __init__(self, groups, decode):
        self.rows, self.decode = groups, decode
        self.offsets = array('Q',[0])
        for group in groups: self.offsets.append(self.offsets[-1]+len(group))
    def __len__(self): return self.offsets[-1]
    def __iter__(self):
        for source,group in enumerate(self.rows):
            for row in group: yield self.decode(source,row)
    def __getitem__(self, index):
        if isinstance(index,slice): return [self[i] for i in range(*index.indices(len(self)))]
        if index < 0: index += len(self)
        if not 0 <= index < len(self): raise IndexError(index)
        source = bisect_right(self.offsets,index)-1
        return self.decode(source,self.rows[source][index-self.offsets[source]])
    def source_indices(self, source): return range(self.offsets[source],self.offsets[source+1])


class CanonicalFragments:
    """Topological exact UTF8 byte fragments; only one root is expanded at a time.

    Leaves contain complete JSON lexical tokens, including punctuation. All
    numeric tokens, even in unused nodes, receive the same TLC domain check.
    Scalar tokens cannot be joined without punctuation or JSON whitespace.
    """
    def __init__(self, nodes):
        require(type(nodes) is list,'canonical fragments must be array')
        self.nodes, self.lengths = nodes, array('Q')
        self.boundaries = []
        seen = set()
        def invalid_constant(token): raise Invalid('non-finite JSON scalar: '+token)
        decoder = json.JSONDecoder(parse_constant=invalid_constant)
        for identity,node in enumerate(nodes):
            require(type(node) is list and len(node) == 2,'invalid canonical fragment shape')
            tag,body = node
            if tag == 'leaf':
                require(type(body) is str and body,'invalid canonical leaf')
                try: length = len(body.encode('utf-8'))
                except UnicodeEncodeError as exc: raise Invalid('canonical leaf is not UTF8') from exc
                position = 0; first = last = None; separated = True
                while position < len(body):
                    char = body[position]
                    if char in ' \t\r\n': position += 1;separated = True;continue
                    if char in '{}[]:,':
                        kind = 'punctuation';position += 1;separated = True
                    else:
                        require(separated or last != 'scalar','canonical leaf joins scalar tokens')
                        try: value,end = decoder.raw_decode(body,position)
                        except (ValueError,RecursionError) as exc: raise Invalid('canonical leaf has incomplete JSON token') from exc
                        # Objects/arrays must use the punctuation path above;
                        # scalar strings remain strings regardless of their text.
                        certify_tlc_numbers(value,'decoded Store canonical JSON fragment')
                        kind = 'scalar';separated = False
                        position = end
                    if first is None: first = kind
                    last = kind
                boundary = (first,last,body[0] in ' \t\r\n',body[-1] in ' \t\r\n')
            else:
                require(tag == 'concat' and type(body) is list and len(body) >= 2,'invalid canonical concat')
                require(all(integer(ref,0,identity-1) for ref in body),'canonical concat reference is not earlier')
                length = 0; boundary = None
                for ref in body:
                    length += self.lengths[ref]
                    require(length <= BUNDLE_MAX_BYTES,'canonical fragment decoded byte bound exceeded')
                    current = self.boundaries[ref]
                    if boundary is None: boundary = current;continue
                    require(not (boundary[1] == 'scalar' and current[0] == 'scalar' and
                        not boundary[3] and not current[2]),'canonical concat joins scalar tokens')
                    boundary = (boundary[0] if boundary[0] is not None else current[0],
                        current[1] if current[1] is not None else boundary[1],boundary[2],current[3])
            require(length <= BUNDLE_MAX_BYTES,'canonical fragment decoded byte bound exceeded')
            encoded = canonical(node)
            require(encoded not in seen,'duplicate exact canonical fragment')
            seen.add(encoded)
            self.lengths.append(length);self.boundaries.append(boundary)
    def text(self, root):
        require(integer(root,0,len(self.nodes)-1),'invalid canonical fragment root')
        # Iterators retain O(depth) traversal state, never an exponentially
        # expanded pending-reference stack or a cache of expanded fragments.
        stream = io.StringIO(); stack = [iter((root,))]
        while stack:
            ref = next(stack[-1],None)
            if ref is None: stack.pop();continue
            tag,body = self.nodes[ref]
            if tag == 'leaf': stream.write(body)
            else: stack.append(iter(body))
        text = stream.getvalue()
        require(len(text.encode('utf-8')) == self.lengths[root],'canonical fragment length mismatch')
        return text


def decode_product(wire):
    """Validate schema5 transport and expose the exact original logical values.

    Pool values and workers are borrowed read-only. Independent successor
    generation copies logical occurrences before every update. Representation
    IDs select wire objects; no digest substitutes for full logical equality.
    """
    exact_keys(wire,PRODUCT_KEYS,'product bundle')
    require(type(wire['schema_version']) is int and wire['schema_version'] == 5 and
        wire['scope'] == 'selected_two_worker_product','unsupported product schema/scope')
    certify_tlc_numbers(wire)
    values, worker_rows, contexts = wire['value_pool'],wire['worker_pool'],wire['contexts']
    require(type(values) is list and type(worker_rows) is list and type(contexts) is list,
        'wire pools/contexts must be arrays')
    exact_keys(wire['store'],{'initial','states','edges','canonical_fragments'},'imported oracle wire relation')
    require(type(wire['store']['states']) is list and type(wire['store']['edges']) is list,
        'Store states/edges must be arrays')
    require(len(wire['store']['states']) <= 20000,'Store canonical identity cap exceeded')
    fragments = CanonicalFragments(wire['store']['canonical_fragments'])
    for name in ('product_states','product_edges','demands','enabledness','edge_descriptors','demand_descriptors'):
        require(type(wire[name]) is list,'wire '+name+' must be array')
    target_counts = {name:len(wire[name]) for name in ('value_pool','worker_pool','contexts','product_states',
        'edge_descriptors','demand_descriptors')}
    target_counts.update({'store.states':len(wire['store']['states']),'store.edges':len(wire['store']['edges']),
        'store.canonical_fragments':len(fragments.nodes)})
    seen = set()
    for value in values:
        certify_tlc_numbers(value,'model input.value_pool')
        encoded = canonical(value)
        require(encoded not in seen,'duplicate exact value_pool entry')
        seen.add(encoded)
    def ref(index):
        require(integer(index,0,len(values)-1),'invalid value_pool reference')
        return values[index]
    def worker_ref(index):
        require(integer(index,0,len(workers)-1),'invalid worker_pool reference')
        return workers[index]
    def context_ref(index):
        require(integer(index,0,len(contexts)-1),'invalid context reference')
        require(type(contexts[index]) is dict and isinstance(contexts[index].get('id'),str),
            'invalid context descriptor')
        return contexts[index]['id']
    def row_shape(row,kind):
        require(type(row) is list and len(row) == len(WIRE_COLUMNS[kind]),'invalid '+kind+' wire row')
        certify_tlc_numbers(row,'model input.'+kind)
        for column,field,target,nullable in WIRE_REFERENCE_COLUMNS[kind]:
            cell = row[column]
            if cell is None and nullable: continue
            require(integer(cell,0,target_counts[target]-1),'invalid '+kind+' '+field+' reference to '+target)
    def branch_shape(index,kind):
        branch = ref(index)
        exact_keys(branch,{'family','worker'} | ({'choice'} if kind == 'edge_descriptor' else set()),
            kind+' branch')
        require(type(branch['family']) is str and branch['family'],'invalid descriptor branch family')
        require(branch['worker'] is None or integer(branch['worker'],0,1),'invalid descriptor branch worker')
        if kind == 'edge_descriptor': require(type(branch['choice']) is dict,'invalid descriptor branch choice')
    fair_vocabulary = set(GLOBAL_FAIR) | {family+'/'+str(worker) for family in FAIR_FAMILIES for worker in (0,1)}
    def fair_shape(index):
        actions = ref(index)
        require(type(actions) is list and all(type(action) is str and action in fair_vocabulary for action in actions),
            'invalid descriptor fair action list')
        require(actions == sorted(set(actions)),'descriptor fair action list is not sorted unique')
    workers = []; seen = set()
    for row in worker_rows:
        row_shape(row,'worker'); worker = {}
        for field,cell in zip(WORKER_COLUMNS,row):
            if field in WORKER_VALUE_REFS: worker[field] = ref(cell)
            else:
                initial = IDLE[field]
                if initial is None:
                    require(cell is None or integer(cell,high=I64_MAX),'invalid nullable worker time cell')
                else:
                    require(type(cell) is type(initial),'worker scalar wire type drift: '+field)
                worker[field] = cell
        encoded = canonical(worker)
        require(encoded not in seen,'duplicate exact worker_pool entry')
        seen.add(encoded); workers.append(worker)
    for name,kind in (('edge_descriptors','edge_descriptor'),('demand_descriptors','demand_descriptor')):
        seen = set()
        for row in wire[name]:
            row_shape(row,kind)
            branch_shape(row[0] if kind == 'edge_descriptor' else row[1],kind)
            if kind == 'edge_descriptor': fair_shape(row[5])
            else: require(row[2] in ('apply','capacity'),'invalid demand descriptor role')
            encoded = canonical(row)
            require(encoded not in seen,'duplicate exact '+kind+' entry')
            seen.add(encoded)
    hashes = bytearray()
    def state(identity,row):
        value = dict(id=identity,context=context_ref(row[0]),sql=row[1],
            w=[worker_ref(row[2]),worker_ref(row[3])],available=ref(row[4]),completed=ref(row[5]),
            mono_now_ms=row[6],wall_anchors=ref(row[7]),mono_anchors=ref(row[8]),claim_loss_keys=ref(row[9]))
        if len(hashes) < (identity+1)*32:
            require(len(hashes) == identity*32,'product digest derivation order mismatch')
            hashes.extend(hashlib.sha256(canonical(product_payload(value)).encode('utf-8')).digest())
        value['canonical_sha256'] = hashes[identity*32:(identity+1)*32].hex()
        return value
    def edge(source,row):
        suffix = wire['edge_descriptors'][row[1]]
        return dict(zip(('from','to','branch','oracle_edge','resolution_edge','read_edge','cleanup_edge','fair_actions'),
            (source,row[0],ref(suffix[0]),*suffix[1:5],ref(suffix[5]))))
    def demand(source,index):
        suffix = wire['demand_descriptors'][index]
        return dict(context=context_ref(wire['product_states'][source][0]),**{'from':source},oracle_from=suffix[0],
            branch=ref(suffix[1]),role=suffix[2],input=ref(suffix[3]),oracle_edge=suffix[4],exclusion=ref(suffix[5]))
    def enabled(identity,index): return dict(state=identity,actions=ref(index))
    logical = dict(wire)
    for identity,row in enumerate(wire['product_states']):
        row_shape(row,'product_state');state(identity,row)
    require(type(wire['product_digest_sequence_sha256']) is str and
        re.fullmatch('[0-9a-f]{64}',wire['product_digest_sequence_sha256']) is not None and
        hashlib.sha256(hashes).hexdigest() == wire['product_digest_sequence_sha256'],
        'ordered original product digest sequence mismatch')
    logical['product_states'] = LogicalRows(wire['product_states'],state)
    for name,kind,decoder in (('product_edges','product_edge',edge),('demands','demand',demand)):
        groups = wire[name]
        require(len(groups) == len(wire['product_states']),'invalid '+name+' outer length')
        for source,group in enumerate(groups):
            require(type(group) is list,'invalid '+name+' source group')
            for row in group:
                row_shape(row if kind == 'product_edge' else [row],kind)
        logical[name] = GroupedLogicalRows(groups,decoder)
    require(len(wire['enabledness']) == len(wire['product_states']),'invalid enabledness outer length')
    for index in wire['enabledness']:
        row_shape([index],'enabledness');fair_shape(index)
    logical['enabledness'] = LogicalRows(wire['enabledness'],enabled)
    memberships = [[] for _ in contexts]
    for identity,row in enumerate(wire['product_states']): memberships[row[0]].append(identity)
    logical['contexts'] = []
    for identity,context in enumerate(contexts):
        exact_keys(context,set(WIRE_CONTEXT_FIELDS),'product wire context')
        logical['contexts'].append({**context,'states':memberships[identity]})
    store_states = []
    for identity,row in enumerate(wire['store']['states']):
        row_shape(row,'store_state')
        require(type(row[7]) is str and re.fullmatch('[0-9a-f]{64}',row[7]) is not None,'invalid Store canonical digest')
        text = fragments.text(row[8])
        require(hashlib.sha256(text.encode('utf-8')).hexdigest() == row[7],'Store canonical preimage hash mismatch')
        try: raw = strict_json(text)
        except (ValueError,RecursionError) as exc: raise Invalid('invalid complete Store canonical JSON') from exc
        certify_tlc_numbers(raw,'decoded Store canonical JSON')
        store_states.append(dict(id=identity,now=row[0],**{field[:-4]:ref(row[column])
            for column,field in enumerate(WIRE_COLUMNS['store_state']) if field.endswith('_ref') and field != 'canonical_fragment_ref'},
            canonical_sha256=row[7],canonical_json=text,raw=raw))
    logical['store'] = {'initial':wire['store']['initial'],'states':store_states,'edges':wire['store']['edges']}
    return logical


CORE_SOURCES = {'store/'+name for name in ('background_jobs.rs','background_jobs_domain.rs',
 'background_jobs_library.rs','background_jobs_schema.sql','background_jobs_resources.sql',
 'background_jobs_library.sql','background_jobs_domain.sql','subtitle_managed_schema.sql',
 'dv_title_jobs.rs','dv_title_jobs.sql','dv_title_jobs_schema.rs',
 'sqlite/mod.rs','sqlite/background_jobs.rs','sqlite/coordination.rs','async_driver_refinement.rs',
 'async_driver_product.rs','mod.rs')} | {'cluster/coordination.rs'}

EXTERNAL_SOURCES = {'Cargo.toml','Cargo.lock','rust-toolchain.toml','crates/plurx-core/Cargo.toml',
 'crates/plurx-worker-control/Cargo.toml','crates/plurxd/Cargo.toml',
 'crates/plurx-worker-control/src/lib.rs','crates/plurx-worker-control/src/library_driver.rs',
 'crates/plurx-worker-control/src/library_lifecycle.rs','crates/plurx-worker-control/src/retry_policy.rs',
 'crates/plurxd/src/background_jobs.rs','crates/plurxd/src/library_work.rs',
 'crates/plurxd/src/library_work/env.rs','crates/plurxd/src/state.rs'}


def binding_path(path):
    return ROOT/path if path in EXTERNAL_SOURCES else ROOT/'crates/plurx-core/src'/path


def product_source_bindings():
    return {path:sha(binding_path(path)) for path in sorted(CORE_SOURCES|EXTERNAL_SOURCES)}


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


def validate_snapshot(snapshot,s,job=None):
    exact_keys(snapshot, {'job','raw_job_id','failed_attempts','claim_id','token_capture'}, 'actual claimed snapshot')
    require(integer(snapshot['job'],0,1) and (job is None or job == snapshot['job']), 'snapshot selected job mismatch')
    require(isinstance(snapshot['raw_job_id'],str) and re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}',snapshot['raw_job_id']), 'snapshot is not raw fixture UUID')
    require(integer(snapshot['failed_attempts'],0,I64_MAX), 'invalid immutable failed-attempt snapshot')
    token = snapshot['token_capture']
    require(token in s['captured_tokens'][snapshot['job']] and token['job_id'] == snapshot['raw_job_id'] and
            token['claim_id'] == snapshot['claim_id'], 'snapshot token/identity is not actual captured claim')


def validate_payload(payload):
    exact_keys(payload, {'kind','not_before_ms','error_code','checkpoint'}, 'ordinary payload')
    require(payload['kind'] in {'yield','retry'} and integer(payload['not_before_ms'],0,I64_MAX), 'invalid saved ordinary payload')
    require(payload['error_code'] == ('library_execution_failed' if payload['kind'] == 'retry' else None) and
            payload['checkpoint'] is None, 'ordinary adapter payload mismatch')


def validate_worker(w,s,driver):
    exact_keys(w, set(IDLE), 'product worker')
    for key,initial in IDLE.items():
        if type(initial) is bool: require(type(w[key]) is bool, 'worker Boolean type drift: '+key)
        elif type(initial) is int: require(integer(w[key],-(2**63),I64_MAX), 'worker integer type drift: '+key)
        elif isinstance(initial,str): require(isinstance(w[key],str), 'worker string type drift: '+key)
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
    if w['claim_request'] is not None: exact_keys(w['claim_request'],CLAIM_REQUEST_FIELDS,'saved original ClaimJob')
    exact_keys(w['claim_recovery'],{'phase','wake_mono_ms'},'claim recovery service')
    require(w['claim_recovery']['phase'] in {'off','ready','waiting'}, 'unknown recovery phase')
    wake = w['claim_recovery']['wake_mono_ms']
    require((w['claim_recovery']['phase'] == 'waiting' and integer(wake,0,I64_MAX)) or
        (w['claim_recovery']['phase'] != 'waiting' and wake is None), 'recovery timer/phase mismatch')
    if w['claim_recovery']['phase'] != 'off':
        require(w['stage'] == 'claim' and w['call'] == 'replied' and w['reply'] == 'Unknown' and w['faultUsed'] and w['claim_request'] is not None and w['claim_snapshot'] is None, 'recovery lacks actual ambiguous request/fault')
    if w['claim_snapshot'] is not None:
        validate_snapshot(w['claim_snapshot'],s,w['job'])
        require(w['claim_snapshot']['claim_id'] == w['attempt'], 'worker snapshot not actual immutable claim identity')
    for key in ('claim_issued_mono_ms','claim_request_now_ms','local_deadline_mono_ms'):
        require(w[key] is None or integer(w[key],0,I64_MAX), 'invalid nullable acquisition time: '+key)
    if w['driver_settlement'] is not None:
        exact_keys(w['driver_settlement'], {'kind','not_before_ms'}, 'saved Driver settlement')
        validate_payload({**w['driver_settlement'],'error_code':'library_execution_failed' if w['driver_settlement']['kind'] == 'retry' else None,'checkpoint':None})
    if w['adapter_reply'] is not None:
        reply = w['adapter_reply'];exact_keys(reply, {'tag','now_ms','retry_delay_ms'}, 'saved adapter reply')
        require(reply['tag'] == w['reply'], 'adapter reply tag mismatch')
        for key in ('now_ms','retry_delay_ms'): require(reply[key] is None or integer(reply[key],0,I64_MAX), 'invalid adapter reply sample')
    o = w['ordinary'];exact_keys(o,set(ORDINARY),'ordinary service state')
    require(o['phase'] in {'off','waiting_lock','check_lost','check_deadline','take_token','sample','sent','applied','finish','done'}, 'unknown ordinary phase')
    if o['phase'] == 'off': require(o == ORDINARY, 'ordinary off state not reset')
    else:
        validate_payload(o['payload'])
        require({k:o['payload'][k] for k in ('kind','not_before_ms')} == w['driver_settlement'], 'request payload drifted from saved Driver')
    require(o['lost_seen'] is None or type(o['lost_seen']) is bool, 'invalid saved loss read')
    for key in ('deadline_seen','guard_mono_ms','request_now_ms'): require(o[key] is None or integer(o[key],0,I64_MAX), 'invalid ordinary saved sample')
    require(o['captured_token'] is None or integer(o['captured_token'],0,len(s['captured_tokens'][w['job']])-1), 'invalid copied ordinary token')
    require(o['api_outcome'] in {None,'accepted','refused',*VALIDATION_ERRORS}, 'unknown ordinary API outcome')
    require(o['observed_outcome'] in {None,'accepted','refused',*VALIDATION_ERRORS,'local_lost','local_expired','local_no_token','clock_error','transport_error'}, 'unknown ordinary observed outcome')
    if o['phase'] in {'sent','applied'}: require(o['captured_token'] is not None and o['request_now_ms'] is not None, 'issued ordinary request lacks saved token/sample')


def validate_semantic_bindings(bindings,verify_sources=True):
    require(isinstance(bindings,dict) and set(bindings) == SEMANTIC_PATHS, 'semantic source registry differs')
    for path,digest in bindings.items():
        require(isinstance(digest,str) and re.fullmatch('[0-9a-f]{64}',digest), 'invalid semantic source digest')
        require(not verify_sources or sha(ROOT/path) == digest, 'semantic source hash mismatch: '+path)


def validate_source_boundaries(boundaries):
    require(isinstance(boundaries,dict) and boundaries == SOURCE_BOUNDARIES,
            'ordinary_settlement_payload_transfer must be pending in selected_two_worker_normal_clock')


def validate_product_schema(schema):
    exact_keys(schema, {'schema_version','scope','kind','root_fields','main_contexts',
        'boundary_contexts','configuration_contexts','fair_families','worker_fields',
        'product_state_fields','product_edge_fields','demand_fields','demand_roles',
        'store_state_additions','source_policy','resource_policy','source_boundaries','global_fair_actions',
        'wire_columns','wire_value_ref_fields','wire_reference_pools','wire_nullable_reference_fields',
        'wire_context_fields','wire_fragment_nodes','wire_digest_sequence'}, 'product schema')
    require(schema.get('kind') == 'closed_wire_vocabulary_not_acceptance', 'product schema kind drift')
    require(schema.get('schema_version') == 5 and schema.get('scope') == 'selected_two_worker_product', 'product schema scope drift')
    require(schema.get('root_fields') == sorted(PRODUCT_KEYS), 'product schema root registry drift')
    validate_source_boundaries(schema['source_boundaries'])
    require(schema.get('main_contexts') == {name:{'stable_success':v[0],'mutation':v[1]} for name,v in CONTEXTS.items()}, 'product schema context drift')
    require(schema.get('boundary_contexts') == BOUNDARY_CONTEXTS and schema.get('configuration_contexts') == CONFIG_CONTEXT, 'product schema configuration drift')
    require(schema['global_fair_actions'] == list(GLOBAL_FAIR), 'product schema global fair relation drift')
    require(schema.get('fair_families') == list(FAIR_FAMILIES) and set(schema.get('worker_fields',[])) == set(IDLE), 'product schema family/worker drift')
    for key,values in {
        'product_state_fields':['id',*PRODUCT_VALUE_FIELDS,'canonical_sha256'],
        'product_edge_fields':['from','to','branch','oracle_edge','resolution_edge','read_edge','cleanup_edge','fair_actions'],
        'demand_fields':['context','from','oracle_from','branch','role','input','oracle_edge','exclusion'],
        'demand_roles':['apply','capacity'],'store_state_additions':['canonical_json']}.items():
        require(schema[key] == values, 'product schema '+key+' drift')
    require(schema['wire_columns'] == WIRE_COLUMNS and schema['wire_value_ref_fields'] == WIRE_VALUE_REF_FIELDS and
        schema['wire_reference_pools'] == WIRE_REFERENCE_POOLS and
        schema['wire_nullable_reference_fields'] == WIRE_NULLABLE_REFERENCE_FIELDS and
        schema['wire_context_fields'] == WIRE_CONTEXT_FIELDS and schema['wire_fragment_nodes'] == WIRE_FRAGMENT_NODES and
        schema['wire_digest_sequence'] == WIRE_DIGEST_SEQUENCE,
        'product schema wire columns/references drift')
    require(schema['source_policy'] == 'exact core and semantic registries; raw driver input digest; full Store canonical identity; independent semantics',
            'product schema source policy drift')
    require(schema.get('resource_policy') == {'store_identity_cap':20000,'product_node_cap':None,
        'bundle_max_bytes':268435456,'artifact_max_bytes':536870912}, 'product schema resource drift')


def require_exact_inventory(actual,expected,label):
    require(isinstance(actual,list),label+' is not an array')
    a,b = [canonical(v) for v in actual],[canonical(v) for v in expected]
    require(len(a) == len(set(a)),label+' has duplicates')
    require(set(a) == set(b),label+' differs from complete independent enumeration')


def dependent_resolve_demands(p,driver,store,oracle,primary):
    """Derive exact chained queries from required actual renewal outcomes, never adjacency."""
    out = []; s = store['states'][p['sql']]
    for d in primary:
        if d['branch']['family'] != 'Environment' or d['input']['op'] != 'renew': continue
        a = d['input']; key = (d['oracle_from'],canonical(a))
        require(key in oracle, 'MissingProductOracleEdge: renewal before dependent resolve')
        e = store['edges'][oracle[key]]
        matches = [w for n,w in enumerate(p['w']) if same_claim(p,s,n,a)]
        if not matches or e['result'] == 'renewed' and all(w['faultUsed'] for w in matches): continue
        action = {'op':'resolve','job':a['job'],'token':a['token']}
        require(canonical(action) in expected_actions(store['states'][e['to']]), 'dependent resolution outside intrinsic capture D')
        out.append({'context':p['context'],'from':p['id'],'oracle_from':e['to'],
            'branch':{'family':'EnvironmentResolve','worker':None},'role':'apply','input':action,'exclusion':None})
    return out


def dependent_claim_demands(p,store,oracle,primary):
    out = []
    for d in primary:
        if d['branch']['family'] != 'ClaimRecover': continue
        key = (d['oracle_from'],canonical(d['input']))
        require(key in oracle, 'MissingProductOracleEdge: initial recovery before dependent query')
        e = store['edges'][oracle[key]];n = d['branch']['worker'];w = p['w'][n]
        if e['result'] == 'running': family,action = 'ClaimRecoverRead',{'op':'read_claim_job','job':w['job']}
        elif e['result'] == 'cancel_requested':
            family,action = 'ClaimRecoverCancel',{'op':'settle','job':w['job'],'token':e['resolved_token'],'disposition':2}
            require(canonical(action) in expected_actions(store['states'][e['to']]), 'initial cleanup outside captured-token D')
        else: continue
        out.append({'context':p['context'],'from':p['id'],'oracle_from':e['to'],
            'branch':{'family':family,'worker':n},'role':'apply','input':action,'exclusion':None})
    return out


def complete_product_demands(p,driver,store,oracle):
    complete = []; used = set(); primary = expected_product_demands(p,driver,store)
    # Missing primary input fails before an actual result can select a dependent query.
    for d in primary:
        if d['exclusion'] is None:
            require((d['oracle_from'],canonical(d['input'])) in oracle, 'MissingProductOracleEdge: eligible apply/capacity demand')
    for d in primary + dependent_resolve_demands(p,driver,store,oracle,primary) + dependent_claim_demands(p,store,oracle,primary):
        index = None
        if d['exclusion'] is None:
            key = (d['oracle_from'],canonical(d['input']))
            require(key in oracle, 'MissingProductOracleEdge: eligible primary/dependent/capacity demand')
            index = oracle[key]; used.add(index)
        complete.append({**d,'oracle_edge':index})
    return complete,used


def product_pair_obligations(p,candidates,by_payload):
    pair_members = {}
    encoded_candidates = [canonical(candidate[1]) for candidate in candidates]
    for encoded,(_,_,_,members,_,_,_) in zip(encoded_candidates,candidates):
        pair_members.setdefault(encoded,set()).update(members)
    edges = []; targets = []; enabled = set(); original = canonical(product_payload(p))
    for encoded,(branch,q,index,members,resolution,read,cleanup) in zip(encoded_candidates,candidates):
        to = by_payload.get(encoded)
        require(to is not None, 'MissingProductSuccessor: original Next target absent')
        targets.append(to); all_members = pair_members[encoded]
        if encoded != original: enabled.update(all_members)
        edges.append({'from':p['id'],'to':to,'branch':branch,'oracle_edge':index,'resolution_edge':resolution,'read_edge':read,'cleanup_edge':cleanup,'fair_actions':sorted(all_members)})
    return edges,{'state':p['id'],'actions':sorted(enabled)},targets


def validate_context_reachability(initial,ids,adjacency):
    reachable = {initial}; todo = deque(reachable)
    while todo:
        for to in adjacency[todo.popleft()]:
            if to not in reachable: reachable.add(to);todo.append(to)
    require(reachable == set(ids), 'orphan/unreachable product certificate')


def eligibility_mechanics_witness(bundle,driver,product_digest=None):
    """Existential actual base path; counterfactual safe windows are not base fairness."""
    states,store,edges = bundle['product_states'],bundle['store'],bundle['product_edges']
    context = next(c for c in bundle['contexts'] if c['id'] == 'base')
    if isinstance(edges,GroupedLogicalRows):
        def outgoing(source):
            return edges.source_indices(source) if states[source]['context'] == 'base' else ()
    else:
        # Also accept independent expanded logical fixtures at this helper seam.
        expanded = [[] for _ in states]
        for i,e in enumerate(edges):
            if states[e['from']]['context'] == 'base': expanded[e['from']].append(i)
        def outgoing(source): return expanded[source]
    parent = {context['initial']:None}; todo = deque(parent)
    while todo:
        src = todo.popleft()
        for i in outgoing(src):
            to = edges[i]['to']
            if to not in parent: parent[to] = i;todo.append(to)
    groups = {}
    for i,e in enumerate(edges):
        if states[e['from']]['context'] != 'base' or e['branch']['family'] != 'OrdinaryCallReturn' or e['oracle_edge'] is None: continue
        actual = store['edges'][e['oracle_edge']]
        if actual['action']['op'] != 'ordinary_settle' or actual['result'] != 'accepted': continue
        j = actual['action']['job']; target = store['states'][states[e['to']]['sql']]; job = target['jobs'][j]
        if job['state'] == 'queued' and not job['request_done'] and job['not_before_ms'] > target['now'] and target['budgets']['claims'][j] < 2:
            groups.setdefault((j,job['not_before_ms']),[]).append(i)
    for (job,deadline),seeds in sorted(groups.items()):
        suffix_parent = {}; origin = {}; todo = deque()
        for i in seeds:
            to = edges[i]['to']
            if to not in suffix_parent: suffix_parent[to] = None;origin[to] = i;todo.append(to)
        while todo:
            src = todo.popleft(); p = states[src]; s = store['states'][p['sql']]
            candidate_ready = any(w['stage'] == 'idle' and p['available'][n] for n,w in enumerate(p['w']))
            if s['now'] >= deadline and candidate_ready:
                suffix = [];cursor = src
                while suffix_parent[cursor] is not None:
                    index = suffix_parent[cursor];suffix.append(index);cursor = edges[index]['from']
                suffix.reverse(); event = origin[src]
                crossing = next((i for i in suffix if edges[i]['branch']['family'] == 'WallAdvance' and
                    store['states'][states[edges[i]['from']]['sql']]['now'] < deadline <= store['states'][states[edges[i]['to']]['sql']]['now']),None)
                if crossing is None: continue
                prefix = [];cursor = edges[event]['from']
                while parent[cursor] is not None:
                    index = parent[cursor];prefix.append(index);cursor = edges[index]['from']
                prefix.reverse()
                raw_id = fixture_jobs(bundle['fixture_identity'])[job]
                return {'context':'base','job':job,'raw_job_id':raw_id,'not_before_ms':deadline,
                    'prefix_edge_ids':prefix+[event],'ordinary_edge':edges[event]['oracle_edge'],
                    'suffix_edge_ids':suffix,'crossing_edge':crossing,'endpoint':src,
                    'remaining_claim_attempts':2-s['budgets']['claims'][job],
                    'product_certificate_sha256':product_digest or canonical_measure(bundle)[1],
                    'claim':'existential eligibility mechanics; no stable-history or universal fair-recovery claim'}
            for i in outgoing(src):
                edge = edges[i];to = edge['to'];q = states[to];after = store['states'][q['sql']]; row = after['jobs'][job]
                if row['state'] != 'queued' or row['request_done'] or row['not_before_ms'] != deadline or after['budgets']['claims'][job] >= 2: continue
                if edge['oracle_edge'] is not None:
                    action = store['edges'][edge['oracle_edge']]['action']
                    if action['op'] == 'remove_owner' or action['op'] in {'claim','settle','ordinary_settle'} and action.get('job') == job: continue
                if edge['branch']['family'] == 'WallAdvance':
                    target = edge['branch']['choice']['now']
                    if target != next_landmark(s['now'],p['wall_anchors']) or not stable_wall_allowed(p,s,target,driver,force=True): continue
                    if not any(r['state'] == 'queued' and not r['request_done'] and r['not_before_ms'] > s['now'] for r in s['jobs']): continue
                if to not in suffix_parent:
                    suffix_parent[to] = i;origin[to] = origin[src];todo.append(to)
    raise Invalid('EligibilityMechanicsWitnessMissing: actual future queued deadline has no source-valid stable-safe crossing path')


def validate_product(bundle,driver,verify_sources=True,driver_sha256=None):
    try: return _validate_product(bundle,driver,verify_sources,driver_sha256)
    except (KeyError,TypeError,IndexError,ValueError) as exc:
        raise Invalid('MalformedProductBundle: '+str(exc)) from exc


def _validate_product(g,driver,verify_sources,driver_sha256):
    exact_keys(g,PRODUCT_KEYS,'product bundle')
    require(type(g['schema_version']) is int and g['schema_version'] == 5 and g['scope'] == 'selected_two_worker_product', 'unsupported product schema/scope')
    validate_source_boundaries(g['source_boundaries'])
    certify_tlc_numbers(g)
    bundle_bytes,bundle_digest = canonical_measure(g)
    require(bundle_bytes <= BUNDLE_MAX_BYTES, 'product bundle byte bound exceeded')
    g = decode_product(g)
    certify_tlc_numbers(g['store']['states'],'decoded Store canonical JSON')
    validate_driver(driver)
    require(isinstance(g['driver_graph_sha256'],str) and re.fullmatch('[0-9a-f]{64}',g['driver_graph_sha256']), 'invalid driver graph digest')
    require(not verify_sources or driver_sha256 is not None, 'raw driver graph digest required for source validation')
    require(driver_sha256 is None or g['driver_graph_sha256'] == driver_sha256, 'driver graph input hash mismatch')
    validate_product_schema(read_json(PACKAGE/'product-schema.json'))
    validate_semantic_bindings(g['semantic_bindings'],verify_sources)
    exact_keys(g['store'], {'initial','states','edges'}, 'imported oracle relation')
    store = g['store']
    certify_tlc_numbers((len(g['value_pool'])+1,len(g['worker_pool'])+1,len(g['contexts'])+1,
        len(g['edge_descriptors'])+1,len(g['demand_descriptors'])+1),'wire pool index/count arithmetic')
    certify_tlc_numbers((len(g['product_states'])+1,len(g['product_edges'])+1,len(driver['states'])+1,len(driver['edges'])+1),'model index/count arithmetic')
    oracle_graph = {'schema_version':1,'scope':'imported_oracle','initial':store['initial'],
       'states':store['states'],'edges':store['edges'],'domain':g['domain'],'source_bindings':g['source_bindings'],
       'schema_bindings':g['schema_bindings'],'coverage':None}
    facts = validate_store(oracle_graph,verify_sources,product=True)
    ids = fixture_jobs(g['fixture_identity'])
    require(store['states'][store['initial']]['now'] == 1000, 'initial normal wall time differs')
    for edge in store['edges']:
        if edge['claim_request'] is not None: require(edge['claim_request']['job_id'] == ids[edge['action']['job']], 'claim request raw fixture drift')
        if edge['job_read'] is not None: require(edge['job_read']['id'] == ids[edge['action']['job']], 'actual reread raw fixture drift')
    for state in store['states']:
        for j,tokens in enumerate(state['captured_tokens']):
            require(all(t['job_id'] == ids[j] for t in tokens), 'raw token ID normalization or fixture drift')
        for snapshot in state['raw']['episode']['claim_snapshots']:
            validate_snapshot(snapshot,state)
            validate_claim_snapshot_identity(snapshot,ids)
            require(snapshot['raw_job_id'] == ids[snapshot['job']], 'raw claimed snapshot differs from seed')
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
    states = g['product_states']; require(isinstance(states,(list,LogicalRows)) and states, 'empty product certificate')
    require([p.get('id') for p in states] == list(range(len(states))), 'product IDs are not contiguous')
    by_payload = ExactPayloadIndex(states); per_context = {name:[] for name in CONTEXTS}
    for p in states:
        exact_keys(p, {'id','canonical_sha256',*PRODUCT_VALUE_FIELDS}, 'product state')
        require(p['context'] in CONTEXTS, 'non-main product state')
        require(integer(p['sql'],0,len(store['states'])-1), 'invalid product Store reference')
        require(isinstance(p['w'],list) and len(p['w']) == 2, 'product requires exactly two workers')
        require(isinstance(p['available'],list) and len(p['available']) == 2 and all(type(v) is bool for v in p['available']), 'invalid physical availability')
        require(isinstance(p['completed'],list) and all(integer(v,0,1) for v in p['completed']) and p['completed'] == sorted(set(p['completed'])), 'invalid completion history')
        require(integer(p['mono_now_ms'],0,I64_MAX), 'invalid monotonic product time')
        for field in ('wall_anchors','mono_anchors'):
            require(isinstance(p[field],list) and all(integer(x,0,I64_MAX) for x in p[field]) and p[field] == sorted(set(p[field])), 'invalid retained anchor set')
        keys = p['claim_loss_keys'];require(isinstance(keys,list) and len(keys) <= 8, 'persistent claim fault-domain exceeded')
        for key in keys:
            exact_keys(key,{'job','node','claim_id'},'persistent claim loss identity')
            require(integer(key['job'],0,1) and integer(key['node'],0,1) and
                key['claim_id'] in {f"00000000-0000-4000-8000-{10+key['job']*4+attempt:012}" for attempt in range(2)}, 'claim loss key outside selected identity domain')
        require(keys == sorted(keys,key=lambda x:(x['job'],x['node'],x['claim_id'])) and len({canonical(k) for k in keys}) == len(keys), 'persistent claim keys not canonical unique')
        for w in p['w']:
            validate_worker(w,store['states'][p['sql']],driver)
            if w['claim_recovery']['phase'] != 'off': require(claim_loss_key(w) in keys, 'ambiguous episode forgot persistent fault key')
        require(anchor_payload(logical_copy(product_payload(p)),store) == product_payload(p), 'missing exact current source anchor')
        encoded = canonical(product_payload(p))
        require(p['canonical_sha256'] == hashlib.sha256(encoded.encode()).hexdigest(), 'product canonical preimage mismatch')
        require(encoded not in by_payload, 'duplicate exact product valuation')
        by_payload[encoded] = p['id']; per_context[p['context']].append(p['id'])
    for name,ids in per_context.items():
        context = contexts[name]
        require(context['states'] == ids, 'context state partition differs')
        initial = {'context':name,'sql':store['initial'],'w':[logical_copy(IDLE),logical_copy(IDLE)],'available':[True,True],
            'completed':[j for j,job in enumerate(store['states'][store['initial']]['jobs']) if job['request_done']],
            'mono_now_ms':0,'wall_anchors':[],'mono_anchors':[],'claim_loss_keys':[]}
        initial = anchor_payload(initial,store)
        require(canonical(initial) in by_payload and context['initial'] == by_payload[canonical(initial)], 'product Init mismatch')
    oracle = {(e['from'],canonical(e['action'])):i for i,e in enumerate(store['edges'])}
    require(len(oracle) == len(store['edges']), 'nondeterministic oracle input')
    used_oracle = set(); adjacency = [[] for _ in states]
    edge_counts, demand_counts = Counter(), Counter()
    edge_count = demand_count = 0
    # Schema5 source grouping is structural. Ranges avoid retaining one Python
    # integer object per edge/demand in addition to the compact wire arrays.
    groups = {'product_edges':g['product_edges'],'demands':g['demands']}
    for p in states:
        demands,used = complete_product_demands(p,driver,store,oracle)
        used_oracle.update(used)
        certify_tlc_arithmetic(p,store)
        candidates = expected_product_successors(p,driver,store,oracle)
        for _,candidate,_,_,_,_,_ in candidates: certify_tlc_numbers(candidate,'independent Next candidate')
        edges,enabled,targets = product_pair_obligations(p,candidates,by_payload)
        identity = p['id']
        for name,expected,label in (('demands',demands,'product demand coverage'),
            ('product_edges',edges,'product successor/state-pair membership'),
            ('enabledness',[enabled],'fair nonstuttering enabledness')):
            require_exact_inventory([g[name][i] for i in (range(identity,identity+1) if name == 'enabledness' else
                groups[name].source_indices(identity))],expected,label)
        adjacency[identity] = targets
        edge_count += len(edges); demand_count += len(demands)
        edge_counts[p['context']] += len(edges); demand_counts[p['context']] += len(demands)
    require(used_oracle == set(range(len(store['edges']))), 'undemanded oracle edge in product bundle')
    for name,ids in per_context.items():
        validate_context_reachability(contexts[name]['initial'],ids,adjacency)
    coverage = g['coverage']
    exact_keys(coverage, {'complete','no_skipped_demands','store_state_count','store_edge_count',
        'product_state_count','product_edge_count','demand_count','by_context'}, 'product coverage')
    require(coverage['complete'] is True and coverage['no_skipped_demands'] is True, 'partial product frontier')
    for key,actual in (('store_state_count',len(store['states'])),('store_edge_count',len(store['edges'])),
      ('product_state_count',len(states)),('product_edge_count',edge_count),('demand_count',demand_count)):
        require(type(coverage[key]) is int and coverage[key] == actual, 'product coverage count mismatch: '+key)
    expected_counts = {name:{'state_count':len(ids),'edge_count':edge_counts[name],
        'demand_count':demand_counts[name],'frontier_count':0} for name,ids in per_context.items()}
    require(coverage['by_context'] == expected_counts, 'context frontier/count coverage mismatch')
    witness = eligibility_mechanics_witness(g,driver,bundle_digest)
    return {**facts,'numeric_certificate':{'tool':'TLC 1.7.4 signed-int','input_and_candidate_arithmetic':'exact_representable','source_i64_saturation':'unreachable_in_certified_artifact','out_of_range':'global_qualification_failure'},'eligibility_mechanics_witness':witness,'product_states':len(states),'product_edges':edge_count,'coverage':coverage,
            'result_class':'independent_product_inventory','acceptance':False,'scope':g['scope'],
            'source_boundaries':dict(g['source_boundaries']),
            'wire':{'schema_version':5,'bytes':bundle_bytes,'value_pool_count':len(g['value_pool']),
                    'worker_pool_count':len(g['worker_pool']),
                    'edge_descriptor_count':len(g['edge_descriptors']),'demand_descriptor_count':len(g['demand_descriptors'])}}


def rejection_controls(driver,store):
    mutations = []
    broken = logical_copy(driver); broken['edges'][0]['to'] = len(driver['states'])
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
        missing = logical_copy(store)
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
    if type(value) is int:
        certify_tlc_numbers(value,'serialized TLA integer')
        return str(value) if value >= 0 else f'({value})'
    if isinstance(value,str): return json.dumps(value,ensure_ascii=True)
    if isinstance(value,list): return '<<' + ','.join(tla(v) for v in value) + '>>'
    if isinstance(value,dict): return '[' + ','.join(f'{k} |-> {tla(v)}' for k,v in value.items()) + ']'
    raise Invalid('unrepresentable TLA value')

def edge_fresh(e, store):
    a = e['action']; op = a['op']
    if op not in {'renew','settle','bind','complete','ordinary_settle'} or e['result'] not in {'accepted','renewed'}:
        return True
    s = store['states'][e['from']]; j = s['jobs'][a['job']]
    t = s['captured_tokens'][a['job']][a['token']]
    owner = {'node-a':0,'node-b':1}.get(t['node_id'])
    request_now = a['request_now_ms'] if op == 'ordinary_settle' else s['now']
    fresh = (j['state'] == 'running' and j['owner'] == t['node_id']
      and j['fence'] == t['fence'] and j['revision'] == t['revision']
      and j['expiry'] == t['lease_expires_ms'] and t['lease_expires_ms'] > request_now
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

def tla_chunks(value):
    """The same logical TLA encoding as tla(), without joining whole tables."""
    if isinstance(value,(list,tuple)):
        yield '<<'
        for i,item in enumerate(value):
            if i: yield ','
            yield from tla_chunks(item)
        yield '>>'
    elif isinstance(value,dict):
        yield '['
        for i,(key,item) in enumerate(value.items()):
            if i: yield ','
            yield key+' |-> '
            yield from tla_chunks(item)
        yield ']'
    else: yield tla(value)


def sequence_chunks(items):
    yield '<<'
    for i,item in enumerate(items):
        if i: yield ','
        yield from tla_chunks(item)
    yield '>>'


def merge_function_chunks(count, entry, start=0):
    """Balanced function merge, with bounded stack and no giant entry list."""
    if not count: yield r'[x \in {} |-> x]'
    elif count == 1:
        chunks = entry(start)
        if isinstance(chunks,str): yield chunks
        else: yield from chunks
    else:
        middle = count//2
        yield '('
        yield from merge_function_chunks(middle,entry,start)
        yield ' @@ '
        yield from merge_function_chunks(count-middle,entry,start+middle)
        yield ')'


def table_chunks(driver,wire):
    bundle = decode_product(wire) if wire.get('scope') == 'selected_two_worker_product' else None
    store = bundle['store'] if bundle is not None else wire
    ranks = validate_driver(driver)
    owners = {None:-1,'node-a':0,'node-b':1,'foreign':2,'foreign-holder':2}
    def store_state(s):
        return {'input_domain':{key:s[key] for key in ('now','jobs','domain','captured_tokens','captured_leases','budgets')},
            'now':s['now'],'jobs':[{'state':j['state'],'active':j['active'],'done':j['request_done'],
                'owner':owners[j['owner']],'reservations':j['reservation_count']} for j in s['jobs']],
            'tokens':[[t['claim_id'] for t in tokens] for tokens in s['captured_tokens']],
            'leases':[{'owner':owners[l['owner_node_id']]} for l in s['captured_leases']]}
    def store_edge(e):
        return {'fresh':edge_fresh(e,store),'monotone':edge_monotone(e,store),'src':e['from'],'dst':e['to'],
            'op':e['action']['op'],'result':e['result'],
            'resolved':e['resolved_token'] if e['resolved_token'] is not None else -1,'input':e['action'],
            'claim_snapshot':e['claim_snapshot'],'error':e['error'],'claim_request':e['claim_request'],'job_read':e['job_read'],
            **{key:e['action'].get(key,-1) for key in ('job','node','token','lease','disposition')}}
    yield '--------------------------- MODULE SourceTables ---------------------------\n'
    yield 'EXTENDS Integers, Sequences, TLC\nDriverInitial == 0\nStoreInitial == 0\nDriverStates == '
    yield from sequence_chunks({'pending':s['pending'],'terminal':s['terminal'],'work':driver_work_class(s['work']),
        **s['held'],'revoked':s['revoked'],'cancelled':s['cancelled'],'rank':rank} for s,rank in zip(driver['states'],ranks))
    yield '\nDriverEdges == {'
    for i,e in enumerate(driver['edges']):
        if i: yield ','
        yield from tla_chunks({'src':e['from'],'dst':e['to'],'reply':e['reply']})
    yield '}\nStoreStates == '
    yield from sequence_chunks(store_state(s) for s in store['states'])
    yield '\nStoreEdges == {'
    for i,e in enumerate(store['edges']):
        if i: yield ','
        yield from tla_chunks(store_edge(e))
    claim_ids = sorted({t['claim_id'] for state in store['states'] for tokens in state['captured_tokens'] for t in tokens})
    yield '}\nClaimIds == {'
    for i,claim_id in enumerate(claim_ids):
        if i: yield ','
        yield tla(claim_id)
    yield '}\n'
    if bundle is None:
        yield 'ProductStates == <<>>\nProductEdges == {}\nProductEnabled == <<>>\nProductOutgoing == <<>>\n'
        yield 'ProductIndex == [x \\in {} |-> x]\nRetryDelays == [x \\in {} |-> x]\n'
    else:
        # Only helper values used by the old logical tables enter TLA. Full
        # branch/input/exclusion values remain in the independently checked wire.
        used_values = {row[i] for row in wire['worker_pool'] for i,field in enumerate(WORKER_COLUMNS)
            if field in WORKER_VALUE_REFS}
        used_values.update(row[i] for row in wire['product_states'] for i in (4,5,7,8,9))
        used_descriptors = {row[1] for group in wire['product_edges'] for row in group}
        used_values.update(wire['edge_descriptors'][identity][5] for identity in used_descriptors)
        used_values.update(wire['enabledness'])
        # Nested values contain no wire references, so this is the full
        # transitive dependency closure. Every generated lookup uses this domain.
        require(all(integer(ref,0,len(wire['value_pool'])-1) for ref in used_values),
            'missing generated value dependency')
        for row in wire['product_states']:
            require(all(row[i] in used_values for i in (4,5,7,8,9)),'missing generated state dependency')
        require(all(wire['edge_descriptors'][identity][5] in used_values for identity in used_descriptors) and
            all(ref in used_values for ref in wire['enabledness']),'missing generated fairness dependency')
        def value_expression(ref):
            require(ref in used_values,'missing generated Worker dependency')
            return 'GeneratedValues['+str(ref+1)+']'
        value_ids = sorted(used_values)
        def value_entry(index):
            identity = value_ids[index]
            yield '('+str(identity+1)+' :> '
            yield from tla_chunks(wire['value_pool'][identity])
            yield ')'
        yield 'GeneratedValues == '
        yield from merge_function_chunks(len(value_ids),value_entry)
        yield '\nGeneratedWorkers == <<'
        for i,row in enumerate(wire['worker_pool']):
            if i: yield ','
            yield '['
            for j,(field,cell) in enumerate(zip(WORKER_COLUMNS,row)):
                if j: yield ','
                yield field+' |-> '
                if field in WORKER_VALUE_REFS: yield value_expression(cell)
                else: yield tla(cell)
            yield ']'
        yield '>>\nGeneratedContexts == '
        yield from tla_chunks([c['id'] for c in wire['contexts']])
        yield '\nGeneratedProductRows == '
        yield from sequence_chunks(row[:10] for row in wire['product_states'])
        yield ("\nGeneratedProductState(r, identity) == [id |-> identity, context |-> GeneratedContexts[r[1]+1], "
            "sql |-> r[2], workers |-> <<GeneratedWorkers[r[3]+1],GeneratedWorkers[r[4]+1]>>, "
            "available |-> GeneratedValues[r[5]+1], completed |-> GeneratedValues[r[6]+1], mono_now_ms |-> r[7], "
            "wall_anchors |-> GeneratedValues[r[8]+1], mono_anchors |-> GeneratedValues[r[9]+1], "
            "claim_loss_keys |-> GeneratedValues[r[10]+1]]\n"
            "ProductStates == [i \\in 1..Len(GeneratedProductRows) |-> GeneratedProductState(GeneratedProductRows[i],i-1)]\n"
            "GeneratedProductKey(p) == <<p.context,p.sql,p.workers,p.available,p.completed,p.mono_now_ms,"
            "p.wall_anchors,p.mono_anchors,p.claim_loss_keys>>\n")
        # Exact descriptor references share full-pair fair sets. The executable
        # AsyncDriverRefinement Next relation remains in its unchanged model.
        descriptor_ids = sorted(used_descriptors)
        yield 'GeneratedProductEdgeFairness == '
        yield from merge_function_chunks(len(descriptor_ids),lambda i:
            '('+str(descriptor_ids[i]+1)+' :> '+value_expression(wire['edge_descriptors'][descriptor_ids[i]][5])+')')
        yield '\nGeneratedProductEdgeRows == '
        yield from sequence_chunks(wire['product_edges'])
        yield ("\nGeneratedProductEdge(source,r) == [src |-> source, dst |-> r[1], fair |-> GeneratedProductEdgeFairness[r[2]+1]]\n"
            "ProductEdges == UNION {{GeneratedProductEdge(i-1,GeneratedProductEdgeRows[i][j]) : "
            "j \\in 1..Len(GeneratedProductEdgeRows[i])} : i \\in 1..Len(GeneratedProductEdgeRows)}\n")
        def outgoing_rows():
            for group in wire['product_edges']:
                # Every independently checked branch for one pair has the
                # same union. Retain only this predecessor's temporary map.
                targets = {row[0]:row[1] for row in group}
                yield [[target,targets[target]] for target in sorted(targets)]
        yield 'GeneratedProductOutgoingRows == '
        yield from sequence_chunks(outgoing_rows())
        yield ("\nProductOutgoing == [i \\in 1..Len(GeneratedProductOutgoingRows) |-> "
            "[j \\in 1..Len(GeneratedProductOutgoingRows[i]) |-> "
            "[dst |-> GeneratedProductOutgoingRows[i][j][1], "
            "fair |-> GeneratedProductEdgeFairness[GeneratedProductOutgoingRows[i][j][2]+1]]]]\n")
        yield 'GeneratedProductEnabledRows == '
        yield from sequence_chunks(wire['enabledness'])
        yield ("\nProductEnabled == [i \\in 1..Len(GeneratedProductEnabledRows) |-> "
            "[state |-> i-1, actions |-> GeneratedValues[GeneratedProductEnabledRows[i]+1]]]\nProductIndex == ")
        yield from merge_function_chunks(len(wire['product_states']),
            lambda i:'(GeneratedProductKey(ProductStates['+str(i+1)+']) :> '+str(i)+')')
        delays = {(snapshot['raw_job_id'],snapshot['failed_attempts']) for state in store['states']
            for snapshot in state['raw']['episode']['claim_snapshots']}
        delays.update((w['claim_snapshot']['raw_job_id'],w['claim_snapshot']['failed_attempts'])
            for p in bundle['product_states'] for w in p['w'] if w['claim_snapshot'] is not None)
        delays = sorted(delays)
        yield '\nRetryDelays == '
        yield from merge_function_chunks(len(delays),
            lambda i:'('+tla(list(delays[i]))+' :> '+str(retry_delay_ms(*delays[i]))+')')
        yield '\n'
    yield '=============================================================================\n'


def bounded_table_write(stream,chunks,max_bytes=BUNDLE_MAX_BYTES):
    size = 0
    for chunk in chunks:
        size += len(chunk.encode('utf-8'))
        require(size <= max_bytes,'SourceTables byte bound exceeded')
        stream.write(chunk)
    return size


def write_tables(path,driver,wire,max_bytes=BUNDLE_MAX_BYTES):
    """Publish only a complete bounded member; a cutoff removes its partial file."""
    path = Path(path); partial = path.with_name(path.name+'.partial')
    try:
        with partial.open('w',encoding='utf-8',newline='\n') as stream:
            size = bounded_table_write(stream,table_chunks(driver,wire),max_bytes)
        partial.replace(path)
        return size
    finally:
        if partial.exists(): partial.unlink()


def tables(driver,store):
    # Small parser fixtures may request text; production prepare streams to disk.
    stream = io.StringIO()
    bounded_table_write(stream,table_chunks(driver,store))
    return stream.getvalue()


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
