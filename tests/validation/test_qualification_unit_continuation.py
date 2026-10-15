"""Refusal controls for complete command-scoped unit evidence."""
import base64
import copy
import hashlib
import unittest
import shlex
import sys
from unittest.mock import patch
from validation import qualification_unit_continuation as q
from validation import python_unit_receipts as receipts

CANDIDATE = 'b' * 40
COMMAND = 'RUST_MIN_STACK=8388608 $(CARGO) test --locked -p plurx-core --features cluster-read-cost-validation,hiqlite-contract-tests --test store_contract -- --test-threads=1'


def log(text):
    raw = text.encode()
    return {'base64': base64.b64encode(raw).decode(), 'sha256': hashlib.sha256(raw).hexdigest()}


def fixture():
    features = ['cluster-read-cost-validation', 'hiqlite-contract-tests', 'hiqlite-store']
    environment = {'platform': 'linux-x64', 'toolchain': 'rust1.97.1', 'features': features,
                   'runtime': {'test_threads': 1, 'rust_min_stack':8388608,'target':'x86_64-unknown-linux-gnu',
                              'rustc_version':q.RUSTC_VERSION,'profile':{'opt_level':'0','debuginfo':0,'debug_assertions':True,'overflow_checks':True,'test':True}}}
    records=[]; logs={}
    for sequence,name in enumerate(['a','b'],1):
        argv=['/owned/store_contract-deadbeef',name,'--exact','--test-threads=1']
        text='COMMAND '+__import__('json').dumps(argv)+'\nrunning 1 test\ntest '+name+' ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored;\nNATIVE_EXIT 0\n'
        raw=log(text);logs[raw['sha256']]=raw
        records.append({'sequence':sequence,'source_commit':CANDIDATE,'environment':copy.deepcopy(environment),
            'origin':{'kind':'attested-local','identity':'actual owned effort executor','command':shlex.join(argv),
                'execution':{'mode':'local-binary','first_line':1,'build_invocation':COMMAND.replace('$(CARGO)','cargo')+' --no-run',
                    'archive_sha256':'d'*64,'binary_sha256_before':'c'*64,'binary_sha256_after':'c'*64,'features':features,
                    'fingerprint':{'features':__import__('json').dumps(features),'profile':16018505129029273480,'rustc':q.RUSTC_FINGERPRINT,'compile_kind':0,'rustflags':[]},
                    'binary_path':argv[0],'fingerprint_path':'/owned/.fingerprint/plurx-core-deadbeef/test-integration-test-store_contract.json'}},
            'changes':{},'log':{'sha256_ref':raw['sha256']},'exit_code':0,
            'coverage':{'mode':'rust','first_line':2,'last_line':5,'format':'rust-serial','outcomes':{name:'pass'}}})
    item = {'name': 'store_contract:1', 'invocation': COMMAND, 'environment': environment,
            'inventory': {'source_commit': CANDIDATE, 'ids': ['a', 'b'], 'list_log': log('a: test\nb: test\n'),
                          'binary_sha256': 'c' * 64, 'features': features}, 'records': records, 'obligations': []}
    return {'version': 1, 'source_commit': CANDIDATE, 'commands': [item], 'logs': logs}


