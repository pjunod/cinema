"""Out-of-core history product, SCC producer, and independent certificates.

Source semantic records must be verified first. Producer discovery is never
trusted by verification: trees and every induced edge are inspected separately.
"""
from __future__ import annotations

from contextlib import nullcontext
import json

from paged_codec import BoundedList, Invalid, Incomplete, bounded_json, canonical, encoded, envelope, inner, ref, require

CLAIMS = tuple(f'00000000-0000-4000-8000-{10+j*4+a:012}' for j in range(2) for a in range(2))
OBLIGATIONS = tuple({'node': n, 'job': j, 'claim_id': a} for n in range(2) for j in range(2) for a in CLAIMS)


def history(product, *, goal, capacity, previous=None):
    pending = set() if previous is None else {canonical(x) for x in previous['pending_attempts']}
    for item in OBLIGATIONS:
        w = product['w'][item['node']]
        matching = w['stage'] == 'driver' and w['job'] == item['job'] and w['attempt'] == item['claim_id']
        if matching and w['started']:
            pending.add(canonical(item))
        if matching and w['dropped']:
            pending.discard(canonical(item))
    return {'seen_goal': goal or (previous is not None and previous['seen_goal']),
            'capacity_ok': capacity and (previous is None or previous['capacity_ok']),
            'pending_attempts': sorted((json.loads(x) for x in pending), key=lambda x: (x['node'], x['job'], x['claim_id']))}


def views(context):
    yield context, 'seen_goal_false', None
    for prop in ('pending_true', 'capacity_ok_pending_true'):
        for obligation in OBLIGATIONS:
            yield context, prop, obligation


def in_view(node, prop, obligation):
    if prop == 'seen_goal_false':
        return not node['seen_goal']
    return obligation in node['pending_attempts'] and (prop != 'capacity_ok_pending_true' or node['capacity_ok'])


def fairness(context):
    """Exact FairRuntime/FairExternal/stable WF/SF families in original Spec."""
    stable = context in ('stable', 'weak_admission', 'no_claim_win')
    runtime = ('ClaimSend', 'ClaimApply', 'ClaimReply', 'ClaimRecover', 'ClaimReturn', 'Dispatch',
               'StoreApply', 'SimpleApply', 'Reply', 'DriverReturn', 'RetireLocal',
               'RetireCallReturn', 'AdapterSample', 'OrdinaryLock', 'OrdinaryLocal',
               'OrdinaryCallReturn', 'OrdinaryObserve', 'ExternalReturn', 'JoinReturn', 'RetireDrain')
    weak = {f + '/' + str(n) for f in runtime for n in range(2)}
    strong = set()
    if context == 'no_claim_win':
        weak -= {'ClaimApply/0', 'ClaimApply/1'}
    if context == 'old_poll_policy':
        weak -= {'ExternalReturn/0', 'ExternalReturn/1'}
    if context == 'withheld_join':
        weak -= {'JoinReturn/0', 'JoinReturn/1'}
    weak.add('ClaimRecoveryWake/global')
    if stable:
        weak.update(('MakeAvailable/0', 'MakeAvailable/1', 'EligibilityAdvance/global'))
        (weak if context == 'weak_admission' else strong).update(('Admission/0', 'Admission/1'))
        if context != 'no_claim_win':
            strong.update(('ClaimWin/0', 'ClaimWin/1'))
    return sorted(weak), sorted(strong)


