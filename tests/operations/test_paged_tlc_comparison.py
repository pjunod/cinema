"""Defined, unrun monolithic/composed TLC comparisons on complete micrographs.

A separately pinned local runtime descriptor is required when final review
later authorizes these fixtures. They are proof-plumbing tests, not selected D.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import unittest

import test_paged_temporal as fixture
import checker
import paged_checker
import paged_codec as codec
import paged_scc as scc


class PagedTlcComparisonFixtures(unittest.TestCase):
    setUp=fixture.PagedTemporalFixtures.setUp
    tearDown=fixture.PagedTemporalFixtures.tearDown
    graph=fixture.PagedTemporalFixtures.graph

    def runtime(self):
        descriptor=os.environ.get('PLURX_PAGED_REVIEW_TLC_RUNTIME')
        if descriptor is None:
            self.skipTest('requires separately pinned TLC runtime after final independent review')
        path=Path(descriptor)
        self.assertTrue(path.is_absolute() and path.is_file() and not path.is_symlink())
        self.assertLessEqual(path.stat().st_size,8192)
        value=json.loads(path.read_text())
        self.assertEqual(set(value),{'java','jar','java_sha256','jar_sha256'})
        for name in ('java','jar'):
            binary=Path(value[name])
            self.assertTrue(binary.is_absolute() and binary.is_file() and not binary.is_symlink())
            self.assertEqual(paged_checker.file_sha(binary),value[name+'_sha256'])
        return value

    def invoke(self,directory,module,config):
        runtime=self.tlc
        log=directory/(module+'.log')
        with log.open('wb') as output:
            result=subprocess.run([runtime['java'],'-Xmx4g','-cp',runtime['jar'],'tlc2.TLC',
                '-workers','1','-metadir',str(directory/'states'),'-config',str(config),str(directory/(module+'.tla'))],
                cwd=directory,stdout=output,stderr=subprocess.STDOUT,timeout=1800,check=False)
        return paged_checker.parse_tlc_log(log,result.returncode,temporal_property='FixtureProperty' if module=='Direct' else 'EventuallyDischarged')

    def monolithic(self,graph,prop):
        directory=Path(self.directory.name)/('direct-'+str(len(self.graphs)))
        directory.mkdir()
        products={}
        for n in graph.nodes:
            products[n]=json.loads(graph.db.execute('SELECT payload FROM v_products WHERE id=?',(n,)).fetchone()[0])
        pending=lambda n:[o for o in scc.OBLIGATIONS if products[n]['w'][o['node']]['stage']=='driver'
            and products[n]['w'][o['node']]['job']==o['job'] and products[n]['w'][o['node']]['attempt']==o['claim_id']
            and products[n]['w'][o['node']]['started'] and not products[n]['w'][o['node']]['dropped']]
        weak=['ClaimSend/0','ClaimReply/0']+(['Admission/0'] if graph.context=='weak_admission' else [])
        strong=['Admission/0'] if graph.context=='stable' else []
        def aset(values):return '{'+','.join(checker.tla(v) for v in values)+'}'
        def function(rows):return '('+' @@ '.join('('+checker.tla(k)+' :> '+v+')' for k,v in rows)+')'
        edges=[{'src':a,'dst':b,'labels':labels} for a,b,labels in graph.edges]
        edge_text='{'+','.join('[src |-> '+str(e['src'])+',dst |-> '+str(e['dst'])+',labels |-> '+aset(e['labels'])+']' for e in edges)+'}'
        capacity={n:bool(graph.db.execute('SELECT capacity FROM v_products WHERE id=?',(n,)).fetchone()[0]) for n in graph.nodes}
        goal={n:n in graph.goals for n in graph.nodes}
        trigger={};drop={}
        for n in graph.nodes:
            trigger[n]=[];drop[n]=[]
            for o in scc.OBLIGATIONS:
                w=products[n]['w'][o['node']]
                if w['stage']=='driver' and w['job']==o['job'] and w['attempt']==o['claim_id']:
                    if w['started']:trigger[n].append(o)
                    if w['dropped']:drop[n].append(o)
        source='''------------------------------ MODULE Direct ------------------------------
EXTENDS Naturals, Sequences, TLC
VARIABLE node,seen,cap,pending
vars == <<node,seen,cap,pending>>
'''
        source+='Edges == '+edge_text+'\n'
        source+='Goals == '+function((n,checker.tla(goal[n])) for n in graph.nodes)+'\n'
        source+='Caps == '+function((n,checker.tla(capacity[n])) for n in graph.nodes)+'\n'
        source+='Enabled == '+function((n,aset(graph.enabled.get(n,[]))) for n in graph.nodes)+'\n'
        source+='Triggers == '+function((n,aset(trigger[n])) for n in graph.nodes)+'\n'
        source+='Drops == '+function((n,aset(drop[n])) for n in graph.nodes)+'\n'
        source+='Obligations == '+aset(scc.OBLIGATIONS)+'\n'
        source+='WeakLabels == '+aset(weak)+'\nStrongLabels == '+aset(strong)+'\n'
        source+='Init == /\\ node='+str(graph.nodes[0])+' /\\ seen=Goals[node] /\\ cap=Caps[node] /\\ pending='+aset(pending(graph.nodes[0]))+'\n'
        source+='''Next == \\E e \\in Edges : /\\ node=e.src /\\ node'=e.dst
 /\\ seen'=(seen \\/ Goals[e.dst]) /\\ cap'=(cap /\\ Caps[e.dst])
 /\\ pending'=((pending \\cup Triggers[e.dst]) \\ Drops[e.dst])
En(a) == a \\in Enabled[node]
Occ(a) == \\E e \\in Edges : node=e.src /\\ node'=e.dst /\\ e.src#e.dst /\\ a \\in e.labels
Fair == /\\ \\A a \\in WeakLabels : ([]<>~En(a)) \\/ ([]<>Occ(a))
 /\\ \\A a \\in StrongLabels : (<>[]~En(a)) \\/ ([]<>Occ(a))
Spec == Init /\\ [][Next]_vars /\\ Fair
'''
        source+=('FixtureProperty == <>seen\n' if prop=='seen_goal_false' else
                 'FixtureProperty == ([]cap) => (\\A o \\in Obligations : []<>(o \\notin pending))\n')
        source+='=============================================================================\n'
        (directory/'Direct.tla').write_text(source)
        config=directory/'direct.cfg'
        config.write_text('SPECIFICATION Spec\nPROPERTY FixtureProperty\nCHECK_DEADLOCK FALSE\n')
        result=self.invoke(directory,'Direct',config)
        self.assertIn(result['first_failure'],(None,'FixtureProperty'))
        return result['first_failure'] is not None

    def compare_tlc(self,graph,prop='seen_goal_false'):
        graph.build()
        direct=self.monolithic(graph,prop)
        bad=False
        for component in graph.components(prop) if prop=='seen_goal_false' else [c for c in graph.index.find('Scc') if c['property']==prop]:
            directory=Path(self.directory.name)/('component-'+str(len(self.graphs))+'-'+str(component['id']))
            directory.mkdir()
            chunks,mapping=paged_checker.component_tables(graph.index,graph.temporal,component)
            paged_checker.bounded_source(directory/'ComponentTables.tla',chunks,graph.index.budget)
            (directory/'id-map.json').write_bytes(codec.encoded(mapping))
            for name in ('PagedComponent.tla','PropertyMonitors.tla','paged-component.cfg'):
                shutil.copyfile(fixture.PACKAGE/name,directory/name)
            result=self.invoke(directory,'PagedComponent',directory/'paged-component.cfg')
            self.assertIn(result['first_failure'],(None,'EventuallyDischarged'))
            observed=result['first_failure'] is not None
            self.assertEqual(observed,graph.temporal.fair_walk(component) is not None)
            bad |= observed
        self.assertEqual(direct,bad)
        return bad

    def test_complete_micrographs_monolithic_and_composed_tlc_agree(self):
        # Resolve the runtime for the whole test, before any subTest: the receipt harness accepts a skip only
        # from the test itself, and a subtest skip would be an unknown fixture outcome.
        self.tlc=self.runtime()
        # Every advertised enabled label has an actual nonstuttering successor.
        cases=[
            dict(nodes=[1],edges=[]),
            dict(nodes=[1,2],edges=[(1,2,['ClaimSend/0'])],enabled={1:['ClaimSend/0']},goals={2}),
            dict(nodes=[1,2],edges=[(1,1,['ClaimSend/0']),(1,2,['ClaimSend/0'])],enabled={1:['ClaimSend/0']},goals={2}),
            dict(nodes=[1,2,3],edges=[(1,2,['ClaimSend/0']),(2,1,['ClaimSend/0']),(2,3,['Admission/0'])],enabled={1:['ClaimSend/0'],2:['ClaimSend/0','Admission/0']},context='weak_admission',goals={3}),
            dict(nodes=[1,2,3],edges=[(1,2,['ClaimSend/0']),(2,1,['ClaimSend/0']),(2,3,['Admission/0'])],enabled={1:['ClaimSend/0'],2:['ClaimSend/0','Admission/0']},context='stable',goals={3}),
            dict(nodes=[1,2,3],edges=[(1,1,[]),(1,2,[]),(2,1,[]),(2,3,['Admission/0'])],enabled={2:['Admission/0']},context='stable',goals={3}),
            dict(nodes=[1,2],edges=[(1,2,['ClaimSend/0']),(1,2,['ClaimReply/0']),(2,1,[])],enabled={1:['ClaimSend/0','ClaimReply/0']}),
            dict(nodes=[1,2,3],edges=[(1,2,['ClaimSend/0']),(2,3,['ClaimSend/0']),(3,3,[])],enabled={1:['ClaimSend/0'],2:['ClaimSend/0']},goals={2}),
        ]
        for fields in cases:
            with self.subTest(fields=fields):self.compare_tlc(self.graph(**fields))
        trigger=dict(fixture.idle(),stage='driver',job=0,attempt=scc.CLAIMS[0],started=True)
        response=dict(trigger,started=False,dropped=True)
        for capacities,workers in (({}, {2:[trigger,fixture.idle()]}),
                                   ({1:False},{2:[trigger,fixture.idle()]}),
                                   ({},{2:[trigger,fixture.idle()],3:[response,fixture.idle()]})):
            with self.subTest(capacities=capacities,workers=workers):
                graph=self.graph(nodes=[1,2,3],edges=[(1,2,['ClaimSend/0']),(2,3,['ClaimSend/0'])],
                    enabled={1:['ClaimSend/0'],2:['ClaimSend/0']},capacities=capacities,workers=workers)
                self.compare_tlc(graph,'capacity_ok_pending_true')


if __name__=='__main__':
    unittest.main()