class CompleteCommandUnitCase(unittest.TestCase):
    def validate(self, proof, commands=None, invalid=()):
        with patch.object(q, 'changed_inputs', return_value=set(invalid)),patch.object(q,'source_archive_digest',return_value='d'*64),patch.object(q,'test_profile',return_value=fixture()['commands'][0]['environment']['runtime']['profile']):
            return q.validate(proof, CANDIDATE, commands=lambda _: commands or {'store_contract:1': COMMAND})

    def test_complete_actual_union_is_pure_and_candidate_bound(self):
        proof = fixture();before = copy.deepcopy(proof)
        result = self.validate(proof)
        self.assertEqual(result['store_contract']['mode'], 'retained-complete-command-union')
        self.assertEqual(proof, before)
        proof['source_commit'] = 'd' * 40
        with self.assertRaises(receipts.ReceiptError): self.validate(proof)

    def test_local_cargo_json_preserves_actual_argv_exit_and_running_artifact(self):
        import json
        command='scripts/require-test-count $(CARGO) test --locked -p hiqlite a --lib -- --exact'
        argv=shlex.split(command.replace('$(CARGO)','cargo'))
        raw='COMMAND '+json.dumps(argv)+'\nRunning unittests src/lib.rs (/owned/hiqlite-deadbeef)\nrunning 1 test\ntest a ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;\nNATIVE_EXIT 0\n'
        record=fixture()['commands'][0]['records'][0]
        record['environment']['features']=[];record['environment']['runtime']['test_threads']='libtest-default'
        record['origin']={'kind':'attested-local','identity':'actual cargo driver','command':shlex.join(argv),
            'execution':{'mode':'local-cargo-json','first_line':1,'historical_name':'transport:2',
                'archive_sha256':'d'*64,'binary_sha256_before':'c'*64,'binary_sha256_after':'c'*64,
                'cargo_artifact':{'reason':'compiler-artifact','target':{'name':'hiqlite'},'features':[],
                    'profile':record['environment']['runtime']['profile'],'executable':'/owned/hiqlite-deadbeef'},
                'cwd':'/owned','fingerprint_path':'/owned/.fingerprint/hiqlite-deadbeef/test-lib-hiqlite.json',
                'fingerprint_observation':'preserved-matching-binary',
                'fingerprint':{'rustc':q.RUSTC_FINGERPRINT,'profile':q.TEST_PROFILE_FINGERPRINTS[0],
                               'compile_kind':0,'rustflags':[],'features':'[]'}}}
        item={'name':'transport:2','invocation':command}
        with patch.object(q,'source_archive_digest',return_value='d'*64):
            self.assertIn('test a ... ok',q.bound_execution(record,item,raw,lambda _: {'transport:2':command}))
            for altered in [raw.replace('NATIVE_EXIT 0','NATIVE_EXIT 101'),
                            raw.replace('Running unittests src/lib.rs (/owned/hiqlite-deadbeef)','Running unittests src/lib.rs (/owned/other-deadbeef)'),
                            raw.replace('NATIVE_EXIT 0','COMMAND []\nNATIVE_EXIT 0')]:
                with self.assertRaises(receipts.ReceiptError):q.bound_execution(record,item,altered,lambda _: {'transport:2':command})
            for key,value in [('rustc',0),('rustflags',['--cfg','hidden']),('compile_kind',1)]:
                altered=copy.deepcopy(record);altered['origin']['execution']['fingerprint'][key]=value
                with self.assertRaises(receipts.ReceiptError):q.bound_execution(altered,item,raw,lambda _: {'transport:2':command})
            altered=copy.deepcopy(record);altered['origin']['execution']['cwd']='/unrelated'
            relative=raw.replace('(/owned/hiqlite-deadbeef)','(hiqlite-deadbeef)')
            with self.assertRaises(receipts.ReceiptError):q.bound_execution(altered,item,relative,lambda _: {'transport:2':command})

    def test_authenticated_executor_interruption_keeps_unknown_native_exit(self):
        record={'exit_code':None,'origin':{'kind':'authenticated-ci','execution':
            {'job_status':'failure','disposition':'executor-interrupted'}}}
        body='test actual ... ok\n⚙️ [runner]: Failed to execute step (but continue-on-error is true): this step has been cancelled: ctx: context deadline exceeded, exec: RUN signal: killed'
        q.require_executor_interrupted(record,body)
        for altered in [body.replace('signal: killed','exit status 1'),body+'\ntest result: ok. 1 passed; 0 failed; 0 ignored;']:
            with self.assertRaises(receipts.ReceiptError):q.require_executor_interrupted(record,altered)
        for key,value in [('exit_code',101)]:
            copy_record=copy.deepcopy(record);copy_record[key]=value
            with self.assertRaises(receipts.ReceiptError):q.require_executor_interrupted(copy_record,body)
        record['origin']['kind']='attested-local'
        with self.assertRaises(receipts.ReceiptError):q.require_executor_interrupted(record,body)

    def test_wal_reference_requires_complete_validated_store_and_exact_isolated_contract(self):
        name='login_token_insert_requires_the_verified_password_version'
        proof=fixture();store=proof['commands'][0];store['records']=store['records'][:1]
        store['inventory']['ids']=[name];store['inventory']['list_log']=log(name+': test\n')
        wal=copy.deepcopy(store);wal.update(name='wal:215',records=[],obligations=[],
            invocation=COMMAND.replace(' -- --test-threads=1',' '+name+' -- --exact --test-threads=1'),
            references=[{'command':'store_contract:1','test_id':name,'policy':'isolated-store-auth-8mib-serial-v1'}])
        proof['commands'].append(wal)
        commands={'store_contract:1':COMMAND,'wal:215':wal['invocation']}
        with patch.object(q,'changed_inputs',return_value=set()),patch.object(q,'bound_execution',return_value=''),patch.object(q,'coverage',return_value={name:'pass'}),patch.object(q,'test_profile',return_value=store['environment']['runtime']['profile']):
            self.assertIn('wal',q.validate(proof,CANDIDATE,commands=lambda _:commands))
            wal['references'][0]['policy']='arbitrary compatibility'
            with self.assertRaises(receipts.ReceiptError):q.validate(proof,CANDIDATE,commands=lambda _:commands)
            wal['references'][0]['policy']='isolated-store-auth-8mib-serial-v1'
            proof['commands'].reverse()
            with self.assertRaises(receipts.ReceiptError):q.validate(proof,CANDIDATE,commands=lambda _:commands)

    def test_authenticated_identity_numbers_are_typed_and_bounded(self):
        self.assertEqual(q.authenticated_ids('run:4954/job:49310'),(4954,49310))
        for identity in ['run:0/job:1','run:01/job:2','run:1/job:0','run:'+'9'*20+'/job:1',
                         'run:1/job:2/extra','run:one/job:two',1,None]:
            with self.assertRaises(receipts.ReceiptError):q.authenticated_ids(identity)

    def test_executor_step_is_actual_make_invocation_not_receipt_argument(self):
        workflow='jobs:\n  cluster_store_legacy:\n    steps:\n      - name: Run Store\n        run: |\n          make cluster-store-check 2>&1 | tee log\n      - name: Record Store\n        run: |\n          python3 -m validation.ci_lane_receipt --command "make cluster-store-check"\n'
        record={'source_commit':CANDIDATE,'origin':{'kind':'authenticated-ci'}}
        lines=['make cluster-store-check','test a ... ok','  ✅ Success - Main Run Store']
        with patch.object(q.retention,'git_bytes',return_value=workflow.encode()):
            self.assertEqual(q.executor_terminal(record,'store_contract',lines,0),(2,True))
        only_receipt=workflow.split('      - name: Run Store',1)[0]+'      - name: Record Store'+workflow.split('      - name: Record Store',1)[1]
        with patch.object(q.retention,'git_bytes',return_value=only_receipt.encode()):
            with self.assertRaises(receipts.ReceiptError):q.executor_terminal(record,'store_contract',lines,0)
        doubled=workflow+'      - name: Second Store\n        run: make cluster-store-check\n'
        with patch.object(q.retention,'git_bytes',return_value=doubled.encode()):
            with self.assertRaises(receipts.ReceiptError):q.executor_terminal(record,'store_contract',lines,0)

    def test_historical_transport_layout_cannot_weaken_current_command_inventory(self):
        source='59440187d0ad16ceec7632f2461493112f3dfc60'
        make='cluster-harness-check: ## old topology\ncluster-transport-recovery-contracts: ## old transport\n'
        def commands(_,target):
            if target in ['cluster-store-check','cluster-wal-check']:return [COMMAND]
            count=8 if target=='cluster-harness-check' else 13
            return ['$(CARGO) test --locked -p plurx-cluster-check case'+str(i)+' --lib -- --exact' for i in range(count)]
        with patch.object(q.retention,'git_bytes',return_value=make.encode()),patch.object(q,'make_commands',side_effect=commands):
            self.assertIn('transport:13',q.historical_canonical(source))
            with self.assertRaises(receipts.ReceiptError):q.canonical(source)
            with self.assertRaises(receipts.ReceiptError):q.historical_canonical(CANDIDATE)

    def test_shared_applicability_manifest_binds_exact_source_and_candidate(self):
        import json
        record=fixture()['commands'][0]['records'][0]
        changes={'input.rs':{'before':'d'*64,'after':'e'*64,'affected_ids':['a'],'reason':'reviewed changed function'}}
        manifest={'source_commit':record['source_commit'],'candidate':CANDIDATE,'changes':changes}
        blob=log(json.dumps(manifest));record['changes']={'sha256_ref':blob['sha256']}
        with patch.object(q.retention,'changed',return_value={'input.rs'}),patch.object(q.retention,'git_bytes',return_value=b'input'),patch.object(q.retention,'digest',side_effect=lambda raw: hashlib.sha256(raw).hexdigest() if raw.startswith(b'{') else 'd'*64):
            # Exact old/current content hashes are still verified after deduplication.
            with self.assertRaises(receipts.ReceiptError):q.changed_inputs(record,CANDIDATE,{blob['sha256']:blob})
            changes['input.rs']['after']='d'*64;blob=log(json.dumps(manifest));record['changes']={'sha256_ref':blob['sha256']}
            self.assertEqual(q.changed_inputs(record,CANDIDATE,{blob['sha256']:blob}),{'a'})
            alias=copy.deepcopy(manifest);alias['changes']['input.rs']['affected_ids']=[];aliased=log(json.dumps(alias))
            with self.assertRaises(receipts.ReceiptError):q.changed_inputs(record,CANDIDATE,{blob['sha256']:aliased})
            for key in ['source_commit','candidate']:
                altered=copy.deepcopy(manifest);altered[key]='a'*40;other=log(json.dumps(altered));record['changes']={'sha256_ref':other['sha256']}
                with self.assertRaises(receipts.ReceiptError):q.changed_inputs(record,CANDIDATE,{other['sha256']:other})
        with self.assertRaises(receipts.ReceiptError):q.changed_inputs(record,CANDIDATE,{})

    def test_original_capture_keeps_unknown_observations_and_refuses_substitution(self):
        import json
        argv=['/owned/store_contract-0123456789abcdef','a','--exact','--test-threads=1']
        raw='source_commit '+CANDIDATE+' branch effort/dv-hdr-repair command '+json.dumps(argv)+'\nrunning 1 test\ntest a ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 367 filtered out; finished in 1.00s\ndiagnostic_exit_code 0'
        digest=log(raw)['sha256'];record=fixture()['commands'][0]['records'][0]
        observed={key:None for key in ['archive_sha256','binary_sha256','binary_sha256_after','fingerprint','features','profile','rust_min_stack','test_threads']};observed['exit_code']=0
        record['log']={'sha256_ref':digest};record['coverage']['outcomes']={'a':'pass'}
        record['origin']['command']=shlex.join(argv)
        record['origin']['execution']={'mode':'local-original-capture','first_line':1,'ordinal':0,
            'binding':None,'observed':observed,'compatibility':'reviewed-original-linux-store-v1'}
        item={'name':'store_contract:1','invocation':COMMAND}
        with self.assertRaises(receipts.ReceiptError):q.bound_execution(record,item,raw)
        with patch.dict(q.ORIGINAL_CAPTURES,{digest:{'source':CANDIDATE,'binding':None,'form':'header','blocks':1}},clear=True):
            self.assertIn('test a ... ok',q.bound_execution(record,item,raw))
            for key,value in [('rust_min_stack',8388608),('binary_sha256','c'*64),('features',record['environment']['features'])]:
                altered=copy.deepcopy(record);altered['origin']['execution']['observed'][key]=value
                with self.assertRaises(receipts.ReceiptError):q.bound_execution(altered,item,raw)
            for key,value in [('features',[]),('runtime',{**record['environment']['runtime'],'rust_min_stack':4194304})]:
                altered=copy.deepcopy(record);altered['environment'][key]=value
                with self.assertRaises(receipts.ReceiptError):q.bound_execution(altered,item,raw)
            altered=copy.deepcopy(record);altered['origin']['execution']['binding']=log('{}')
            with self.assertRaises(receipts.ReceiptError):q.bound_execution(altered,item,raw)
            altered=copy.deepcopy(record);altered['origin']['execution']['compatibility']='arbitrary compatibility'
            with self.assertRaises(receipts.ReceiptError):q.bound_execution(altered,item,raw)
            for text in [raw.replace('diagnostic_exit_code 0','diagnostic_exit_code 101'),raw.replace('367 filtered','366 filtered')]:
                with self.assertRaises(receipts.ReceiptError):q.bound_execution(record,item,text)

    def test_original_sidecar_target_exit_and_bulk_selection_are_checked(self):
        import json
        argv=['/owned/store_contract-0123456789abcdef','a','--exact','--test-threads=1']
        raw='COMMAND '+json.dumps(argv)+'\nrunning 1 test\ntest a ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 367 filtered out; finished in 1.00s\nNATIVE_EXIT 0\n'
        digest=log(raw)['sha256'];record=fixture()['commands'][0]['records'][0]
        record['log']={'sha256_ref':digest};record['coverage']['outcomes']={'a':'pass'};record['origin']['command']=shlex.join(argv)
        fp={'rustc':q.RUSTC_FINGERPRINT,'profile':q.TEST_PROFILE_FINGERPRINTS[0],'compile_kind':0,'rustflags':[],
            'features':json.dumps(record['environment']['features']),
            'local':[{'CheckDepInfo':{'dep_info':'debug/.fingerprint/plurx-core-0123456789abcdef/dep-test-integration-test-store_contract'}}]}
        side={'source_commit':CANDIDATE,'branch':'effort/dv-hdr-repair','records':[{'command':argv,'exit_code':0}],
              'fingerprint':fp,'binary_sha256':'c'*64,'binary_sha256_after':'c'*64,'rust_min_stack':8388608,'test_threads':1}
        item={'name':'store_contract:1','invocation':COMMAND}
        def inspect(data,text=raw,form='blocks'):
            binding=log(json.dumps(data));raw_digest=log(text)['sha256']
            altered=copy.deepcopy(record);altered['log']={'sha256_ref':raw_digest}
            observed={key:data.get(key) for key in ['archive_sha256','binary_sha256','binary_sha256_after','fingerprint','features','profile','rust_min_stack','test_threads']}
            observed['exit_code']=data['records'][0]['exit_code'] if form=='blocks' else data.get('exit_code')
            altered['origin']['execution']={'mode':'local-original-capture','first_line':1,'ordinal':0,'binding':binding,
                'observed':observed,'compatibility':'reviewed-original-linux-store-v1'}
            with patch.dict(q.ORIGINAL_CAPTURES,{raw_digest:{'source':CANDIDATE,'binding':binding['sha256'],'form':form,'blocks':1}},clear=True):
                return q.bound_execution(altered,item,text)
        self.assertIn('test a ... ok',inspect(side))
        for field,value in [('rustc',0),('profile',0),('features','[]'),('rustflags',['--cfg','hidden']),('compile_kind',1),
                            ('local',[{'CheckDepInfo':{'dep_info':'debug/.fingerprint/plurx-core-0123456789abcdef/dep-test-integration-test-other'}}])]:
            changed=copy.deepcopy(side);changed['fingerprint'][field]=value
            with self.assertRaises(receipts.ReceiptError):inspect(changed)
        changed=copy.deepcopy(side);changed['records'][0]['exit_code']=101
        with self.assertRaises(receipts.ReceiptError):inspect(changed)
        changed=copy.deepcopy(side);changed['records'][0]['command'][1]='other'
        with self.assertRaises(receipts.ReceiptError):inspect(changed)
        bulk='running 1 test\ntest a ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 1.00s\n'
        data={'source_commit':CANDIDATE,'branch':'effort/dv-hdr-repair','command':[argv[0],'--exact','--test-threads=1','--skip','b'],
              'inventory':['a','b'],'exit_code':0}
        # Bound argv remains the original observed command, including exact skips.
        original=record['origin']['command'];record['origin']['command']=shlex.join(data['command'])
        self.assertIn('test a ... ok',inspect(data,bulk,'bulk'))
        with self.assertRaises(receipts.ReceiptError):inspect(data,bulk.replace('1 filtered out','2 filtered out'),'bulk')
        changed=copy.deepcopy(data);changed['command']+=['--skip','b'];record['origin']['command']=shlex.join(changed['command'])
        with self.assertRaises(receipts.ReceiptError):inspect(changed,bulk,'bulk')
        record['origin']['command']=shlex.join(data['command'])
        changed=copy.deepcopy(data);changed['inventory'].append('c')
        with self.assertRaises(receipts.ReceiptError):inspect(changed,bulk,'bulk')
        changed=copy.deepcopy(data);changed['command'][-1]='a';record['origin']['command']=shlex.join(changed['command'])
        with self.assertRaises(receipts.ReceiptError):inspect(changed,bulk,'bulk')
        record['origin']['command']=original

    def test_original_unknown_exit_failure_needs_its_exact_obligation(self):
        record=fixture()['commands'][0]['records'][0];record['exit_code']=None
        record['origin']['execution']['mode']='local-original-capture'
        record['log']={'sha256_ref':'810a808e88213d60a1ff5647dda57cf36a93f46cdb26acab0629db5bf67a0f04'}
        record['coverage']['outcomes']={'a':'fail'}
        item={'invocation':COMMAND,'obligations':[{'sequence':record['sequence'],'category':'native-failure-exit-unrecorded'}]}
        body='test a ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored;'
        q.completion_obligations(item,[record],{'a':'pass'},{record['sequence']:body})
        for category in ['native-failure','executor-interrupted','arbitrary waiver']:
            altered=copy.deepcopy(item);altered['obligations'][0]['category']=category
            with self.assertRaises(receipts.ReceiptError):q.completion_obligations(altered,[record],{'a':'pass'},{record['sequence']:body})
        for value in [0,101]:
            altered=copy.deepcopy(record);altered['exit_code']=value
            with self.assertRaises(receipts.ReceiptError):q.completion_obligations(item,[altered],{'a':'pass'},{record['sequence']:body})
        altered=copy.deepcopy(record);altered['log']['sha256_ref']='c'*64
        with self.assertRaises(receipts.ReceiptError):q.completion_obligations(item,[altered],{'a':'pass'},{record['sequence']:body})
        altered=copy.deepcopy(item);altered['obligations']=[]
        with self.assertRaises(receipts.ReceiptError):q.completion_obligations(altered,[record],{'a':'pass'},{record['sequence']:body})

    def test_original_cohort_cannot_omit_failure_capture_or_reorder_ordinals(self):
        rows=[];ordered=list(q.ORIGINAL_CAPTURES);ordered[1],ordered[2]=ordered[2],ordered[1]
        lineage={'39ed30d207e8b03c52b5401103f880a54960a552c4167ce9fcbbed57a68fb919',
                 'e402da6e8ed9c8bfe50fcfc549b09be033fca651178bd1cef6812f6c00cb3b44'}
        rows.append({'sequence':1,'log':{'sha256_ref':'f4de403db2e806a1894a3434dbd8edb5dd886ccafeeca37c5f0e49a127dffdf2'},
                     'origin':{'kind':'authenticated-ci','execution':{'mode':'cargo-command'}}})
        for digest in ordered:
            if digest in lineage:continue
            for ordinal in range(q.ORIGINAL_CAPTURES[digest]['blocks']):
                rows.append({'sequence':len(rows)+1,'log':{'sha256_ref':digest},
                    'origin':{'execution':{'mode':'local-original-capture','ordinal':ordinal}}})
        lineage_rows=[{'sequence':index+1,'log':{'sha256_ref':digest},
            'origin':{'execution':{'mode':'local-original-capture','ordinal':0}}}
            for index,digest in enumerate([digest for digest in ordered if digest in lineage])]
        q.complete_original_capture_ordinals(lineage_rows)
        with self.assertRaises(receipts.ReceiptError):q.complete_original_capture_ordinals(rows+lineage_rows)
        q.complete_original_capture_ordinals(rows)
        for altered in [rows[:2]+rows[3:],rows[:1]+rows[2:],rows[1:]]:
            with self.assertRaises(receipts.ReceiptError):q.complete_original_capture_ordinals(altered)
        altered=copy.deepcopy(rows);altered[1]['sequence'],altered[2]['sequence']=altered[2]['sequence'],altered[1]['sequence']
        with self.assertRaises(receipts.ReceiptError):q.complete_original_capture_ordinals(altered)
        altered=copy.deepcopy(rows);altered.append(copy.deepcopy(rows[-1]))
        with self.assertRaises(receipts.ReceiptError):q.complete_original_capture_ordinals(altered)

    def test_historical_global_and_job_compiler_overrides_refuse(self):
        workflow='env:\n  CARGO_TERM_COLOR: always\njobs:\n  cluster_wal:\n    runs-on: [self-hosted, Linux, X64, lab]\n    steps:\n      - uses: https://github.com/dtolnay/rust-toolchain@4716b85f2fac3e324e64fa2810f6b5c3905760a5\n'
        blobs={'.github/actions/cargo-cache/action.yml':b'cache','rust-toolchain.toml':b'toolchain'}
        hashes={b'cache':'b22fa9dcb68f0cb063ae9de4f80a420631d19f61af60b32e1c6b47e2d9aadb43',b'toolchain':'03cbfb27fcdc8253524b3e4c71c832457a504f7e9b1da5e6220178404b77f4e4'}
        with patch.object(q.retention,'git_bytes',side_effect=lambda _,path:blobs.get(path)),patch.object(q.retention,'digest',side_effect=lambda raw:hashes[raw]):
            q.historical_rust_environment_contract(CANDIDATE,workflow,'wal')
            for altered in [workflow.replace('  CARGO_TERM_COLOR: always','  CARGO_TERM_COLOR: always\n  RUSTFLAGS: --cfg hidden'),
                            workflow.replace('    steps:','    env:\n      RUSTC: /unbound/compiler\n    steps:'),
                            workflow.replace('    steps:','    env:\n      CARGO_BUILD_RUSTC: /unbound/compiler\n    steps:'),
                            workflow+'      - run: export RUSTC=/unbound/compiler\n',
                            workflow.replace('    steps:', '    env: {RUSTC: /unbound/compiler}\n    steps:')]:
                with self.assertRaises(receipts.ReceiptError):q.historical_rust_environment_contract(CANDIDATE,altered,'wal')

    def test_pending_non_rust_enrollment_is_refused_before_any_adoption(self):
        proof=fixture();item=proof['commands'][0]
        for kind in ['validation','operations','android_jvm']:
            item['name']=kind;item['invocation']=q.COMMANDS[kind]
            with self.assertRaisesRegex(receipts.ReceiptError,'pending non-Rust'):
                self.validate(proof,{kind:q.COMMANDS[kind]})

    def test_no_explicit_features_accepts_actual_empty_feature_graph(self):
        proof=fixture();item=proof['commands'][0]
        command=COMMAND.replace(' --features cluster-read-cost-validation,hiqlite-contract-tests','')
        item['invocation']=command;item['environment']['features']=[];item['inventory']['features']=[]
        for record in item['records']:
            record['environment']['features']=[]
            witness=record['origin']['execution'];witness['features']=[]
            witness['fingerprint']['features']='[]'
            witness['build_invocation']=command.replace('$(CARGO)','cargo')+' --no-run'
        self.assertEqual(self.validate(proof,{'store_contract:1':command})['store_contract']['mode'],
                         'retained-complete-command-union')

    def test_missing_command_and_wrong_invocation_refuse(self):
        for mutate in [lambda p: p['commands'][0].update(invocation=COMMAND+' --ignored'),
                       lambda p: p['commands'][0].update(name='wal:99')]:
            proof=fixture();mutate(proof)
            with self.assertRaises(receipts.ReceiptError):self.validate(proof)
        with self.assertRaises(receipts.ReceiptError):
            self.validate(fixture(), {'store_contract:1': COMMAND, 'store_contract:2': COMMAND+' --lib'})

    def test_native_inventory_filter_and_binary_identity_are_required(self):
        for mutate in [lambda i:i.update(ids=['a']),lambda i:i.update(binary_sha256='unknown'),
                       lambda i:i.update(source_commit='d'*40),lambda i:i.update(list_log=log('a: test\na: test\n'))]:
            proof=fixture();mutate(proof['commands'][0]['inventory'])
            with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_runtime_stack_thread_and_feature_mismatch_refuse(self):
        for key,value in [('rust_min_stack',None),('test_threads',8)]:
            proof=fixture();proof['commands'][0]['environment']['runtime'][key]=value
            with self.assertRaises(receipts.ReceiptError):self.validate(proof)
        proof=fixture();proof['commands'][0]['records'][0]['environment']['features']=[]
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)

        import json
        proof=fixture();default=proof['commands'][0]
        default['invocation']=COMMAND.removesuffix(' -- --test-threads=1')
        default['environment']['runtime']['test_threads']='libtest-default'
        proof['logs']={}
        for row in default['records']:
            row['environment']['runtime']['test_threads']='libtest-default'
            row['origin']['execution']['harness_environment']={'RUST_TEST_THREADS':None}
            words=shlex.split(row['origin']['command']);words.remove('--test-threads=1')
            row['origin']['command']=shlex.join(words)
            name=next(iter(row['coverage']['outcomes']))
            raw=log('COMMAND '+json.dumps(words)+'\nrunning 1 test\ntest '+name+' ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored;\nNATIVE_EXIT 0\n')
            row['log']={'sha256_ref':raw['sha256']};proof['logs'][raw['sha256']]=raw
        self.assertIn('store_contract',self.validate(proof,commands={'store_contract:1':default['invocation']}))

        item=fixture()['commands'][0]
        item['invocation']=COMMAND.removesuffix(' -- --test-threads=1')
        record=item['records'][0]
        record['environment']['runtime']['test_threads']='libtest-default'
        record['origin']['execution']['harness_environment']={'RUST_TEST_THREADS':None}
        argv=['/owned/store_contract-deadbeef','a','--exact','--nocapture']
        def inspect(changed=record, words=argv, terminal='test result: ok. 1 passed; 0 failed; 0 ignored;'):
            changed=copy.deepcopy(changed);changed['origin']['command']=shlex.join(words)
            raw='COMMAND '+json.dumps(words)+'\nrunning 1 test\ntest a ... ok\n'+terminal+'\nNATIVE_EXIT 0\n'
            with patch.object(q,'source_archive_digest',return_value='d'*64):
                return q.bound_execution(changed,item,raw)
        self.assertIn('test a ... ok',inspect())
        for words in [argv+['--test-threads=1'],argv+['--exact'],argv+['--skip','b'],
                      argv+['b'],argv+['--list'],argv+['--ignored'],argv+['--bench'],
                      argv+['--unknown'],argv[0:2]+['--nocapture']]:
            with self.subTest(words=words),self.assertRaises(receipts.ReceiptError):inspect(words=words)
        for terminal in ['', 'test result: ok. 0 passed; 0 failed; 0 ignored;',
                         'test result: ok. 1 passed; 0 failed; 0 ignored;\ntest result: ok. 0 passed; 0 failed; 0 ignored;']:
            with self.subTest(terminal=terminal),self.assertRaises(receipts.ReceiptError):inspect(terminal=terminal)
        missing=copy.deepcopy(record);del missing['origin']['execution']['harness_environment']
        overridden=copy.deepcopy(record);overridden['origin']['execution']['harness_environment']['RUST_TEST_THREADS']='1'
        serial=copy.deepcopy(record);serial['environment']['runtime']['test_threads']=1
        for altered in [missing,overridden,serial]:
            with self.assertRaises(receipts.ReceiptError):inspect(changed=altered)

    def test_tampered_shared_log_and_unknown_outcome_refuse(self):
        proof=fixture();next(iter(proof['logs'].values()))['sha256']='0'*64
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)
        proof=fixture();proof['commands'][0]['records'][0]['coverage']['outcomes']['a']='fail'
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_changed_input_cannot_reuse_an_invalidated_pass(self):
        with self.assertRaises(receipts.ReceiptError):self.validate(fixture(), invalid={'a'})

    def test_ambiguous_chronology_and_duplicate_commands_refuse(self):
        proof=fixture();proof['commands'][0]['records'].append(copy.deepcopy(proof['commands'][0]['records'][0]))
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)
        proof=fixture();proof['commands'].append(copy.deepcopy(proof['commands'][0]))
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_original_unexplained_command_failure_cannot_be_cleared_by_reason(self):
        proof=fixture();proof['commands'][0]['records'][0]['exit_code']=101
        proof['commands'][0]['obligations']=[{'sequence':1,'source_commit':CANDIDATE,'invocation':COMMAND,
            'native_pass_count':2,'reason':'freeform repaired wrapper'}]
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_full_python_summary_uses_actual_frozen_discovery(self):
        record={'source_commit':CANDIDATE,'coverage':{'mode':'python-discovery','first_line':1,'last_line':3,'ids':['A.a','A.b']}}
        self.assertEqual(q.coverage(record,'Ran 2 tests in 0.2s\n\nOK','validation',lambda c,k:['A.a','A.b']),{'A.a':'pass','A.b':'pass'})
        with self.assertRaises(receipts.ReceiptError):q.coverage(record,'Ran 2 tests in 0.2s\n\nOK','validation',lambda c,k:['A.a'])
        with self.assertRaises(receipts.ReceiptError):q.coverage(record,'Ran 2 tests in 0.2s\n\nOK\nRan 2 tests in 0.3s\n\nOK','validation',lambda c,k:['A.a','A.b'])

    def test_jvm_whole_success_stays_opaque_and_requires_real_tasks(self):
        record={'coverage':{'mode':'jvm-whole-command','first_line':1,'last_line':3,'tasks':['testDebugUnitTest','lintDebug']}}
        raw='> Task :app:testDebugUnitTest\n> Task :app:lintDebug\nBUILD SUCCESSFUL in 1s'
        self.assertEqual(q.coverage(record,raw,'android_jvm'),{'whole-command':'pass'})
        with self.assertRaises(receipts.ReceiptError):q.coverage(record,raw.replace('testDebugUnitTest','testDebugUnitTest UP-TO-DATE'),'android_jvm')

    def test_wal_identity_retains_environment_target_features_and_ignored_policy(self):
        identity=q.rust_identity('RUST_MIN_STACK=8388608 '+COMMAND.split(' ',1)[1].replace('--test-threads=1','--exact --ignored --test-threads=1'))
        self.assertEqual(identity['cargo']['--test'],'store_contract')
        self.assertIn('--ignored',identity['harness'])
        with self.assertRaises(receipts.ReceiptError):q.rust_identity('$(CARGO) test --locked --lib --arbitrary')

    def test_every_changed_path_includes_the_adapter_and_workflow(self):
        record={'source_commit':'a'*40,'changes':{'.github/workflows/ci.yml':{'before':q.retention.digest(b'old'),'after':q.retention.digest(b'new'),'affected_ids':['a'],'reason':'changes current executor'}}}
        with patch.object(q.retention,'changed',return_value={'.github/workflows/ci.yml'}),patch.object(q.retention,'git_bytes',side_effect=lambda c,p:b'old' if c=='a'*40 else b'new'):
            self.assertEqual(q.changed_inputs(record,CANDIDATE),{'a'})
            record['changes']={}
            with self.assertRaises(receipts.ReceiptError):q.changed_inputs(record,CANDIDATE)

    def test_later_invalidated_failure_requires_a_later_applicable_pass(self):
        proof=fixture();item=proof['commands'][0]
        failed=copy.deepcopy(item['records'][0]);failed['sequence']=3;failed['source_commit']='a'*40
        item['records'].append(failed)
        with patch.object(q,'source_archive_digest',return_value='d'*64),patch.object(q,'test_profile',return_value=item['environment']['runtime']['profile']),patch.object(q,'bound_execution',return_value=''),patch.object(q,'changed_inputs',side_effect=[set(),set(),{'a'}]),patch.object(q,'coverage',side_effect=[{'a':'pass'},{'b':'pass'},{'a':'fail'}]):
            with self.assertRaises(receipts.ReceiptError):q.validate(proof,CANDIDATE,commands=lambda _: {'store_contract:1':COMMAND})

    def test_retained_store_refuses_accelerated_and_shadow_before_jobs(self):
        for mode in ['accelerated', 'shadow', 'unknown']:
            with self.assertRaises(receipts.ReceiptError): q.require_store_mode({'store_contract':{}},mode)
        q.require_store_mode({'store_contract':{}},'legacy')
        q.require_store_mode({},'shadow')

    def test_owned_command_timeout_and_output_limit_refuse(self):
        for command,timeout,limit in [([sys.executable,'-c','import time;time.sleep(5)'],0.05,1024),
                                       ([sys.executable,'-c','print("x"*65536)'],5,1024)]:
            with self.assertRaises(receipts.ReceiptError):q.owned_output(command,timeout=timeout,limit=limit)

    def test_local_hash_malformed_boundary_and_target_mismatch_refuse(self):
        for change in [lambda w:w.update(binary_sha256_after='e'*64),
                       lambda w:w.update(first_line=2),
                       lambda w:w.update(build_invocation=w['build_invocation'].replace('--test store_contract','--test different'))]:
            proof=fixture();change(proof['commands'][0]['records'][0]['origin']['execution'])
            with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_complete_block_cannot_crop_a_failed_outcome(self):
        proof=fixture();row=proof['commands'][0]['records'][0]
        original=q.command_log(proof,row);raw=log(original.replace('NATIVE_EXIT 0','test hidden ... FAILED\nNATIVE_EXIT 0'))
        proof['logs'][raw['sha256']]=raw;row['log']={'sha256_ref':raw['sha256']}
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_pinned_absolute_executor_is_narrow_and_command_semantics_stay_bound(self):
        absolute='/home/runner/.rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu/bin/cargo'
        actual=q.normalized_cargo(COMMAND.replace('$(CARGO)',absolute))
        q.same_target(actual,q.rust_identity(COMMAND))
        for other in ['/tmp/cargo','/home/runner/.rustup/toolchains/stable/bin/cargo']:
            with self.assertRaises(receipts.ReceiptError):q.normalized_cargo(COMMAND.replace('$(CARGO)',other))
        for change in ['--test different','--features different']:
            altered=COMMAND.replace('--test store_contract',change) if change.startswith('--test') else COMMAND.replace('--features cluster-read-cost-validation,hiqlite-contract-tests',change)
            with self.assertRaises(receipts.ReceiptError):q.same_target(q.rust_identity(COMMAND),q.rust_identity(altered))

    def test_generic_failure_reason_and_ambiguous_count_diagnostics_refuse(self):
        command=COMMAND.replace('RUST_MIN_STACK=8388608','PLURX_EXPECT_TEST_COUNT=2 RUST_MIN_STACK=8388608')
        old=command.replace('PLURX_EXPECT_TEST_COUNT=2','PLURX_EXPECT_TEST_COUNT=1').replace('$(CARGO)','scripts/require-test-count cargo')
        item={'invocation':command,'obligations':[{'sequence':1,'category':'stale-count-wrapper'}]}
        record={'sequence':1,'exit_code':1,'source_commit':CANDIDATE,'origin':{'command':old},'coverage':{'outcomes':{'a':'pass','b':'pass'}}}
        terminal='test result: ok. 2 passed; 0 failed; 0 ignored;\nexpected 1 matching Rust test(s), but 2 passed\n'
        for body in [terminal+terminal,terminal+'fatal: unexplained failure\n',terminal.replace('expected 1','expected 3')]:
            with self.assertRaises(receipts.ReceiptError):q.completion_obligations(item,[record],{'a':'pass','b':'pass'},{1:body})
        item['obligations'][0]['category']='arbitrary-fixed-command'
        with self.assertRaises(receipts.ReceiptError):q.completion_obligations(item,[record],{'a':'pass','b':'pass'},{1:terminal})

    def test_current_wrapper_count_minimum_and_required_name_predicates_are_real(self):
        for prefix in ['PLURX_EXPECT_TEST_COUNT=3','PLURX_EXPECT_TEST_COUNT_AT_LEAST=3','PLURX_EXPECT_TEST_NAMES=missing']:
            item={'invocation':prefix+' '+COMMAND,'obligations':[]}
            with self.assertRaises(receipts.ReceiptError):q.completion_obligations(item,[],{'a':'pass','b':'pass'},{})

    def test_implicit_target_cannot_hide_a_matching_case_in_an_auxiliary_binary(self):
        identity=q.rust_identity('$(CARGO) test --locked -p plurx-cluster-check topology:: -- --nocapture')
        target={'name':'plurx_cluster_check','kind':['lib'],'binary_sha256':'c'*64,'features':[],'list_log':log('topology::a: test\n')}
        proof={'source_commit':CANDIDATE,'ids':['topology::a'],'targets':[target]}
        self.assertEqual(q.target_inventory(proof,CANDIDATE,identity),['topology::a'])
        auxiliary=copy.deepcopy(target);auxiliary.update(name='harness',kind=['test'],list_log=log('topology::hidden: test\n'))
        proof['targets'].append(auxiliary)
        with self.assertRaises(receipts.ReceiptError):q.target_inventory(proof,CANDIDATE,identity)

    def test_cancellation_during_retirement_does_not_skip_wait_or_restoration(self):
        import os,signal
        from unittest.mock import Mock
        read,write=os.pipe();os.write(write,b'over limit');os.close(write)
        process=Mock(pid=123,stdout=os.fdopen(read,'rb'))
        before={signum:signal.getsignal(signum) for signum in (signal.SIGTERM,signal.SIGINT)}
        def retire(**_):
            handler=signal.getsignal(signal.SIGTERM);handler(signal.SIGTERM,None);handler(signal.SIGTERM,None)
            return -signal.SIGKILL
        process.wait.side_effect=retire
        with patch.object(q.subprocess,'Popen',return_value=process),patch.object(q.os,'killpg') as kill:
            with self.assertRaises(receipts.ReceiptError):q.owned_output(['cargo'],timeout=1,limit=1)
        kill.assert_called_once_with(123,signal.SIGKILL);process.wait.assert_called_once_with(timeout=10)
        self.assertTrue(process.stdout.closed)
        self.assertEqual(before,{signum:signal.getsignal(signum) for signum in before})

    def test_successful_child_cancelled_during_retirement_does_not_return_success(self):
        import os,signal
        from unittest.mock import Mock
        read,write=os.pipe();os.close(write)
        process=Mock(pid=123,stdout=os.fdopen(read,'rb'),returncode=0)
        process.poll.return_value=0
        before={signum:signal.getsignal(signum) for signum in (signal.SIGTERM,signal.SIGINT)}
        def retire(**_):
            signal.getsignal(signal.SIGTERM)(signal.SIGTERM,None)
            return 0
        process.wait.side_effect=retire
        with patch.object(q.subprocess,'Popen',return_value=process),patch.object(q.os,'killpg'):
            with self.assertRaisesRegex(receipts.ReceiptError,'cancelled'):
                q.owned_output(['cargo'],timeout=1,limit=1024)
        process.wait.assert_called_once_with(timeout=10)
        self.assertTrue(process.stdout.closed)
        self.assertEqual(before,{signum:signal.getsignal(signum) for signum in before})

    def test_actual_wrapper_failure_cannot_be_labelled_exit_zero(self):
        old='PLURX_EXPECT_TEST_COUNT=1 '+COMMAND.replace('$(CARGO)','scripts/require-test-count cargo')
        body='test a ... ok\ntest b ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored;\nexpected 1 matching Rust test(s), but 2 passed\n'
        with self.assertRaises(receipts.ReceiptError):q.require_native_terminals(body,{'a':'pass','b':'pass'},0,old)
        body=body.split('expected 1')[0]
        with self.assertRaises(receipts.ReceiptError):q.require_native_terminals(body,{'a':'pass','b':'pass'},0,old)

    def test_positive_native_rows_require_complete_target_terminal(self):
        body='test a ... ok\n'
        with self.assertRaises(receipts.ReceiptError):q.require_native_terminals(body,{'a':'pass'},0,COMMAND.replace('$(CARGO)','cargo'))

    def test_current_fingerprint_refuses_hidden_compiler_target_or_flags(self):
        import tempfile,json
        with tempfile.TemporaryDirectory() as temporary:
            root=q.Path(temporary)/'debug';binary=root/'deps'/'store_contract-deadbeef'
            fingerprint=root/'.fingerprint'/'plurx-core-deadbeef'/'test-integration-test-store_contract.json'
            fingerprint.parent.mkdir(parents=True)
            artifact={'executable':str(binary),'features':[]}
            base={'rustc':q.RUSTC_FINGERPRINT,'profile':q.TEST_PROFILE_FINGERPRINTS[0],
                  'compile_kind':0,'rustflags':[],'features':'[]'}
            profile=fixture()['commands'][0]['environment']['runtime']['profile']
            fingerprint.write_text(json.dumps(base));q.require_current_fingerprint(artifact,profile)
            for key,value in [('rustc',0),('profile',0),('compile_kind',1),('rustflags',['--cfg','hidden'])]:
                fingerprint.write_text(json.dumps({**base,key:value}))
                with self.assertRaises(receipts.ReceiptError):q.require_current_fingerprint(artifact,profile)

    def test_semantic_cargo_environment_overrides_refuse(self):
        q.require_unmodified_cargo_environment({'CARGO_TARGET_DIR':'/owned/target','CARGO_BUILD_JOBS':'1'})
        for key in ['RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','CARGO_BUILD_TARGET','RUSTC',
                    'RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER','CARGO_PROFILE_TEST_DEBUG',
                    'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS']:
            with self.assertRaises(receipts.ReceiptError):q.require_unmodified_cargo_environment({key:'override'})

    def test_rust_environment_unknown_platform_profile_or_runtime_extra_refuse(self):
        for mutate in [lambda env:env.update(platform='unknown'),
                       lambda env:env['runtime'].update(unreviewed_flag=True),
                       lambda env:env['runtime'].update(target='aarch64-apple-darwin'),
                       lambda env:env['runtime']['profile'].update(debuginfo=2)]:
            proof=fixture();mutate(proof['commands'][0]['environment'])
            with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_same_suffix_wrong_native_target_or_fingerprint_refuses(self):
        for key,value in [('binary_path','/owned/different-deadbeef'),
                          ('fingerprint_path','/owned/.fingerprint/plurx-core-deadbeef/test-integration-test-other.json'),
                          ('fingerprint_path','/owned/.fingerprint/other-package-deadbeef/test-integration-test-store_contract.json')]:
            proof=fixture();record=proof['commands'][0]['records'][0]
            record['origin']['execution'][key]=value
            if key=='binary_path':
                import json
                raw=q.command_log(proof,record).replace('/owned/store_contract-deadbeef',value)
                bound=log(raw);proof['logs'][bound['sha256']]=bound;record['log']['sha256_ref']=bound['sha256']
                argv=json.loads(raw.splitlines()[0].removeprefix('COMMAND '));record['origin']['command']=shlex.join(argv)
            with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_native_argv_must_be_the_preserved_fingerprint_binary(self):
        for key,value in [('binary_path','/different/store_contract-deadbeef'),('fingerprint_path','/owned/.fingerprint/plurx-core-ffffffff/test-integration-test-store_contract.json')]:
            proof=fixture();proof['commands'][0]['records'][0]['origin']['execution'][key]=value
            with self.assertRaises(receipts.ReceiptError):self.validate(proof)