class Temporal:
    def __init__(self, index):
        self.index, self.db, self.budget = index, index.db, index.budget
        self.execution = getattr(index, 'execution', None)
        self.db.executescript('''
        CREATE TABLE IF NOT EXISTS tm_nodes(id INTEGER PRIMARY KEY,context TEXT,product INTEGER,history BLOB,discovered_by INTEGER,expanded INTEGER DEFAULT 0,UNIQUE(product,history));
        CREATE INDEX IF NOT EXISTS tm_context ON tm_nodes(context,id);
        CREATE TABLE IF NOT EXISTS tm_edges(id INTEGER PRIMARY KEY,src INTEGER,dst INTEGER,product_edge INTEGER,UNIQUE(src,dst,product_edge));
        CREATE INDEX IF NOT EXISTS tm_src ON tm_edges(src,id);
        CREATE INDEX IF NOT EXISTS tm_dst ON tm_edges(dst,id);
        CREATE TABLE IF NOT EXISTS tm_roots(context TEXT PRIMARY KEY,node INTEGER);
        CREATE TABLE IF NOT EXISTS tm_views(view TEXT PRIMARY KEY,context TEXT,property TEXT,obligation BLOB);
        CREATE TABLE IF NOT EXISTS tm_members(view TEXT,node INTEGER,component INTEGER,PRIMARY KEY(view,node));
        CREATE INDEX IF NOT EXISTS tm_component ON tm_members(component,node);
        CREATE TABLE IF NOT EXISTS graph_nodes(node INTEGER PRIMARY KEY);
        CREATE TABLE IF NOT EXISTS graph_edges(edge INTEGER PRIMARY KEY,src INTEGER,dst INTEGER);
        CREATE INDEX IF NOT EXISTS graph_src ON graph_edges(src,edge);
        CREATE INDEX IF NOT EXISTS graph_dst ON graph_edges(dst,edge);
        CREATE TABLE IF NOT EXISTS dfs_seen(node INTEGER PRIMARY KEY,finish INTEGER);
        CREATE TABLE IF NOT EXISTS dfs_stack(depth INTEGER PRIMARY KEY,node INTEGER,last_edge INTEGER);
        CREATE TABLE IF NOT EXISTS dfs_components(node INTEGER PRIMARY KEY,component INTEGER);
        CREATE TABLE IF NOT EXISTS graph_queue(ticket INTEGER PRIMARY KEY AUTOINCREMENT,node INTEGER UNIQUE);
        CREATE TABLE IF NOT EXISTS tree_seen(node INTEGER PRIMARY KEY,parent_edge INTEGER);
        CREATE TABLE IF NOT EXISTS component_rank(component INTEGER PRIMARY KEY,rank INTEGER);
        ''')

    def product(self, identity):
        if self.execution is not None:self.index.get('Product',identity)
        row = self.db.execute('SELECT payload FROM v_products WHERE id=?', (identity,)).fetchone()
        require(row is not None, 'monitor missing verified product')
        return bounded_json(row[0],self.budget)

    def facts(self, product):
        row = self.db.execute('SELECT goal,capacity FROM v_products WHERE id=?', (product,)).fetchone()
        require(row is not None, 'unverified monitor product')
        return bool(row[0]), bool(row[1])

    def node(self, identity):
        row = self.db.execute('SELECT context,product,history FROM tm_nodes WHERE id=?', (identity,)).fetchone()
        require(row is not None, 'missing monitor node')
        return {'id': identity, 'context': row[0], 'product': row[1], **bounded_json(row[2],self.budget)}

    def _intern(self, product, previous=None, edge=None):
        p = self.product(product)
        goal, capacity = self.facts(product)
        h = history(p, goal=goal, capacity=capacity, previous=previous)
        self.db.execute('INSERT OR IGNORE INTO tm_nodes(context,product,history,discovered_by) VALUES(?,?,?,?)',
                        (p['context'], product, encoded(h), edge))
        return self.db.execute('SELECT id FROM tm_nodes WHERE product=? AND history=?', (product, encoded(h))).fetchone()[0]

    def produce_monitors(self):
        if self.db.execute("SELECT 1 FROM operation_state WHERE operation='produce_monitor'").fetchone() is None:
            with self.step('produce_monitor'):
                for context, product in self.db.execute('SELECT context,initial FROM v_roots ORDER BY context'):
                    identity = self._intern(product)
                    self.db.execute('INSERT OR IGNORE INTO tm_roots VALUES(?,?)', (context,identity))
                self.db.execute("INSERT INTO operation_state VALUES('produce_monitor',?,?)",(encoded(None),encoded({'initialized':True})))
        while row := self.db.execute('SELECT id,product FROM tm_nodes WHERE expanded=0 ORDER BY id LIMIT 1').fetchone():
            identity, product = row
            before = self.node(identity)
            # One entire predecessor is atomic, including every original branch.
            with self.step('produce_monitor', position=None):
                for edge, target in self.db.execute('SELECT id,dst FROM v_edges WHERE src=? ORDER BY id', (product,)):
                    nxt = self._intern(target, before, edge)
                    self.db.execute('INSERT OR IGNORE INTO tm_edges(src,dst,product_edge) VALUES(?,?,?)', (identity, nxt, edge))
                self.db.execute('UPDATE tm_nodes SET expanded=1 WHERE id=?', (identity,))
            self.budget.check(predecessors=1)
        for identity, in self.db.execute('SELECT id FROM tm_nodes ORDER BY id'):
            self.index.propose(envelope('MonitorNode', self.node(identity)),'produce_monitor')
        for identity, src, dst, pe in self.db.execute('SELECT * FROM tm_edges ORDER BY id'):
            self.index.propose(envelope('MonitorEdge', {'id': identity, 'from_monitor': src, 'to_monitor': dst, 'product_edge': pe}),'produce_monitor')
        with self.step('produce_monitor'):
            self.db.execute("INSERT OR REPLACE INTO v_progress VALUES('monitor_build_complete',0,1)")

    def verify_monitors(self):
        saved = self.db.execute("SELECT state FROM operation_state WHERE operation='verify_monitors'").fetchone()
        state = {'phase':'nodes','after':0} if saved is None else bounded_json(saved[0],self.budget)
        if saved is None:
            with self.step('check_monitor_node'):
                for table in ('checked_monitors','checked_monitor_edges','monitor_queue'):
                    self.db.execute('DELETE FROM '+table)
                self.db.execute("INSERT INTO operation_state VALUES('verify_monitors',?,?)",(encoded(None),encoded(state)))
        if state['phase']=='nodes':
            for n in self.index.find_after('MonitorNode',state['after']):
                if n['id']<=state['after']:
                    continue
                with self.step('check_monitor_node',claim='monitor_range_checked',coverage={
                        'record_kind':'MonitorNode','lower_exclusive':state['after'],'upper_inclusive':n['id'],
                        'count':1,'scope':self.execution.cursor()['scope'] if self.execution else {}}) as quantum:
                    p=self.product(n['product'])
                    require(n['context']==p['context'],'monitor context drift')
                    require(n['pending_attempts']==sorted(n['pending_attempts'],key=lambda x:(x['node'],x['job'],x['claim_id'])) and
                        all(x in OBLIGATIONS for x in n['pending_attempts']),'monitor fixed obligation universe/order')
                    try:
                        self.db.execute('INSERT INTO checked_monitors(id,product,history,context) VALUES(?,?,?,?)',
                            (n['id'],n['product'],encoded({k:n[k] for k in ('seen_goal','capacity_ok','pending_attempts')}),n['context']))
                    except Exception as error:
                        raise Invalid('duplicate exact monitor identity') from error
                    if quantum is not None:
                        quantum.credit('checked_monitor_node',n['id'])
                    state['after']=n['id']
                    self.db.execute("UPDATE operation_state SET state=? WHERE operation='verify_monitors'",(encoded(state),))
            with self.step('check_monitor_edge'):
                state={'phase':'edges','after':0}
                self.db.execute("UPDATE operation_state SET state=? WHERE operation='verify_monitors'",(encoded(state),))
        if state['phase']=='edges':
            for e in self.index.find_after('MonitorEdge',state['after']):
                if e['id']<=state['after']:
                    continue
                with self.step('check_monitor_edge',claim='monitor_range_checked',coverage={
                        'record_kind':'MonitorEdge','lower_exclusive':state['after'],'upper_inclusive':e['id'],
                        'count':1,'scope':self.execution.cursor()['scope'] if self.execution else {}}) as quantum:
                    src=self.index.get('MonitorNode',e['from_monitor'])
                    dst=self.index.get('MonitorNode',e['to_monitor'])
                    edge=self.index.get('ProductEdge',e['product_edge'])
                    require((src['product'],dst['product'])==(edge['from_product'],edge['to_product']),'monitor edge projection')
                    goal,capacity=self.facts(dst['product'])
                    expected=history(self.product(dst['product']),goal=goal,capacity=capacity,previous=src)
                    require(all(dst[k]==v for k,v in expected.items()),'monitor transition history reset')
                    try:
                        self.db.execute('INSERT INTO checked_monitor_edges VALUES(?,?,?,?)',(e['id'],src['id'],dst['id'],edge['id']))
                    except Exception as error:
                        raise Invalid('duplicate monitored original branch') from error
                    if quantum is not None:
                        quantum.credit('checked_monitor_edge',e['id'])
                    state['after']=e['id']
                    self.db.execute("UPDATE operation_state SET state=? WHERE operation='verify_monitors'",(encoded(state),))
            with self.step('check_monitor_successors'):
                state={'phase':'successors','after':0}
                self.db.execute("UPDATE operation_state SET state=? WHERE operation='verify_monitors'",(encoded(state),))
        if state['phase']=='successors':
            for n in self.index.find_after('MonitorNode',state['after']):
                if n['id']<=state['after']:
                    continue
                with self.step('check_monitor_successors'):
                    require(self.db.execute('SELECT id FROM v_edges WHERE src=? EXCEPT SELECT pe FROM checked_monitor_edges WHERE src=?',(n['product'],n['id'])).fetchone() is None,'omitted monitor successor')
                    state['after']=n['id']
                    self.db.execute("UPDATE operation_state SET state=? WHERE operation='verify_monitors'",(encoded(state),))
            with self.step('monitor_reachability'):
                self.db.execute('DELETE FROM tm_roots')
                for context,product in self.db.execute('SELECT context,initial FROM v_roots'):
                    goal,capacity=self.facts(product)
                    h=encoded(history(self.product(product),goal=goal,capacity=capacity))
                    root=self.db.execute('SELECT id FROM checked_monitors WHERE product=? AND history=?',(product,h)).fetchone()
                    require(root is not None,'missing actual Init history')
                    self.db.execute('INSERT INTO tm_roots VALUES(?,?)',(context,root[0]))
                    self.db.execute('INSERT OR IGNORE INTO monitor_queue(node) VALUES(?)',root)
                state={'phase':'reachability','after':0}
                self.db.execute("UPDATE operation_state SET state=? WHERE operation='verify_monitors'",(encoded(state),))
        if state['phase']=='reachability':
            while row:=self.db.execute('SELECT ticket,node FROM monitor_queue ORDER BY ticket LIMIT 1').fetchone():
                with self.step('monitor_reachability'):
                    self.db.execute('DELETE FROM monitor_queue WHERE ticket=?',(row[0],))
                    marked=self.db.execute('UPDATE checked_monitors SET reachable=1 WHERE id=? AND reachable=0',(row[1],)).rowcount
                    if marked:
                        self.db.execute('INSERT OR IGNORE INTO monitor_queue(node) SELECT e.dst FROM checked_monitor_edges e JOIN checked_monitors n ON n.id=e.dst WHERE e.src=? AND n.reachable=0',(row[1],))
            with self.step('monitor_reachability'):
                require(self.db.execute('SELECT 1 FROM checked_monitors WHERE reachable=0 LIMIT 1').fetchone() is None,'orphan monitor history')
                # Existing generated state must agree; it cannot override the
                # independent monitor scan. No dropping/recreating prefix state.
                require(self.db.execute('SELECT id,src,dst,product_edge FROM tm_edges EXCEPT SELECT id,src,dst,pe FROM checked_monitor_edges').fetchone() is None,'producer monitor edges differ')
                self.db.execute('INSERT OR IGNORE INTO tm_edges SELECT id,src,dst,pe FROM checked_monitor_edges')
                self.db.execute('INSERT OR IGNORE INTO tm_nodes(id,context,product,history,expanded) SELECT id,context,product,history,1 FROM checked_monitors')
                state={'phase':'complete','after':0}
                self.db.execute("UPDATE operation_state SET state=? WHERE operation='verify_monitors'",(encoded(state),))

    def select_view(self, context, prop, obligation):
        view = canonical([context, prop, obligation])
        mode = 'verification' if self.db.execute("SELECT complete FROM v_progress WHERE phase='scc_build_complete'").fetchone() == (1,) else 'production'
        active = encoded([mode, view])
        saved = self.db.execute("SELECT scope,state FROM operation_state WHERE operation='select_view'").fetchone()
        scope = {'context':context, 'property':prop, 'obligation':obligation, 'input_root':None, 'family':None}
        if saved is None or saved[0] != active:
            with self.step('select_view', scope=scope):
                self.db.execute('INSERT OR IGNORE INTO tm_views VALUES(?,?,?,?)', (view,context,prop,encoded(obligation)))
                self.db.execute("DELETE FROM operation_state WHERE operation='graph_scope'")
                self.db.execute('DELETE FROM graph_nodes')
                self.db.execute('DELETE FROM graph_edges')
                state = {'phase':'nodes','after':0}
                self.db.execute("INSERT OR REPLACE INTO operation_state VALUES('select_view',?,?)", (active,encoded(state)))
        else:
            state = bounded_json(saved[1],self.budget)
        while state['phase'] == 'nodes':
            row = self.db.execute('SELECT id FROM tm_nodes WHERE context=? AND id>? ORDER BY id LIMIT 1', (context,state['after'])).fetchone()
            with self.step('select_view', scope=scope):
                if row is None:
                    state = {'phase':'edges','after':0}
                else:
                    if in_view(self.node(row[0]),prop,obligation):
                        self.db.execute('INSERT INTO graph_nodes VALUES(?)',row)
                    state['after'] = row[0]
                self.db.execute("UPDATE operation_state SET state=? WHERE operation='select_view'", (encoded(state),))
        while state['phase'] == 'edges':
            row = self.db.execute('SELECT id,src,dst FROM tm_edges WHERE id>? ORDER BY id LIMIT 1',(state['after'],)).fetchone()
            with self.step('select_view', scope=scope):
                if row is None:
                    state['phase']='complete'
                else:
                    if self.db.execute('SELECT count(*) FROM graph_nodes WHERE node IN (?,?)',(row[1],row[2])).fetchone()[0] == (1 if row[1]==row[2] else 2):
                        self.db.execute('INSERT INTO graph_edges VALUES(?,?,?)',row)
                    state['after']=row[0]
                self.db.execute("UPDATE operation_state SET state=? WHERE operation='select_view'", (encoded(state),))
        return view

    def step(self, operation, *, scope=None, position=None, claim='algorithm_checkpoint', coverage=None):
        if self.execution is None:
            return nullcontext()  # Synthetic plumbing fixtures have no CI credit.
        if getattr(self,'_fair_active',False):
            operation='fair_search'
        return self.execution.quantum(operation, stage='monitor_scc_verify' if operation not in ('fair_search',) else 'temporal_components',
            scope=scope, position=position, claim=claim, coverage=coverage)

    def graph_scope(self):
        """A restartable internal identity for the complete selected graph."""
        import hashlib
        saved=self.db.execute("SELECT state FROM operation_state WHERE operation='graph_scope'").fetchone()
        state=({'phase':'nodes','after':0,'hash':hashlib.sha256(b'plurx-exact-monitored-graph-chain-v1\0').hexdigest()}
               if saved is None else bounded_json(saved[0],self.budget))
        while state['phase']!='complete':
            with self.step('dfs_forward'):
                if state['phase']=='nodes':
                    row=self.db.execute('SELECT node FROM graph_nodes WHERE node>? ORDER BY node LIMIT 1',(state['after'],)).fetchone()
                    if row is None:
                        state['phase'],state['after']='edges',0
                    else:
                        state['hash']=hashlib.sha256(bytes.fromhex(state['hash'])+encoded(['node',row[0]])).hexdigest()
                        state['after']=row[0]
                else:
                    row=self.db.execute('SELECT edge,src,dst FROM graph_edges WHERE edge>? ORDER BY edge LIMIT 1',(state['after'],)).fetchone()
                    if row is None:
                        state['phase']='complete'
                    else:
                        state['hash']=hashlib.sha256(bytes.fromhex(state['hash'])+encoded(['edge',*row])).hexdigest()
                        state['after']=row[0]
                self.db.execute("INSERT OR REPLACE INTO operation_state VALUES('graph_scope',?,?)",(encoded(None),encoded(state)))
        return state['hash']

    def partition(self):
        """Persist every DFS frame, finish counter, reverse queue and rank step."""
        scope = self.graph_scope()
        stored = self.db.execute("SELECT scope,state FROM operation_state WHERE operation='partition'").fetchone()
        if stored is None or stored[0] != encoded(scope):
            with self.step('dfs_forward'):
                for table in ('dfs_seen', 'dfs_stack', 'dfs_components', 'graph_queue', 'component_rank'):
                    self.db.execute('DELETE FROM ' + table)
                state = {'phase':'forward', 'finish':0, 'component':0, 'rank':0}
                self.db.execute("INSERT OR REPLACE INTO operation_state VALUES('partition',?,?)", (encoded(scope), encoded(state)))
        else:
            state = bounded_json(stored[1],self.budget)
        while state['phase'] == 'forward':
            with self.step('dfs_forward'):
                frame = self.db.execute('SELECT depth,node,last_edge FROM dfs_stack ORDER BY depth DESC LIMIT 1').fetchone()
                if frame is None:
                    root = self.db.execute('SELECT g.node FROM graph_nodes g LEFT JOIN dfs_seen s ON s.node=g.node WHERE s.node IS NULL ORDER BY g.node LIMIT 1').fetchone()
                    if root is None:
                        state['phase'] = 'reverse'
                    else:
                        self.db.execute('INSERT INTO dfs_seen VALUES(?,NULL)', root)
                        self.db.execute('INSERT INTO dfs_stack VALUES(0,?,0)', root)
                else:
                    depth, node, last = frame
                    edge = self.db.execute('SELECT edge,dst FROM graph_edges WHERE src=? AND edge>? ORDER BY edge LIMIT 1', (node,last)).fetchone()
                    if edge:
                        self.db.execute('UPDATE dfs_stack SET last_edge=? WHERE depth=?', (edge[0],depth))
                        if not self.db.execute('SELECT 1 FROM dfs_seen WHERE node=?', (edge[1],)).fetchone():
                            self.db.execute('INSERT INTO dfs_seen VALUES(?,NULL)', (edge[1],))
                            self.db.execute('INSERT INTO dfs_stack VALUES(?,?,0)', (depth+1,edge[1]))
                    else:
                        state['finish'] += 1
                        self.db.execute('UPDATE dfs_seen SET finish=? WHERE node=?', (state['finish'],node))
                        self.db.execute('DELETE FROM dfs_stack WHERE depth=?', (depth,))
                self.db.execute("UPDATE operation_state SET state=? WHERE operation='partition'", (encoded(state),))
        while state['phase'] == 'reverse':
            with self.step('dfs_reverse'):
                pending = self.db.execute('SELECT ticket,node FROM graph_queue ORDER BY ticket LIMIT 1').fetchone()
                if pending:
                    self.db.execute('DELETE FROM graph_queue WHERE ticket=?', (pending[0],))
                    if self.db.execute('INSERT OR IGNORE INTO dfs_components VALUES(?,?)', (pending[1],state['component'])).rowcount:
                        self.db.execute('INSERT OR IGNORE INTO graph_queue(node) SELECT e.src FROM graph_edges e LEFT JOIN dfs_components d ON d.node=e.src WHERE e.dst=? AND d.node IS NULL', (pending[1],))
                else:
                    root = self.db.execute('SELECT s.node FROM dfs_seen s LEFT JOIN dfs_components d ON d.node=s.node WHERE d.node IS NULL ORDER BY s.finish DESC LIMIT 1').fetchone()
                    if root is None:
                        state['phase'] = 'rank'
                    else:
                        state['component'] += 1
                        self.db.execute('INSERT INTO graph_queue(node) VALUES(?)', root)
                self.db.execute("UPDATE operation_state SET state=? WHERE operation='partition'", (encoded(state),))
        while state['phase'] == 'rank':
            with self.step('condensation_rank'):
                row = self.db.execute('''SELECT DISTINCT d.component FROM dfs_components d LEFT JOIN component_rank r ON r.component=d.component
                    WHERE r.component IS NULL AND NOT EXISTS (SELECT 1 FROM graph_edges e JOIN dfs_components a ON a.node=e.src
                        JOIN dfs_components b ON b.node=e.dst LEFT JOIN component_rank x ON x.component=a.component
                        WHERE b.component=d.component AND a.component!=b.component AND x.component IS NULL) ORDER BY d.component LIMIT 1''').fetchone()
                if row:
                    self.db.execute('INSERT INTO component_rank VALUES(?,?)', (row[0],state['rank']))
                    state['rank'] += 1
                else:
                    require(state['rank'] == state['component'], 'producer condensation cycle')
                    state['phase'] = 'complete'
                self.db.execute("UPDATE operation_state SET state=? WHERE operation='partition'", (encoded(state),))
        return state['component']

    def tree(self,root,component,*,reverse=False):
        partition=self.db.execute("SELECT scope FROM operation_state WHERE operation='partition'").fetchone()
        require(partition is not None,'tree without exact partition scope')
        key=canonical([bounded_json(partition[0],self.budget),component,reverse])
        stored=self.db.execute('SELECT state FROM tree_status WHERE scope=?',(key,)).fetchone()
        state={'phase':'walk','after_edge':0} if stored is None else bounded_json(stored[0],self.budget)
        if stored is None:
            with self.step('build_tree'):
                self.db.execute('DELETE FROM tree_seen')
                self.db.execute('DELETE FROM graph_queue')
                self.db.execute('INSERT INTO tree_seen VALUES(?,NULL)',(root,))
                self.db.execute('INSERT INTO graph_queue(node) VALUES(?)',(root,))
                self.db.execute('INSERT INTO tree_status VALUES(?,?)',(key,encoded(state)))
        src,dst=('dst','src') if reverse else ('src','dst')
        while state['phase']=='walk':
            pending=self.db.execute('SELECT ticket,node FROM graph_queue ORDER BY ticket LIMIT 1').fetchone()
            with self.step('build_tree'):
                if pending is None:
                    state={'phase':'copy','after_node':0}
                else:
                    edge=self.db.execute('SELECT e.edge,e.'+dst+' FROM graph_edges e JOIN dfs_components d ON d.node=e.'+dst+' WHERE e.'+src+'=? AND d.component=? AND e.edge>? ORDER BY e.edge LIMIT 1',(pending[1],component,state['after_edge'])).fetchone()
                    if edge:
                        if self.db.execute('INSERT OR IGNORE INTO tree_seen VALUES(?,?)',(edge[1],edge[0])).rowcount:
                            self.db.execute('INSERT INTO graph_queue(node) VALUES(?)',(edge[1],))
                        state['after_edge']=edge[0]
                    else:
                        self.db.execute('DELETE FROM graph_queue WHERE ticket=?',(pending[0],))
                        state['after_edge']=0
                self.db.execute('UPDATE tree_status SET state=? WHERE scope=?',(encoded(state),key))
        while state['phase']=='copy':
            row=self.db.execute('SELECT node,parent_edge FROM tree_seen WHERE node>? ORDER BY node LIMIT 1',(state['after_node'],)).fetchone()
            with self.step('build_tree'):
                if row:
                    self.db.execute('INSERT INTO tree_certificates VALUES(?,?,?)',(key,row[0],row[1]))
                    state['after_node']=row[0]
                else:
                    state['phase']='complete'
                self.db.execute('UPDATE tree_status SET state=? WHERE scope=?',(encoded(state),key))
        return BoundedList(self.budget,'Scc',(r[0] for r in self.db.execute('SELECT parent_edge FROM tree_certificates WHERE scope=? AND parent_edge IS NOT NULL ORDER BY node',(key,))))

    def produce_sccs(self):
        self.db.execute('CREATE TABLE IF NOT EXISTS completed_views(view TEXT PRIMARY KEY,last_id INTEGER)')
        identity = 0
        for context, in self.db.execute('SELECT context FROM v_roots ORDER BY context'):
            for _, prop, obligation in views(context):
                view = canonical([context, prop, obligation])
                complete = self.db.execute('SELECT last_id FROM completed_views WHERE view=?', (view,)).fetchone()
                if complete is not None:
                    identity = complete[0]
                    continue
                view = self.select_view(context, prop, obligation)
                count = self.partition()
                for component in range(1,count+1):
                    if self.db.execute("SELECT 1 FROM proposed_records WHERE kind='Scc' AND key=?",(canonical(identity+1),)).fetchone():
                        identity+=1
                        continue
                    size = self.db.execute('SELECT count(*) FROM dfs_components WHERE component=?', (component,)).fetchone()[0]
                    if size * 128 > self.budget.member_limit():
                        raise Incomplete('indivisible SCC exceeds admitted resident/table limit')
                    members = BoundedList(self.budget,'Scc',(r[0] for r in self.db.execute('SELECT node FROM dfs_components WHERE component=? ORDER BY node',(component,))))
                    root = members[0]
                    forward = self.tree(root, component)
                    reverse = self.tree(root, component, reverse=True)
                    deps = self.dependencies(members)
                    identity += 1
                    rank = self.db.execute('SELECT rank FROM component_rank WHERE component=?', (component,)).fetchone()[0]
                    record = {'id': identity, 'context': context, 'property': prop, 'obligation': obligation,
                              'monitor_root': root, 'members': members, 'forward_tree': forward, 'reverse_tree': reverse,
                              'condensation_rank': rank, 'outgoing_dependencies': deps}
                    with self.step('build_tree'):
                        self.index.propose(envelope('Scc',record),'build_tree')
                        self.db.executemany('INSERT OR IGNORE INTO tm_members VALUES(?,?,?)',((view,n,identity) for n in members))
                with self.step('build_tree'):
                    self.db.execute('INSERT INTO completed_views VALUES(?,?)',(view,identity))
                self.budget.check()
        with self.step('build_tree'):
            self.db.execute("INSERT OR REPLACE INTO v_progress VALUES('scc_build_complete',0,1)")

    def dependencies(self,members):
        # One indivisible component is already resident-admitted. Its exact
        # outgoing inventory includes exits and original enabledness records.
        wanted={}
        result=BoundedList(self.budget,'Scc')
        def include(kind,key):
            if (kind,key) not in wanted:
                value=ref(self.index.get_envelope(kind,key))
                result.append(value)  # Reserve before growing the key index.
                wanted[(kind,key)]=True
        for identity in members:
            node=self.node(identity)
            key=canonical([node['context'],node['product']])
            include('Enabledness',key)
            for edge,pe in self.db.execute('SELECT id,product_edge FROM tm_edges WHERE src=?',(identity,)):
                include('MonitorEdge',edge)
                include('ProductEdge',pe)
        result.sort(key=lambda r:(r['kind'],canonical(r['id'])))
        return result

    def verify_sccs(self):
        if self.execution is not None and self.db.execute("SELECT complete FROM v_progress WHERE phase='scc_verified_complete'").fetchone()==(1,):
            require(self.index.count('Scc')==self.db.execute('SELECT count(*) FROM verified_sccs').fetchone()[0] and
                self.index.count('MonitorNode')==self.db.execute('SELECT count(*) FROM checked_monitors').fetchone()[0] and
                self.index.count('MonitorEdge')==self.db.execute('SELECT count(*) FROM checked_monitor_edges').fetchone()[0],'completed temporal inventory changed after source verification')
            return
        for s in self.index.find_unchecked('Scc','verified_sccs'):
            if self.db.execute('SELECT 1 FROM verified_sccs WHERE id=?',(s['id'],)).fetchone():continue
            view=canonical([s['context'],s['property'],s['obligation']])
            scope={'context':s['context'],'property':s['property'],'obligation':s['obligation'],
                'input_root':ref(envelope('Scc',s))['sha256'],'family':None}
            saved=self.db.execute('SELECT state FROM scc_check_state WHERE id=?',(s['id'],)).fetchone()
            state={'phase':'members','after':0,'reverse':False} if saved is None else bounded_json(saved[0],self.budget)
            if saved is None:
                with self.step('check_scc',scope=scope):
                    require((s['obligation'] is None)==(s['property']=='seen_goal_false'),'fixed obligation selector')
                    require(s['obligation'] is None or s['obligation'] in OBLIGATIONS,'unknown fixed SCC obligation')
                    require(s['members'] and s['members']==sorted(set(s['members'])) and s['monitor_root'] in s['members'],'component member/root inventory')
                    self.db.execute('INSERT INTO scc_check_state VALUES(?,?)',(s['id'],encoded(state)))
            while state['phase']=='members' and state['after']<len(s['members']):
                identity=s['members'][state['after']]
                with self.step('check_scc',scope=scope):
                    n=self.index.get('MonitorNode',identity)
                    require(n['context']==s['context'] and in_view(n,s['property'],s['obligation']),'component member outside induced view')
                    try:self.db.execute('INSERT INTO verified_scc_members VALUES(?,?,?)',(view,identity,s['id']))
                    except Exception as error:raise Invalid('overlapping independent component partition') from error
                    state['after']+=1
                    self.db.execute('UPDATE scc_check_state SET state=? WHERE id=?',(encoded(state),s['id']))
            if state['phase']=='members':
                with self.step('check_scc',scope=scope):
                    require(s['outgoing_dependencies']==self.dependencies(s['members']),'SCC full outgoing dependency closure')
                    state={'phase':'tree_init','after':0,'reverse':False}
                    self.db.execute('UPDATE scc_check_state SET state=? WHERE id=?',(encoded(state),s['id']))
            while state['phase']!='complete':
                edges=s['reverse_tree' if state['reverse'] else 'forward_tree']
                if state['phase']=='tree_init':
                    with self.step('check_scc',scope=scope):
                        require(len(edges)==len(s['members'])-1 and len(edges)==len(set(edges)),'arborescence edge count')
                        self.db.execute('DELETE FROM certificate_tree')
                        self.db.execute('DELETE FROM tree_seen')
                        self.db.execute('DELETE FROM graph_queue')
                        self.db.execute('INSERT INTO tree_seen VALUES(?,NULL)',(s['monitor_root'],))
                        self.db.execute('INSERT INTO graph_queue(node) VALUES(?)',(s['monitor_root'],))
                        state['phase']='tree_edges';state['after']=0
                        self.db.execute('UPDATE scc_check_state SET state=? WHERE id=?',(encoded(state),s['id']))
                while state['phase']=='tree_edges' and state['after']<len(edges):
                    with self.step('check_scc',scope=scope):
                        edge=edges[state['after']]
                        e=self.index.get('MonitorEdge',edge)
                        a,b=(e['to_monitor'],e['from_monitor']) if state['reverse'] else (e['from_monitor'],e['to_monitor'])
                        require(b!=s['monitor_root'] and all(self.db.execute('SELECT 1 FROM verified_scc_members WHERE view=? AND node=? AND component=?',(view,node,s['id'])).fetchone() for node in (a,b)),'tree edge leaves component/root parent')
                        try:self.db.execute('INSERT INTO certificate_tree VALUES(?,?,?)',(a,b,edge))
                        except Exception as error:raise Invalid('tree duplicate parent') from error
                        state['after']+=1
                        self.db.execute('UPDATE scc_check_state SET state=? WHERE id=?',(encoded(state),s['id']))
                if state['phase']=='tree_edges':
                    with self.step('check_scc',scope=scope):
                        state['phase']='tree_walk';state['after']=0
                        self.db.execute('UPDATE scc_check_state SET state=? WHERE id=?',(encoded(state),s['id']))
                while state['phase']=='tree_walk':
                    pending=self.db.execute('SELECT ticket,node FROM graph_queue ORDER BY ticket LIMIT 1').fetchone()
                    with self.step('check_scc',scope=scope):
                        if pending is None:
                            require(self.db.execute('SELECT count(*) FROM tree_seen').fetchone()[0]==len(s['members']),'tree not rooted/connected')
                            state={'phase':'complete' if state['reverse'] else 'tree_init','after':0,'reverse':True}
                        else:
                            child=self.db.execute('SELECT dst,edge FROM certificate_tree WHERE src=? AND edge>? ORDER BY edge LIMIT 1',(pending[1],state['after'])).fetchone()
                            if child:
                                if self.db.execute('INSERT OR IGNORE INTO tree_seen VALUES(?,?)',child).rowcount:
                                    self.db.execute('INSERT INTO graph_queue(node) VALUES(?)',(child[0],))
                                state['after']=child[1]
                            else:
                                self.db.execute('DELETE FROM graph_queue WHERE ticket=?',(pending[0],));state['after']=0
                        self.db.execute('UPDATE scc_check_state SET state=? WHERE id=?',(encoded(state),s['id']))
            with self.step('check_scc',scope=scope,claim='scc_scope_checked',coverage={'record_kind':'Scc','lower_exclusive':None,'upper_inclusive':s['id'],'count':1,'scope':scope}) as quantum:
                self.index.get('Scc',s['id'])
                self.db.execute('INSERT INTO verified_sccs VALUES(?)',(s['id'],))
                if quantum is not None:quantum.credit('verified_scc',s['id'])
        # Every fixed obligation view, including empty ones, is regenerated.
        for context, in self.db.execute('SELECT context FROM v_roots ORDER BY context'):
            for _,prop,obligation in views(context):
                view=canonical([context,prop,obligation])
                if self.db.execute('SELECT 1 FROM verified_views WHERE view=?',(view,)).fetchone():continue
                self.select_view(context,prop,obligation)
                scope={'context':context,'property':prop,'obligation':obligation,'input_root':None,'family':None}
                saved=self.db.execute('SELECT state FROM view_check_state WHERE view=?',(view,)).fetchone()
                state={'phase':'members','after':0} if saved is None else bounded_json(saved[0],self.budget)
                if saved is None:
                    with self.step('check_scc',scope=scope):self.db.execute('INSERT INTO view_check_state VALUES(?,?)',(view,encoded(state)))
                while state['phase']=='members':
                    node=self.db.execute('SELECT node FROM graph_nodes WHERE node>? ORDER BY node LIMIT 1',(state['after'],)).fetchone()
                    with self.step('check_scc',scope=scope):
                        if node:
                            require(self.db.execute('SELECT 1 FROM verified_scc_members WHERE view=? AND node=?',(view,node[0])).fetchone() is not None,'omitted relevant obligation/component member')
                            state['after']=node[0]
                        else:state={'phase':'edges','after':0}
                        self.db.execute('UPDATE view_check_state SET state=? WHERE view=?',(encoded(state),view))
                while state['phase']=='edges':
                    edge=self.db.execute('SELECT edge,src,dst FROM graph_edges WHERE edge>? ORDER BY edge LIMIT 1',(state['after'],)).fetchone()
                    with self.step('check_scc',scope=scope):
                        if edge:
                            ca=self.db.execute('SELECT component FROM verified_scc_members WHERE view=? AND node=?',(view,edge[1])).fetchone()[0]
                            cb=self.db.execute('SELECT component FROM verified_scc_members WHERE view=? AND node=?',(view,edge[2])).fetchone()[0]
                            if ca!=cb:require(self.index.get('Scc',ca)['condensation_rank']<self.index.get('Scc',cb)['condensation_rank'],'SCC condensation edge not increasing')
                            state['after']=edge[0]
                        else:state={'phase':'complete','after':0}
                        self.db.execute('UPDATE view_check_state SET state=? WHERE view=?',(encoded(state),view))
                with self.step('check_scc',scope=scope,claim='scc_scope_checked') as quantum:
                    require(self.db.execute('SELECT node FROM verified_scc_members WHERE view=? EXCEPT SELECT node FROM graph_nodes',(view,)).fetchone() is None,'extra component member')
                    self.db.execute('INSERT INTO verified_views VALUES(?)',(view,))
                    if quantum is not None:quantum.credit('verified_view',view)
        if self.execution is not None:
            with self.execution.quantum('stage_finish',stage='monitor_scc_verify'):
                self.db.execute("INSERT OR REPLACE INTO v_progress VALUES('scc_verified_complete',0,1)")

    def replay_witness(self, witness):
        self.index.schema.check(witness, 'Counterexample')
        require(witness['source'] == self.index.source, 'counterexample source drift')
        s = self.index.get('Scc', witness['monitor_component'])
        require(s['property'] in ('seen_goal_false','capacity_ok_pending_true'),'counterexample view does not express the named original property')
        root = self.db.execute('SELECT initial FROM v_roots WHERE context=?', (s['context'],)).fetchone()[0]
        require(root == witness['original_initial'], 'witness did not start at original Init')
        current = self.db.execute('SELECT node FROM tm_roots WHERE context=?', (s['context'],)).fetchone()[0]
        for edge in witness['prefix']:
            e = self.index.get('MonitorEdge', edge)
            require(e['from_monitor'] == current, 'discontinuous original-Init prefix')
            current = e['to_monitor']
        start = current
        visited, occurred = {current}, set()
        for edge in witness['cycle']:
            e = self.index.get('MonitorEdge', edge)
            require(e['from_monitor'] == current, 'discontinuous fair cycle')
            pair = self.index.get('ProductEdge', e['product_edge'])
            a, b = self.node(e['from_monitor']), self.node(e['to_monitor'])
            if a['product'] != b['product']:
                occurred.update(self.index.get('FairPair', canonical([s['context'], a['product'], b['product']]))['labels'])
            current = e['to_monitor']
            visited.add(current)
        if witness['cycle']:
            require(current == start and witness['stutter_at'] is None, 'counterexample cycle not closed')
        else:
            require(witness['stutter_at'] == self.node(current)['product'], 'implicit original Product stutter endpoint differs')
        require(all(n in s['members'] for n in visited), 'witness recurrent walk outside fixed component')
        weak, strong = fairness(s['context'])
        enabled = [set(self.index.get('Enabledness', canonical([s['context'], self.node(n)['product']]))['full_graph_labels']) for n in visited]
        require(all(label in occurred or any(label not in en for en in enabled) for label in weak), 'unfair weak-fair cycle/stutter')
        require(all(label in occurred or all(label not in en for en in enabled) for label in strong), 'unfair strong-fair cycle/stutter')
        for n in visited:
            require(in_view(self.node(n), s['property'], s['obligation']), 'witness property/history violation absent')
        conditional = s['property'] == 'capacity_ok_pending_true'
        require(witness['capacity_prefix_verified'] == conditional, 'capacity witness annotation mismatch')
        require(not conditional or all(self.node(n)['capacity_ok'] for n in visited), 'capacity was false in original prefix')
        expected = 'BothSelectedCompletion' if s['property'] == 'seen_goal_false' else 'ConditionalAttemptCleanup'
        require(witness['expected_first_failure'] == expected, 'witness named property mismatch')
        require(witness['full_graph_fairness']['kind'] == 'fairness_receipt', 'witness fairness receipt kind')
        from pathlib import Path
        from paged_codec import sha_bytes
        reference=witness['full_graph_fairness']
        row=self.index.db.execute('SELECT digest,path FROM blobs WHERE kind=? AND key=?',(reference['kind'],canonical(reference['id']))).fetchone()
        require(row is not None and row[0]==reference['sha256'],'fairness receipt is not authenticated')
        path=Path(row[1])
        require(path.is_file() and not path.is_symlink() and path.stat().st_size<=self.budget.member_limit(),'unsafe/oversized fairness receipt')
        raw=path.read_bytes()
        require(sha_bytes(raw)==reference['sha256'],'fairness receipt bytes changed')
        value=bounded_json(raw,self.budget)
        expected={'schema_version':6,'source':self.index.source,'session_id':self.index.session_id,
            'component':s['id'],'property':s['property'],'obligation':s['obligation'],
            'full_original_enabledness':True,'nonstuttering_pair_occurrence':True,
            'prefix':witness['prefix'],'cycle':witness['cycle'],'stutter_at':witness['stutter_at']}
        require(value==expected and encoded(value)==raw,'fairness receipt differs from actual replayed scope/history/walk')

        return True

    def _path(self, start, target, *, restricted=False):
        """Restartable actual-edge BFS; every committed edge has a cursor."""
        if start == target:
            return []
        scope=encoded([start,target,restricted,self.graph_scope() if restricted else 'full'])
        saved=self.db.execute("SELECT scope,state FROM operation_state WHERE operation='witness_path'").fetchone()
        if saved is None or saved[0]!=scope:
            with self.step('fair_search'):
                self.db.execute('DELETE FROM path_seen')
                self.db.execute('DELETE FROM graph_queue')
                self.db.execute('INSERT INTO path_seen VALUES(?,NULL,NULL)',(start,))
                self.db.execute('INSERT INTO graph_queue(node) VALUES(?)',(start,))
                state={'found':False,'node':None,'after':0}
                self.db.execute("INSERT OR REPLACE INTO operation_state VALUES('witness_path',?,?)",(scope,encoded(state)))
        else:
            state=bounded_json(saved[1],self.budget)
        table,edge_col=('graph_edges','edge') if restricted else ('tm_edges','id')
        while not state['found']:
            with self.step('fair_search'):
                if state['node'] is None:
                    row=self.db.execute('SELECT ticket,node FROM graph_queue ORDER BY ticket LIMIT 1').fetchone()
                    require(row is not None,'required actual-edge witness path absent')
                    self.db.execute('DELETE FROM graph_queue WHERE ticket=?',(row[0],))
                    state['node'],state['after']=row[1],0
                edge=self.db.execute('SELECT '+edge_col+',dst FROM '+table+' WHERE src=? AND '+edge_col+'>? ORDER BY '+edge_col+' LIMIT 1',(state['node'],state['after'])).fetchone()
                if edge is None:
                    state['node']=None
                else:
                    state['after']=edge[0]
                    if self.db.execute('INSERT OR IGNORE INTO path_seen VALUES(?,?,?)',(edge[1],state['node'],edge[0])).rowcount:
                        self.db.execute('INSERT INTO graph_queue(node) VALUES(?)',(edge[1],))
                    state['found']=edge[1]==target
                self.db.execute("UPDATE operation_state SET state=? WHERE operation='witness_path'",(encoded(state),))
        result,node=BoundedList(self.budget,'Counterexample'),target
        while node!=start:
            row=self.db.execute('SELECT parent,edge FROM path_seen WHERE node=?',(node,)).fetchone()
            require(row is not None,'persisted witness path lost its predecessor')
            node,edge=row
            result.append(edge)
            self.budget.check()
        result.reverse()
        return result

    def fair_walk(self, component):
        """Exact fair-language search with durable pruning and walk progress.

        SF without an occurrence removes its enabled nodes and repartitions;
        WF enabled everywhere without an occurrence rejects that recurrent SCC.
        All enabledness queries use the original complete successor graph.
        """
        self._fair_active=True
        try:
            return self._fair_walk(component)
        finally:
            self._fair_active=False

    def _fair_walk(self, component):
        key='fair_walk:'+str(component['id'])
        scope=encoded(component)
        saved=self.db.execute('SELECT scope,state FROM operation_state WHERE operation=?',(key,)).fetchone()
        if saved is None:
            with self.step('fair_search'):
                self.db.execute('DELETE FROM fair_groups')
                self.db.execute('DELETE FROM fair_work')
                group=self.db.execute('INSERT INTO fair_work DEFAULT VALUES').lastrowid
                self.db.executemany('INSERT INTO fair_groups VALUES(?,?)',((group,n) for n in component['members']))
                state={'phase':'group','group':None,'part':1,'count':0,'result':None}
                self.db.execute('INSERT INTO operation_state VALUES(?,?,?)',(key,scope,encoded(state)))
        else:
            require(saved[0]==scope,'fair-search component changed under its cursor')
            state=bounded_json(saved[1],self.budget)
        weak,strong=fairness(component['context'])
        def save():
            raw=encoded(state)
            if len(raw)>self.budget.member_limit():
                raise Incomplete('indivisible fair-search history exceeds resident allowance')
            self.db.execute('UPDATE operation_state SET state=? WHERE operation=?',(raw,key))
        def select_nodes(nodes):
            self.db.execute("DELETE FROM operation_state WHERE operation='graph_scope'")
            self.db.execute('DELETE FROM graph_nodes')
            self.db.executemany('INSERT INTO graph_nodes VALUES(?)',((n,) for n in nodes))
            self.db.execute('DELETE FROM graph_edges')
            self.db.execute('INSERT INTO graph_edges SELECT e.id,e.src,e.dst FROM tm_edges e JOIN graph_nodes a ON a.node=e.src JOIN graph_nodes b ON b.node=e.dst')
        while state['phase']!='complete':
            if state['phase']=='group':
                with self.step('fair_search'):
                    item=self.db.execute('SELECT id FROM fair_work WHERE status=0 ORDER BY id LIMIT 1').fetchone()
                    if item is None:
                        state['phase']='complete'
                    else:
                        state['group']=item[0]
                        self.db.execute("DELETE FROM operation_state WHERE operation='graph_scope'")
                        self.db.execute('DELETE FROM graph_nodes')
                        self.db.execute('INSERT INTO graph_nodes SELECT node FROM fair_groups WHERE group_id=?',(item[0],))
                        self.db.execute('DELETE FROM graph_edges')
                        self.db.execute('INSERT INTO graph_edges SELECT e.id,e.src,e.dst FROM tm_edges e JOIN graph_nodes a ON a.node=e.src JOIN graph_nodes b ON b.node=e.dst')
                        state['phase']='partition'
                    save()
            elif state['phase']=='partition':
                count=self.partition()
                with self.step('fair_search'):
                    state.update(phase='part',part=1,count=count)
                    save()
            elif state['phase']=='part':
                with self.step('fair_search'):
                    if state['part']>state['count']:
                        self.db.execute('UPDATE fair_work SET status=1 WHERE id=?',(state['group'],))
                        state['phase']='group'
                        save()
                        continue
                    nodes=BoundedList(self.budget,'Scc')
                    for row in self.db.execute('SELECT node FROM dfs_components WHERE component=? ORDER BY node',(state['part'],)):
                        nodes.append(row[0])
                    require(nodes,'empty recurrent SCC')
                    enabled={}
                    enabled_charge=BoundedList(self.budget,None)
                    for n in nodes:
                        labels=self.index.get('Enabledness',canonical([component['context'],self.node(n)['product']]))['full_graph_labels']
                        enabled_charge.append([n,labels])
                        enabled[n]=set(labels)
                    occurrence={}
                    for edge,src,dst in self.db.execute('SELECT e.edge,e.src,e.dst FROM graph_edges e JOIN dfs_components a ON a.node=e.src JOIN dfs_components b ON b.node=e.dst WHERE a.component=? AND b.component=? ORDER BY e.edge',(state['part'],state['part'])):
                        p,q=self.node(src)['product'],self.node(dst)['product']
                        if p!=q:
                            for label in self.index.get('FairPair',canonical([component['context'],p,q]))['labels']:
                                occurrence.setdefault(label,edge)
                    if any(label not in occurrence and all(label in enabled[n] for n in nodes) for label in weak):
                        state['part']+=1
                    else:
                        rejected=next((label for label in strong if label not in occurrence and any(label in enabled[n] for n in nodes)),None)
                        if rejected is not None:
                            survivors=BoundedList(self.budget,'Scc')
                            for n in nodes:
                                if rejected not in enabled[n]:survivors.append(n)
                            if survivors:
                                nxt=self.db.execute('INSERT INTO fair_work DEFAULT VALUES').lastrowid
                                self.db.executemany('INSERT INTO fair_groups VALUES(?,?)',((nxt,n) for n in survivors))
                            state['part']+=1
                        else:
                            select_nodes(nodes)
                            tasks=BoundedList(self.budget,'Counterexample')
                            for target in nodes[1:]:tasks.append({'target':target,'edge':None})
                            for label in sorted(set(weak+strong)&set(occurrence)):
                                e=self.index.get('MonitorEdge',occurrence[label])
                                tasks.append({'target':e['from_monitor'],'edge':e['id']})
                            tasks.append({'target':nodes[0],'edge':None})
                            state.update(phase='walk',start=nodes[0],current=nodes[0],tasks=tasks,task=0,cycle=[])
                    save()
            elif state['phase']=='walk':
                if state['task']==len(state['tasks']):
                    with self.step('fair_search'):
                        state['phase']='prefix'
                        save()
                    continue
                task=state['tasks'][state['task']]
                path=self._path(state['current'],task['target'],restricted=True)
                with self.step('fair_search'):
                    cycle=BoundedList(self.budget,'Counterexample')
                    for edge in state['cycle']:cycle.append(edge)
                    for edge in path:cycle.append(edge)
                    state['current']=task['target']
                    if task['edge'] is not None:
                        e=self.index.get('MonitorEdge',task['edge'])
                        require(e['from_monitor']==state['current'],'fair occurrence path endpoint differs')
                        cycle.append(e['id'])
                        state['current']=e['to_monitor']
                    state['cycle']=cycle
                    state['task']+=1
                    save()
            elif state['phase']=='prefix':
                root=self.db.execute('SELECT node FROM tm_roots WHERE context=?',(component['context'],)).fetchone()[0]
                prefix=self._path(root,state['start'])
                with self.step('fair_search'):
                    state['result']=[prefix,state['cycle'],None if state['cycle'] else self.node(state['start'])['product']]
                    state['phase']='complete'
                    save()
            else:
                require(False,'unknown persisted fair-search phase')
            self.budget.check()
        return None if state['result'] is None else tuple(state['result'])
